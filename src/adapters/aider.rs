//! Aider adapter: read-only index of per-repo `.aider.chat.history.md`.
//! One file accumulates many runs (one per aider launch) delimited by
//! `# aider chat started at` headers. Markdown-derived, so roles are
//! reconstructed from line prefixes (lower fidelity than the JSONL adapters);
//! an assistant `#### ` heading or `> ` blockquote is a known misclassification.
//! No per-message timestamps; `started` is the run header (local time, assumed
//! UTC). Reads are size-capped; discovery is bounded and logs nothing.

use crate::adapters::{Adapter, Store};
use crate::model::{Message, Role, Session};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};
use std::time::Instant;
use walkdir::WalkDir;

struct Run {
    started: Option<DateTime<Utc>>,
    body: String,
}

/// Parse aider's header timestamp `%Y-%m-%d %H:%M:%S` (local naive, no tz) and
/// assume UTC. `parse_ts` in mod.rs is RFC3339-only and returns None for these.
fn parse_aider_ts(s: &str) -> Option<DateTime<Utc>> {
    chrono::NaiveDateTime::parse_from_str(s.trim(), "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|n| DateTime::<Utc>::from_naive_utc_and_offset(n, Utc))
}

/// Split a history file into runs on the `# aider chat started at ` header (the
/// only single-`#` line aider writes). Bytes before the first header belong to
/// no run and are dropped. A header with no body keeps its slot, so positional
/// run indices never renumber when new runs are appended.
fn split_runs(content: &str) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    let mut cur: Option<Run> = None;
    for line in content.lines() {
        if let Some(ts) = line.strip_prefix("# aider chat started at ") {
            if let Some(r) = cur.take() {
                runs.push(r);
            }
            cur = Some(Run {
                started: parse_aider_ts(ts),
                body: String::new(),
            });
        } else if let Some(r) = cur.as_mut() {
            r.body.push_str(line);
            r.body.push('\n');
        }
        // lines before the first header (cur == None) are dropped
    }
    if let Some(r) = cur.take() {
        runs.push(r);
    }
    runs
}

fn push_unique(v: &mut Vec<String>, s: &str) {
    let s = s.trim().to_string();
    if !s.is_empty() && !v.contains(&s) {
        v.push(s);
    }
}

fn flush(messages: &mut Vec<Message>, role: Option<Role>, buf: &mut Vec<String>) {
    if let Some(role) = role {
        let text = buf.join("\n").trim().to_string();
        // Keep empty user turns (aider writes `#### ` for empty input); drop
        // empty assistant/tool noise.
        if !text.is_empty() || role == Role::User {
            messages.push(Message {
                role,
                text,
                ts: None,
            });
        }
    }
    buf.clear();
}

/// Reconstruct turns from one run body. `#### ` = user, `> ` = tool (every aider
/// tool/warning/error line is blockquoted), blank lines continue the current
/// turn, everything else is assistant (the default container). Edited files come
/// from `> Applied edit to` / `> Creating empty file` (relative paths). Known
/// limitation: an assistant `#### ` heading or `> ` blockquote is misclassified.
fn parse_turns(body: &str) -> (Vec<Message>, Vec<String>) {
    let mut messages: Vec<Message> = Vec::new();
    let mut touched: Vec<String> = Vec::new();
    let mut cur_role: Option<Role> = None;
    let mut buf: Vec<String> = Vec::new();

    for raw in body.lines() {
        let (line_role, content): (Option<Role>, String) =
            if let Some(r) = raw.strip_prefix("#### ") {
                (Some(Role::User), r.trim_end().to_string())
            } else if raw == "####" {
                (Some(Role::User), String::new())
            } else if let Some(r) = raw.strip_prefix("> ") {
                if let Some(p) = r.strip_prefix("Applied edit to ") {
                    push_unique(&mut touched, p);
                } else if let Some(p) = r.strip_prefix("Creating empty file ") {
                    push_unique(&mut touched, p);
                }
                (Some(Role::Tool), r.to_string())
            } else if raw == ">" {
                (Some(Role::Tool), String::new())
            } else if raw.trim().is_empty() {
                (None, String::new()) // blank: continue the current turn
            } else {
                (Some(Role::Assistant), raw.to_string())
            };

        match line_role {
            Some(role) => {
                if cur_role != Some(role) {
                    flush(&mut messages, cur_role, &mut buf);
                    cur_role = Some(role);
                }
                buf.push(content);
            }
            None => {
                if cur_role.is_some() {
                    buf.push(String::new());
                }
            }
        }
    }
    flush(&mut messages, cur_role, &mut buf);
    (messages, touched)
}

