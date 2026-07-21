use super::{dedup_paths, ok_or_flag, parse_ts, title_from_messages, Adapter, Discovered};
use crate::model::{Message, Role, Session};
use crate::util::{short_id, truncate};
use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Codex CLI stores one JSONL rollout per session under
/// `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl`.
/// Lines carry a `type` plus a `payload`; the schema has shifted across
/// versions, so both `response_item` and `event_msg` shapes are handled.
pub struct Codex;

impl Adapter for Codex {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn root(&self) -> Option<PathBuf> {
        Some(dirs::home_dir()?.join(".codex").join("sessions"))
    }

    fn discover(&self) -> Discovered {
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
            .filter(|e| {
                let name = e.file_name().to_string_lossy();
                name.starts_with("rollout-") && name.ends_with(".jsonl")
            })
            .map(|e| e.into_path())
            .collect();
        Discovered { files, had_error }
    }

    fn parse(&self, path: &Path) -> Result<Session> {
        // Over the byte cap: window (head+tail) instead of dropping the session.
        let (lines, windowed) = crate::util::session_lines(path)?;

        let mut messages: Vec<Message> = Vec::new();
        let mut touched: Vec<String> = Vec::new();
        let mut cwd: Option<String> = None;
        let mut started = None;
        let mut ended = None;
        // Current rollouts carry each user prompt TWICE (event_msg AND
        // response_item). Counted multisets cancel each pair exactly once, so
        // a genuinely repeated prompt ("continue" twice) still indexes twice.
        let mut response_user_texts: HashMap<String, u32> = HashMap::new();
        let mut event_user_texts: HashMap<String, u32> = HashMap::new();

        for line in &lines {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };

            let ts = v
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(parse_ts);
            if let Some(t) = ts {
                if started.is_none() {
                    started = Some(t);
                }
                ended = Some(t);
            }

            match v.get("type").and_then(Value::as_str) {
                Some("session_meta") => {
                    if cwd.is_none() {
                        cwd = v
                            .pointer("/payload/cwd")
                            .and_then(Value::as_str)
                            .map(String::from);
                    }
                }
                Some("response_item") => {
                    match v.pointer("/payload/type").and_then(Value::as_str) {
                        Some("message") => {
                            let role = match v.pointer("/payload/role").and_then(Value::as_str) {
                                Some("user") => Role::User,
                                Some("assistant") => Role::Assistant,
                                _ => continue,
                            };
                            let Some(Value::Array(blocks)) = v.pointer("/payload/content") else {
                                continue;
                            };
                            for b in blocks {
                                let Some(text) = b.get("text").and_then(Value::as_str) else {
                                    continue;
                                };
                                if role == Role::User && is_boilerplate(text) {
                                    continue;
                                }
                                if role == Role::User {
                                    if let Some(n) =
                                        event_user_texts.get_mut(text).filter(|n| **n > 0)
                                    {
                                        *n -= 1; // pairs with an already-pushed event_msg
                                        continue;
                                    }
                                    *response_user_texts.entry(text.to_string()).or_insert(0) += 1;
                                }
                                push(&mut messages, role, text, ts);
                            }
                        }
                        Some("function_call" | "custom_tool_call") => {
                            let name = v
                                .pointer("/payload/name")
                                .and_then(Value::as_str)
                                .unwrap_or("?");
                            let args = v
                                .pointer("/payload/arguments")
                                .or_else(|| v.pointer("/payload/input"))
                                .and_then(Value::as_str)
                                .unwrap_or("");
                            // Codex applies edits via an apply_patch envelope,
                            // whether the call is named apply_patch or a shell
                            // wrapping it. Scan the full args for the file
                            // markers before the message text is truncated.
                            collect_patched_paths(args, &mut touched);
                            let text = format!("{name} {}", truncate(args, 300));
                            push(&mut messages, Role::Tool, &text, ts);
                        }
                        // function_call_output and reasoning are skipped on
                        // purpose: they dominate file size and pollute search.
                        _ => {}
                    }
                }
                Some("event_msg") => match v.pointer("/payload/type").and_then(Value::as_str) {
                    Some("user_message") => {
                        if let Some(t) = v.pointer("/payload/message").and_then(Value::as_str) {
                            if !is_boilerplate(t) {
                                if let Some(n) = response_user_texts.get_mut(t).filter(|n| **n > 0)
                                {
                                    *n -= 1; // pairs with an already-pushed response_item
                                } else {
                                    *event_user_texts.entry(t.to_string()).or_insert(0) += 1;
                                    push(&mut messages, Role::User, t, ts);
                                }
                            }
                        }
                    }
                    Some("agent_message") => {
                        if let Some(t) = v.pointer("/payload/message").and_then(Value::as_str) {
                            push(&mut messages, Role::Assistant, t, ts);
                        }
                    }
                    _ => {}
                },
                Some("message") => {
                    let role = match v.get("role").and_then(Value::as_str) {
                        Some("user") => Role::User,
                        Some("assistant") => Role::Assistant,
                        _ => continue,
                    };
                    let Some(Value::Array(blocks)) = v.get("content") else {
                        continue;
                    };
                    for b in blocks {
                        let Some(text) = b.get("text").and_then(Value::as_str) else {
                            continue;
                        };
                        if role == Role::User && is_boilerplate(text) {
                            continue;
                        }
                        push(&mut messages, role, text, ts);
                    }
                }
                _ => {}
            }
        }

        let project = cwd.unwrap_or_default();
        let title = if windowed {
            format!("[large] {}", title_from_messages(&messages))
        } else {
            title_from_messages(&messages)
        };

        Ok(Session {
            id: short_id(&path.to_string_lossy()),
            tool: self.name(),
            path: path.to_path_buf(),
            project,
            started,
            ended,
            title,
            subagent: false,
            messages,
            touched: dedup_paths(touched),
            edits: Vec::new(),
        })
    }
}

/// Extract the files an apply_patch touched from the raw call arguments. The
/// patch format names each file on a header line - `*** Add File: path`,
/// `*** Update File: path`, `*** Delete File: path`, `*** Move to: path` -
/// regardless of whether the call arrives as a dedicated apply_patch function
/// or a shell command wrapping a heredoc. Newlines may be JSON-escaped (\\n)
/// when the patch is embedded in an arguments string, so handle both.
fn collect_patched_paths(args: &str, out: &mut Vec<String>) {
    const MARKERS: [&str; 4] = [
        "*** Add File: ",
        "*** Update File: ",
        "*** Delete File: ",
        "*** Move to: ",
    ];
    let normalized = args.replace("\\n", "\n");
    for line in normalized.lines() {
        let line = line.trim();
        for m in MARKERS {
            if let Some(rest) = line.strip_prefix(m) {
                let path = rest.trim().trim_matches('"');
                if !path.is_empty() {
                    out.push(path.to_string());
                }
            }
        }
    }
}

/// Codex wraps instructions and environment dumps in pseudo-XML tags and
/// repeats them in every session. Indexing them buries real matches.
fn is_boilerplate(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("<user_instructions>")
        || t.starts_with("<environment_context>")
        || t.starts_with("<ENVIRONMENT_CONTEXT>")
        || t.starts_with("<turn_context>")
        || t.starts_with("# AGENTS.md instructions")
        || t.starts_with("<INSTRUCTIONS>")
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
