//! `doctor`: a self-diagnosis of this machine's setup. sessionwiki reads a dozen
//! drifting session formats and an on-disk index; when something looks empty or
//! stale the cause is usually mundane (no store on this box, an index behind a
//! schema bump, a store that will not parse). This surfaces that at a glance so a
//! bug report starts from facts, not guesses.

use rusqlite::Connection;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Serialize)]
pub struct Check {
    pub name: String,
    pub status: Status,
    pub detail: String,
}

impl Check {
    fn new(status: Status, name: &str, detail: String) -> Self {
        Check {
            name: name.into(),
            status,
            detail,
        }
    }
    pub fn ok(name: &str, detail: String) -> Self {
        Self::new(Status::Ok, name, detail)
    }
    pub fn warn(name: &str, detail: String) -> Self {
        Self::new(Status::Warn, name, detail)
    }
    pub fn fail(name: &str, detail: String) -> Self {
        Self::new(Status::Fail, name, detail)
    }
}

/// Health of the on-disk index: schema currency, integrity, and what it holds.
/// `expected_schema` is the binary's `SCHEMA_VERSION` (injected so this stays a
/// pure function of the connection).
pub fn index_checks(conn: &Connection, expected_schema: i64) -> Vec<Check> {
    let mut checks = Vec::new();

    let v: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap_or(-1);
    checks.push(if v == expected_schema {
        Check::ok("index schema", format!("v{v}, current"))
    } else {
        Check::warn(
            "index schema",
            format!("v{v}, expected v{expected_schema} - the next query rebuilds the cache"),
        )
    });

    // A read-only-safe structural check. (PRAGMA integrity_check can't run on the
    // read-only connection: validating the FTS5 index needs write access.)
    const CORE: &[&str] = &["files", "messages", "edits", "archive"];
    let present: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table'
               AND name IN ('files','messages','edits','archive')",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    checks.push(if present as usize == CORE.len() {
        Check::ok("index tables", "all core tables present".into())
    } else {
        Check::warn(
            "index tables",
            format!(
                "{present}/{} core tables - the next query rebuilds the cache",
                CORE.len()
            ),
        )
    });

    let sessions: i64 = conn
        .query_row("SELECT count(*) FROM files", [], |r| r.get(0))
        .unwrap_or(0);
    let archived: i64 = conn
        .query_row(
            "SELECT count(*) FROM files WHERE archived_at IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    checks.push(Check::ok(
        "indexed sessions",
        format!("{sessions} ({archived} kept after the tool deleted them)"),
    ));

    checks
}

/// Which session stores exist on this machine, and how many sessions each holds.
/// A store that is simply absent is normal (not every tool is installed) and is
/// left off the list; the warn fires only when NONE are found.
pub fn store_checks() -> Vec<Check> {
    let mut checks = Vec::new();
    for adapter in crate::adapters::all() {
        if let Some(r) = crate::adapters::report(adapter.as_ref()) {
            checks.push(Check::ok(
                &format!("store: {}", adapter.name()),
                format!("{} sessions in {}", r.files, r.root.display()),
            ));
        }
    }
    if checks.is_empty() {
        checks.push(Check::warn(
            "stores",
            "no session stores found on this machine".into(),
        ));
    }
    checks
}

/// Run every check and print the report (`--json` for scripts). Opens the index
/// read-only so `doctor` never mutates it.
pub fn run(json: bool) -> anyhow::Result<()> {
    let mut checks = store_checks();
    match crate::index::open_readonly() {
        Ok(conn) => checks.extend(index_checks(&conn, crate::index::SCHEMA_VERSION)),
        Err(_) => checks.push(Check::warn(
            "index",
            "not built yet - run `sessionwiki search` to build it".into(),
        )),
    }
    checks.push(Check::ok("version", env!("CARGO_PKG_VERSION").into()));

    if json {
        println!("{}", serde_json::to_string_pretty(&checks)?);
        return Ok(());
    }
    for c in &checks {
        let mark = match c.status {
            Status::Ok => "ok  ",
            Status::Warn => "warn",
            Status::Fail => "FAIL",
        };
        println!("[{mark}] {} - {}", c.name, c.detail);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE files(path TEXT PRIMARY KEY, session_id TEXT NOT NULL, archived_at TEXT);
             CREATE TABLE messages(id INTEGER PRIMARY KEY);
             CREATE TABLE edits(session_id TEXT);
             CREATE TABLE archive(session_id TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn index_checks_flag_a_stale_schema_and_report_counts() {
        let c = conn();
        c.pragma_update(None, "user_version", 7i64).unwrap();
        c.execute("INSERT INTO files(path, session_id) VALUES('a', 's1')", [])
            .unwrap();
        c.execute(
            "INSERT INTO files(path, session_id, archived_at) VALUES('b', 's2', '2026-01-01')",
            [],
        )
        .unwrap();

        let checks = index_checks(&c, 8);
        let schema = checks.iter().find(|c| c.name == "index schema").unwrap();
        assert_eq!(schema.status, Status::Warn, "v7 vs expected v8 is a warn");
        let tables = checks.iter().find(|c| c.name == "index tables").unwrap();
        assert_eq!(tables.status, Status::Ok, "all 4 core tables present");
        let sessions = checks
            .iter()
            .find(|c| c.name == "indexed sessions")
            .unwrap();
        assert!(
            sessions.detail.starts_with("2 "),
            "2 sessions: {}",
            sessions.detail
        );
        assert!(
            sessions.detail.contains("1 "),
            "1 archived: {}",
            sessions.detail
        );
    }

    #[test]
    fn index_checks_pass_a_current_schema() {
        let c = conn();
        c.pragma_update(None, "user_version", 8i64).unwrap();
        let schema = index_checks(&c, 8)
            .into_iter()
            .find(|c| c.name == "index schema")
            .unwrap();
        assert_eq!(schema.status, Status::Ok);
    }
}
