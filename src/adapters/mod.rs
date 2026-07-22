mod aider;
mod claude_code;
mod cline;
mod codex;
mod continue_dev;
mod gajae;
mod gemini;
mod gptme;
pub mod harness;
mod opencode;
mod prodex;
pub use prodex::thread_url_for_task as prodex_thread_url;

use crate::model::{Session, StoreReport};
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};

/// One supported agent tool. An adapter knows where the tool keeps its
/// session files on disk and how to parse one file into a `Session`.
///
/// Adding support for a new tool means implementing this trait in a new
/// module and registering it in `all()`. PRs for new adapters are the main
/// way this project grows.
/// A store that holds many sessions in one place (e.g. a SQLite database)
/// rather than one file per session. Returned by [`Adapter::store`] when the
/// file-per-session model does not fit; the indexer then enumerates sessions
/// from `keys` (re-parsing only changed ones) instead of `discover`/`parse`.
pub struct Store {
    /// `(stable key, change-token)` for every session, cheap to compute without
    /// a full parse. The change-token (e.g. the session's updated-time in ms)
    /// drives incremental re-indexing; the key identifies the session for
    /// [`Adapter::parse_key`]. The key doubles as the session's stored path.
    pub keys: Vec<(String, i64)>,
    /// The backing files (the database), for `scan` size accounting.
    pub files: Vec<PathBuf>,
    /// True if a backing store existed but could not be read this run (locked,
    /// half-written, permissions). The indexer then skips deletion
    /// reconciliation, so a transient read failure cannot archive the whole
    /// corpus off an incomplete key set.
    pub had_error: bool,
}

/// The result of file discovery. `had_error` means the listing is PARTIAL: a
/// directory that exists could not be read (permissions, transient IO). The
/// indexer still indexes what was found but skips deletion reconciliation, so
/// a partial walk cannot archive live sessions off an incomplete listing - the
/// same guard [`Store::had_error`] gives shared-store adapters. A root that
/// simply does not exist on this machine is normal, not an error.
pub struct Discovered {
    pub files: Vec<PathBuf>,
    pub had_error: bool,
}

impl From<Vec<PathBuf>> for Discovered {
    fn from(files: Vec<PathBuf>) -> Self {
        Discovered {
            files,
            had_error: false,
        }
    }
}

/// `result.ok()` that records the failure in `had_error` instead of dropping
/// it silently - for walk loops that must report a partial listing.
pub(crate) fn ok_or_flag<T, E>(r: std::result::Result<T, E>, had_error: &mut bool) -> Option<T> {
    match r {
        Ok(v) => Some(v),
        Err(_) => {
            *had_error = true;
            None
        }
    }
}

pub trait Adapter {
    fn name(&self) -> &'static str;
    /// Store root, e.g. ~/.claude/projects. May not exist on this machine.
    fn root(&self) -> Option<PathBuf>;
    /// All session files under the root, with a partial-walk flag the indexer
    /// uses to protect deletion reconciliation (see [`Discovered`]).
    fn discover(&self) -> Discovered;
    /// Parse one session file. Must never panic on malformed input;
    /// skip bad lines and return what could be read.
    fn parse(&self, path: &Path) -> Result<Session>;
    /// A shared store (many sessions in one database) for tools that do not use
    /// one file per session. When `Some`, the indexer uses it instead of
    /// `discover`/`parse`. Default `None` = ordinary file-per-session adapter.
    fn store(&self) -> Option<Store> {
        None
    }
    /// Parse one session out of a shared store by its key (from `store().keys`).
    /// Only called for adapters that return a [`Store`].
    fn parse_key(&self, _key: &str) -> Result<Session> {
        anyhow::bail!("this adapter is not a shared store")
    }
}

pub fn all() -> Vec<Box<dyn Adapter>> {
    vec![
        Box::new(claude_code::ClaudeCode),
        Box::new(codex::Codex),
        Box::new(gemini::Gemini),
        Box::new(opencode::OpenCode),
        Box::new(cline::Cline),
        Box::new(cline::RooCode),
        Box::new(cline::KiloCode),
        Box::new(gajae::GajaeCode),
        Box::new(continue_dev::Continue),
        Box::new(gptme::Gptme),
        Box::new(aider::Aider::default()),
        Box::new(prodex::Prodex),
    ]
}