const HISTORY_FILE: &str = ".aider.chat.history.md";
const MAX_DEPTH: usize = 4;
const MAX_DIRS: usize = 50_000;
const MAX_FILES: usize = 5_000;
const WALK_BUDGET_SECS: u64 = 2;
const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".cache",
    "Library",
    "go",
    ".cargo",
    ".rustup",
    ".npm",
    ".pnpm-store",
    ".gradle",
    ".m2",
    "vendor",
    "dist",
    "build",
    ".venv",
    "venv",
    "__pycache__",
    ".svn",
    ".hg",
];

fn aider_roots() -> Vec<PathBuf> {
    if let Some(v) = std::env::var_os("SESSIONWIKI_AIDER_ROOTS") {
        return std::env::split_paths(&v)
            .filter_map(|p| std::fs::canonicalize(p).ok())
            .collect();
    }
    // Default: a bounded, capped walk of the home directory (NOT unbounded - the
    // depth/dir/file/time caps below keep it cheap; an over-cap sets had_error).
    dirs::home_dir().into_iter().collect()
}

fn is_skipped(entry: &walkdir::DirEntry) -> bool {
    entry.file_type().is_dir()
        && entry
            .file_name()
            .to_str()
            .is_some_and(|n| SKIP_DIRS.contains(&n))
}

/// Find `.aider.chat.history.md` files under `roots`, bounded and reading
/// nothing but matching files. Returns (files, had_error). `had_error` is set on
/// ANY cap-hit or walk error so the indexer skips deletion reconciliation on a
/// partial result (the only safe guard for this rootless adapter). Logs nothing.
fn discover_history(roots: &[PathBuf]) -> (Vec<PathBuf>, bool) {
    let mut files = Vec::new();
    let mut had_error = false;
    let mut dirs = 0usize;
    let start = Instant::now();
    'outer: for root in roots {
        let walker = WalkDir::new(root)
            .max_depth(MAX_DEPTH)
            .follow_links(false) // never escape via symlinks (first walk over user space)
            .into_iter()
            .filter_entry(|e| !is_skipped(e));
        for entry in walker {
            if start.elapsed().as_secs() >= WALK_BUDGET_SECS {
                had_error = true;
                break 'outer;
            }
            match entry {
                Ok(e) => {
                    if e.file_type().is_dir() {
                        dirs += 1;
                        if dirs > MAX_DIRS {
                            had_error = true;
                            break 'outer;
                        }
                    } else if e.file_name() == HISTORY_FILE {
                        files.push(e.into_path());
                        if files.len() > MAX_FILES {
                            had_error = true;
                            break 'outer;
                        }
                    }
                }
                Err(_) => had_error = true, // permission etc.: incomplete walk
            }
        }
    }
    (files, had_error)
}

/// U+001F, matching the shared-store key separator used by the OpenCode adapter,
/// so display rendering stays uniform. Never appears in a path.
const KEY_SEP: char = '\u{1f}';

fn make_key(path: &str, idx: usize) -> String {
    format!("{path}{KEY_SEP}{idx}")
}

/// Render a store key as `<path>#<idx>` for human/`show`/web output.
fn display_path(key: &str) -> String {
    match key.split_once(KEY_SEP) {
        Some((p, i)) => format!("{p}#{i}"),
        None => key.to_string(),
    }
}

fn title_from(messages: &[Message]) -> String {
    messages
        .iter()
        .find(|m| m.role == Role::User && !m.text.trim().is_empty())
        .map(|m| {
            let t = m.text.trim();
            t.lines().next().unwrap_or(t).chars().take(80).collect()
        })
        .unwrap_or_default()
}

