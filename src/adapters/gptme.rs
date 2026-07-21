use super::{ok_or_flag, parse_ts, title_from_messages, Adapter, Discovered};
use crate::model::{Message, Role, Session};
use crate::util::short_id;
use anyhow::Result;
use chrono::NaiveDateTime;
use serde_json::Value;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// gptme stores one JSONL file per session under
/// `~/.local/share/gptme/logs/<session-name>/conversation.jsonl`.
/// Each line is a message with `role`, `content`, and `timestamp` fields.
/// Lines with `"pinned": true` are system-prompt boilerplate injected at
/// startup; `"system"` role lines are context injections or compaction
/// notices; both are dropped so only the real conversation is indexed.
pub struct Gptme;

impl Adapter for Gptme {
    fn name(&self) -> &'static str {
        "gptme"
    }

    fn root(&self) -> Option<PathBuf> {
        Some(dirs::data_local_dir()?.join("gptme").join("logs"))
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
            .max_depth(2)
            .into_iter()
            .filter_map(|e| ok_or_flag(e, &mut had_error))
            .filter(|e| e.file_type().is_file())
            .filter(|e| e.file_name().to_string_lossy() == "conversation.jsonl")
            .map(|e| e.into_path())
            .collect();
        Discovered { files, had_error }
    }

    fn parse(&self, path: &Path) -> Result<Session> {
        // Over the byte cap: window (head+tail) instead of dropping the session.
        let (lines, windowed) = crate::util::session_lines(path)?;

        let mut messages: Vec<Message> = Vec::new();
        let mut started = None;
        let mut ended = None;

        for line in &lines {
            let Ok(Value::Object(mut v)) = serde_json::from_str::<Value>(line) else {
                continue;
            };

            // Drop pinned lines (system-prompt boilerplate injected at startup).
            if v.get("pinned").and_then(Value::as_bool).unwrap_or(false) {
                continue;
            }

            let role = match v.get("role").and_then(Value::as_str) {
                Some("user") => Role::User,
                Some("assistant") => Role::Assistant,
                // Skip system role (context injections, compaction notices).
                _ => continue,
            };

            let ts = v
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(parse_gptme_ts);
            if let Some(t) = ts {
                if started.is_none() {
                    started = Some(t);
                }
                ended = Some(t);
            }

            let text = match v.remove("content") {
                Some(Value::String(s)) => s,
                Some(Value::Array(blocks)) => blocks
                    .into_iter()
                    .filter_map(|b| match b {
                        Value::Object(mut map) => match map.remove("content") {
                            Some(Value::String(s)) => Some(s),
                            _ => None,
                        },
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => continue,
            };
            let text = text.trim();
            if !text.is_empty() {
                messages.push(Message {
                    role,
                    text: text.to_string(),
                    ts,
                });
            }
        }

        // Label by session directory name (human-readable slug).
        let project = path
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
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
            touched: Vec::new(),
            edits: Vec::new(),
        })
    }
}

/// gptme uses Python's `datetime.now().isoformat()`, which produces naive
/// timestamps with no UTC offset (e.g. `2026-06-08T10:00:01.000000`).
/// Try RFC 3339 first; fall back to naive-datetime parsing and assume UTC.
fn parse_gptme_ts(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    parse_ts(s).or_else(|| s.parse::<NaiveDateTime>().ok().map(|n| n.and_utc()))
}