pub fn by_name(name: &str) -> Option<Box<dyn Adapter>> {
    all().into_iter().find(|a| a.name() == name)
}

/// Filesystem-only summary of a store, used by `scan`. No parsing involved.
pub fn report(adapter: &dyn Adapter) -> Option<StoreReport> {
    let root = adapter.root()?;
    // Shared store (e.g. SQLite): presence comes from the store itself (the db
    // can live outside `root` via OPENCODE_DB), so this is checked before the
    // root-exists gate. Size is the backing files, count is the sessions, and
    // the time span comes from the per-session change-tokens.
    if let Some(store) = adapter.store() {
        if store.keys.is_empty() {
            return None; // present but no sessions yet - not worth a scan row
        }
        let bytes = store
            .files
            .iter()
            .filter_map(|f| f.metadata().ok())
            .map(|m| m.len())
            .sum();
        // Tokens are last-activity ms; drop the 0 sentinel (a missing timestamp)
        // so it cannot backdate the span to 1970.
        let oldest = store
            .keys
            .iter()
            .map(|(_, t)| *t)
            .filter(|t| *t > 0)
            .min()
            .and_then(DateTime::from_timestamp_millis);
        let newest = store
            .keys
            .iter()
            .map(|(_, t)| *t)
            .filter(|t| *t > 0)
            .max()
            .and_then(DateTime::from_timestamp_millis);
        return Some(StoreReport {
            tool: adapter.name(),
            root,
            files: store.keys.len(),
            bytes,
            oldest,
            newest,
        });
    }
    if !root.exists() {
        return None;
    }
    let files = adapter.discover().files;
    let mut bytes: u64 = 0;
    let mut oldest: Option<DateTime<Utc>> = None;
    let mut newest: Option<DateTime<Utc>> = None;
    let mut count = 0usize;
    for f in &files {
        let Ok(meta) = f.metadata() else { continue };
        count += 1;
        bytes += meta.len();
        if let Ok(modified) = meta.modified() {
            let t: DateTime<Utc> = modified.into();
            if oldest.is_none_or(|o| t < o) {
                oldest = Some(t);
            }
            if newest.is_none_or(|n| t > n) {
                newest = Some(t);
            }
        }
    }
    if count == 0 {
        return None; // root exists but holds no session files
    }
    Some(StoreReport {
        tool: adapter.name(),
        root,
        files: count,
        bytes,
        oldest,
        newest,
    })
}

/// Shared helper: pick a session title from messages when the tool does not
/// store one. First user message that is not harness boilerplate wins.
pub(crate) fn title_from_messages(messages: &[crate::model::Message]) -> String {
    messages
        .iter()
        .find(|m| {
            m.role == crate::model::Role::User
                && !m.text.trim_start().starts_with('<')
                && !m.text.trim().is_empty()
        })
        .map(|m| crate::util::truncate(&m.text, 80))
        .unwrap_or_else(|| "(no user prompt)".into())
}

pub(crate) fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Path hygiene shared by `touched` and `edits`: trim, and reject the shapes
/// that are not a real edited path (empty, embedded newline, absurdly long).
/// Returns the cleaned path or None to drop it, so the two provenance layers
/// apply IDENTICAL rules and never diverge.
pub(crate) fn clean_path(p: &str) -> Option<String> {
    let p = p.trim();
    (!p.is_empty() && !p.contains('\n') && p.len() <= 4096).then(|| p.to_string())
}

/// Tidy the list of files a session touched: `clean_path` each, then de-duplicate
/// while preserving first-seen order. A session edits the same file many times;
/// the link cares only that it did.
pub(crate) fn dedup_paths(paths: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    paths
        .into_iter()
        .filter_map(|p| clean_path(&p))
        .filter(|p| seen.insert(p.clone()))
        .collect()
}
