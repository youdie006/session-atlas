use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use std::path::Path;

/// The largest session file we will read into memory. Session transcripts are
/// text; a file past this is malformed or hostile, so it is skipped rather than
/// allowed to exhaust memory.
pub const MAX_SESSION_FILE_BYTES: u64 = 256 * 1024 * 1024;

/// Bytes read from the head/tail of an over-cap session for its window.
const WINDOW_HEAD_BYTES: u64 = 256 * 1024;
const WINDOW_TAIL_BYTES: u64 = 512 * 1024;

/// The JSONL lines to parse for a session, and whether it was WINDOWED. Under
/// the cap: every line. Over the cap: only the first 256 KB and last 512 KB of
/// lines (the partial line at each cut boundary is dropped), so a huge session
/// is still indexed - searchable, and its start + recent turns readable via
/// `session_window`/`show` - instead of being dropped entirely. Byte-bounded, so
/// even a 256 MB+ file is read cheaply (no full scan).
pub fn session_lines(path: &Path) -> Result<(Vec<String>, bool)> {
    session_lines_windowed(
        path,
        MAX_SESSION_FILE_BYTES,
        WINDOW_HEAD_BYTES,
        WINDOW_TAIL_BYTES,
    )
}

fn session_lines_windowed(
    path: &Path,
    cap: u64,
    head_bytes: u64,
    tail_bytes: u64,
) -> Result<(Vec<String>, bool)> {
    use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len <= cap {
        let lines = BufReader::new(f)
            .lines()
            .map_while(std::result::Result::ok)
            .collect();
        return Ok((lines, false));
    }
    let head_n = head_bytes.min(len) as usize;
    let mut head_buf = vec![0u8; head_n];
    f.read_exact(&mut head_buf)?;
    let tail_n = tail_bytes.min(len) as usize;
    let mut tail_buf = vec![0u8; tail_n];
    f.seek(SeekFrom::End(-(tail_n as i64)))?;
    f.read_exact(&mut tail_buf)?;

    let head_s = String::from_utf8_lossy(&head_buf);
    let tail_s = String::from_utf8_lossy(&tail_buf);
    let mut lines: Vec<String> = Vec::new();
    // Head: drop the last line (cut mid-line at the byte boundary).
    let mut hl: Vec<&str> = head_s.lines().collect();
    if hl.len() > 1 {
        hl.pop();
    }
    lines.extend(hl.iter().map(|s| s.to_string()));
    // Tail: drop the first line (cut mid-line at the seek boundary).
    let mut tl: Vec<&str> = tail_s.lines().collect();
    if tl.len() > 1 {
        tl.remove(0);
    }
    lines.extend(tl.iter().map(|s| s.to_string()));
    Ok((lines, true))
}

/// Read a file to a string, refusing anything over [`MAX_SESSION_FILE_BYTES`]
/// so a malicious or corrupt session file can't OOM the process.
pub fn read_to_string_capped(path: &Path) -> Result<String> {
    let len = std::fs::metadata(path)
        .with_context(|| format!("stat {}", path.display()))?
        .len();
    if len > MAX_SESSION_FILE_BYTES {
        bail!(
            "{} is {} - over the {} cap; skipping",
            path.display(),
            human_size(len),
            human_size(MAX_SESSION_FILE_BYTES)
        );
    }
    std::fs::read_to_string(path).with_context(|| format!("open {}", path.display()))
}

/// Stable short id from a path string. FNV-1a is implemented by hand because
/// std's DefaultHasher is not guaranteed stable across Rust releases.
pub fn short_id(s: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")[..12].to_string()
}

/// Normalize text to Unicode NFC before it enters or queries the index.
///
/// The FTS5 trigram tokenizer windows over raw bytes, so the same grapheme in
/// two normalization forms never matches: macOS stores Hangul as NFD
/// (decomposed jamo - "회사" as combining scalars) while a typed query is NFC,
/// so without this an NFD-stored Korean session is invisible to an NFC search,
/// and the `trace` suffix match misses the same way. Normalizing both the
/// indexed text and the query to NFC makes them line up. Pure ASCII is already
/// NFC, so this is a cheap near-no-op for English; the cost is per-message and
/// negligible next to parsing and the SQLite write.
pub fn nfc(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    s.nfc().collect()
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

pub fn fmt_date(ts: Option<DateTime<Utc>>) -> String {
    match ts {
        Some(t) => t.format("%Y-%m-%d %H:%M").to_string(),
        None => "-".into(),
    }
}

pub fn rel_time(ts: Option<DateTime<Utc>>) -> String {
    let Some(t) = ts else { return "-".into() };
    let secs = (Utc::now() - t).num_seconds().max(0);
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86399 => format!("{}h ago", secs / 3600),
        86400..=2591999 => format!("{}d ago", secs / 86400),
        _ => t.format("%Y-%m-%d").to_string(),
    }
}

