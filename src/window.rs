//! A bounded, agent-consumable render of a session: the actual conversation
//! (not a lossy summary), but with tool-output bulk folded to head+tail and an
//! optional total budget that keeps the recent tail. This is what a caller (an
//! agent via MCP, or `show --window`) reads to know what another session is
//! doing without flooding its own context. The four things that make it
//! parseable: an orientation header, role labels, tool CALLS kept but tool
//! RESULTS folded, and a drill-down hint to `show <id> --full`.

use crate::model::{Message, Role, Session};
use serde_json::{json, Value};

/// Schema id for the JSON window returned over MCP. Bump the version ONLY on a
/// breaking change to field names or semantics, so a consuming agent can rely on
/// the contract.
pub const WINDOW_SCHEMA: &str = "sessionwiki.window/1";
/// Schema id for a single drilled-down turn (`session_window` with `turn`).
pub const TURN_SCHEMA: &str = "sessionwiki.turn/1";
/// Char cap for a single drilled-down turn's full text, sized to stay inside the
/// MCP text-content window; `clipped` reports when the original was longer.
pub const TURN_TEXT_CAP: usize = 23_000;

pub struct WindowOpts {
    /// Lines kept at the head of a folded tool result.
    pub tool_head: usize,
    /// Lines kept at the tail of a folded tool result.
    pub tool_tail: usize,
    /// Char cap for a single user/assistant message (head+tail beyond it).
    pub per_msg_chars: usize,
    /// Total char budget across the rendered turns; `None` folds the whole
    /// session. When set, the most RECENT turns are kept (walk from the end).
    pub budget_chars: Option<usize>,
    /// Byte cap for a single folded tool result. Line folding alone can't bound a
    /// result that is one enormous line, so the folded text is additionally
    /// clipped to this many bytes (on a char boundary).
    pub tool_byte_cap: usize,
}

impl Default for WindowOpts {
    fn default() -> Self {
        // ~4 chars/token: per-msg ~800 tok, budget (when set) ~1.5k tok.
        WindowOpts {
            tool_head: 3,
            tool_tail: 3,
            per_msg_chars: 3200,
            budget_chars: None,
            tool_byte_cap: 2000,
        }
    }
}

/// Largest index `<= max` that lands on a UTF-8 char boundary (stable-Rust
/// substitute for the unstable `str::floor_char_boundary`).
fn floor_char_boundary(s: &str, max: usize) -> usize {
    if max >= s.len() {
        return s.len();
    }
    let mut i = max;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Fold a multi-line tool result to its head and tail lines, marking the gap,
/// then byte-bound the result so a single huge line can't blow the budget.
/// Returns the folded text and whether anything was shortened. Short results
/// pass through unchanged with `false`.
fn fold_tool(text: &str, head: usize, tail: usize, byte_cap: usize) -> (String, bool) {
    let trimmed = text.trim_end();
    let lines: Vec<&str> = trimmed.lines().collect();
    let mut folded = lines.len() > head + tail + 1;
    let mut out = if folded {
        let elided = lines.len() - head - tail;
        let mut v: Vec<String> = lines[..head].iter().map(|s| s.to_string()).collect();
        v.push(format!("  [… {elided} lines …]"));
        v.extend(lines[lines.len() - tail..].iter().map(|s| s.to_string()));
        v.join("\n")
    } else {
        trimmed.to_string()
    };
    if out.len() > byte_cap {
        let cut = floor_char_boundary(&out, byte_cap);
        let dropped = out.len() - cut;
        out.truncate(cut);
        out.push_str(&format!("\n  [… {dropped} bytes …]"));
        folded = true;
    }
    (out, folded)
}

/// Cap a user/assistant message to `max` chars, keeping head and tail so the
/// intent and the conclusion both survive. Cuts on a char boundary.
fn cap_msg(text: &str, max: usize) -> String {
    let t = text.trim();
    if t.chars().count() <= max {
        return t.to_string();
    }
    let half = max / 2;
    let head: String = t.chars().take(half).collect();
    let tail: String = {
        let all: Vec<char> = t.chars().collect();
        all[all.len() - half..].iter().collect()
    };
    let elided = t.chars().count() - 2 * half;
    format!("{head}\n  [… {elided} chars …]\n{tail}")
}

fn render_msg(m: &crate::model::Message, opts: &WindowOpts) -> String {
    match m.role {
        Role::User => format!("[user]\n{}", cap_msg(&m.text, opts.per_msg_chars)),
        Role::Assistant => format!("[assistant]\n{}", cap_msg(&m.text, opts.per_msg_chars)),
        Role::Tool => format!(
            "[tool]\n{}",
            fold_tool(&m.text, opts.tool_head, opts.tool_tail, opts.tool_byte_cap).0
        ),
    }
}

/// Render the bounded window for `session`.
pub fn render_window(session: &Session, opts: &WindowOpts) -> String {
    let started = session
        .started
        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "?".into());
    let header = format!(
        "## {} [{}] · {} · {} · {} messages",
        session.title,
        session.tool,
        if session.project.is_empty() {
            "(no project)"
        } else {
            &session.project
        },
        started,
        session.messages.len()
    );

    let blocks: Vec<String> = session
        .messages
        .iter()
        .map(|m| render_msg(m, opts))
        .collect();

    let (kept, omitted) = match opts.budget_chars {
        None => (blocks, 0usize),
        Some(budget) => {
            // Keep the most recent turns within the budget (walk backward).
            let mut acc = 0usize;
            let mut taken: Vec<String> = Vec::new();
            for b in blocks.iter().rev() {
                if acc + b.len() > budget && !taken.is_empty() {
                    break;
                }
                acc += b.len();
                taken.push(b.clone());
            }
            let omitted = blocks.len() - taken.len();
            taken.reverse();
            (taken, omitted)
        }
    };

    let mut out = String::new();
    out.push_str(&header);
    out.push_str("\n\n");
    if omitted > 0 {
        out.push_str(&format!("[… {omitted} earlier turn(s) omitted …]\n\n"));
    }
    out.push_str(&kept.join("\n\n"));
    out.push_str(&format!(
        "\n\n→ full: sessionwiki show {} --full",
        session.id
    ));
    out
}

