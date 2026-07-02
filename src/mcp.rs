//! A hand-rolled, stdio-only MCP server: newline-delimited JSON-RPC 2.0,
//! three read-only tools over the local session index. No network (stdio only),
//! no sync, no writes (the DB handle is read-only). The server owns every byte
//! written to stdout; all logging goes to stderr.

use rusqlite::Connection;
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};

const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
const KNOWN_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];
const LATEST_VERSION: &str = "2025-11-25";
const MAX_LINE_BYTES: u64 = 1024 * 1024;

fn ok_response(id: Value, result: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

fn err_response(id: Value, code: i64, message: &str) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string()
}

/// Pure dispatcher. `None` means "notification or non-request: write nothing".
/// The returned String is a bare compact JSON message (no trailing newline);
/// the serve loop adds the single framing newline.
pub(crate) fn handle_line(conn: &mut Option<Connection>, line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let msg: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return Some(err_response(Value::Null, -32700, "parse error")),
    };
    if msg.is_array() {
        return Some(err_response(
            Value::Null,
            -32600,
            "batch requests are not supported",
        ));
    }
    let method = msg.get("method").and_then(Value::as_str);
    let id = msg.get("id").cloned();
    match (method, id) {
        (Some(method), Some(id)) => {
            let params = msg.get("params").cloned().unwrap_or(Value::Null);
            let id_for_panic = id.clone();
            // The server is long-lived and must not die on an unexpected panic:
            // convert one into an internal-error response instead of exiting.
            let dispatched = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                dispatch(conn, method, &params, id)
            }));
            Some(
                dispatched.unwrap_or_else(|_| err_response(id_for_panic, -32603, "internal error")),
            )
        }
        // Notification (method, no id) or a stray response (no method): ignore.
        _ => None,
    }
}

fn dispatch(conn: &mut Option<Connection>, method: &str, params: &Value, id: Value) -> String {
    match method {
        "initialize" => ok_response(id, initialize_result(params)),
        "ping" => ok_response(id, json!({})),
        "tools/list" => ok_response(id, tools_list()),
        "tools/call" => match tool_call(conn, params) {
            Ok(result) => ok_response(id, result),
            Err((code, msg)) => err_response(id, code, &msg),
        },
        other => err_response(id, -32601, &format!("Method not found: {other}")),
    }
}

fn initialize_result(params: &Value) -> Value {
    let requested = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or("");
    // Echo a version we know; otherwise offer our newest and let the client
    // decide whether to proceed (MCP negotiation rule).
    let version = if KNOWN_VERSIONS.contains(&requested) {
        requested
    } else {
        LATEST_VERSION
    };
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "sessionwiki", "version": SERVER_VERSION},
    })
}

fn tools_list() -> Value {
    json!({"tools": [
        {
            "name": "search_sessions",
            "title": "Search AI coding sessions",
            "description": "Full-text search across every indexed AI coding session (Claude Code, Codex, Gemini, aider, and more). Returns matching sessions with a snippet. Read-only, local.",
            "annotations": {"readOnlyHint": true},
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Search text, minimum 3 characters."},
                    "tool": {"type": "string", "description": "Restrict to one tool, e.g. codex."},
                    "project": {"type": "string", "description": "Restrict to a project (substring match)."},
                    "limit": {"type": "integer", "description": "Max results, 1-50 (default 10)."}
                },
                "required": ["query"]
            }
        },
        {
            "name": "trace_file",
            "title": "Trace a file to the sessions that touched it",
            "description": "List the AI coding sessions that edited or created a given file, across every tool. Read-only, local.",
            "annotations": {"readOnlyHint": true},
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "A file path; the repo-relative tail is matched, e.g. src/auth.rs."}
                },
                "required": ["path"]
            }
        },
        {
            "name": "get_session_brief",
            "title": "Get a bounded briefing of one session",
            "description": "Return a markdown briefing (head and tail) of one session by its short id from search or trace results. Read-only, local.",
            "annotations": {"readOnlyHint": true},
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "The short session id from search_sessions or trace_file."},
                    "max_chars": {"type": "integer", "description": "Max briefing length, 1-20000 (default 4000)."}
                },
                "required": ["id"]
            }
        }
    ]})
}

