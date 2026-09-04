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

    // Real reads of each core table: a query error is a genuine problem (missing
    // table, lock, read-corruption), never a healthy empty index - so it must not
    // become a silent "ok, 0". (Full PRAGMA integrity_check needs write access for
    // the FTS5 index, so it can't run on this read-only connection.)
    const CORE: &[&str] = &["files", "messages", "edits", "archive", "summaries"];
    let unreadable: Vec<&str> = CORE
        .iter()
        .copied()
        .filter(|t| {
            conn.query_row(&format!("SELECT count(*) FROM {t}"), [], |r| {
                r.get::<_, i64>(0)
            })
            .is_err()
        })
        .collect();
    checks.push(if unreadable.is_empty() {
        Check::ok("index tables", "all core tables readable".into())
    } else {
        Check::fail(
            "index tables",
            format!("unreadable: {}", unreadable.join(", ")),
        )
    });

    // Session counts (main sessions, matching the rest of the CLI's kind='main'
    // convention). A count error is a Fail, not a healthy-looking 0.
    match conn.query_row("SELECT count(*) FROM files WHERE kind='main'", [], |r| {
        r.get::<_, i64>(0)
    }) {
        Ok(main) => {
            let archived: i64 = conn
                .query_row(
                    "SELECT count(*) FROM files WHERE archived_at IS NOT NULL",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            checks.push(Check::ok(
                "indexed sessions",
                format!("{main} ({archived} kept after the tool deleted them)"),
            ));
        }
        Err(e) => checks.push(Check::fail(
            "indexed sessions",
            format!("count failed: {e}"),
        )),
    }

    checks
}

/// Which session stores exist on this machine, and how many sessions each holds.
/// A store that is simply absent is normal (not every tool is installed) and is
/// left off the list; the warn fires only when NONE are found.
/// What doctor should say about an adapter whose store location could not be
/// worked out. `None` when the location IS known - there is nothing to add.
///
/// Skipping it, which is what used to happen, gave the same silence as a tool
/// that is simply not installed. Those are different facts: one is "there is
/// nothing to look at", the other is "this machine would not tell me where to
/// look", and only the second is a gap in what doctor can report.
///
/// This is defence, not a fix for something observed: on Unix `dirs` falls
/// back to the passwd database, so unsetting HOME does not make `root()`
/// answer None. It can on a platform or a build where that fallback is not
/// there, and doctor should not go quiet when it does.
pub fn unlocatable_store(name: &str, root: Option<&std::path::Path>) -> Option<Check> {
    root.is_none().then(|| {
        Check::warn(
            &format!("store: {name}"),
            "cannot work out where its sessions would live on this machine \
             (no home or data directory) - it was not checked"
                .into(),
        )
    })
}

pub fn store_checks() -> Vec<Check> {
    let mut checks = Vec::new();
    let mut any = false;
    for adapter in crate::adapters::all() {
        let located = adapter.root();
        if let Some(c) = unlocatable_store(adapter.name(), located.as_deref()) {
            checks.push(c);
            any = true;
            continue;
        }
        let root = located.expect("checked just above");
        if !root.exists() {
            continue; // an absent store is normal - not every tool is installed
        }
        any = true;
        // Confirm the root is readable WITHOUT a full recursive walk: a large
        // store (a 40GB codex history) would make `doctor` crawl. A shallow
        // read_dir still catches a permission problem.
        match std::fs::read_dir(&root) {
            Ok(_) => checks.push(Check::ok(
                &format!("store: {}", adapter.name()),
                format!("present at {}", root.display()),
            )),
            Err(e) => checks.push(Check::warn(
                &format!("store: {}", adapter.name()),
                format!("present but unreadable: {e}"),
            )),
        }
    }
    if !any {
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
    use rusqlite::OpenFlags;
    let mut checks = store_checks();
    // existing_db_path() has NO side effects (unlike open()/open_readonly, which
    // create the data dir and can rename legacy dirs) - so `doctor` stays truly
    // read-only. Absent index vs an index that won't open are DIFFERENT problems.
    match crate::index::existing_db_path() {
        None => checks.push(Check::warn(
            "index",
            "not built yet - run `sessionwiki search` to build it".into(),
        )),
        Some(path) => match Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
            Ok(conn) => checks.extend(index_checks(&conn, crate::index::SCHEMA_VERSION)),
            Err(e) => checks.push(Check::fail(
                "index",
                format!("present but cannot open ({}): {e}", path.display()),
            )),
        },
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
            "CREATE TABLE files(path TEXT PRIMARY KEY, session_id TEXT NOT NULL, archived_at TEXT,
                 kind TEXT NOT NULL DEFAULT 'main');
             CREATE TABLE messages(id INTEGER PRIMARY KEY);
             CREATE TABLE edits(session_id TEXT);
             CREATE TABLE archive(session_id TEXT);
             CREATE TABLE summaries(session_id TEXT);",
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
        assert_eq!(tables.status, Status::Ok, "all core tables readable");
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
    fn a_missing_core_table_is_a_fail_not_a_healthy_zero() {
        let c = conn();
        c.execute("DROP TABLE edits", []).unwrap();
        let tables = index_checks(&c, 8)
            .into_iter()
            .find(|c| c.name == "index tables")
            .unwrap();
        assert_eq!(
            tables.status,
            Status::Fail,
            "an unreadable core table must fail, not look like an empty index"
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

#[cfg(test)]
mod unlocatable_store_tests {
    use super::*;

    /// `store_checks` skipped an adapter whose `root()` answered None with the
    /// same silence it uses for a tool that simply is not installed. Those are
    /// different facts: one is "there is nothing to look at", the other is
    /// "this machine would not tell me where to look" - and doctor exists to
    /// report the second. Not reproduced: on Unix `dirs` falls back to the
    /// passwd database, so even with HOME unset `root()` still answers. This
    /// pins the behaviour for the platforms where it would not.
    #[test]
    fn an_unlocatable_store_is_reported_not_skipped() {
        let c = unlocatable_store("gptme", None).expect("this is worth saying");
        assert_eq!(c.status, Status::Warn);
        assert!(c.name.contains("gptme"), "names the tool: {}", c.name);
    }

    #[test]
    fn a_located_store_has_nothing_extra_to_say() {
        let p = std::path::Path::new("/home/someone/.local/share/gptme/logs");
        assert!(unlocatable_store("gptme", Some(p)).is_none());
    }
}
