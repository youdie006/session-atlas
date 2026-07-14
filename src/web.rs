use crate::{adapters, index, resume};
use anyhow::{Context, Result};
use rusqlite::Connection;
use serde_json::json;
use tiny_http::{Header, Response, Server};

/// Local read-only viewer over the index. By default it does not sync, so it
/// stays snappy and can run while a CLI index pass is in progress (WAL allows
/// concurrent readers) - which means sessions created after the last
/// `list`/`search` won't show until the index is refreshed. Pass `sync` to
/// bring the index up to date once before serving.
pub fn serve(port: u16, no_open: bool, sync: bool) -> Result<()> {
    let mut conn = index::open()?;
    if sync {
        index::sync(&mut conn, None)?;
    }
    let addr = format!("127.0.0.1:{port}");
    let server = Server::http(&addr).map_err(|e| anyhow::anyhow!("bind {addr}: {e}"))?;
    let url = format!("http://{addr}");
    println!("sessionwiki web: {url}");
    if !sync {
        println!("(read-only view; run `sessionwiki list` or `web --sync` to refresh the index)");
    }
    let wsl = is_wsl();
    if should_open_browser(no_open, wsl) {
        open_browser(&url);
    } else if wsl && !no_open {
        println!("(WSL detected - open the URL above in your browser; auto-open is off here to avoid stray windows)");
    }

    for request in server.incoming_requests() {
        // Defend against DNS rebinding: a page on a malicious site can rebind its
        // own hostname to 127.0.0.1 and have the browser fetch this server, then
        // read your sessions same-origin. Such a request carries the attacker's
        // hostname in the Host header, so only serve requests addressed to the
        // loopback name we actually bound.
        let host = request
            .headers()
            .iter()
            .find(|h| h.field.equiv("Host"))
            .map(|h| h.value.as_str().to_string());
        let origin = request
            .headers()
            .iter()
            .find(|h| h.field.equiv("Origin"))
            .map(|h| h.value.as_str().to_string());
        // Host check defeats DNS rebinding (an attacker hostname rebound to
        // 127.0.0.1 carries its name in Host). The Origin check is a second layer
        // against a plain cross-origin fetch from a malicious site, which the
        // browser stamps with its Origin - we don't rely on the same-origin
        // policy alone.
        if !host_matches(host.as_deref(), port) || !origin_ok(origin.as_deref(), port) {
            let _ = request.respond(Response::from_string("forbidden").with_status_code(403));
            continue;
        }
        let url = request.url().to_string();
        let (path, query) = url.split_once('?').unwrap_or((url.as_str(), ""));
        let result = match path {
            "/" => html(INDEX_HTML),
            "/api/stats" => api_stats(&conn),
            "/api/sessions" => api_sessions(&conn, query),
            "/api/search" => api_search(&conn, query),
            "/api/trace" => api_trace(&conn, query),
            "/api/projects" => api_projects(&conn),
            p if p.starts_with("/api/related/") => {
                api_related(&conn, p.trim_start_matches("/api/related/"))
            }
            p if p.starts_with("/api/session/") => {
                api_session(&conn, p.trim_start_matches("/api/session/"))
            }
            _ => Ok(Response::from_string("not found")
                .with_status_code(404)
                .boxed()),
        };
        let response = match result {
            Ok(r) => r,
            Err(e) => Response::from_string(json!({ "error": e.to_string() }).to_string())
                .with_status_code(500)
                .boxed(),
        };
        let _ = request.respond(response);
    }
    Ok(())
}

type Boxed = Response<Box<dyn std::io::Read + Send>>;

/// True only for the loopback host:port we bound. Anything else (notably an
/// attacker hostname rebound to 127.0.0.1) is rejected, defeating DNS rebinding.
fn host_matches(host: Option<&str>, port: u16) -> bool {
    match host {
        Some(h) => {
            h == format!("127.0.0.1:{port}")
                || h == format!("localhost:{port}")
                || h == format!("[::1]:{port}")
        }
        None => false,
    }
}

/// A present Origin that is not our own loopback origin means a cross-origin
/// request (e.g. a `fetch` from a malicious site) - refuse it. A missing Origin
/// (top-level navigation, same-origin GET) is allowed.
fn origin_ok(origin: Option<&str>, port: u16) -> bool {
    match origin {
        None => true,
        Some(o) => {
            o == format!("http://127.0.0.1:{port}")
                || o == format!("http://localhost:{port}")
                || o == format!("http://[::1]:{port}")
        }
    }
}

