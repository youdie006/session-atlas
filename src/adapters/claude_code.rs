use super::{
    clean_path, dedup_paths, ok_or_flag, parse_ts, title_from_messages, Adapter, Discovered,
};
use crate::model::{Message, Role, Session};
use crate::util::{short_id, truncate};
use anyhow::Result;
use serde_json::Value;
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
        // A session over the byte cap is WINDOWED (head+tail) rather than
        // dropped, so the biggest sessions - the ones most worth referencing -
        // stay searchable and partially readable.
        let (lines, windowed) = crate::util::session_lines(path)?;

        let mut messages: Vec<Message> = Vec::new();
        let mut touched: Vec<String> = Vec::new();
        let mut edits: Vec<crate::model::EditEvent> = Vec::new();
        let mut cwd: Option<String> = None;
        let mut summary: Option<String> = None;
        let mut ai_title: Option<String> = None;
        let mut started = None;
        let mut ended = None;

        for line in &lines {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
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
                                if let Some(ev) = edit_event(name, b.get("input"), ts) {
                                    touched.push(ev.path.clone());
                                    edits.push(ev);
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
        let base_title = summary
            .or(ai_title)
            .map(|s| truncate(&s, 80))
            .unwrap_or_else(|| title_from_messages(&messages));
        // Flag a windowed session so search/list/window make the partial read obvious.
        let title = if windowed {
            format!("[large] {base_title}")
        } else {
            base_title
        };
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
            edits,
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

const SNIPPET_CAP: usize = 200;

/// Extract the structured edit evidence from one write tool call - the richer
/// sibling of `edited_path`: not just WHICH file, but what kind of change and a
/// bounded snippet of it. Returns None for non-write tools.
fn edit_event(
    name: &str,
    input: Option<&Value>,
    ts: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<crate::model::EditEvent> {
    use crate::model::{EditEvent, EditKind};
    // clean_path applies the SAME hygiene dedup_paths gives `touched`, so an
    // edits row never survives for a path touched would have dropped.
    let path = clean_path(&edited_path(name, input)?)?;
    let input = input?;
    // Every name edited_path() accepts must map here, so `edits` and `touched`
    // never diverge (a touched path with no evidence, or vice versa).
    let kind = match name {
        "Edit" | "str_replace_based_edit_tool" => EditKind::Edit,
        "Write" => EditKind::Write,
        "MultiEdit" => EditKind::MultiEdit,
        "NotebookEdit" => EditKind::NotebookEdit,
        _ => return None,
    };
    let raw = match kind {
        // str_replace_based_edit_tool names the field new_str / file_text.
        EditKind::Edit => str_field(input, &["new_string", "new_str", "file_text"]),
        EditKind::Write => str_field(input, &["content"]),
        EditKind::MultiEdit => input
            .get("edits")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(|e| e.get("new_string").and_then(Value::as_str))
            .unwrap_or(""),
        EditKind::NotebookEdit => str_field(input, &["new_source"]),
    };
    let snippet = truncate(raw.trim(), SNIPPET_CAP);
    Some(EditEvent {
        path,
        kind,
        snippet,
        ts,
    })
}

/// First present string among `keys`, or "".
fn str_field<'a>(v: &'a Value, keys: &[&str]) -> &'a str {
    keys.iter()
        .find_map(|k| v.get(k).and_then(Value::as_str))
        .unwrap_or("")
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::EditKind;
    use serde_json::json;

    #[test]
    fn edit_event_captures_kind_and_new_string_snippet() {
        let input = json!({
            "file_path": "/repo/src/auth.rs",
            "old_string": "let x = 1;",
            "new_string": "let x = 2; // fixed",
        });
        let ev = edit_event("Edit", Some(&input), None).expect("Edit yields an edit event");
        assert_eq!(ev.path, "/repo/src/auth.rs");
        assert_eq!(ev.kind, EditKind::Edit);
        assert!(
            ev.snippet.contains("let x = 2;"),
            "snippet should show the new code, got: {:?}",
            ev.snippet
        );
    }

    #[test]
    fn write_event_snippet_comes_from_content() {
        let input = json!({ "file_path": "/repo/new.rs", "content": "fn main() {}\n" });
        let ev = edit_event("Write", Some(&input), None).expect("Write yields an edit event");
        assert_eq!(ev.kind, EditKind::Write);
        assert!(ev.snippet.contains("fn main()"), "got: {:?}", ev.snippet);
    }

    #[test]
    fn multiedit_event_snippet_comes_from_first_edit() {
        let input = json!({
            "file_path": "/repo/a.rs",
            "edits": [
                { "old_string": "a", "new_string": "alpha" },
                { "old_string": "b", "new_string": "beta" },
            ],
        });
        let ev =
            edit_event("MultiEdit", Some(&input), None).expect("MultiEdit yields an edit event");
        assert_eq!(ev.kind, EditKind::MultiEdit);
        assert!(ev.snippet.contains("alpha"), "got: {:?}", ev.snippet);
    }

    #[test]
    fn non_write_tools_are_not_edit_events() {
        let input = json!({ "command": "ls", "description": "list" });
        assert!(edit_event("Bash", Some(&input), None).is_none());
        assert!(edit_event("Read", Some(&json!({ "file_path": "/x" })), None).is_none());
    }

    #[test]
    fn edit_event_cleans_the_path_like_touched() {
        // Whitespace is trimmed so edits.path matches the dedup_paths-cleaned
        // touched path (they must not diverge).
        let ev = edit_event(
            "Edit",
            Some(&json!({ "file_path": "  /repo/a.rs  ", "new_string": "x" })),
            None,
        )
        .expect("a trimmable path still yields an edit");
        assert_eq!(ev.path, "/repo/a.rs");
        // A path the touched filter rejects (embedded newline) yields no edit
        // either - never an edits-only record touched would have dropped.
        assert!(edit_event(
            "Edit",
            Some(&json!({ "file_path": "/re\npo/a.rs", "new_string": "x" })),
            None
        )
        .is_none());
    }

    #[test]
    fn parse_populates_edits_alongside_touched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sess.jsonl");
        let line = r#"{"type":"assistant","timestamp":"2026-07-01T10:00:00Z","message":{"content":[{"type":"tool_use","name":"Edit","input":{"file_path":"/repo/src/auth.rs","old_string":"a","new_string":"let fixed = true;"}}]}}"#;
        std::fs::write(&path, format!("{line}\n")).unwrap();

        let session = ClaudeCode.parse(&path).unwrap();

        assert_eq!(session.edits.len(), 1, "one edit event extracted");
        assert_eq!(session.edits[0].path, "/repo/src/auth.rs");
        assert_eq!(session.edits[0].kind, EditKind::Edit);
        assert!(session.edits[0].snippet.contains("let fixed = true;"));
        // touched stays consistent with edits (same path).
        assert_eq!(session.touched, vec!["/repo/src/auth.rs".to_string()]);
    }
}
