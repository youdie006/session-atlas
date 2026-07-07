//! prodex (github.com/youdie006/prodex) - a local bridge that lets coding
//! agents consult a logged-in ChatGPT Pro. Every consult is a durable task
//! (`.bridge/tasks/<id>.json`, the QUESTION) paired with a result
//! (`.bridge/results/<id>.json`, the ANSWER) and, for pro consults, a full
//! answer artifact (`.bridge/artifacts/pro-consults/<id>.md`).
//!
//! Bridges are per-repo and scattered; prodex >=0.11.0 registers every bridge
//! root in `~/.local/share/prodex/bridges.json`, which is the discovery
//! entry point here. `SESSIONWIKI_PRODEX_REGISTRY` overrides it for tests.

use super::{Adapter, Discovered};
use crate::model::{Message, Role, Session};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub struct Prodex;

fn registry_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SESSIONWIKI_PRODEX_REGISTRY") {
        return Some(p.into());
    }
    Some(
        dirs::home_dir()?
            .join(".local")
            .join("share")
            .join("prodex")
            .join("bridges.json"),
    )
}

fn bridge_roots() -> Vec<PathBuf> {
    let Some(path) = registry_path() else {
        return Vec::new();
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return Vec::new(); // no prodex on this machine - normal
    };
    let Ok(v) = serde_json::from_slice::<Value>(&bytes) else {
        return Vec::new();
    };
    v["roots"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|r| r.as_str())
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default()
}

fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// `task_YYYYMMDD_HHMMSS_slug` -> a coarse UTC timestamp, the fallback when a
/// task has no `claimed_at` yet.
fn ts_from_id(id: &str) -> Option<DateTime<Utc>> {
    let mut parts = id.split('_');
    if parts.next() != Some("task") {
        return None;
    }
    let (d, t) = (parts.next()?, parts.next()?);
    // ASCII-digit gate BEFORE any byte slicing: ids are ASCII by construction,
    // but this parses untrusted on-disk data - a multibyte char at a slice
    // boundary must degrade to None, never panic (a parse panic would abort
    // the whole indexer).
    if d.len() != 8
        || t.len() != 6
        || !d.bytes().all(|b| b.is_ascii_digit())
        || !t.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let iso = format!(
        "{}-{}-{}T{}:{}:{}Z",
        &d[0..4],
        &d[4..6],
        &d[6..8],
        &t[0..2],
        &t[2..4],
        &t[4..6]
    );
    parse_ts(&iso)
}

