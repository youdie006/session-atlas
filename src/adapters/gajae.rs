use super::{dedup_paths, ok_or_flag, parse_ts, title_from_messages, Adapter, Discovered};
use crate::model::{Message, Role, Session};
use crate::util::{short_id, truncate};
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// gajae-code (가재코드, github Yeachan-Heo/gajae-code) is a Korean terminal
/// agent built on Mario Zechner's "Pi" (pi-mono); the two share a byte-identical
/// session format, so this adapter reads `~/.gjc/agent/sessions` (and honors the
/// same env overrides Pi uses).
///
/// One session is one JSONL file under `<sessions>/<encoded-cwd>/<iso>_<uuid>.jsonl`.
/// Line 1 is a `{"type":"session", id, cwd, timestamp, title}` header; the rest
/// are `{"type":"message", timestamp, message:{role, content, timestamp}}` lines
/// (other `type`s - model_change, compaction, ... - are skippable bookkeeping).
/// Tool calls are `{"type":"toolCall","name","arguments"}` content blocks - note
/// `toolCall`/`arguments`, not Claude Code's `tool_use`/`input`.
pub struct GajaeCode;

impl Adapter for GajaeCode {
    fn name(&self) -> &'static str {
        "gajae-code"
    }

    fn root(&self) -> Option<PathBuf> {
        sessions_dirs().into_iter().next()
    }

    fn discover(&self) -> Discovered {
        let mut out = Vec::new();
        let mut had_error = false;
        for dir in sessions_dirs() {
            if !dir.exists() {
                continue; // this candidate root isn't on this machine - normal
            }
            // <sessions>/<encoded-cwd>/<file>.jsonl
            for entry in WalkDir::new(&dir)
                .min_depth(2)
                .max_depth(2)
                .into_iter()
                .filter_map(|e| ok_or_flag(e, &mut had_error))
            {
                if entry.file_type().is_file()
                    && entry.path().extension().is_some_and(|x| x == "jsonl")
                {
                    out.push(entry.into_path());
                }
            }
        }
        Discovered {
            files: out,
            had_error,
        }
    }

    fn parse(&self, path: &Path) -> Result<Session> {
        parse_jsonl(self.name(), path)
    }
}

/// Candidate session roots, honoring the same env overrides as Pi/gajae-code.
fn sessions_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(agent) = std::env::var_os("GJC_CODING_AGENT_DIR") {
        dirs.push(PathBuf::from(agent).join("sessions"));
    }
    let config_base = std::env::var_os("GJC_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".gjc")));
    if let Some(base) = config_base {
        dirs.push(base.join("agent").join("sessions"));
    }
    // On Linux/macOS gajae-code flattens to $XDG_DATA_HOME/gjc/sessions when set.
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        let p = PathBuf::from(xdg);
        if !p.as_os_str().is_empty() {
            dirs.push(p.join("gjc").join("sessions"));
        }
    }
    dirs
}

/// Tool calls whose `arguments` name a file the session created or edited.
/// `write`/`edit`/`apply_patch` use `arguments.path`; `ast_edit` uses
/// `arguments.paths` (an array). `read` is excluded - reading is not authorship.
const EDIT_TOOLS: &[&str] = &["write", "edit", "apply_patch", "ast_edit"];

fn parse_jsonl(tool: &'static str, path: &Path) -> Result<Session> {
    let raw = crate::util::read_to_string_capped(path)?;

    let mut session_id = String::new();
    let mut project = String::new();
    let mut started: Option<DateTime<Utc>> = None;
    let mut header_title: Option<String> = None;
    let mut last_ts: Option<DateTime<Utc>> = None;

    let mut messages: Vec<Message> = Vec::new();
    let mut touched: Vec<String> = Vec::new();

    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Lenient: a single malformed line must not abort the session.
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };

        // Outer timestamp is an RFC3339 string; track the latest for `ended`.
        let outer_ts = entry
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_ts);
        if let Some(t) = outer_ts {
            last_ts = Some(t);
        }

        match entry.get("type").and_then(Value::as_str) {
            Some("session") => {
                session_id = entry
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                project = entry
                    .get("cwd")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                started = outer_ts;
                header_title = entry
                    .get("title")
                    .and_then(Value::as_str)
                    .filter(|t| !t.trim().is_empty())
                    .map(|t| truncate(t, 80));
            }
            Some("message") => {
                let Some(msg) = entry.get("message") else {
                    continue;
                };
                let role = match msg.get("role").and_then(Value::as_str) {
                    Some("user") => Role::User,
                    Some("assistant") => Role::Assistant,
                    Some("toolResult") => Role::Tool,
                    // developer / bashExecution / custom / ... - skip.
                    _ => continue,
                };
                // Inner timestamp is epoch ms; prefer it, fall back to outer.
                let ts = msg
                    .get("timestamp")
                    .and_then(Value::as_i64)
                    .and_then(DateTime::from_timestamp_millis)
                    .or(outer_ts);

                match msg.get("content") {
                    Some(Value::String(s)) => push(&mut messages, role, s, ts),
                    Some(Value::Array(blocks)) => {
                        for block in blocks {
                            match block.get("type").and_then(Value::as_str) {
                                Some("text") => {
                                    if let Some(t) = block.get("text").and_then(Value::as_str) {
                                        push(&mut messages, role, t, ts);
                                    }
                                }
                                Some("toolCall") => {
                                    let name =
                                        block.get("name").and_then(Value::as_str).unwrap_or("?");
                                    let args = block.get("arguments");
                                    if EDIT_TOOLS.contains(&name) {
                                        for p in arg_paths(args) {
                                            touched.push(p);
                                        }
                                    }
                                    let a = args.map(|v| v.to_string()).unwrap_or_default();
                                    push(
                                        &mut messages,
                                        Role::Tool,
                                        &format!("{name} {}", truncate(&a, 300)),
                                        ts,
                                    );
                                }
                                // thinking / redactedThinking / image - skip.
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            // session bookkeeping (model_change, compaction, ...) - skip.
            _ => {}
        }
    }

    let _ = session_id;
    let title = header_title.unwrap_or_else(|| title_from_messages(&messages));

    Ok(Session {
        id: short_id(&path.to_string_lossy()),
        tool,
        path: path.to_path_buf(),
        project,
        started,
        ended: last_ts,
        title,
        subagent: false,
        messages,
        touched: dedup_paths(touched),
        edits: Vec::new(),
    })
}

/// A tool call's file path(s): `arguments.path` (string) or `arguments.paths`
/// (array, used by `ast_edit`).
fn arg_paths(args: Option<&Value>) -> Vec<String> {
    let Some(args) = args else {
        return vec![];
    };
    if let Some(p) = args.get("path").and_then(Value::as_str) {
        if !p.is_empty() {
            return vec![p.to_string()];
        }
    }
    if let Some(Value::Array(ps)) = args.get("paths") {
        return ps
            .iter()
            .filter_map(Value::as_str)
            .filter(|p| !p.is_empty())
            .map(String::from)
            .collect();
    }
    vec![]
}

fn push(messages: &mut Vec<Message>, role: Role, text: &str, ts: Option<DateTime<Utc>>) {
    let text = text.trim();
    if !text.is_empty() {
        messages.push(Message {
            role,
            text: text.to_string(),
            ts,
        });
    }
}