fn tool_call(_conn: &mut Option<Connection>, _params: &Value) -> Result<Value, (i64, String)> {
    Err((-32602, "no tools yet".into()))
}

/// The blocking stdio serve loop. Owns every byte written to stdout; EOF on
/// stdin exits cleanly.
pub fn serve() {
    let stdin = std::io::stdin();
    let mut reader = std::io::BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut conn: Option<Connection> = None;
    loop {
        let mut buf = Vec::new();
        let n = match (&mut reader)
            .take(MAX_LINE_BYTES)
            .read_until(b'\n', &mut buf)
        {
            Ok(0) => break, // EOF -> clean exit
            Ok(n) => n,
            Err(_) => break,
        };
        let has_newline = buf.last() == Some(&b'\n');
        if !has_newline && n as u64 == MAX_LINE_BYTES {
            // Oversized line with no terminator: drain to the next newline and
            // reject, rather than buffer an unbounded line into memory.
            let mut discard = Vec::new();
            let _ = reader.read_until(b'\n', &mut discard);
            let msg = err_response(Value::Null, -32700, "message too large");
            if write_line(&mut out, &msg).is_err() {
                break;
            }
            continue;
        }
        let line = String::from_utf8_lossy(&buf);
        if let Some(reply) = handle_line(&mut conn, &line) {
            if write_line(&mut out, &reply).is_err() {
                break; // client closed stdout; stop
            }
        }
    }
}

fn write_line<W: Write>(out: &mut W, msg: &str) -> std::io::Result<()> {
    out.write_all(msg.as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn call(line: &str) -> Option<Value> {
        let mut conn = None;
        handle_line(&mut conn, line).map(|s| {
            assert!(!s.contains('\n'), "reply must be single-line: {s}");
            serde_json::from_str(&s).unwrap()
        })
    }

    #[test]
    fn initialize_echoes_known_version_and_advertises_tools() {
        let v = call(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"c","version":"1"}}}"#).unwrap();
        assert_eq!(v["id"], 1);
        assert_eq!(v["result"]["protocolVersion"], "2025-06-18");
        assert!(v["result"]["capabilities"]["tools"].is_object());
        assert_eq!(v["result"]["serverInfo"]["name"], "sessionwiki");
    }

    #[test]
    fn initialize_unknown_version_falls_back_to_latest() {
        let v = call(r#"{"jsonrpc":"2.0","id":"x","method":"initialize","params":{"protocolVersion":"2099-01-01"}}"#).unwrap();
        assert_eq!(v["id"], "x");
        assert_eq!(v["result"]["protocolVersion"], "2025-11-25");
    }

    #[test]
    fn ping_returns_empty_result() {
        let v = call(r#"{"jsonrpc":"2.0","id":9,"method":"ping"}"#).unwrap();
        assert_eq!(v["result"], serde_json::json!({}));
    }

    #[test]
    fn notifications_get_no_reply() {
        assert!(call(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).is_none());
        assert!(
            call(r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{}}"#).is_none()
        );
    }

    #[test]
    fn unknown_method_is_minus_32601() {
        let v = call(r#"{"jsonrpc":"2.0","id":3,"method":"resources/list"}"#).unwrap();
        assert_eq!(v["error"]["code"], -32601);
    }

    #[test]
    fn parse_error_is_minus_32700_id_null() {
        let v = call("{not json").unwrap();
        assert_eq!(v["error"]["code"], -32700);
        assert!(v["id"].is_null());
    }

    #[test]
    fn tools_list_has_three_readonly_tools() {
        let v = call(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap();
        let tools = v["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            ["search_sessions", "trace_file", "get_session_brief"]
        );
        for t in tools {
            assert_eq!(t["annotations"]["readOnlyHint"], true);
            assert_eq!(t["inputSchema"]["type"], "object");
        }
        assert!(v["result"].get("nextCursor").is_none());
    }

    #[test]
    fn batch_array_is_minus_32600() {
        let v = call(r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#).unwrap();
        assert_eq!(v["error"]["code"], -32600);
    }
}