/// Assemble one already-split run into a Session.
fn session_from_run(path: &Path, idx: usize, run: &Run) -> Result<Session> {
    let (messages, touched) = parse_turns(&run.body);
    let key = make_key(&path.to_string_lossy(), idx);
    let project = path
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(Session {
        id: crate::util::short_id(&key),
        tool: "aider",
        path: std::path::PathBuf::from(display_path(&key)),
        project,
        started: run.started,
        ended: None,
        title: title_from(&messages),
        subagent: false,
        messages,
        touched,
        edits: Vec::new(),
    })
}

/// Read a history file, split it, and assemble the run at `idx` into a Session.
/// The indexer goes through the cached `parse_key` path instead; this direct
/// road is for tests.
#[cfg(test)]
fn build_session(path: &Path, idx: usize) -> Result<Session> {
    let content = crate::util::read_to_string_capped(path)
        .with_context(|| format!("read {}", path.display()))?;
    let runs = split_runs(&content);
    let run = runs
        .get(idx)
        .with_context(|| format!("no run {idx} in {}", path.display()))?;
    session_from_run(path, idx, run)
}

/// The last split history file, so indexing N runs of one file costs one read
/// and one split instead of N (the indexer asks for a file's runs one key at a
/// time, consecutively - re-reading the whole file per run made a long history
/// O(runs^2)). Keyed by (path, mtime): any rewrite invalidates it.
struct RunCache {
    path: PathBuf,
    mtime_ms: i64,
    runs: Vec<Run>,
}

#[derive(Default)]
pub struct Aider {
    cache: std::sync::Mutex<Option<RunCache>>,
}