pub fn truncate(s: &str, max: usize) -> String {
    // Session titles (and other indexed strings) are untrusted input: a planted
    // or prompt-poisoned session can set any title, and this is the choke point
    // that renders titles to the terminal across list/search/trace/resume/blame.
    // Strip control characters so a title can't smuggle ANSI/terminal-control
    // escapes into our output. \n and \t become spaces; every other C0 control,
    // DEL, and the C1 range is dropped (same posture as clean_snippet).
    let clean: String = s
        .chars()
        .filter_map(|c| match c {
            '\n' | '\t' => Some(' '),
            c if (c as u32) < 0x20 || c == '\u{7f}' || ('\u{80}'..='\u{9f}').contains(&c) => None,
            c => Some(c),
        })
        .collect();
    let clean = clean.trim();
    if clean.chars().count() <= max {
        clean.to_string()
    } else {
        let cut: String = clean.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}\u{2026}")
    }
}

// Minimal ANSI helpers. Respect NO_COLOR and non-tty stdout.
pub fn color_enabled() -> bool {
    use std::io::IsTerminal;
    std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal()
}

pub fn paint(code: &str, s: &str) -> String {
    if color_enabled() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn bold(s: &str) -> String {
    paint("1", s)
}
pub fn dim(s: &str) -> String {
    paint("2", s)
}
pub fn cyan(s: &str) -> String {
    paint("36", s)
}
pub fn yellow(s: &str) -> String {
    paint("33", s)
}
pub fn green(s: &str) -> String {
    paint("32", s)
}

#[cfg(test)]
mod tests {
    use super::{session_lines_windowed, truncate};

    #[test]
    fn truncate_strips_control_characters() {
        // ESC, DEL, and C1 controls are dropped so an untrusted title cannot
        // inject terminal escape sequences; the remaining literal text stays.
        let title = "ok\u{1b}[31mred\u{7f}\u{9b}end";
        let out = truncate(title, 100);
        assert!(!out.contains('\u{1b}'), "ESC must be stripped");
        assert!(!out.contains('\u{7f}'), "DEL must be stripped");
        assert!(!out.contains('\u{9b}'), "C1 must be stripped");
        assert_eq!(out, "ok[31mredend");
    }

    #[test]
    fn truncate_collapses_whitespace_controls() {
        assert_eq!(truncate("a\nb\tc", 100), "a b c");
    }

    #[test]
    fn truncate_adds_ellipsis_when_too_long() {
        assert_eq!(truncate("abcdef", 4), "abc\u{2026}");
    }

    #[test]
    fn truncate_keeps_unicode_titles() {
        assert_eq!(truncate("한국어 검색", 100), "한국어 검색");
    }

    #[test]
    fn windows_an_over_cap_session_to_head_and_tail() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.jsonl");
        // 10 lines of "lineNN\n" (7 bytes each = 70 bytes total).
        let content: String = (0..10).map(|i| format!("line{i:02}\n")).collect();
        std::fs::write(&p, &content).unwrap();
        // Under the cap: every line, not windowed.
        let (all, w) = session_lines_windowed(&p, 10_000, 22, 22).unwrap();
        assert_eq!(all.len(), 10);
        assert!(!w);
        // Over the cap with small budgets: head + tail only, middle dropped.
        let (win, w2) = session_lines_windowed(&p, 30, 22, 22).unwrap();
        assert!(w2, "flagged windowed");
        assert!(win.iter().any(|l| l == "line00"), "head kept: {win:?}");
        assert!(win.iter().any(|l| l == "line09"), "tail kept: {win:?}");
        assert!(
            !win.iter().any(|l| l == "line05"),
            "middle dropped: {win:?}"
        );
    }
}
