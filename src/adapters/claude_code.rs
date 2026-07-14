use super::{dedup_paths, ok_or_flag, parse_ts, title_from_messages, Adapter, Discovered};
use crate::model::{Message, Role, Session};
use crate::util::{short_id, truncate};
use anyhow::{Context, Result};
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Claude Code stores one JSONL file per session under
/// `~/.claude/projects/<sanitized-cwd>/<session-uuid>.jsonl`.
/// Each line is an event: user/assistant messages, tool results,
/// summaries, and harness bookkeeping.
pub struct ClaudeCode;

impl Adapter for ClaudeCode {
    fn name(&self) -> &'static str {
        "claude-code"
    }

    fn root(&self) -> Option<PathBuf> {
        Some(dirs::home_dir()?.join(".claude").join("projects"))
    }

    fn discover(&self) -> Discovered {
        // Main sessions live at <project>/<uuid>.jsonl; subagent transcripts
        // at <project>/<uuid>/subagents/agent-*.jsonl and nest further when
        // subagents spawn subagents, so no depth limit here.
        let Some(root) = self.root() else {
            return Vec::new().into();
        };
        if !root.exists() {
            return Vec::new().into(); // no store on this machine - normal
        }
        let mut had_error = false;
        let files = WalkDir::new(root)
            .into_iter()
            .filter_map(|e| ok_or_flag(e, &mut had_error))
            .filter(|e| e.file_type().is_file())
            .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
            .map(|e| e.into_path())
            .collect();
        Discovered { files, had_error }
    }

    fn parse(&self, path: &Path) -> Result<Session> {
        let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        if file.metadata().map(|m| m.len()).unwrap_or(0) > crate::util::MAX_SESSION_FILE_BYTES {
            anyhow::bail!("{} is over the size cap; skipping", path.display());
        }
        let reader = BufReader::new(file);

        let mut messages: Vec<Message> = Vec::new();
        let mut touched: Vec<String> = Vec::new();
        let mut cwd: Option<String> = None;
        let mut summary: Option<String> = None;
        let mut ai_title: Option<String> = None;
        let mut started = None;
        let mut ended = None;

        for line in reader.lines() {
            let Ok(line) = line else { continue };
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };

            if cwd.is_none() {
                if let Some(c) = v.get("cwd").and_then(Value::as_str) {
                    cwd = Some(c.to_string());
                }
            }
            if let Some(ts) = v
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(parse_ts)
            {
                if started.is_none() {
                    started = Some(ts);
                }
                ended = Some(ts);
            }

            match v.get("type").and_then(Value::as_str) {
                Some("summary") => {
                    if summary.is_none() {
                        summary = v.get("summary").and_then(Value::as_str).map(String::from);
                    }
                }
                Some("ai-title") => {
                    if ai_title.is_none() {
                        ai_title = v.get("aiTitle").and_then(Value::as_str).map(String::from);
                    }
                }
                Some("user") => {
                    // Skip harness meta lines; keep real prompts and tool results.
                    if v.get("isMeta").and_then(Value::as_bool) == Some(true) {
                        continue;
                    }
                    let ts = v
                        .get("timestamp")
                        .and_then(Value::as_str)
                        .and_then(parse_ts);
                    let Some(content) = v.pointer("/message/content") else {
                        continue;
                    };
                    match content {
                        Value::String(s) => push(&mut messages, Role::User, s, ts),
                        Value::Array(blocks) => {
                            for b in blocks {
                                match b.get("type").and_then(Value::as_str) {
                                    Some("text") => {
                                        if let Some(t) = b.get("text").and_then(Value::as_str) {
                                            push(&mut messages, Role::User, t, ts);
                                        }
                                    }
                                    Some("tool_result") => {
                                        let t = block_text(b.get("content"));
                                        if !t.is_empty() {
                                            push(&mut messages, Role::Tool, &truncate(&t, 500), ts);
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Some("assistant") => {
                    let ts = v
                        .get("timestamp")
                        .and_then(Value::as_str)
                        .and_then(parse_ts);
                    let Some(Value::Array(blocks)) = v.pointer("/message/content") else {
                        continue;
                    };
                    for b in blocks {
                        match b.get("type").and_then(Value::as_str) {
                            Some("text") => {
                                if let Some(t) = b.get("text").and_then(Value::as_str) {
                                    push(&mut messages, Role::Assistant, t, ts);
                                }
                            }
                            Some("tool_use") => {
                                let name = b.get("name").and_then(Value::as_str).unwrap_or("?");
                                if let Some(p) = edited_path(name, b.get("input")) {
                                    touched.push(p);
                                }
                                let input =
                                    b.get("input").map(|i| i.to_string()).unwrap_or_default();
                                let text = format!("{name} {}", truncate(&input, 300));
                                push(&mut messages, Role::Tool, &text, ts);
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }

        let project = cwd.unwrap_or_else(|| {
            // Fall back to the sanitized directory name.
            path.parent()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        let title = summary
            .or(ai_title)
            .map(|s| truncate(&s, 80))
            .unwrap_or_else(|| title_from_messages(&messages));
        let subagent = path.to_string_lossy().contains("/subagents/");

        Ok(Session {
            id: short_id(&path.to_string_lossy()),
            tool: self.name(),
            path: path.to_path_buf(),
            project,
            started,
            ended,
            title,
            subagent,
            messages,
            touched: dedup_paths(touched),
        })
    }
}

/// Pull the file a Claude Code edit tool acted on from its `input`. Only the
/// tools that write to disk count; reads, searches and shell commands do not
/// establish authorship. The field name varies by tool (`file_path`,
/// `notebook_path`, or the generic `path`).
fn edited_path(name: &str, input: Option<&Value>) -> Option<String> {
    let writes = matches!(
        name,
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" | "str_replace_based_edit_tool"
    );
    if !writes {
        return None;
    }
    let input = input?;
    for key in ["file_path", "notebook_path", "path"] {
        if let Some(p) = input.get(key).and_then(Value::as_str) {
            if !p.is_empty() {
                return Some(p.to_string());
            }
        }
    }
    None
}

fn push(
    messages: &mut Vec<Message>,
    role: Role,
    text: &str,
    ts: Option<chrono::DateTime<chrono::Utc>>,
) {
    let text = text.trim();
    if !text.is_empty() {
        messages.push(Message {
            role,
            text: text.to_string(),
            ts,
        });
    }
}

/// tool_result content is either a string or an array of text blocks.
fn block_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}
