//! A partial directory walk (an unreadable project dir) must not archive live
//! sessions: `Discovered::had_error` makes sync skip deletion reconciliation
//! until a clean walk, mirroring the shared-store `Store::had_error` guard.

#![cfg(unix)]

use rusqlite::Connection;
use sessionwiki::index;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

fn archived(conn: &Connection, like: &str) -> Option<bool> {
    conn.query_row(
        "SELECT archived_at IS NOT NULL FROM files WHERE path LIKE ?1",
        [format!("%{like}%")],
        |r| r.get(0),
    )
    .ok()
}

fn chmod(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn partial_walk_does_not_archive_live_sessions() {
    // Root can read through 000 permissions, which would void the scenario.
    if unsafe { libc_geteuid() } == 0 {
        eprintln!("skipping: running as root");
        return;
    }

    // Isolated HOME (the claude-code root is derived from it) + isolated index.
    let home = std::env::temp_dir().join("sessionwiki-test-sync-guard");
    let _ = fs::remove_dir_all(&home); // leftover dirs were restored to 755 below
    let proj = home.join(".claude").join("projects").join("proj-a");
    fs::create_dir_all(&proj).unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/claude-code/proj-a/0a000000-0000-4000-8000-000000000001.jsonl");
    let session = proj.join("0a000000-0000-4000-8000-000000000001.jsonl");
    fs::copy(&fixture, &session).unwrap();

    std::env::set_var("HOME", &home);
    std::env::set_var("SESSIONWIKI_DATA", home.join("data"));

    // Clean sync: the session is indexed and live.
    let mut conn = index::open().unwrap();
    index::sync(&mut conn, Some("claude-code")).unwrap();
    assert_eq!(
        archived(&conn, "0a000000"),
        Some(false),
        "session indexed live after the first sync"
    );

    // Make the project dir unreadable: the walk still runs but cannot descend,
    // so the listing is partial. Without the guard this sync would archive the
    // session as if the tool had deleted it.
    chmod(&proj, 0o000);
    let r = index::sync(&mut conn, Some("claude-code"));
    chmod(&proj, 0o755); // restore before asserting so cleanup always works
    r.unwrap();
    assert_eq!(
        archived(&conn, "0a000000"),
        Some(false),
        "a partial walk must not archive live sessions"
    );

    // Clean walk again: still live.
    index::sync(&mut conn, Some("claude-code")).unwrap();
    assert_eq!(archived(&conn, "0a000000"), Some(false));

    // Control: a genuine deletion on a CLEAN walk still archives - the guard
    // must not have disabled reconciliation altogether.
    fs::remove_file(&session).unwrap();
    index::sync(&mut conn, Some("claude-code")).unwrap();
    assert_eq!(
        archived(&conn, "0a000000"),
        Some(true),
        "real deletions still archive on a clean walk"
    );
}

#[test]
fn parse_failures_are_warned_and_counted_not_swallowed() {
    if unsafe { libc_geteuid() } == 0 {
        eprintln!("skipping: running as root");
        return;
    }

    let home = std::env::temp_dir().join("sessionwiki-test-sync-warn");
    let _ = fs::remove_dir_all(&home);
    let proj = home.join(".claude").join("projects").join("proj-a");
    fs::create_dir_all(&proj).unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/claude-code/proj-a/0a000000-0000-4000-8000-000000000001.jsonl");
    fs::copy(
        &fixture,
        proj.join("0a000000-0000-4000-8000-000000000001.jsonl"),
    )
    .unwrap();
    // A session file that exists (discover lists it) but cannot be opened:
    // parse must fail loudly, not vanish from the corpus in silence.
    let bad = proj.join("0b000000-0000-4000-8000-000000000002.jsonl");
    fs::write(&bad, "{}").unwrap();
    chmod(&bad, 0o000);

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_sessionwiki"))
        .args(["sync", "--tool", "claude-code"])
        .env("HOME", &home)
        .env("SESSIONWIKI_DATA", home.join("data"))
        .output()
        .unwrap();
    chmod(&bad, 0o644); // restore for cleanup
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "sync must not die: {stderr}");
    assert!(
        stderr.contains("failed to parse"),
        "the failure must be warned: {stderr}"
    );
    assert!(
        stderr.contains("(1 failed to parse)"),
        "the count line must be honest: {stderr}"
    );
    assert!(
        stderr.contains("indexed 1/2"),
        "only the good session counts as indexed: {stderr}"
    );
}

/// The mirror this file's own header names, tested on the OTHER side.
///
/// Both paths carry the rule; only one said so. The discover path prints
/// "skipping deletion reconciliation this run"; the shared-store path - the one
/// aider takes - skipped in silence. Aider's walk is capped at two seconds over
/// the whole home, so on a large home the flag can be set every run,
/// reconciliation never happens, and nothing says why a session the tool really
/// did delete is still listed.
#[test]
fn a_partial_shared_store_walk_says_it_skipped_reconciliation() {
    if unsafe { libc_geteuid() } == 0 {
        eprintln!("skipping: running as root");
        return;
    }
    let t = tempfile::tempdir().unwrap();
    let data = t.path().join("data");
    let roots = t.path().join("roots");
    fs::create_dir_all(&data).unwrap();
    let locked = roots.join("locked");
    fs::create_dir_all(&locked).unwrap();
    chmod(&locked, 0o000);

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_sessionwiki"))
        .args(["sync", "--tool", "aider"])
        .env("SESSIONWIKI_DATA", &data)
        .env("SESSIONWIKI_AIDER_ROOTS", &roots)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    chmod(&locked, 0o755); // so the tempdir can be removed

    assert!(
        stderr.contains("skipping deletion reconciliation"),
        "a partial shared-store walk must say so rather than skipping in silence:\n{stderr}"
    );
}

extern "C" {
    #[link_name = "geteuid"]
    fn libc_geteuid() -> u32;
}
