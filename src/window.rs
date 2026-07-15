//! A bounded, agent-consumable render of a session: the actual conversation
//! (not a lossy summary), but with tool-output bulk folded to head+tail and an
//! optional total budget that keeps the recent tail. This is what a caller (an
//! agent via MCP, or `show --window`) reads to know what another session is
//! doing without flooding its own context. The four things that make it
//! parseable: an orientation header, role labels, tool CALLS kept but tool
//! RESULTS folded, and a drill-down hint to `show <id> --full`.

use crate::model::{Role, Session};

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
}

impl Default for WindowOpts {
    fn default() -> Self {
        // ~4 chars/token: per-msg ~800 tok, budget (when set) ~1.5k tok.
        WindowOpts {
            tool_head: 3,
            tool_tail: 3,
            per_msg_chars: 3200,
            budget_chars: None,
        }
    }
}

/// Fold a multi-line tool result to its head and tail lines, marking the gap.
/// Short results pass through unchanged.
fn fold_tool(text: &str, head: usize, tail: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= head + tail + 1 {
        return text.trim_end().to_string();
    }
    let elided = lines.len() - head - tail;
    let mut out: Vec<String> = lines[..head].iter().map(|s| s.to_string()).collect();
    out.push(format!("  [… {elided} lines …]"));
    out.extend(lines[lines.len() - tail..].iter().map(|s| s.to_string()));
    out.join("\n")
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
            fold_tool(&m.text, opts.tool_head, opts.tool_tail)
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
        let folded = fold_tool(&big, 3, 3);
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
        assert_eq!(fold_tool("a\nb", 3, 3), "a\nb");
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
