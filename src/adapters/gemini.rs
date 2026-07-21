use super::{ok_or_flag, parse_ts, title_from_messages, Adapter, Discovered};
use crate::model::{Message, Role, Session};
use crate::util::short_id;
use anyhow::{Context, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Gemini CLI stores one JSON document per saved chat under
/// `~/.gemini/tmp/<project>/chats/session-*.json`:
/// `{ sessionId, startTime, lastUpdated, messages: [{ type, content, timestamp }] }`.
pub struct Gemini;

impl Adapter for Gemini {
    fn name(&self) -> &'static str {
        "gemini"
    }

    fn root(&self) -> Option<PathBuf> {
        Some(dirs::home_dir()?.join(".gemini").join("tmp"))
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
            .max_depth(3)
            .into_iter()
            .filter_map(|e| ok_or_flag(e, &mut had_error))
            .filter(|e| e.file_type().is_file())
            .filter(|e| {
                e.path().extension().is_some_and(|x| x == "json")
                    && e.path()
                        .parent()
                        .and_then(|p| p.file_name())
                        .is_some_and(|d| d == "chats")
            })
            .map(|e| e.into_path())
            .collect();
        Discovered { files, had_error }
    }

    fn parse(&self, path: &Path) -> Result<Session> {
        let raw = crate::util::read_to_string_capped(path)?;
        let v: Value =
            serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;

        let started = v
            .get("startTime")
            .and_then(Value::as_str)
            .and_then(parse_ts);
        let ended = v
            .get("lastUpdated")
            .and_then(Value::as_str)
            .and_then(parse_ts);

        let mut messages: Vec<Message> = Vec::new();
        if let Some(Value::Array(items)) = v.get("messages") {
            for m in items {
                let role = match m.get("type").and_then(Value::as_str) {
                    Some("user") => Role::User,
                    Some("gemini") | Some("assistant") | Some("model") => Role::Assistant,
                    _ => continue,
                };
                let ts = m
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .and_then(parse_ts);
                let text = match m.get("content") {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Array(blocks)) => blocks
                        .iter()
                        .filter_map(|b| b.get("text").and_then(Value::as_str))
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
        }

        // Project label: ~/.gemini/tmp/<project>/chats/file.json
        let project = path
            .ancestors()
            .nth(2)
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let title = title_from_messages(&messages);

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
            // Gemini CLI chat logs do not record structured file edits, so
            // there is nothing to link to the codebase here.
            touched: Vec::new(),
            edits: Vec::new(),
        })
    }
}