/// Split a leading `[large] ` flag (set by adapters when a session was indexed
/// head+tail because it was over the size cap) off a title.
fn split_large(title: &str) -> (bool, &str) {
    match title.strip_prefix("[large] ") {
        Some(rest) => (true, rest),
        None => (false, title),
    }
}

fn role_str(r: Role) -> &'static str {
    match r {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

/// One turn as JSON, plus the char length used for budget accounting. User and
/// assistant turns are head+tail capped (`truncated`); tool turns are folded
/// head+tail and byte-bounded (`folded`), carrying the original `bytes`. `i` is
/// the turn's index in the full session - the stable per-turn drill-down anchor.
fn turn_json(i: usize, m: &Message, opts: &WindowOpts) -> (Value, usize) {
    match m.role {
        Role::User | Role::Assistant => {
            let truncated = m.text.trim().chars().count() > opts.per_msg_chars;
            let text = cap_msg(&m.text, opts.per_msg_chars);
            let len = text.len();
            (
                json!({"i": i, "role": role_str(m.role), "text": text, "truncated": truncated}),
                len,
            )
        }
        Role::Tool => {
            let (text, folded) =
                fold_tool(&m.text, opts.tool_head, opts.tool_tail, opts.tool_byte_cap);
            let len = text.len();
            (
                json!({"i": i, "role": "tool", "text": text, "folded": folded, "bytes": m.text.len()}),
                len,
            )
        }
    }
}

/// Render `session` as the versioned, agent-parseable JSON window (schema
/// [`WINDOW_SCHEMA`]): orientation header, role-labelled turns with tool output
/// folded, the recent tail kept within `budget_chars`. Pure and deterministic
/// for a fixed (session, opts) - the MCP layer handles neutralization and the
/// final size guard.
pub fn render_window_json(session: &Session, opts: &WindowOpts) -> Value {
    let (large, title) = split_large(&session.title);

    let rendered: Vec<(Value, usize)> = session
        .messages
        .iter()
        .enumerate()
        .map(|(i, m)| turn_json(i, m, opts))
        .collect();

    let (kept, omitted) = match opts.budget_chars {
        None => (
            rendered.into_iter().map(|(v, _)| v).collect::<Vec<_>>(),
            0usize,
        ),
        Some(budget) => {
            // Keep the most recent turns within the budget (walk backward).
            let mut acc = 0usize;
            let mut taken: Vec<Value> = Vec::new();
            for (v, len) in rendered.iter().rev() {
                if acc + len > budget && !taken.is_empty() {
                    break;
                }
                acc += len;
                taken.push(v.clone());
            }
            let omitted = rendered.len() - taken.len();
            taken.reverse();
            (taken, omitted)
        }
    };

    json!({
        "schema": WINDOW_SCHEMA,
        "id": session.id,
        "tool": session.tool,
        "project": session.project,
        "title": title,
        "started": session.started.map(|d| d.format("%Y-%m-%d %H:%M").to_string()),
        "ended": session.ended.map(|d| d.format("%Y-%m-%d %H:%M").to_string()),
        "messages": session.messages.len(),
        "large": large,
        "budget_tokens": opts.budget_chars.map(|c| c / 4),
        "omitted_leading": omitted,
        "turns": kept,
        "drilldown": format!("sessionwiki show {} --full", session.id),
    })
}

/// Render ONE turn's full RETAINED text as JSON (schema [`TURN_SCHEMA`]) - the
/// drill-down for `session_window(id, turn=i)`. "Full" means untruncated by the
/// window's folding/cap; tool outputs are already capped at parse time, so this
/// recovers folded-out lines, not adapter-dropped bulk. Bounded to [`TURN_TEXT_CAP`]
/// chars for MCP transport; `clipped` and `bytes` report the true size. `None`
/// if `i` is out of range.
pub fn render_turn_json(session: &Session, i: usize) -> Option<Value> {
    let m = session.messages.get(i)?;
    let full = m.text.trim_end();
    let clipped = full.chars().count() > TURN_TEXT_CAP;
    let text = if clipped {
        full.chars().take(TURN_TEXT_CAP).collect::<String>()
    } else {
        full.to_string()
    };
    Some(json!({
        "schema": TURN_SCHEMA,
        "id": session.id,
        "i": i,
        "role": role_str(m.role),
        "text": text,
        "bytes": m.text.len(),
        "clipped": clipped,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Message, Role, Session};

    fn msg(role: Role, text: &str) -> Message {
        Message {
            role,
            text: text.to_string(),
            ts: None,
        }
    }

    fn session(messages: Vec<Message>) -> Session {
        Session {
            id: "abc123".into(),
            tool: "codex",
            path: std::path::PathBuf::from("/x.jsonl"),
            project: "proj".into(),
            started: None,
            ended: None,
            title: "refactor token guard".into(),
            subagent: false,
            messages,
            touched: vec![],
        }
    }

    #[test]
    fn folds_tool_output_to_head_and_tail() {
        let big = (1..=50)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (folded, was_folded) = fold_tool(&big, 3, 3, 2000);
        assert!(was_folded, "reported folded");
        assert!(
            folded.contains("line1") && folded.contains("line3"),
            "head kept"
        );
        assert!(
            folded.contains("line48") && folded.contains("line50"),
            "tail kept"
        );
        assert!(
            folded.contains("[… 44 lines …]"),
            "middle elided with count"
        );
        assert!(!folded.contains("line25"), "bulk dropped");
        // A short result is untouched.
        assert_eq!(fold_tool("a\nb", 3, 3, 2000), ("a\nb".to_string(), false));
    }

    #[test]
    fn fold_tool_byte_bounds_a_single_huge_line() {
        // One enormous line has too few lines to line-fold, but must still be
        // byte-bounded so it can't blow the budget.
        let huge = "x".repeat(50_000);
        let (out, folded) = fold_tool(&huge, 3, 3, 2000);
        assert!(folded, "byte-clip counts as folded");
        assert!(out.len() < 2_200, "clipped near the byte cap");
        assert!(out.contains("bytes …]"), "byte elision marked");
    }

    #[test]
    fn window_json_has_versioned_schema_roles_and_drilldown() {
        let big_tool = (1..=40)
            .map(|i| format!("l{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let s = session(vec![
            msg(
                Role::User,
                "refuse switch when a session runs on the same slot",
            ),
            msg(Role::Assistant, "adding a fail-closed check"),
            msg(Role::Tool, &big_tool),
        ]);
        let v = render_window_json(&s, &WindowOpts::default());
        assert_eq!(v["schema"], "sessionwiki.window/1");
        assert_eq!(v["id"], "abc123");
        assert_eq!(v["tool"], "codex");
        assert_eq!(v["messages"], 3);
        assert_eq!(v["large"], false);
        let turns = v["turns"].as_array().unwrap();
        assert_eq!(turns.len(), 3);
        assert_eq!(turns[0]["role"], "user");
        assert_eq!(turns[0]["i"], 0);
        assert_eq!(turns[2]["role"], "tool");
        assert_eq!(turns[2]["folded"], true);
        assert!(turns[2]["bytes"].as_u64().unwrap() > 0);
        assert_eq!(v["drilldown"], "sessionwiki show abc123 --full");
    }

    #[test]
    fn window_json_strips_large_flag_into_a_boolean() {
        let mut s = session(vec![msg(Role::User, "hi there friend")]);
        s.title = "[large] refactor token guard".into();
        let v = render_window_json(&s, &WindowOpts::default());
        assert_eq!(v["large"], true);
        assert_eq!(
            v["title"], "refactor token guard",
            "flag stripped from title"
        );
    }

    #[test]
    fn window_json_budget_keeps_recent_tail_and_is_deterministic() {
        let msgs: Vec<Message> = (0..20)
            .map(|i| msg(Role::User, &format!("turn number {i} with some words")))
            .collect();
        let s = session(msgs);
        let opts = WindowOpts {
            budget_chars: Some(120),
            ..Default::default()
        };
        let a = render_window_json(&s, &opts);
        let b = render_window_json(&s, &opts);
        assert_eq!(a, b, "deterministic for a fixed (session, budget)");
        assert!(
            a["omitted_leading"].as_u64().unwrap() > 0,
            "older turns omitted"
        );
        let turns = a["turns"].as_array().unwrap();
        let last = turns.last().unwrap();
        assert!(
            last["text"].as_str().unwrap().contains("turn number 19"),
            "most recent kept"
        );
    }

    #[test]
    fn turn_json_returns_full_untruncated_turn() {
        let s = session(vec![
            msg(Role::User, "short question"),
            msg(Role::Tool, &"y".repeat(500)),
        ]);
        let v = render_turn_json(&s, 1).unwrap();
        assert_eq!(v["schema"], "sessionwiki.turn/1");
        assert_eq!(v["i"], 1);
        assert_eq!(v["role"], "tool");
        assert_eq!(v["clipped"], false);
        assert_eq!(
            v["text"].as_str().unwrap().len(),
            500,
            "full text, not folded"
        );
        assert!(render_turn_json(&s, 9).is_none(), "out of range is None");
    }

    #[test]
    fn window_has_header_roles_tool_fold_and_drilldown() {
        let big_tool = (1..=40)
            .map(|i| format!("l{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let s = session(vec![
            msg(
                Role::User,
                "refuse switch when a session runs on the same slot",
            ),
            msg(Role::Assistant, "adding a fail-closed check; running tests"),
            msg(Role::Tool, &big_tool),
        ]);
        let w = render_window(&s, &WindowOpts::default());
        assert!(
            w.starts_with("## refactor token guard [codex]"),
            "orientation header"
        );
        assert!(
            w.contains("[user]") && w.contains("[assistant]") && w.contains("[tool]"),
            "role labels"
        );
        assert!(w.contains("[… 34 lines …]"), "tool result folded");
        assert!(
            w.contains("adding a fail-closed check"),
            "assistant text kept verbatim"
        );
        assert!(
            w.contains("→ full: sessionwiki show abc123 --full"),
            "drill-down hint"
        );
    }

    #[test]
    fn budget_keeps_the_recent_tail_and_marks_omissions() {
        let msgs: Vec<Message> = (0..20)
            .map(|i| msg(Role::User, &format!("turn number {i} with some words")))
            .collect();
        let s = session(msgs);
        let opts = WindowOpts {
            budget_chars: Some(120),
            ..Default::default()
        };
        let w = render_window(&s, &opts);
        assert!(w.contains("turn number 19"), "most recent kept");
        assert!(!w.contains("turn number 0"), "oldest dropped by budget");
        assert!(w.contains("earlier turn(s) omitted"), "omission is marked");
    }
}