fn html(body: &str) -> Result<Boxed> {
    Ok(Response::from_string(body)
        .with_header(
            Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..]).unwrap(),
        )
        .boxed())
}

fn json_response(v: serde_json::Value) -> Result<Boxed> {
    Ok(Response::from_string(v.to_string())
        .with_header(
            Header::from_bytes(
                &b"Content-Type"[..],
                &b"application/json; charset=utf-8"[..],
            )
            .unwrap(),
        )
        .boxed())
}

fn api_stats(conn: &Connection) -> Result<Boxed> {
    let mut stmt = conn.prepare(
        "SELECT tool, count(*), sum(size), sum(msg_count) FROM files GROUP BY tool ORDER BY 2 DESC",
    )?;
    let rows: Vec<serde_json::Value> = stmt
        .query_map([], |r| {
            Ok(json!({
                "tool": r.get::<_, String>(0)?,
                "sessions": r.get::<_, i64>(1)?,
                "bytes": r.get::<_, i64>(2)?,
                "messages": r.get::<_, i64>(3)?,
            }))
        })?
        .collect::<rusqlite::Result<_>>()?;
    json_response(json!({ "tools": rows }))
}

fn api_sessions(conn: &Connection, query: &str) -> Result<Boxed> {
    let tool = param(query, "tool");
    let project = param(query, "project");
    let tag = param(query, "tag");
    let limit = param(query, "limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);
    let rows = index::recent(
        conn,
        limit,
        tool.as_deref(),
        project.as_deref(),
        tag.as_deref(),
        false,
    )?;
    json_response(json!(rows.iter().map(row_json).collect::<Vec<_>>()))
}

fn api_projects(conn: &Connection) -> Result<Boxed> {
    let rows = index::projects(conn)?;
    json_response(json!(rows
        .iter()
        .map(|p| json!({
            "project": p.project,
            "sessions": p.sessions,
            "messages": p.messages,
            "newest": p.newest,
        }))
        .collect::<Vec<_>>()))
}

fn api_related(conn: &Connection, id: &str) -> Result<Boxed> {
    let rel = index::related(conn, id, 8)?;
    json_response(json!(rel.iter().map(row_json).collect::<Vec<_>>()))
}

fn api_search(conn: &Connection, query: &str) -> Result<Boxed> {
    let q = param(query, "q").unwrap_or_default();
    let qt = q.trim();
    if qt.is_empty() {
        return json_response(json!([]));
    }
    let tool = param(query, "tool");
    let limit = param(query, "limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(50);
    // <3 chars (incl. 2-syllable Korean) is below the trigram floor; LIKE-scan it.
    let hits = if crate::util::nfc(qt).chars().count() < 3 {
        index::search_like(conn, qt, limit, tool.as_deref(), None)?
    } else {
        index::search(conn, qt, limit, tool.as_deref(), None)?
    };
    json_response(json!(hits
        .iter()
        .map(|h| {
            let mut v = row_json(&h.row);
            let (plain, marked) = crate::commands::clean_snippet(&h.snippet);
            v["snippet"] = json!(plain);
            v["snippet_marked"] = json!(marked);
            v["role"] = json!(h.role);
            v
        })
        .collect::<Vec<_>>()))
}

/// Reverse provenance lookup: sessions that touched a file. Each result
/// carries the matched stored path, so the UI can show what it resolved to.
fn api_trace(conn: &Connection, query: &str) -> Result<Boxed> {
    let path = param(query, "path").unwrap_or_default();
    if path.is_empty() {
        return json_response(json!([]));
    }
    let hits = index::sessions_for_file(conn, &path, 50)?;
    json_response(json!(hits
        .iter()
        .map(|(r, matched)| {
            let mut v = row_json(r);
            v["matched"] = json!(matched);
            v
        })
        .collect::<Vec<_>>()))
}

fn api_session(conn: &Connection, id: &str) -> Result<Boxed> {
    let matches = index::resolve(conn, id)?;
    let row = matches.first().context("session not found")?;
    let path = std::path::Path::new(&row.path);
    // Archived sessions (original deleted by the tool) are served from the
    // index; live ones are re-parsed from the file for full fidelity.
    let session = if path.exists() {
        let adapter = adapters::by_name(&row.tool).context("unknown tool")?;
        adapter.parse(path)?
    } else {
        index::session_from_index(conn, row)?
    };
    let mut v = serde_json::to_value(&session)?;
    if row.archived {
        v["archived"] = json!(true);
    }
    if let Some(info) = resume::for_session(&row.tool, path, &row.project) {
        v["resume"] = json!(info.command_line());
    }
    if let Some(s) = &row.summary {
        v["summary"] = json!(s);
    }
    if let Some(t) = &row.tags {
        v["tags"] = json!(t.split(',').collect::<Vec<_>>());
    }
    if let Some(note) = index::note_for(conn, &row.session_id)? {
        v["note"] = json!(note);
    }
    json_response(v)
}

/// Delegate to SessionRow's Serialize derive so the web JSON and the CLI
/// `--json` contract are the exact same field set (id/msgs/tags-array/...),
/// and can never drift apart.
fn row_json(r: &index::SessionRow) -> serde_json::Value {
    serde_json::to_value(r).unwrap_or_else(|_| json!({}))
}

fn param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key && !v.is_empty()).then(|| url_decode(v))
    })
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => match (hex(bytes.get(i + 1)), hex(bytes.get(i + 2))) {
                (Some(h), Some(l)) => {
                    out.push(h * 16 + l);
                    i += 3;
                }
                _ => {
                    out.push(b'%');
                    i += 1;
                }
            },
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: Option<&u8>) -> Option<u8> {
    (*b? as char).to_digit(16).map(|d| d as u8)
}

