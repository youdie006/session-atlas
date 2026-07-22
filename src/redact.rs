//! Strip credentials from text before it is stored in the index. Sessions
//! contain the code and conversations an agent produced, which routinely
//! include secrets (a `.env` written, a key pasted into a prompt). Because the
//! index outlives the original session (archive mode), those secrets must not
//! land in it. This runs at index time over message text and edit snippets.
//!
//! No dependency: the patterns are matched by hand (the project keeps its
//! dependency tree small enough to audit). The bar is HIGH-confidence shapes -
//! we would rather miss an exotic token than redact ordinary code.

use std::borrow::Cow;

/// Redact known secret shapes from `s`, replacing each with a `[redacted:<kind>]`
/// marker. Returns the input unchanged (borrowed) when nothing matched.
pub fn redact(s: &str) -> Cow<'_, str> {
    // Cheap gate: the markers below all require one of these substrings, so a
    // string without any of them (the overwhelming majority) allocates nothing.
    let suspicious = s.contains("eyJ")
        || s.contains("PRIVATE KEY")
        || s.contains("sk-")
        || s.contains("AKIA")
        || s.contains("gh")
        || s.contains("xox")
        || s.contains("AIza")
        || s.contains("_live_");
    if !suspicious {
        return Cow::Borrowed(s);
    }
    let step1 = redact_pem_blocks(s);
    let step2 = redact_tokens(&step1);
    if step2 == s {
        Cow::Borrowed(s)
    } else {
        Cow::Owned(step2)
    }
}

/// Replace whole `-----BEGIN ...PRIVATE KEY----- ... -----END ...PRIVATE KEY-----`
/// blocks with one marker. Matches on the ASCII armor, so byte offsets are safe.
fn redact_pem_blocks(s: &str) -> Cow<'_, str> {
    if !s.contains("PRIVATE KEY-----") {
        return Cow::Borrowed(s);
    }
    const BEGIN: &str = "-----BEGIN ";
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let mut changed = false;
    while i < s.len() {
        let Some(rel) = s[i..].find(BEGIN) else {
            out.push_str(&s[i..]);
            break;
        };
        let begin = i + rel;
        let after = &s[begin + BEGIN.len()..];
        let Some(hdr_rel) = after.find("-----") else {
            out.push_str(&s[i..]);
            break;
        };
        let header = &after[..hdr_rel];
        let hdr_end = begin + BEGIN.len() + hdr_rel + 5;
        if !header.contains("PRIVATE KEY") {
            // Not a private-key block: keep it verbatim and continue past it.
            out.push_str(&s[i..hdr_end]);
            i = hdr_end;
            continue;
        }
        // The footer must be a real `-----END ...PRIVATE KEY-----`, not just the
        // next `PRIVATE KEY-----` (which could be another BEGIN header).
        let footer = s[hdr_end..].find("-----END").and_then(|e| {
            s[hdr_end + e..]
                .find("PRIVATE KEY-----")
                .map(|k| e + k + "PRIVATE KEY-----".len())
        });
        out.push_str(&s[i..begin]);
        out.push_str("[redacted:private-key]");
        changed = true;
        i = match footer {
            Some(f) => hdr_end + f,
            None => s.len(), // unterminated: redact to the end
        };
    }
    if changed {
        Cow::Owned(out)
    } else {
        Cow::Borrowed(s)
    }
}

fn is_token_char(ch: char) -> bool {
    // alnum + the separators our secret shapes use (JWT dots, prefix dashes and
    // underscores). Deliberately NOT `= + /`, so `KEY=sk-...` splits the name off
    // the token and the prefix is seen at the token start.
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.')
}

/// The secret kind a single token looks like, or None. HIGH-confidence shapes
/// only - a known prefix plus a plausible length, or the three-segment JWT form.
fn secret_kind(t: &str) -> Option<&'static str> {
    if t.starts_with("eyJ") {
        let parts: Vec<&str> = t.split('.').collect();
        if parts.len() == 3 && parts[1].starts_with("eyJ") && parts.iter().all(|p| !p.is_empty()) {
            return Some("jwt");
        }
    }
    if t.starts_with("sk-") && t.len() >= 20 {
        return Some("openai");
    }
    if (t.starts_with("sk_live_") || t.starts_with("rk_live_")) && t.len() >= 20 {
        return Some("stripe");
    }
    if t.starts_with("AIza") && t.len() >= 30 {
        return Some("google");
    }
    // AWS access key IDs are EXACTLY 20 chars (AKIA + 16 upper/digit), so the
    // exact length keeps a longer uppercase identifier from matching.
    if t.starts_with("AKIA")
        && t.len() == 20
        && t[4..]
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    {
        return Some("aws");
    }
    if t.starts_with("github_pat_") && t.len() >= 40 {
        return Some("github");
    }
    if matches!(
        &t[..t.len().min(4)],
        "ghp_" | "gho_" | "ghu_" | "ghs_" | "ghr_"
    ) && t.len() >= 20
    {
        return Some("github");
    }
    if t.len() >= 15
        && matches!(
            &t[..t.len().min(5)],
            "xoxb-" | "xoxp-" | "xoxa-" | "xoxr-" | "xoxs-"
        )
    {
        return Some("slack");
    }
    None
}

