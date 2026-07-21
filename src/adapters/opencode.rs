use super::{dedup_paths, ok_or_flag, title_from_messages, Adapter, Discovered, Store};
use crate::model::{Message, Role, Session};
use crate::util::{short_id, truncate};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OpenFlags};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// OpenCode (sst/opencode) keeps its data under `$XDG_DATA_HOME/opencode`
/// (default `~/.local/share/opencode`, the same on macOS - it uses
/// xdg-basedir, not the platform data dir). Two storage generations exist:
///
///   - **SQLite (v1.2.0+, the current default):** `opencode.db` (and
///     `opencode-<channel>.db`) with `session` / `message` / `part` tables.
///     This is the source of truth on any recent install, so when a db is
///     present we index it as a shared [`Store`] and ignore the JSON below.
///   - **Legacy JSON:** `storage/session|message|part/**.json`, one file each.
///     Still read on pre-1.2.0 installs that never migrated.
pub struct OpenCode;

impl Adapter for OpenCode {
    fn name(&self) -> &'static str {
        "opencode"
    }

    fn root(&self) -> Option<PathBuf> {
        // The opencode data dir - exists for both the SQLite and JSON layouts,
        // so `scan` presence and deletion reconciliation work either way.
        data_dir()
    }

    fn discover(&self) -> Discovered {
        // Legacy JSON only. When a db exists, `store()` is Some and the indexer
        // never calls this (the JSON files are stale post-migration leftovers).
        let Some(dir) = data_dir() else {
            return Vec::new().into();
        };
        let sessions = dir.join("storage").join("session");
        if !sessions.exists() {
            return Vec::new().into(); // no legacy layout - normal
        }
        let mut had_error = false;
        let files = WalkDir::new(sessions)
            .min_depth(2)
            .max_depth(2)
            .into_iter()
            .filter_map(|e| ok_or_flag(e, &mut had_error))
            .filter(|e| e.file_type().is_file())
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .map(|e| e.into_path())
            .collect();
        Discovered { files, had_error }
    }

    fn store(&self) -> Option<Store> {
        let dbs: Vec<PathBuf> = db_paths().into_iter().filter(|p| p.is_file()).collect();
        if dbs.is_empty() {
            return None;
        }
        let mut keys: Vec<(String, i64)> = Vec::new();
        let mut had_error = false;
        for db in &dbs {
            let conn = match open_ro(db) {
                Ok(c) => c,
                Err(_) => {
                    had_error = true;
                    continue;
                }
            };
            let mut stmt = match conn.prepare("SELECT id, time_updated, time_created FROM session")
            {
                Ok(s) => s,
                Err(_) => {
                    had_error = true;
                    continue;
                }
            };
            let rows = stmt.query_map([], |r| {
                let id: String = r.get(0)?;
                let updated: Option<i64> = r.get(1)?;
                let created: Option<i64> = r.get(2)?;
                Ok((id, updated.or(created).unwrap_or(0)))
            });
            match rows {
                Ok(rows) => {
                    for (id, token) in rows.flatten() {
                        keys.push((make_key(db, &id), token));
                    }
                }
                Err(_) => had_error = true,
            }
        }
        Some(Store {
            keys,
            files: dbs,
            had_error,
        })
    }

    fn parse_key(&self, key: &str) -> Result<Session> {
        let (db, sid) = key.split_once(KEY_SEP).context("malformed opencode key")?;
        let conn = open_ro(Path::new(db)).with_context(|| format!("open {db}"))?;

        let (project, raw_title, created, updated): (String, String, Option<i64>, Option<i64>) =
            conn.query_row(
                "SELECT directory, title, time_created, time_updated
                 FROM session WHERE id = ?1",
                params![sid],
                |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                        r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                        r.get(2)?,
                        r.get(3)?,
                    ))
                },
            )?;
        // parent_id is fetched separately and tolerantly: an older or partial
        // schema without the column degrades to "not a subagent" rather than
        // failing the whole session.
        let parent: Option<String> = conn
            .query_row(
                "SELECT parent_id FROM session WHERE id = ?1",
                params![sid],
                |r| r.get(0),
            )
            .unwrap_or(None);

        // role per message, in transcript order.
        // LIMIT caps memory if a malicious db packs millions of rows into one
        // session; a real session is orders of magnitude below this.
        let mut mstmt = conn.prepare(
            "SELECT id, time_created, data FROM message
             WHERE session_id = ?1 ORDER BY time_created, id LIMIT 200000",
        )?;
        let messages_meta: Vec<(String, Option<i64>, Value)> = mstmt
            .query_map(params![sid], |r| {
                let id: String = r.get(0)?;
                let tc: Option<i64> = r.get(1)?;
                let data: String = r.get(2)?;
                Ok((id, tc, serde_json::from_str(&data).unwrap_or(Value::Null)))
            })?
            .filter_map(|x| x.ok())
            .collect();

        // content parts, grouped per message in order.
        let mut pstmt = conn.prepare(
            "SELECT message_id, data FROM part
             WHERE session_id = ?1 ORDER BY time_created, id LIMIT 200000",
        )?;
        let mut parts: HashMap<String, Vec<Value>> = HashMap::new();
        let prows = pstmt.query_map(params![sid], |r| {
            let mid: String = r.get(0)?;
            let data: String = r.get(1)?;
            Ok((mid, data))
        })?;
        for (mid, data) in prows.flatten() {
            if let Ok(v) = serde_json::from_str::<Value>(&data) {
                parts.entry(mid).or_default().push(v);
            }
        }

        let mut messages: Vec<Message> = Vec::new();
        let mut touched: Vec<String> = Vec::new();
        for (mid, tc, mdata) in &messages_meta {
            let role = match mdata.get("role").and_then(Value::as_str) {
                Some("user") => Role::User,
                Some("assistant") => Role::Assistant,
                _ => continue,
            };
            let ts = tc.and_then(DateTime::from_timestamp_millis);
            let Some(ps) = parts.get(mid) else { continue };
            for p in ps {
                match p.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(t) = p.get("text").and_then(Value::as_str) {
                            push(&mut messages, role, t, ts);
                        }
                    }
                    Some("tool") => {
                        let tool = p.get("tool").and_then(Value::as_str).unwrap_or("?");
                        if let Some(fp) = edited_path(tool, p.pointer("/state/input")) {
                            touched.push(fp);
                        }
                        let input = p
                            .pointer("/state/input")
                            .map(|i| i.to_string())
                            .unwrap_or_default();
                        push(
                            &mut messages,
                            Role::Tool,
                            &format!("{tool} {}", truncate(&input, 300)),
                            ts,
                        );
                    }
                    Some("patch") => {
                        if let Some(Value::Array(files)) = p.get("files") {
                            for f in files {
                                if let Some(fp) = f.as_str() {
                                    touched.push(fp.to_string());
                                }
                            }
                        }
                    }
                    // reasoning / step-* / file / snapshot - skip.
                    _ => {}
                }
            }
        }

        let title = if raw_title.trim().is_empty() || raw_title.starts_with("New session") {
            title_from_messages(&messages)
        } else {
            truncate(&raw_title, 80)
        };

        Ok(Session {
            id: short_id(key),
            tool: self.name(),
            path: PathBuf::from(display_path(key)),
            project,
            started: created.and_then(DateTime::from_timestamp_millis),
            ended: updated.and_then(DateTime::from_timestamp_millis),
            title,
            subagent: parent.is_some_and(|p| !p.is_empty()),
            messages,
            touched: dedup_paths(touched),
            edits: Vec::new(),
        })
    }

    fn parse(&self, path: &Path) -> Result<Session> {
        parse_json(self.name(), path)
    }
}