/// True when running under WSL, where `xdg-open` pops a WSLg/Windows browser
/// window on EVERY `web` start (a stray blank window during repeated testing)
/// and often can't even reach the loopback server. Detected via the kernel
/// string, which carries "microsoft" on WSL1/WSL2.
fn is_wsl() -> bool {
    std::fs::read_to_string("/proc/version")
        .map(|v| v.to_ascii_lowercase().contains("microsoft"))
        .unwrap_or(false)
}

/// Whether to auto-open the browser: never when `--no-open`, and never on WSL
/// (print the URL there instead - see `is_wsl`).
fn should_open_browser(no_open: bool, wsl: bool) -> bool {
    !no_open && !wsl
}

fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(target_os = "windows")]
    let cmd = "explorer";
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let cmd = "xdg-open";
    let _ = std::process::Command::new(cmd)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

const INDEX_HTML: &str = include_str!("webui.html");

#[cfg(test)]
mod tests {
    use super::{host_matches, origin_ok, should_open_browser};

    #[test]
    fn wsl_never_auto_opens_a_browser() {
        // Native (not WSL): open unless the user said --no-open.
        assert!(should_open_browser(false, false));
        assert!(!should_open_browser(true, false));
        // WSL: never auto-open, so `web` stops popping a stray Chrome/WSLg
        // window on every start - the URL is printed instead.
        assert!(!should_open_browser(false, true));
        assert!(!should_open_browser(true, true));
    }

    #[test]
    fn host_matches_only_loopback() {
        assert!(host_matches(Some("127.0.0.1:7575"), 7575));
        assert!(host_matches(Some("localhost:7575"), 7575));
        assert!(host_matches(Some("[::1]:7575"), 7575));
        // DNS-rebinding and cross-origin attempts:
        assert!(!host_matches(Some("evil.com:7575"), 7575));
        assert!(!host_matches(Some("127.0.0.1:7575"), 7576)); // wrong port
        assert!(!host_matches(Some("127.0.0.1"), 7575)); // no port
        assert!(!host_matches(None, 7575)); // missing Host
    }

    #[test]
    fn origin_ok_rejects_cross_origin() {
        assert!(origin_ok(None, 7575)); // same-origin GET / navigation
        assert!(origin_ok(Some("http://127.0.0.1:7575"), 7575));
        assert!(origin_ok(Some("http://localhost:7575"), 7575));
        assert!(!origin_ok(Some("http://evil.com"), 7575)); // cross-origin fetch
        assert!(!origin_ok(Some("https://127.0.0.1:7575"), 7575)); // wrong scheme
        assert!(!origin_ok(Some("http://127.0.0.1:7576"), 7575)); // wrong port
    }
}
