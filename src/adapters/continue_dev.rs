use super::{dedup_paths, ok_or_flag, title_from_messages, Adapter, Discovered};
use crate::model::{Message, Role, Session};
use crate::util::{short_id, truncate};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Continue (github continuedev/continue) stores one session per file at
/// `~/.continue/sessions/<sessionId>.json`, plus a `sessions.json` index. The
/// session file has no timestamps; the only time signal is `dateCreated`
/// (epoch-ms as a string) in the index, which we read for `started`.
///
/// `history` is an array of items shaped `{ message: {role, content, toolCalls},
/// toolCallStates: [...] }` - role/text live under `message`. File edits are in
/// `toolCallStates[].parsedArgs.filepath` (already parsed) or, failing that, in
/// `message.toolCalls[].function.arguments` (a JSON-encoded string).
pub struct Continue;

impl Adapter for Continue {
    fn name(&self) -> &'static str {
        "continue"
    }

    fn root(&self) -> Option<PathBuf> {
        Some(continue_dir()?.join("sessions"))
    }

    fn discover(&self) -> Discovered {
        let Some(dir) = self.root() else {
            return Vec::new().into();
        };
        if !dir.exists() {
            return Vec::new().into(); // no store on this machine - normal
        }
        let mut had_error = false;
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            // The directory exists but cannot be listed: a PARTIAL (empty)
            // result. Without the flag, reconciliation would archive every
            // Continue session over a transient permission error.
            Err(_) => {
                return Discovered {
                    files: vec![],
                    had_error: true,
                }
            }
        };
        let files = entries
            .filter_map(|e| ok_or_flag(e, &mut had_error))
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            // The index lives beside the sessions; it is not one.
            .filter(|p| p.file_name().is_some_and(|n| n != "sessions.json"))
            .collect();
        Discovered { files, had_error }
    }

    fn parse(&self, path: &Path) -> Result<Session> {
        parse_session(self.name(), path)
    }
}

/// `$CONTINUE_GLOBAL_DIR` overrides the tree on every OS; otherwise `~/.continue`.
fn continue_dir() -> Option<PathBuf> {
    std::env::var_os("CONTINUE_GLOBAL_DIR")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| dirs::home_dir().map(|h| h.join(".continue")))
}

/// Tools whose args name a file the session created or edited. The path is
/// always `filepath`. Read/search/ls tools are excluded.
const EDIT_TOOLS: &[&str] = &[
    "create_new_file",
    "edit_existing_file",
    "single_find_and_replace",
    "multi_edit",
];

fn parse_session(tool: &'static str, path: &Path) -> Result<Session> {
    let raw = crate::util::read_to_string_capped(path)?;
    let s: Value =
        serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;

    let project = s
        .get("workspaceDirectory")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let stored_title = s
        .get("title")
        .and_then(Value::as_str)
        .filter(|t| !t.trim().is_empty() && *t != "New Session")
        .map(|t| truncate(t, 80));

    let mut messages: Vec<Message> = Vec::new();
    let mut touched: Vec<String> = Vec::new();

    if let Some(history) = s.get("history").and_then(Value::as_array) {
        for item in history {
            let msg = item.get("message");
            let role = match msg.and_then(|m| m.get("role")).and_then(Value::as_str) {
                Some("user") => Role::User,
                Some("assistant") | Some("thinking") => Role::Assistant,
                Some("tool") => Role::Tool,
                // system / unknown - skip.
                _ => continue,
            };
            if let Some(text) = msg.and_then(|m| m.get("content")).map(content_text) {
                push(&mut messages, role, &text);
            }

            // File edits: prefer the already-parsed toolCallStates, fall back to
            // the raw toolCalls on the message.
            let calls = tool_calls(item);
            for (name, filepath, arg) in calls {
                if EDIT_TOOLS.contains(&name.as_str()) && !filepath.is_empty() {
                    touched.push(filepath.clone());
                }
                let detail = if filepath.is_empty() { arg } else { filepath };
                push(
                    &mut messages,
                    Role::Tool,
                    &format!("{name} {}", truncate(&detail, 300)),
                );
            }
        }
    }

    let title = stored_title.unwrap_or_else(|| title_from_messages(&messages));
    let started = started_from_index(path);

    Ok(Session {
        id: short_id(&path.to_string_lossy()),
        tool,
        path: path.to_path_buf(),
        project,
        started,
        ended: None,
        title,
        subagent: false,
        messages,
        touched: dedup_paths(touched),
        edits: Vec::new(),
    })
}

/// `content` is `string | MessagePart[]`; join the text parts, drop images.
fn content_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// `(tool name, filepath, raw-args)` for each tool call on a history item.
/// `toolCallStates[].parsedArgs` is preferred (already an object); otherwise
/// parse the JSON string in `message.toolCalls[].function.arguments`.
fn tool_calls(item: &Value) -> Vec<(String, String, String)> {
    if let Some(states) = item.get("toolCallStates").and_then(Value::as_array) {
        if !states.is_empty() {
            return states
                .iter()
                .map(|st| {
                    let name = st
                        .pointer("/toolCall/function/name")
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                        .to_string();
                    let filepath = st
                        .pointer("/parsedArgs/filepath")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let arg = st
                        .get("parsedArgs")
                        .map(|v| v.to_string())
                        .unwrap_or_default();
                    (name, filepath, arg)
                })
                .collect();
        }
    }
    item.get("message")
        .and_then(|m| m.get("toolCalls"))
        .and_then(Value::as_array)
        .map(|calls| {
            calls
                .iter()
                .map(|c| {
                    let name = c
                        .pointer("/function/name")
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                        .to_string();
                    let arg = c
                        .pointer("/function/arguments")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    // arguments is a JSON-encoded string.
                    let filepath = serde_json::from_str::<Value>(&arg)
                        .ok()
                        .and_then(|v| v.get("filepath").and_then(Value::as_str).map(String::from))
                        .unwrap_or_default();
                    (name, filepath, arg)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The session file carries no time; `sessions.json` records `dateCreated`
/// (epoch ms, as a string) per id. Match by the file's stem.
fn started_from_index(path: &Path) -> Option<DateTime<Utc>> {
    let id = path.file_stem()?.to_string_lossy();
    let index = path.parent()?.join("sessions.json");
    let raw = crate::util::read_to_string_capped(&index).ok()?;
    let list: Value = serde_json::from_str(&raw).ok()?;
    let created = list.as_array()?.iter().find_map(|e| {
        let sid = e.get("sessionId").and_then(Value::as_str)?;
        if sid == id {
            e.get("dateCreated").and_then(Value::as_str)
        } else {
            None
        }
    })?;
    DateTime::from_timestamp_millis(created.parse::<i64>().ok()?)
}

fn push(messages: &mut Vec<Message>, role: Role, text: &str) {
    let text = text.trim();
    if !text.is_empty() {
        messages.push(Message {
            role,
            text: text.to_string(),
            ts: None,
        });
    }
}
