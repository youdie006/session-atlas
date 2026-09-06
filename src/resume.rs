use std::path::{Path, PathBuf};

/// How to reopen a session in its original tool.
///
/// Built from the session file path for the two tools that can be resumed
/// headlessly - Claude Code and Codex - because both encode their native
/// session id in the filename. The other ten adapters have no headless resume
/// and `for_session` answers None for them; the caller says so rather than
/// offering a command that would not work.
pub struct ResumeInfo {
    pub program: &'static str,
    pub args: Vec<String>,
    /// The tool resolves sessions relative to the project, so the command
    /// should run there. None when the session has no usable cwd.
    pub cwd: Option<PathBuf>,
    /// Whether `cwd` is trusted as genuinely belonging to this session. A
    /// session file is untrusted input and can claim any directory, so a caller
    /// that *launches* the tool there (loading that dir's CLAUDE.md/.mcp.json/
    /// etc.) must not do so automatically when this is false.
    pub verified_cwd: bool,
    pub note: Option<String>,
}

impl ResumeInfo {
    pub fn command_line(&self) -> String {
        let mut s = String::from(self.program);
        for a in &self.args {
            s.push(' ');
            s.push_str(a);
        }
        s
    }
}

pub fn for_session(tool: &str, path: &Path, project: &str) -> Option<ResumeInfo> {
    let (cwd, verified_cwd) = safe_cwd(tool, path, project);
    match tool {
        "claude-code" => {
            let p = path.to_string_lossy();
            if p.contains("/subagents/") {
                // Subagent transcripts cannot be resumed directly, but the
                // parent session id is the directory just above "subagents":
                // <project>/<parent-uuid>/subagents/agent-*.jsonl
                let comps: Vec<String> = path
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect();
                let idx = comps.iter().position(|c| c == "subagents")?;
                let parent = comps.get(idx.checked_sub(1)?)?.clone();
                if !looks_like_uuid(&parent) {
                    return None;
                }
                return Some(ResumeInfo {
                    program: "claude",
                    args: vec!["--resume".into(), parent],
                    cwd,
                    verified_cwd,
                    note: Some("this is a subagent transcript; resuming its parent session".into()),
                });
            }
            let id = path.file_stem()?.to_string_lossy().into_owned();
            looks_like_uuid(&id).then(|| ResumeInfo {
                program: "claude",
                args: vec!["--resume".into(), id],
                cwd,
                verified_cwd,
                note: None,
            })
        }
        "codex" => {
            // rollout-<timestamp>-<uuid>.jsonl
            let stem = path.file_stem()?.to_string_lossy().into_owned();
            let id = stem.get(stem.len().checked_sub(36)?..)?.to_string();
            looks_like_uuid(&id).then(|| ResumeInfo {
                program: "codex",
                args: vec!["resume".into(), id],
                cwd,
                verified_cwd,
                note: None,
            })
        }
        // Gemini CLI saved chats are resumed from inside the REPL
        // (/chat resume); there is no headless resume-by-file today.
        _ => None,
    }
}

/// Decide which working directory to resume in, and whether we trust it.
///
/// A session's recorded project/cwd is *self-asserted data from the file*, which
/// is untrusted input: a planted or prompt-poisoned session could point it at an
/// attacker directory, and launching `claude`/`codex` there would load that
/// directory's `CLAUDE.md` / `AGENTS.md` / `.mcp.json` / settings into the
/// resumed agent. So we only trust a cwd that is a real local directory, and for
/// Claude Code we additionally require it to match the store folder the session
/// actually lives in (the folder name encodes the cwd), which a planted file in
/// some other folder cannot fake.
fn safe_cwd(tool: &str, path: &Path, project: &str) -> (Option<PathBuf>, bool) {
    if project.is_empty() || !project.starts_with('/') {
        // No directory claim; resuming in the user's own shell cwd is fine.
        return (None, true);
    }
    let Ok(canon) = std::fs::canonicalize(project) else {
        // Recorded directory does not resolve on this machine. Keep the claimed
        // path so the caller can show "run it where the project lives", but it
        // is not verified, so the caller must not auto-launch into it.
        return (Some(PathBuf::from(project)), false);
    };
    if !canon.is_dir() {
        return (Some(canon), false);
    }
    let verified = if tool == "claude-code" {
        let folder = path
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned());
        folder.as_deref()
            == Some(crate::migrate::claude_project_folder(&canon.to_string_lossy()).as_str())
    } else {
        // Codex / Gemini are not folder-scoped, so the recorded cwd cannot be
        // tied to this session at all - a planted session can name any directory
        // (with an attacker-planted AGENTS.md / .codex there). Never auto-launch
        // into it; resume prints the command for the user to run after a look.
        false
    };
    (Some(canon), verified)
}

fn looks_like_uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0a000000-0000-4000-8000-000000000001.jsonl";

    #[test]
    fn resume_only_trusts_a_cwd_it_can_verify() {
        let base = std::env::temp_dir().join(format!("sw-resume-{}", std::process::id()));
        let proj = base.join("realproj");
        std::fs::create_dir_all(&proj).unwrap();
        let canon = std::fs::canonicalize(&proj)
            .unwrap()
            .to_string_lossy()
            .to_string();

        // Claude Code session stored under the folder that encodes this cwd -> trusted.
        let good = base
            .join(crate::migrate::claude_project_folder(&canon))
            .join(ID);
        assert!(
            for_session("claude-code", &good, &canon)
                .unwrap()
                .verified_cwd
        );

        // Same cwd claim, but the file lives under a different folder (a planted
        // session pointing at someone else's directory) -> not trusted.
        let bad = base.join("-totally-different").join(ID);
        assert!(
            !for_session("claude-code", &bad, &canon)
                .unwrap()
                .verified_cwd
        );

        // A recorded directory that doesn't exist on this machine -> not trusted,
        // so resume won't auto-launch into it.
        let cdx = Path::new("/x/rollout-1-0a000000-0000-4000-8000-000000000001.jsonl");
        assert!(
            !for_session("codex", cdx, "/no/such/dir/here")
                .unwrap()
                .verified_cwd
        );

        // Codex/Gemini are never auto-trusted, even for an existing directory:
        // their cwd can't be tied to the session, so a planted one can't make
        // resume launch there.
        let cdx2 = Path::new("/x/rollout-1-0a000000-0000-4000-8000-000000000001.jsonl");
        assert!(!for_session("codex", cdx2, &canon).unwrap().verified_cwd);

        std::fs::remove_dir_all(&base).ok();
    }
}
