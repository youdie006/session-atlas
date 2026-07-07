//! swapdex integration: sessions are attributed to the account profile active
//! when they started, by reading swapdex's switch timeline read-only. No
//! timeline -> account stays null. Field is part of the --json contract.

use rusqlite::{params, Connection};
use sessionwiki::index;
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());

fn fresh(tag: &str) -> Connection {
    let dir = std::env::temp_dir().join(format!("sessionwiki-test-acct-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("SESSIONWIKI_DATA", &dir);
    let conn = index::open().unwrap();
    conn.execute_batch(
        "DELETE FROM files; DELETE FROM messages; DELETE FROM tags;
         DELETE FROM notes; DELETE FROM touched; DELETE FROM archive;",
    )
    .unwrap();
    conn
}

fn seed(conn: &Connection, id: &str, tool: &str, started: &str) {
    conn.execute(
        "INSERT OR REPLACE INTO files
         (path, mtime, size, session_id, tool, project, title, started, ended, msg_count, kind)
         VALUES (?1,0,0,?2,?3,'/proj/api','fix auth bug',?4,?4,2,'main')",
        params![format!("/abs/{id}.jsonl"), id, tool, started],
    )
    .unwrap();
}

#[test]
fn rows_carry_the_active_account_from_the_swapdex_timeline() {
    let _g = LOCK.lock().unwrap();
    let conn = fresh("attr");
    // Switch to 'work' at t=1000, to 'personal' at t=2000 (codex only).
    let tl = std::env::temp_dir().join("sessionwiki-test-acct-attr-timeline.jsonl");
    std::fs::write(
        &tl,
        concat!(
            "{\"ts\":1000,\"tool\":\"codex\",\"account\":\"work\",\"action\":\"use\"}\n",
            "{\"ts\":2000,\"tool\":\"codex\",\"account\":\"personal\",\"action\":\"use\"}\n",
        ),
    )
    .unwrap();
    std::env::set_var("SESSIONWIKI_SWAPDEX_TIMELINE", &tl);

    let t1500 = chrono::DateTime::from_timestamp(1500, 0)
        .unwrap()
        .to_rfc3339();
    let t2500 = chrono::DateTime::from_timestamp(2500, 0)
        .unwrap()
        .to_rfc3339();
    let t500 = chrono::DateTime::from_timestamp(500, 0)
        .unwrap()
        .to_rfc3339();
    seed(&conn, "s-mid", "codex", &t1500); // -> work
    seed(&conn, "s-new", "codex", &t2500); // -> personal
    seed(&conn, "s-old", "codex", &t500); // predates switches -> null
    seed(&conn, "s-claude", "claude-code", &t2500); // no claude events -> null

    let rows = index::recent(&conn, 10, None, None, None, false).unwrap();
    let acct = |id: &str| {
        rows.iter()
            .find(|r| r.session_id == id)
            .unwrap()
            .account
            .clone()
    };
    assert_eq!(acct("s-mid").as_deref(), Some("work"));
    assert_eq!(acct("s-new").as_deref(), Some("personal"));
    assert_eq!(acct("s-old"), None, "predates every switch");
    assert_eq!(
        acct("s-claude"),
        None,
        "other tool's events never bleed over"
    );

    // JSON contract: the field is named `account` (null when unknown).
    let j = serde_json::to_string(&rows).unwrap();
    assert!(j.contains("\"account\":\"work\""), "{j}");
    assert!(j.contains("\"account\":null"), "{j}");

    std::env::remove_var("SESSIONWIKI_SWAPDEX_TIMELINE");
}

#[test]
fn no_timeline_means_null_accounts() {
    let _g = LOCK.lock().unwrap();
    std::env::set_var(
        "SESSIONWIKI_SWAPDEX_TIMELINE",
        std::env::temp_dir().join("sessionwiki-test-acct-none.jsonl"), // absent
    );
    let conn = fresh("none");
    let t = chrono::DateTime::from_timestamp(1500, 0)
        .unwrap()
        .to_rfc3339();
    seed(&conn, "s1", "codex", &t);
    let rows = index::recent(&conn, 10, None, None, None, false).unwrap();
    assert_eq!(rows[0].account, None);
    std::env::remove_var("SESSIONWIKI_SWAPDEX_TIMELINE");
}

#[test]
fn trace_and_related_rows_carry_accounts_too() {
    let _g = LOCK.lock().unwrap();
    let conn = fresh("trace");
    let tl = std::env::temp_dir().join("sessionwiki-test-acct-trace-tl.jsonl");
    std::fs::write(
        &tl,
        "{\"ts\":1000,\"tool\":\"codex\",\"account\":\"work\",\"action\":\"use\"}\n",
    )
    .unwrap();
    std::env::set_var("SESSIONWIKI_SWAPDEX_TIMELINE", &tl);
    let t = chrono::DateTime::from_timestamp(1500, 0)
        .unwrap()
        .to_rfc3339();
    seed(&conn, "s-t", "codex", &t);
    // touched: file -> session join used by trace
    conn.execute(
        "INSERT INTO touched(session_id, path) VALUES ('s-t','src/main.rs')",
        [],
    )
    .unwrap();
    let rows = index::sessions_for_file(&conn, "src/main.rs", 10).unwrap();
    assert!(!rows.is_empty(), "trace found the session");
    assert_eq!(
        rows[0].0.account.as_deref(),
        Some("work"),
        "trace rows must be annotated like list rows"
    );
    std::env::remove_var("SESSIONWIKI_SWAPDEX_TIMELINE");
}

// trace with an ABSOLUTE editor path must find a session whose stored path is
// repo-relative (prodex bundles relative paths; editors show absolute ones).
#[test]
fn trace_matches_absolute_query_against_relative_stored_path() {
    let _g = LOCK.lock().unwrap();
    let conn = fresh("abs");
    let t = chrono::DateTime::from_timestamp(1500, 0)
        .unwrap()
        .to_rfc3339();
    seed(&conn, "s-abs", "prodex", &t);
    conn.execute(
        "INSERT INTO touched(session_id, path) VALUES ('s-abs','src/db-pool.ts')",
        [],
    )
    .unwrap();
    let hit = index::sessions_for_file(&conn, "/home/dev/api/src/db-pool.ts", 10).unwrap();
    assert_eq!(hit.len(), 1, "absolute query reaches the relative path");
    // Boundary is honored: a different file with the same suffix chars only.
    let miss = index::sessions_for_file(&conn, "/home/dev/api/srcx/db-pool.ts", 10).unwrap();
    assert!(miss.len() <= 1, "no metachar explosion");
    let none = index::sessions_for_file(&conn, "/home/dev/b-pool.ts", 10).unwrap();
    assert!(none.is_empty(), "substring-without-boundary must not match");
}