/// Key encoding for a shared-store session: `<db path><US><session id>`. The
/// unit separator never appears in a path or a uuid.
const KEY_SEP: char = '\u{1f}';
fn make_key(db: &Path, session_id: &str) -> String {
    format!("{}{KEY_SEP}{session_id}", db.display())
}

/// A human-facing form of a shared-store key for `Session.path` (shown in
/// `show`/`brief`/web). The raw key carries a U+001F separator that must not
/// leak into output or an LLM briefing; render it as `<db>#<id>`.
fn display_path(key: &str) -> String {
    key.replace(KEY_SEP, "#")
}

/// `$XDG_DATA_HOME/opencode`, or `~/.local/share/opencode`.
fn data_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| dirs::home_dir().map(|h| h.join(".local").join("share")))?;
    Some(base.join("opencode"))
}

/// The session databases: `opencode.db` plus any `opencode-<channel>.db`, or a
/// single path from `$OPENCODE_DB` (absolute, or relative to the data dir).
fn db_paths() -> Vec<PathBuf> {
    if let Some(over) = std::env::var_os("OPENCODE_DB") {
        let p = PathBuf::from(&over);
        if !p.as_os_str().is_empty() && p.as_path() != Path::new(":memory:") {
            return vec![if p.is_absolute() {
                p
            } else {
                data_dir().map(|d| d.join(&p)).unwrap_or(p)
            }];
        }
    }
    let Some(dir) = data_dir() else {
        return vec![];
    };
    let mut out = vec![dir.join("opencode.db")];
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let p = e.path();
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with("opencode-") && name.ends_with(".db") {
                out.push(p);
            }
        }
    }
    out
}