impl Adapter for Aider {
    fn name(&self) -> &'static str {
        "aider"
    }

    /// Rootless: returns home (or the first configured root) so the tool appears
    /// in `scan`/`report`. Because that always exists, `store_present` is always
    /// true, so `Store.had_error` (not `store_present`) guards against archiving
    /// the corpus on a partial walk.
    fn root(&self) -> Option<PathBuf> {
        aider_roots().into_iter().next()
    }

    fn discover(&self) -> crate::adapters::Discovered {
        Vec::new().into() // shared-store adapter; see `store`
    }

    fn parse(&self, _path: &Path) -> Result<Session> {
        anyhow::bail!("aider is a shared-store adapter; use parse_key")
    }

    fn store(&self) -> Option<Store> {
        let (history_files, had_error) = discover_history(&aider_roots());
        let mut keys: Vec<(String, i64)> = Vec::new();
        for path in &history_files {
            // token = file mtime (ms), shared across all runs of the file, so any
            // write re-parses the whole file (one capped read).
            let token = std::fs::metadata(path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            let Ok(content) = crate::util::read_to_string_capped(path) else {
                continue;
            };
            let runs = split_runs(&content);
            for idx in 0..runs.len() {
                keys.push((make_key(&path.to_string_lossy(), idx), token));
            }
        }
        Some(Store {
            keys,
            files: history_files,
            had_error,
        })
    }

    fn parse_key(&self, key: &str) -> Result<Session> {
        let (path, idx) = key.split_once(KEY_SEP).context("malformed aider key")?;
        let idx: usize = idx.parse().context("bad run index")?;
        let path = Path::new(path);
        let mtime_ms = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let mut guard = self.cache.lock().unwrap();
        let hit = guard
            .as_ref()
            .is_some_and(|c| c.path == path && c.mtime_ms == mtime_ms);
        if !hit {
            let content = crate::util::read_to_string_capped(path)
                .with_context(|| format!("read {}", path.display()))?;
            *guard = Some(RunCache {
                path: path.to_path_buf(),
                mtime_ms,
                runs: split_runs(&content),
            });
        }
        let cache = guard.as_ref().expect("cache filled above");
        let run = cache
            .runs
            .get(idx)
            .with_context(|| format!("no run {idx} in {}", path.display()))?;
        session_from_run(path, idx, run)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_runs_keeps_empty_and_drops_preamble() {
        let c = "junk before any header\n\
                 # aider chat started at 2026-06-09 14:01:00\n\
                 #### hi\n\
                 answer\n\
                 # aider chat started at 2026-06-09 15:00:00\n\
                 # aider chat started at 2026-06-09 16:00:00\n\
                 #### again\n";
        let runs = split_runs(c);
        assert_eq!(runs.len(), 3, "empty middle run keeps its slot");
        assert_eq!(runs[0].started, parse_aider_ts("2026-06-09 14:01:00"));
        assert!(
            runs[1].body.trim().is_empty(),
            "header-only run has empty body"
        );
        assert!(runs[0].body.contains("#### hi"));
    }

    #[test]
    fn parse_aider_ts_handles_naive_local_as_utc_and_rejects_garbage() {
        assert!(parse_aider_ts("2026-06-09 14:01:00").is_some());
        assert!(parse_aider_ts("not a date").is_none());
    }

    #[test]
    fn assistant_markdown_and_tool_lines_classified() {
        let body = "#### fix the bug\n\
                    Here is the fix.\n\
                    Some prose.\n\
                    > Applied edit to src/a.py\n\
                    > Applied edit to src/a.py\n\
                    > Creating empty file src/b.py\n\
                    > Did not apply edit to src/c.py (--dry-run)\n";
        let (msgs, touched) = parse_turns(body);
        let roles: Vec<Role> = msgs.iter().map(|m| m.role).collect();
        assert_eq!(roles, vec![Role::User, Role::Assistant, Role::Tool]);
        assert_eq!(msgs[0].text, "fix the bug");
        assert!(msgs[1].text.contains("Here is the fix."));
        assert_eq!(touched, vec!["src/a.py", "src/b.py"]); // dedup; dry-run ignored
        assert!(msgs.iter().all(|m| m.ts.is_none()));
    }

    #[test]
    fn blank_lines_do_not_split_an_assistant_message() {
        let body = "#### q\n\
                    para one\n\
                    \n\
                    para two\n";
        let (msgs, _) = parse_turns(body);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[1].role, Role::Assistant);
        assert!(msgs[1].text.contains("para one") && msgs[1].text.contains("para two"));
    }

    #[test]
    fn build_session_from_a_fixture_file() {
        use std::io::Write;
        let dir = std::env::temp_dir().join("sw-aider-t3");
        let _ = std::fs::remove_dir_all(&dir);
        let repo = dir.join("myrepo");
        std::fs::create_dir_all(&repo).unwrap();
        let path = repo.join(".aider.chat.history.md");
        let mut f = std::fs::File::create(&path).unwrap();
        write!(
            f,
            "# aider chat started at 2026-06-09 14:01:00\n#### add retry\nDone.\n> Applied edit to src/x.py\n"
        )
        .unwrap();

        let s = build_session(&path, 0).unwrap();
        assert_eq!(s.tool, "aider");
        assert_eq!(s.project, repo.to_string_lossy());
        assert_eq!(s.title, "add retry");
        assert_eq!(s.touched, vec!["src/x.py"]);
        assert!(s.started.is_some());
        assert!(!s.subagent);
        // id is stable for the same (path, idx)
        assert_eq!(s.id, build_session(&path, 0).unwrap().id);
    }

    #[test]
    fn discover_finds_history_and_skips_node_modules() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("proj");
        std::fs::create_dir_all(repo.join("node_modules/skipme")).unwrap();
        std::fs::write(repo.join(".aider.chat.history.md"), "x\n").unwrap();
        std::fs::write(repo.join("README.md"), "x\n").unwrap();
        std::fs::write(
            repo.join("node_modules/skipme/.aider.chat.history.md"),
            "x\n",
        )
        .unwrap();

        let (files, had_error) = discover_history(&[dir.path().to_path_buf()]);
        assert_eq!(files.len(), 1, "found the repo file, skipped node_modules");
        assert!(files[0].ends_with(".aider.chat.history.md"));
        assert!(!had_error);
    }
}
