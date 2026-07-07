//! Optional swapdex integration: attribute each session to the ACCOUNT profile
//! that was active when it started, by reading swapdex's switch timeline
//! (read-only). No swapdex on the machine -> no events -> no badges, silently.
//!
//! The join mirrors swapdex's own `sessions` attribution: a session belongs to
//! the last `use`/`restore` event for its tool with ts <= session start. A
//! session that predates every switch stays unattributed (None) - a missing
//! badge, never a guess.

use serde_json::Value;
use std::path::PathBuf;

pub struct SwitchEvent {
    pub ts: i64,
    pub tool: String,
    pub account: String,
}

/// swapdex's timeline location (same `dirs::data_dir` convention swapdex
/// uses). `SESSIONWIKI_SWAPDEX_TIMELINE` overrides for tests.
fn timeline_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SESSIONWIKI_SWAPDEX_TIMELINE") {
        return Some(p.into());
    }
    Some(dirs::data_dir()?.join("swapdex").join("timeline.jsonl"))
}

/// Parse the timeline defensively: only `use`/`restore` events count (they are
/// the moments the active account changed), malformed lines are skipped.
pub fn load_events() -> Vec<SwitchEvent> {
    let Some(path) = timeline_path() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if !matches!(v["action"].as_str(), Some("use") | Some("restore")) {
            continue;
        }
        if let (Some(ts), Some(tool), Some(account)) =
            (v["ts"].as_i64(), v["tool"].as_str(), v["account"].as_str())
        {
            out.push(SwitchEvent {
                ts,
                tool: tool.to_string(),
                account: account.to_string(),
            });
        }
    }
    out
}

/// The profile active when a session of `tool` started, or None (no events for
/// that tool before the start - including the no-swapdex case).
pub fn account_for(
    events: &[SwitchEvent],
    tool: &str,
    started_rfc3339: Option<&str>,
) -> Option<String> {
    let started = chrono::DateTime::parse_from_rfc3339(started_rfc3339?)
        .ok()?
        .timestamp();
    events
        .iter()
        .filter(|e| e.tool == tool && e.ts <= started)
        .max_by_key(|e| e.ts)
        .map(|e| e.account.clone())
}

/// Fill `account` on freshly queried rows. One timeline read per query call;
/// zero work when no swapdex timeline exists.
pub fn annotate<'a, I: IntoIterator<Item = &'a mut crate::index::SessionRow>>(rows: I) {
    let events = load_events();
    if events.is_empty() {
        return;
    }
    for r in rows {
        r.account = account_for(&events, &r.tool, r.started.as_deref());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(ts: i64, tool: &str, account: &str) -> SwitchEvent {
        SwitchEvent {
            ts,
            tool: tool.into(),
            account: account.into(),
        }
    }

    #[test]
    fn attributes_to_the_last_switch_before_start() {
        let events = vec![
            ev(100, "codex", "work"),
            ev(200, "codex", "personal"),
            ev(150, "claude-code", "work"),
        ];
        // codex session started at t=250 -> personal (the t=200 switch).
        let started = chrono::DateTime::from_timestamp(250, 0)
            .unwrap()
            .to_rfc3339();
        assert_eq!(
            account_for(&events, "codex", Some(&started)).as_deref(),
            Some("personal")
        );
        // claude session at t=250 -> work (its own tool's events only).
        assert_eq!(
            account_for(&events, "claude-code", Some(&started)).as_deref(),
            Some("work")
        );
        // A session that predates every switch stays unattributed.
        let early = chrono::DateTime::from_timestamp(50, 0)
            .unwrap()
            .to_rfc3339();
        assert_eq!(account_for(&events, "codex", Some(&early)), None);
        // Unknown start time -> None, never a guess.
        assert_eq!(account_for(&events, "codex", None), None);
    }
}
