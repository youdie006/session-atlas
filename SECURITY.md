# Security

sessionwiki reads your AI coding session files (which contain your code and
conversations), builds a local index, and serves a local web UI. Its security
posture is deliberately small:

- **No network calls.** There is not a single one in the codebase. You can
  verify this with one grep over `src/`. The only feature that touches an LLM
  is `summarize`, which runs a shell command **you** supply (`--cmd` or the
  `SESSIONWIKI_SUMMARIZER` env var, default `claude -p`) with the session text
  on stdin. It runs exactly what you configured, on your machine; do not point
  it at a command you would not run yourself.
- **No telemetry, no accounts.**
- **The web UI binds to `127.0.0.1` only** and serves the index read-only; it
  never writes to your session stores.
- **Your existing session files are never modified.** Tags, notes, and summaries
  live in sessionwiki's own index, not in your tools' files.
- **Three commands act beyond the index, explicitly and on demand:**
  - `migrate <id> <dir>` *copies* a session into another tool's store so you can
    resume it from a different directory. It writes a new file; it never modifies
    or deletes an existing session.
  - `resume <id>` launches the original tool (`claude` / `codex`) in the session's
    recorded project directory. A session file is untrusted input and can claim
    any directory (with an attacker-planted `CLAUDE.md` / `AGENTS.md` there), so
    sessionwiki verifies that directory before launching: for Claude Code it
    checks the recorded cwd against the store folder the session actually lives in.
    Codex and Gemini sessions are not tied to a directory, so their cwd cannot be
    confirmed and `resume` never auto-launches them &mdash; it prints the command
    for you to run after a look. It only auto-launches when the directory is
    verified.
  - `blame <file>` shells out to `git` in the file's repository to attribute
    lines. Because a repository's own config can make git run commands, the
    child runs hardened: system config disabled, pager off, `core.fsmonitor`
    and `core.hooksPath` neutralized, inherited `GIT_*` cleared, and
    `safe.directory` never bypassed. The file path is passed after `--` (never
    parsed as a flag) and `-L` is validated to integers. Untrusted session
    titles and git author strings are control-stripped before they reach the
    terminal.

- **The recall hook injects untrusted recall into the agent as fenced data.**
  The optional Claude Code SessionStart hook (`sessionwiki hook session-start`)
  auto-injects a brief of your prior sessions in the launch directory into a new
  agent's context. Session titles are untrusted (a planted/synced/shared session
  can set any title), so the brief: wraps everything in a labeled
  `<sessionwiki-recall trust="untrusted-data" nonce=...>` fence that tells the
  model to treat it as data, not instructions; strips the fence tag, control
  characters, and markdown structure from each field (so a title cannot forge
  the fence or impersonate the prompt) and length-caps it; leads with low-free-
  text fields (date, tool, touched files) and does NOT auto-inject the LLM
  synopsis; scopes by exact directory match; and prints nothing for a project
  with no history. The hook is opt-in (installed with the plugin, `startup`-only)
  and reads the index only (`--no-sync`, no network). Fencing reduces but cannot
  fully eliminate model prompt-injection; users who sync or share session stores
  inherit this trust boundary.
- **The `mcp` server is stdio-only and read-only.** `sessionwiki mcp` speaks
  newline-delimited JSON-RPC over stdin/stdout with no sockets and no network,
  and opens the index with a read-only handle, so a tool call can never write,
  migrate, or VACUUM your data. Its three tools (`search_sessions`, `trace_file`,
  `get_session_brief`) never sync (no store walk). Session content returned to an
  agent is untrusted data: it is control-stripped, short free-text fields are
  neutralized against tag/fence forgery, briefs are prefixed with a treat-as-data
  line, and no absolute session-file paths are ever emitted. Inbound lines are
  size-capped and numeric arguments clamped, so a malformed or hostile client
  cannot exhaust memory. The server trusts `SESSIONWIKI_DATA` exactly as the CLI
  does (it points at your own local index).

## Reporting a vulnerability

If you find a security issue, please open a
[GitHub Security Advisory](https://github.com/youdie006/sessionwiki/security/advisories/new)
or a regular issue if it is low-risk. Since the tool handles potentially
sensitive local data, reports about accidental data exposure, a path that
could write outside the index directory, or any outbound network behavior are
especially welcome.