fn flush_token(token: &mut String, out: &mut String) {
    if token.is_empty() {
        return;
    }
    // Trailing dots are sentence punctuation, not part of a secret (a JWT ending
    // a sentence would otherwise split into 4 segments and be missed).
    let core = token.trim_end_matches('.');
    match secret_kind(core) {
        Some(kind) => {
            out.push_str("[redacted:");
            out.push_str(kind);
            out.push(']');
            out.push_str(&token[core.len()..]); // re-emit the trailing dots
        }
        None => out.push_str(token),
    }
    token.clear();
}

/// Walk `s`, and for each maximal run of token characters replace it with a
/// marker if it looks like a secret. Non-token characters pass through.
fn redact_tokens(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut token = String::new();
    for ch in s.chars() {
        if is_token_char(ch) {
            token.push(ch);
        } else {
            flush_token(&mut token, &mut out);
            out.push(ch);
        }
    }
    flush_token(&mut token, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::redact;

    #[test]
    fn redacts_a_jwt_but_keeps_surrounding_text() {
        let s = "here: eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U end";
        let out = redact(s);
        assert!(!out.contains("eyJhbGci"), "jwt gone: {out}");
        assert!(
            out.contains("here:") && out.contains("end"),
            "context kept: {out}"
        );
        assert!(out.contains("[redacted:jwt]"), "marker: {out}");
    }

    #[test]
    fn redacts_prefixed_api_tokens() {
        for (raw, kind) in [
            ("sk-abcdef012345678901234567890123", "openai"),
            ("AKIAIOSFODNN7EXAMPLE", "aws"),
            ("ghp_016C7f9aBcDeFgHiJkLmNoPqRsTuVwXyZ012", "github"),
            // split so GitHub secret-scanning doesn't flag this fake test token
            (concat!("xox", "b-1234567890-abcdefghijklmnop"), "slack"),
        ] {
            let s = format!("key={raw} rest");
            let out = redact(&s);
            assert!(!out.contains(raw), "{kind} token redacted: {out}");
            assert!(
                out.contains("key=") && out.contains("rest"),
                "context kept: {out}"
            );
            assert!(out.contains("[redacted:"), "marker for {kind}: {out}");
        }
    }

    #[test]
    fn redacts_a_pem_private_key_block() {
        let s = "cfg\n-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAA\nAAAAABG5vbmUAAA\n-----END OPENSSH PRIVATE KEY-----\ndone";
        let out = redact(s);
        assert!(!out.contains("b3BlbnNz"), "key body gone: {out}");
        assert!(!out.contains("BEGIN OPENSSH"), "header gone: {out}");
        assert!(
            out.contains("cfg") && out.contains("done"),
            "context kept: {out}"
        );
        assert!(out.contains("[redacted:private-key]"), "marker: {out}");
    }

    #[test]
    fn redacts_a_jwt_with_trailing_punctuation() {
        // A JWT ending a sentence: the trailing '.' merges into the token and
        // used to yield 4 segments, missing the secret.
        let s = "issued eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ4In0.abc123_def-XYZ.";
        let out = redact(s);
        assert!(!out.contains("eyJhbGci"), "jwt gone: {out}");
        assert!(out.contains("[redacted:jwt]"), "marker: {out}");
        assert!(out.trim_end().ends_with('.'), "trailing period kept: {out}");
    }

    #[test]
    fn redacts_more_provider_tokens() {
        for raw in [
            "AIzaSyA1234567890abcdefghijklmnopqrstuvw",
            "sk_live_0123456789abcdefghijklmnop",
            "github_pat_11ABCDE0000abcdefghij_0123456789abcdefghijklmnopqrstuvwxyzABCD",
        ] {
            let s = format!("k={raw} z");
            let out = redact(&s);
            assert!(!out.contains(raw), "redacted: {out}");
            assert!(
                out.contains("[redacted:") && out.contains("k=") && out.contains("z"),
                "{out}"
            );
        }
    }

    #[test]
    fn leaves_ordinary_code_and_prose_untouched() {
        for ok in [
            "let sk = 3; // a short name, not a secret",
            "the ghost wrote authenticate() and returned early",
            "https://example.com/path?x=1 and a sha like abc123",
            "AKIA is a prefix but AKIAshort is too short",
        ] {
            assert_eq!(redact(ok), ok, "false positive on: {ok}");
        }
    }
}