/// Open another tool's database strictly read-only - never creating, writing,
/// checkpointing, or recovering it. A plain read-only handle can fail on a WAL
/// db when the shared-memory file is missing/locked; the fallback retries with
/// an `immutable=1` URI, which tells SQLite to assume the file cannot change and
/// skip all locking and shared-memory, so it still cannot write. (Trade-off:
/// changes a running OpenCode makes mid-read may be missed - fine for indexing.)
fn open_ro(db: &Path) -> Result<Connection> {
    Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .or_else(|_| {
            let uri = format!("file:{}?immutable=1", db.display());
            Connection::open_with_flags(
                &uri,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
            )
        })
        .map_err(Into::into)
}

// --- legacy JSON layout (pre-1.2.0) ---

fn parse_json(tool: &'static str, path: &Path) -> Result<Session> {
    let raw = crate::util::read_to_string_capped(path)?;
    let s: Value =
        serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;

    let session_id = s
        .get("id")
        .and_then(Value::as_str)
        .map(String::from)
        .unwrap_or_else(|| stem(path));
    let project = s
        .get("directory")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let started = s.pointer("/time/created").and_then(epoch_ms);
    let ended = s.pointer("/time/updated").and_then(epoch_ms);
    let subagent = s.get("parentID").is_some();
    let session_title = s
        .get("title")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty() && !t.starts_with("New session"))
        .map(|t| truncate(t, 80));

    let store = path
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent());

    let mut messages: Vec<Message> = Vec::new();
    let mut touched: Vec<String> = Vec::new();

    if let Some(store) = store {
        for msg_path in sorted_json(&store.join("message").join(&session_id)) {
            let Ok(mraw) = crate::util::read_to_string_capped(&msg_path) else {
                continue;
            };
            let Ok(m) = serde_json::from_str::<Value>(&mraw) else {
                continue;
            };
            let role = match m.get("role").and_then(Value::as_str) {
                Some("user") => Role::User,
                Some("assistant") => Role::Assistant,
                _ => continue,
            };
            let mid = m
                .get("id")
                .and_then(Value::as_str)
                .map(String::from)
                .unwrap_or_else(|| stem(&msg_path));
            let ts = m.pointer("/time/created").and_then(epoch_ms);

            for part_path in sorted_json(&store.join("part").join(&mid)) {
                let Ok(praw) = crate::util::read_to_string_capped(&part_path) else {
                    continue;
                };
                let Ok(p) = serde_json::from_str::<Value>(&praw) else {
                    continue;
                };
                match p.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(t) = p.get("text").and_then(Value::as_str) {
                            push(&mut messages, role, t, ts);
                        }
                    }
                    Some("tool") => {
                        let tool = p.get("tool").and_then(Value::as_str).unwrap_or("?");
                        if let Some(fp) = edited_path(tool, p.pointer("/state/input")) {
                            touched.push(fp);
                        }
                        let input = p
                            .pointer("/state/input")
                            .map(|i| i.to_string())
                            .unwrap_or_default();
                        push(
                            &mut messages,
                            Role::Tool,
                            &format!("{tool} {}", truncate(&input, 300)),
                            ts,
                        );
                    }
                    Some("patch") => {
                        if let Some(Value::Array(files)) = p.get("files") {
                            for f in files {
                                if let Some(fp) = f.as_str() {
                                    touched.push(fp.to_string());
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    let title = session_title.unwrap_or_else(|| title_from_messages(&messages));

    Ok(Session {
        id: short_id(&path.to_string_lossy()),
        tool,
        path: path.to_path_buf(),
        project,
        started,
        ended,
        title,
        subagent,
        messages,
        touched: dedup_paths(touched),
        edits: Vec::new(),
    })
}

/// Both layouts name an edited file in `state.input.filePath` for the
/// `edit`/`write`/`multiedit` tools. Reads are not authorship.
fn edited_path(tool: &str, input: Option<&Value>) -> Option<String> {
    if !matches!(tool, "edit" | "write" | "multiedit") {
        return None;
    }
    input?
        .get("filePath")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty())
        .map(String::from)
}

fn sorted_json(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    v.sort();
    v
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Timestamps are epoch milliseconds (numbers), not RFC3339 strings.
fn epoch_ms(v: &Value) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp_millis(v.as_i64()?)
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

#[cfg(test)]
mod tests {
    use super::{make_key, OpenCode};
    use crate::adapters::Adapter;
    use chrono::DateTime;
    use rusqlite::Connection;

    #[test]
    fn parses_a_sqlite_session() {
        // Build a minimal opencode.db (session/message/part) and read one
        // session back out of it through the SQLite path.
        let db = std::env::temp_dir().join(format!("sw-oc-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&db);
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE session(id TEXT PRIMARY KEY, parent_id TEXT, project_id TEXT,
                     directory TEXT, title TEXT, time_created INTEGER, time_updated INTEGER);
                 CREATE TABLE message(id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, data TEXT);
                 CREATE TABLE part(id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT, time_created INTEGER, data TEXT);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO session VALUES ('ses1', NULL, 'p1', '/home/dev/app', 'Add retry', 1718630400000, 1718630500000)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO message VALUES ('m1','ses1',1718630400000,'{\"role\":\"user\"}')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO message VALUES ('m2','ses1',1718630450000,'{\"role\":\"assistant\"}')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO part VALUES ('pa','m1','ses1',1718630400000,'{\"type\":\"text\",\"text\":\"add retry to the client\"}')",
                [],
            ).unwrap();
            conn.execute(
                "INSERT INTO part VALUES ('pb','m2','ses1',1718630450000,'{\"type\":\"text\",\"text\":\"I will add it.\"}')",
                [],
            ).unwrap();
            conn.execute(
                "INSERT INTO part VALUES ('pc','m2','ses1',1718630451000,'{\"type\":\"tool\",\"tool\":\"edit\",\"state\":{\"input\":{\"filePath\":\"src/client.ts\"}}}')",
                [],
            ).unwrap();
        }

        let key = make_key(&db, "ses1");
        let s = OpenCode.parse_key(&key).unwrap();

        assert_eq!(s.tool, "opencode");
        assert_eq!(s.project, "/home/dev/app");
        assert_eq!(s.title, "Add retry");
        assert!(!s.subagent);
        let roles: Vec<_> = s.messages.iter().map(|m| m.role.label()).collect();
        assert_eq!(roles, ["user", "assistant", "tool"]);
        assert_eq!(s.touched, ["src/client.ts"]);
        assert_eq!(s.started, DateTime::from_timestamp_millis(1718630400000));
        assert_eq!(s.ended, DateTime::from_timestamp_millis(1718630500000));

        let _ = std::fs::remove_file(&db);
        let _ = std::fs::remove_file(db.with_extension("db-wal"));
        let _ = std::fs::remove_file(db.with_extension("db-shm"));
    }
}
