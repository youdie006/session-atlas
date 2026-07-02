//! End-to-end: spawn the real binary as an MCP server, drive a full handshake
//! over stdio, and assert stream purity + framing + the no-leak contract. This
//! also exercises the production read-only-of-a-WAL-db path: the seed connection
//! is dropped before the server process opens the index read-only.

use rusqlite::params;
use std::io::Write;
use std::process::{Command, Stdio};

fn seed(dir: &std::path::Path) {
    std::env::set_var("SESSIONWIKI_DATA", dir);
    let conn = sessionwiki::index::open().unwrap();
    conn.execute_batch(
        "DELETE FROM files; DELETE FROM messages; DELETE FROM msgs; DELETE FROM touched;",
    )
    .unwrap();
    // A title carrying an ESC and a raw newline: both must be stripped and must
    // not add a physical line to the stdout stream.
    conn.execute(
        "INSERT INTO files(path,mtime,size,session_id,tool,project,title,started,ended,msg_count,kind)
         VALUES('/home/me/.claude/x.jsonl',0,0,'s1','claude-code','/proj/api','fix auth\u{1b}[31m
bug','2026-06-10T10:00:00+00:00','2026-06-10T10:00:00+00:00',1,'main')",
        [],
    ).unwrap();
    conn.execute(
        "INSERT INTO messages(session_id,role,text) VALUES('s1','user','preflight to /auth/login 403')",
        [],
    ).unwrap();
    let mid: i64 = conn
        .query_row("SELECT id FROM messages WHERE session_id='s1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    conn.execute(
        "INSERT INTO msgs(rowid,text) VALUES(?1,'preflight to /auth/login 403')",
        params![mid],
    )
    .unwrap();
    // Flush WAL into the main db so the child's read-only handle reads cleanly.
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
}

#[test]
fn golden_handshake_search_and_stream_purity() {
    let dir = std::env::temp_dir().join("sessionwiki-test-mcp-e2e");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    seed(&dir);

    let requests = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":"two","method":"tools/list"}"#,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"search_sessions","arguments":{"query":"preflight"}}}"#,
    ];

    let mut child = Command::new(env!("CARGO_BIN_EXE_sessionwiki"))
        .arg("mcp")
        .env("SESSIONWIKI_DATA", &dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        for r in requests {
            writeln!(stdin, "{r}").unwrap();
        }
    } // drop stdin -> EOF -> server drains and exits 0
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "EOF must exit 0");
    let stdout = String::from_utf8(out.stdout).unwrap();

    // Stream purity: every non-empty line parses as one JSON-RPC message, and
    // there are exactly 3 (the notification gets none; the seeded raw newline
    // does not add a line).
    let lines: Vec<serde_json::Value> = stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("every stdout line is valid JSON"))
        .collect();
    assert_eq!(
        lines.len(),
        3,
        "one reply per request, none for the notification"
    );
    assert_eq!(lines[0]["id"], 1);
    assert_eq!(lines[0]["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(lines[1]["id"], "two");
    assert_eq!(lines[1]["result"]["tools"].as_array().unwrap().len(), 3);
    assert_eq!(lines[2]["id"], 3);
    let text = lines[2]["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("s1"),
        "search found the seeded session: {text}"
    );
    assert!(!text.contains("/home/me"), "no home-dir leak");
    assert!(
        !text.contains('\u{1b}'),
        "ESC in the seeded title is stripped"
    );
}