impl Adapter for Prodex {
    fn name(&self) -> &'static str {
        "prodex"
    }

    fn root(&self) -> Option<PathBuf> {
        // For display/presence (`scan` shows this): the registry's directory,
        // consistent with every other adapter showing a store DIRECTORY.
        // Discovery reads the registry file itself.
        registry_path().and_then(|p| p.parent().map(|d| d.to_path_buf()))
    }

    fn discover(&self) -> Discovered {
        let mut files = Vec::new();
        let mut had_error = false;
        for root in bridge_roots() {
            let tasks = root.join(".bridge").join("tasks");
            if !tasks.is_dir() {
                continue; // a registered repo may be gone - normal, not an error
            }
            match std::fs::read_dir(&tasks) {
                Ok(rd) => {
                    for e in rd.flatten() {
                        let p = e.path();
                        if p.extension().is_some_and(|x| x == "json") {
                            files.push(p);
                        }
                    }
                }
                Err(_) => had_error = true,
            }
        }
        Discovered { files, had_error }
    }

    fn parse(&self, path: &Path) -> Result<Session> {
        let task: Value = serde_json::from_slice(
            &std::fs::read(path).with_context(|| format!("read {}", path.display()))?,
        )
        .with_context(|| format!("parse {}", path.display()))?;
        let task_id = task["id"].as_str().context("task has no id")?.to_string();
        // Task ids share a long `task_YYYYMMDD_...` prefix, so as SESSION ids
        // they would defeat prefix addressing (list shows 13 chars; several
        // tasks per day collide there). Derive the same short, stable 12-hex
        // id shape every other tool uses; the real task id stays reachable
        // via the stored path.
        let id = {
            let d = Sha256::digest(task_id.as_bytes());
            let mut hex = String::with_capacity(12);
            for b in &d[..6] {
                hex.push_str(&format!("{b:02x}"));
            }
            hex
        };
        let prompt = task["prompt"].as_str().unwrap_or("").trim().to_string();
        // The QUESTION is the most informative title a consult can have -
        // prodex auto-titles most consults identically ("GPT Pro consult"),
        // which makes a list of them indistinguishable. Task title is the
        // fallback for promptless tasks.
        let title = {
            let head: String = prompt
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(80)
                .collect();
            if !head.is_empty() {
                head
            } else {
                task["title"]
                    .as_str()
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .unwrap_or("(untitled task)")
                    .to_string()
            }
        };
        // tasks/<id>.json -> the bridge root is two levels up from tasks/.
        let bridge = path.parent().and_then(|p| p.parent());
        let repo = bridge.and_then(|b| b.parent());
        let project = repo
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(unknown)".into());

        let started = task["claimed_at"]
            .as_str()
            .and_then(parse_ts)
            .or_else(|| ts_from_id(&task_id));

        let mut messages = Vec::new();
        if !prompt.is_empty() {
            messages.push(Message {
                role: Role::User,
                text: prompt,
                ts: started,
            });
        }
        // The answer: the full pro-consult artifact when present, else the
        // result summary. The artifact is read INDEPENDENTLY of the result -
        // a crash between the two writes must not make an answer that exists
        // on disk unindexable. Bounded read; an artifact is normally a few KB.
        let mut ended = None;
        if let Some(bridge) = bridge {
            let artifact_text = std::fs::read_to_string(
                bridge
                    .join("artifacts")
                    .join("pro-consults")
                    .join(format!("{task_id}.md")),
            )
            .ok()
            .map(|t| {
                let mut t = t.trim().to_string();
                const CAP: usize = 64 * 1024;
                if t.len() > CAP {
                    let mut end = CAP;
                    while !t.is_char_boundary(end) {
                        end -= 1;
                    }
                    t.truncate(end);
                }
                t
            })
            .filter(|t| !t.is_empty());
            let mut summary = None;
            if let Ok(bytes) = std::fs::read(bridge.join("results").join(format!("{task_id}.json")))
            {
                if let Ok(result) = serde_json::from_slice::<Value>(&bytes) {
                    ended = result["created_at"].as_str().and_then(parse_ts);
                    summary = result["summary"]
                        .as_str()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty());
                }
            }
            if let Some(text) = artifact_text.or(summary) {
                messages.push(Message {
                    role: Role::Assistant,
                    text,
                    ts: ended.or(started),
                });
            }
        }

        // Files the task bundled become provenance links (trace integration).
        let touched: Vec<String> = task["files"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|f| {
                        f.as_str()
                            .map(str::to_string)
                            .or_else(|| f["path"].as_str().map(str::to_string))
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(Session {
            id,
            tool: "prodex",
            path: path.to_path_buf(),
            project,
            started,
            ended: ended.or(started),
            title,
            subagent: false,
            messages,
            touched,
        })
    }
}

/// The ChatGPT thread URL a bridge's consults land in, from the newest
/// `.bridge/sessions/*.json` next to `task_path`. Only an `https://chatgpt.com/`
/// URL is ever surfaced - a tampered session file cannot inject anything else.
pub fn thread_url_for_task(task_path: &Path) -> Option<String> {
    let sessions = task_path.parent()?.parent()?.join("sessions");
    let mut candidates: Vec<(std::time::SystemTime, String)> = Vec::new();
    for e in std::fs::read_dir(sessions).ok()?.flatten() {
        let p = e.path();
        if p.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<Value>(&std::fs::read(&p).ok()?) else {
            continue;
        };
        let Some(url) = v["thread"].as_str() else {
            continue;
        };
        if !url.starts_with("https://chatgpt.com/") {
            continue;
        }
        let mtime = e
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        candidates.push((mtime, url.to_string()));
    }
    candidates.sort_by_key(|(t, _)| *t);
    candidates.pop().map(|(_, u)| u)
}
