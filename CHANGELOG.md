# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/), and the project aims to follow
semantic versioning once it reaches 1.0.

## [Unreleased]

### Added

- **File history - "how this file was written".** Clicking a file in the web UI
  now shows its evidence chain: the sessions that edited it, newest first, each
  with what it actually changed - the edit kind (edit/write/multiedit) and a
  bounded snippet of the new code, not just a bare list of sessions that touched
  the path. This answers *why does this file look like this*. Backed by a new
  `edits(session_id, path, kind, ts, snippet)` cache table (extracted at parse
  time from each tool call, rebuilt on a schema bump like the other cache
  tables), an `evidence_for(path)` assembly, and a `GET /api/file?path=`
  endpoint. `trace`/`blame` still give the path-level provenance.

## [0.22.0] - 2026-07-20

Open any live session by its native id (codex-rollout or claude-transcript
UUID), closing the harness tower to sessionwiki join gap.

### Added

- `session_window`/`show` now resolve a session by its native codex-rollout or
  claude-transcript UUID (full or prefix), in addition to the sessionwiki short
  id. Live sessions not yet indexed are found via a disk-scan fallback (no
  sync), so a session started today opens by native id in one call.
- `list`, `recent_sessions`, and `search` JSON now expose a `native_id` field
  (the UUID only, never the absolute path). Closes the harness tower to
  sessionwiki join gap.

## [0.21.0] - 2026-07-16

Harden the MCP read path for in-loop agent-to-agent reads: a stable window
contract and freshness on every read tool.

### Added
- **`session_window` (MCP) now returns versioned JSON** (schema `sessionwiki.window/1`) instead of an unversioned text render, so a reading agent can parse a sibling's window rather than scrape it. Fields: `id`, `tool`, `project`, `title`, `started`/`ended`, `messages` (total turns), `large` (indexed head+tail), `budget_tokens`, `omitted_leading`, `turns[]` (each with `i` = stable turn index, `role` = user/assistant/tool, `text`, and `truncated` or `folded`+`bytes`), and a `drilldown` hint. Deterministic for a fixed (id, budget); the recent tail is kept within budget and the whole object is size-guarded so it is always valid JSON. Tool output is folded head+tail and byte-bounded.
- **Per-turn drill-down:** `session_window(id, turn=<i>)` returns that one turn's full retained text (schema `sessionwiki.turn/1`), untruncated by the window's folding/cap - fetch-full-content-on-demand for a turn seen in a window. (Tool outputs are already capped at parse time, so this recovers folded-out lines, not adapter-dropped bulk.)

### Changed
- **`search_sessions` (MCP) freshens in the background**, matching `recent_sessions` (0.20.2): it serves the current index immediately and kicks a bounded incremental sync, so a just-created sibling shows on a subsequent search without the request ever waiting on the store walk. `session_window` already reads the target file directly (live). This closes the last MCP read tool that did no freshen; incremental parsing (only files whose mtime/size changed) was already in place.

## [0.20.2] - 2026-07-16

### Changed
- **`recent_sessions` (MCP) freshens in the background.** It returns the current index immediately (~200ms) and kicks a bounded incremental sync, so a just-started sibling session shows on the next call - without the multi-second blocking sync a full 46GB-corpus walk would cost. New `index::sync_bounded` re-parses only files modified since a floor and never archives a live session on a bounded run.

## [0.20.1] - 2026-07-16

### Fixed
- **Over-cap sessions are windowed (head+tail) instead of dropped.** A session past the 256MB read cap was skipped entirely - so the biggest sessions, the ones most worth referencing across sessions, vanished from search and `session_window`. They are now indexed from the first 256KB + last 512KB of lines (byte-bounded, no full scan), partially readable, and flagged `[large]` in the title.

## [0.20.0] - 2026-07-15

Cross-session context for agents: one agent can now see what a sibling session
(or its other self) is doing, as the ACTUAL recent conversation - not a lossy
summary - bounded so it can't flood the reader's context.

### Added
- **`sessionwiki show <id> --window [--budget N]`** renders a session's real
  recent turns with tool outputs folded to head+tail, capped to the recent tail
  by a token budget, an orientation header + role labels, and a
  `show <id> --full` drill-down hint. `--live` reads the session file directly
  (0-delay), bypassing the index sync.
- **MCP discovery + read tools** so an agent can find and read sibling sessions:
  - `session_window(id, budget_tokens)` - the bounded window above, over stdio.
  - `recent_sessions(limit, tool, exclude_id)` - the most recent sessions across
    every tool ("who's around"), bounded to a recent window.
  - `related_sessions(id, limit, exclude_id)` - a session's siblings by shared
    project, edited files, or tags.
  Both discovery tools take `exclude_id` to drop your own session. This closes
  the chain: `recent`/`search`/`related` -> id -> `session_window`. Verified
  cross-tool (Claude Code and Codex sessions see each other).

### Fixed
- (from 0.19.4) `sessionwiki web` no longer auto-opens a browser on WSL, where
  `xdg-open` pops a stray WSLg/Windows window on every start and often can't
  reach the loopback server; it prints the URL instead. Native Linux/macOS keep
  auto-open.

## [0.19.4] - 2026-07-14

### Fixed
- **`sessionwiki web` no longer pops a stray browser window on WSL.** On WSL,
  `xdg-open` launches a WSLg/Windows browser on every `web` start - a blank
  window each time you restart the server during development, and it often
  cannot even reach the loopback address. `web` now detects WSL and prints the
  URL instead of auto-opening; native Linux/macOS keep the existing auto-open
  (still overridable with `--no-open`).

## [0.19.3] - 2026-07-14

Codex/Claude transcript formats drifted; a comprehensive audit against the
real corpus caught five parsing gaps, all fixed here.

### Fixed
- **Codex user messages no longer index twice.** Current rollouts carry each
  prompt BOTH as an `event_msg` and as a `response_item`; the adapter now
  cancels each pair exactly once (counted, so a genuinely repeated prompt -
  "continue" twice - still keeps every occurrence).
- **AGENTS.md instructions no longer become session titles.** The new
  `# AGENTS.md instructions for <path>` / `<INSTRUCTIONS>` user-message shape
  is treated as boilerplate, like the older environment-context wrappers.
- **2025-era Codex rollouts parse again.** Files that predate the `payload`
  envelope (bare `{"type":"message"}` lines) indexed as empty "(no user
  prompt)" sessions; they are now parsed like their modern counterparts.
- **`custom_tool_call` items are indexed** and their inputs feed `trace` /
  `blame` path extraction, so edits made through custom tools are no longer
  invisible.
- **Claude titles use the `ai-title` event.** Current Claude Code transcripts
  stopped emitting `summary` events; titles now fall back summary ->
  ai-title -> first user message instead of always the first prompt.

## [0.19.2] - 2026-07-07

### Fixed
- A crafted/corrupt prodex task id with a multibyte character at a slice
  boundary could panic the parser - and a parse panic aborts the WHOLE
  indexer, so one bad `.bridge` file bricked every `list`/`search`/`scan`.
  The id timestamp parse now gates on ASCII digits before slicing (found by
  an adversarial audit; same bug class swapdex fixed a release earlier).
- `list --account` no longer silently returns fewer rows than `-n`: the
  badge filter is computed post-query, so the query now over-fetches and
  truncates after filtering.

## [0.19.1] - 2026-07-07

Ecosystem-walkthrough fixes, from a fresh user's chair.

### Added
- `--account <name>` on `list` and `search`: filter by the swapdex @badge,
  which the help text now explains.
- prodex consult titles are the QUESTION (prompt first line) instead of
  prodex's identical auto-title, so a list of consults is tellable apart.
  The derived-cache version was bumped, so existing rows re-index once.

### Fixed
- A consult whose answer artifact was written but whose result JSON was not
  (the crash path the durable ledger exists for) is indexed again: the
  artifact is read independently of the result.
- `trace` with an ABSOLUTE editor path now finds sessions whose stored paths
  are repo-relative (prodex bundles relative paths; matching honors the `/`
  boundary, so suffix lookalikes cannot match).
- `--tool` help enumerates prodex and points at `scan` for the full list;
  the scan footer pluralizes correctly; scan's prodex PATH shows the store
  directory (not the registry file); the prodex resume error message no
  longer has a hanging indent.

## [0.19.0] - 2026-07-07

### Added
- prodex adapter: every ChatGPT Pro consult made through
  [prodex](https://github.com/youdie006/prodex) becomes a searchable session -
  the bridge task is the question, the result/answer artifact is the answer,
  and files bundled with a task become `trace` provenance links. Discovery
  reads the machine-wide bridge registry prodex >=0.11.0 maintains
  (`~/.local/share/prodex/bridges.json`); registered repos that no longer
  exist are skipped silently. `resume` on a prodex session prints the ChatGPT
  thread URL it ran in (only an `https://chatgpt.com/` URL is ever surfaced).
- Session ids for prodex are the same short stable 12-hex shape as every
  other tool (task ids share a long date prefix that would defeat prefix
  addressing).

### Changed
- `list`/`search` truncate the ID column display at 13 chars so an unusually
  long id cannot shear the table (ids are prefix-addressable).

## [0.18.1] - 2026-07-07

### Fixed
- `trace` and `related` rows now carry the swapdex `account` field like
  `list`/`search` do (the annotate call was missing on those two query paths,
  so the same session showed a badge in search but never in a file trace -
  across CLI, web, and MCP).

### Changed
- Timeline reads are capped to the newest 4000 lines (defense in depth;
  swapdex itself bounds the file to ~1000 events), and account names are
  control-char-stripped at the source so every consumer gets a terminal-safe
  badge. `search` no longer prints a trailing space on badge-less lines.
- The web UI shows the account badge in each session's meta line.

## [0.18.0] - 2026-07-07

### Added
- swapdex integration: `list` and `search` badge each session with the
  account profile active when it ran (`@work`), and `--json` rows carry an
  `account` field (null when unknown). Attribution reads swapdex's switch
  timeline read-only and never guesses - sessions that predate the first
  recorded switch stay unbadged. No swapdex on the machine, no change.
  The MCP server neutralizes the new field like every other free-text one.

## [0.17.0] - 2026-07-03

### Added
- `sessionwiki mcp`: a stdio MCP server exposing three read-only tools
  (`search_sessions`, `trace_file`, `get_session_brief`) to any MCP client
  (Claude Code, Cursor, and others). Cross-tool session search as agent tools;
  100% local, read-only, no sync. Register with
  `claude mcp add sessionwiki -- sessionwiki mcp`.
- Homebrew: `brew install youdie006/tap/sessionwiki` (macOS and Linux).

### Changed
- aider re-indexing is now linear instead of O(runs^2): a long
  `.aider.chat.history.md` is split once per sync and cached, so a file with
  thousands of runs re-indexes in a fraction of the time (measured 1600 runs:
  ~11s to ~0.2s).

### Fixed
- A partial directory walk (an unreadable project directory) can no longer
  archive live sessions: file-per-session adapters now report a partial listing
  and sync skips deletion reconciliation for that run, the same guard shared
  stores already had.
- `sync` warns when a session fails to parse and reports honest counts
  ("indexed 12/14 (2 failed to parse)") instead of silently dropping it.
- Tags are NFC-normalized on add, filter, and remove, so a tag typed in
  decomposed form (macOS IME) matches everywhere, like every other indexed
  string.
- `resume` checks tool support before file existence, so tools without headless
  resume (aider, OpenCode) get the right message instead of a false "the
  session file is gone".
- `digest --since` with an out-of-range duration returns an error instead of
  panicking.
- A failed data-directory migration now warns with instructions instead of
  silently starting an empty index.

## [0.16.1] - 2026-06-19

### Fixed
- crates.io README: the dark-mode banner now uses an absolute image URL so it
  renders on the crates.io page, which (unlike GitHub) does not rewrite relative
  `<source srcset>` paths. Docs only; no code change.

## [0.16.0] - 2026-06-19

### Added
- `aider` adapter (11th tool): indexes Aider sessions from per-repo
  `.aider.chat.history.md`. Markdown-derived (roles reconstructed from line
  prefixes, no per-message timestamps); edited files come from aider's
  `Applied edit to` / `Creating empty file` lines. Discovery is a bounded,
  capped, symlink-safe walk (home by default, `SESSIONWIKI_AIDER_ROOTS` to
  scope) that opens only `.aider.chat.history.md` and logs nothing.
- Recall loop (Claude Code plugin): a SessionStart hook auto-injects a small,
  fenced "prior work in this project" brief at the start of a new session, so the
  agent gets long-term memory without being asked. Exact-project scoped,
  `--no-sync` (tens of ms), zero output on fresh projects, always exits 0. The
  injected recall is fenced as untrusted data and sanitized; the LLM synopsis is
  not auto-injected. New internal `sessionwiki hook session-start` command.

### Changed
- Schema stability on the road to 1.0: the durable index tables (summaries,
  tags, notes, archive) now version independently of the disposable cache via a
  `meta.durable_version` and a forward-only, additive-only migration runner, so
  user curation and archived sessions survive every upgrade. All index
  connections now set a `busy_timeout`. No behavior change in this release (zero
  migrations registered).

### Security
- `search` strips control bytes (ESC/OSC/C1/DEL) from terminal snippets, so an
  untrusted session body can no longer inject terminal escape sequences into the
  operator's terminal. The `--json` output and other commands already did this.

## [0.15.0] - 2026-06-18

### Added
- `blame <file> [-L start,end]`: git blame for the AI era - attributes each line
  to the AI session most likely behind the commit that last changed it, by
  joining `git blame --line-porcelain` with the index's per-file touch records
  and session time windows. Best-effort by design (`ambiguous`/`unattributed`
  are normal) and it falls back to file-level `trace` whenever git can't carry
  the weight. The git child runs with a hardened environment (no system config,
  no pager, `core.fsmonitor`/hooks neutralized, inherited `GIT_*` cleared), the
  path is passed after `--`, and `-L` is validated. `--json` for agents.

### Security
- `truncate()` - the choke point that renders session titles to the terminal in
  `list`/`search`/`trace`/`resume` - now strips C0/C1/DEL control characters, not
  just newlines and tabs. A session title is untrusted input (a planted session
  can set any title), so an unstripped ESC could have injected ANSI/terminal
  escape sequences into output. The web UI was already safe (it builds the DOM
  with `textContent`, never `innerHTML`).

## [0.14.0] - 2026-06-18

### Added
- **gptme adapter** (10th tool): indexes sessions from [gptme](https://github.com/gptme/gptme)
  (`~/.local/share/gptme/logs/<session>/conversation.jsonl`). Drops `pinned: true`
  system-prompt boilerplate and bare `system` role lines; handles Python's naive
  `datetime.now().isoformat()` timestamps (no UTC offset) by falling back to
  UTC-assumed parsing. Size-capped against `MAX_SESSION_FILE_BYTES`.
- Windows install: `scripts/install.ps1`, a PowerShell one-liner (`irm ... | iex`)
  that downloads + checksum-verifies the latest Windows binary and adds it to
  PATH. The shell installer already covers WSL, which is Linux.

## [0.13.0] - 2026-06-18

### Security
Hardening from a red-team pass of the launch surface (all confirmed with PoCs):
- The local web server now rejects requests whose `Host` header isn't the
  loopback name it bound (defeats DNS rebinding) and whose `Origin` is a
  different site (defeats a plain cross-origin `fetch`), so a malicious page you
  visit while `web` is running can't read your sessions from `127.0.0.1`.
- `resume` no longer auto-launches Codex or Gemini in a directory the session
  file claims. Those tools aren't directory-scoped, so a planted session could
  name any directory (with an attacker's `AGENTS.md` there) and have `resume`
  load it; it now prints the command for you to run unless the directory is
  verified, which only Claude Code's store layout allows.
- `resolve` escapes `LIKE` wildcards in a session-id prefix, so a web request for
  id `%` can no longer match and dump an arbitrary session.
- install.sh documents honestly that its checksum only detects a corrupted
  download, not a malicious release (the hash is co-located with the binary).

## [0.12.0] - 2026-06-18

### Added
- `digest [--since 7d]`: a markdown rollup of recent sessions grouped by project
  &mdash; what you worked on, the files each session touched, and any cached
  synopsis. It composes the timeline, provenance, and summaries the index already
  has, over a time window (`--since 2w`/`24h`/`90m`, `--project`, `--tool`,
  `--json`). The standup / PR-description / "what did I ship this week" view.

### Security
- `resume` no longer auto-launches the upstream tool (`claude`/`codex`) in the
  directory a session file *claims* unless that directory can be verified as the
  session's own &mdash; for Claude Code, by matching the store folder the session
  lives in (the folder name encodes the cwd). A session file is untrusted input;
  before this, a planted or prompt-poisoned session could point its `cwd` at an
  attacker directory and have `resume` launch the agent there, loading that
  directory's `CLAUDE.md` / `.mcp.json` / settings. When the directory can't be
  verified, `resume` now prints the command for you to run instead of launching.
- Session-file reads are size-capped (256 MB) and the OpenCode SQLite message/
  part reads are `LIMIT`ed, so a malicious oversized file or a db packing
  millions of rows into one session can't exhaust memory.

### Changed
- `SECURITY.md` now documents that `migrate` writes copies into the target tool's
  store and that `resume` validates the session-derived directory, instead of
  implying the tool only ever reads.

## [0.11.0] - 2026-06-18

### Added
- `migrate <id> <dir>`: copy a session so it can be resumed from a different
  project directory. Each tool ties a session to a directory differently, so
  migrate does the right thing per tool: Claude Code's resume is scoped to the
  project folder, so the transcript is copied into the target's folder; Codex
  resumes by id from any directory, so nothing is copied (it just prints the
  command); Gemini copies the chat into the target project's store
  (`~/.gemini/tmp/<sha256(dir)>/chats/`) and rewrites its `projectHash`. The
  original session is never modified.

## [0.10.0] - 2026-06-17

### Added
- `recall <query>`: one-shot recall — searches, lists the candidate matches, and
  briefs the top one, collapsing the usual search -> pick id -> brief loop into a
  single command (`--tool`/`--project`/`-n`/`--max-chars`/`--json`/`--no-sync`).
- `sync [--tool]`: build or refresh the index on demand, so later queries can run
  with `--no-sync`.
- `--no-sync` on `search`/`list`/`recall`/`show`/`brief`/`resume`/`trace`: query
  the already-built index without walking the stores — the fast path when the
  index is kept warm (e.g. a cron running `sessionwiki sync`).

### Changed
- `show`/`brief`/`resume`/`recall` resolve the id against the existing index first
  and only sync (all tools) when it is genuinely unknown (no prefix match) - an
  already-indexed id, including an *ambiguous* prefix, no longer triggers a full
  walk of every store (notably the large Codex one). The tradeoff: a session that
  has grown since the last sync is served at its last-synced length until the next
  `sync`.
- Harness labeling (the oh-my-* tags from 0.9.0) is now detected from the
  filesystem only - the `.omc`/`.omo` orchestration directory in a session's
  project - dropping the transcript-text markers. Those markers were the tools'
  own source/doc literals, so a session that merely *discusses* a harness (e.g.
  while building OSS) was mislabeled as run by it; the filesystem signal cannot
  be confused that way. oh-my-codex leaves no directory and is no longer tagged.
- A tag filter (`list --tag`, web) now includes subagent transcripts, and `list`
  marks them `[subagent]`, so a tag carried only by a subagent is no longer shown
  in the tag cloud yet hidden from the listing.

### Fixed
- `recall --json` includes the plain `snippet` next to `snippet_marked` in each
  candidate, matching `search --json`; `recall`'s candidate list marks subagent
  transcripts `[subagent]`.
- `sync` reports the count of top-level live sessions (matching `stats`/`list`)
  instead of every file row, which had included subagent transcripts.

## [0.9.0] - 2026-06-17

### Added
- Five new adapters, bringing supported tools to nine: the **Cline family**
  (Cline, Roo Code, Kilo Code) from one parser over their shared VS Code
  `globalStorage/<ext>/tasks/<id>/` layout - handling both native `tool_use`
  blocks and the XML-in-text tool calls Cline writes, and pulling edited files
  from either; **gajae-code** (and upstream Pi) from `~/.gjc/agent/sessions`
  JSONL; and **Continue** from `~/.continue/sessions/*.json`. Each extracts the
  files a session edited so they show up in `trace`.
- Harness labeling for the Korean "oh-my-*" wrappers. oh-my-claudecode,
  oh-my-codex, and the oh-my-openagent / lazyclaudecode / lazycodex family run
  on top of Claude Code, Codex, and OpenCode, so their conversations are already
  indexed via those adapters; sessionwiki now detects which harness drove a
  session (from the markers each injects) and auto-tags it `oh-my-claudecode` /
  `oh-my-codex` / `oh-my-openagent`, so `list --tag oh-my-claudecode` works. No
  new adapter required.

### Changed
- The OpenCode adapter now reads OpenCode's SQLite store
  (`~/.local/share/opencode/opencode.db`, plus `opencode-<channel>.db` and
  `OPENCODE_DB`), which has been the default since OpenCode v1.2.0 - the
  previous JSON-only reader missed every session created on a current install.
  The legacy `storage/**` JSON layout is still read on pre-1.2.0 installs. This
  needed a small shared-store path in the indexer (one SQLite db holds many
  sessions, so the file-per-session + mtime model did not fit); `scan` sizes the
  db and re-indexes only sessions whose updated-time changed.

## [0.8.0] - 2026-06-17

### Added
- OpenCode adapter: indexes sst/opencode sessions from
  `~/.local/share/opencode/storage` (the multi-file session/message/part JSON
  layout, joined in id order), including the files each session edited (the
  `edit`/`write` tools' `filePath` and `patch` file lists) for `trace`. Brings
  supported tools to four.
- Provenance: sessions are linked to the files they edited or created, read
  from their tool calls (Claude's `Edit`/`Write`/`MultiEdit`, Codex's
  `apply_patch`). New commands: `files <id>` (a session's edits) and
  `trace <path>` (the sessions that touched a file, newest first, matched by
  suffix so a relative path resolves to the absolute one on disk). The scope is
  honest — sessions that *touched* a file, not line-level authorship.
- `related` now also links sessions that edited the same file, even across
  projects; `stats` reports the count of files linked to a session.
- Web UI: a session's touched files appear as chips in its header; clicking one
  lists every session that touched it (new `/api/trace` endpoint).
- Archive mode: when a tool deletes a session's original file, sessionwiki
  keeps the indexed copy instead of dropping it, so `search`, `trace`, and
  `brief` keep working for it. A durable `archive` table survives schema
  rebuilds; archived sessions are served from the index (`show`/`brief`/web),
  flagged `[archived]` in `list`/`show`/web, and counted in `stats`. `sync`
  reports how many were kept. New `forget <id>` purges a session for good;
  re-appearing files un-archive automatically; `SESSIONWIKI_NO_ARCHIVE` reverts
  to delete-on-prune. A store that vanishes wholesale (uninstall, unmount) is
  not mass-archived.
- Korean/CJK search: 2-syllable words (회사, 검색) - the most common Korean word
  length - are now searchable via a LIKE fallback below the trigram floor, and
  all indexed text + queries are NFC-normalized so macOS NFD input no longer
  silently returns nothing. "Zero-setup CJK search" is now actually true.
- `--json` on `search`, `list`, `related`, `brief`, `trace`, and `files`: stable
  snake_case output (matching the web API) for agent consumption, with the
  absolute path hidden. Snippets are control-code-free with a `snippet_marked`
  variant. The agent-native half of the tool.
- Claude Code plugin (`/plugin marketplace add youdie006/sessionwiki`): a
  `session-recall` skill + `/sessionwiki:recall` command that use the `--json`
  commands so an agent recalls past sessions as long-term memory, fully offline,
  degrading gracefully when the CLI isn't installed.

### Changed
- `show` pages long transcripts through your pager (`SESSIONWIKI_PAGER`, then
  `PAGER`, default `less -FRX`) when stdout is a terminal, so `show --full` of a
  huge session no longer floods the screen, and falls back to plain printing when
  no pager is installed; piped/redirected output is unchanged.
- `summarize` states up front that it pipes each transcript to the summarizer
  (the default `claude -p` sends them to the Anthropic API) - the one place data
  can leave the machine, and only on explicit `summarize`.
- CLI demo is a clean static full-frame terminal (no camera zoom). The web demo
  zooms to exact element regions measured from the live DOM, at 50fps with
  cubic ease-in-out moves and still holds.

### Fixed
- `tag` now rejects a tag containing a comma or an empty tag: a comma corrupted
  the comma-joined `tags` array in `--json` and the web API, and an empty tag
  emitted `""`. JSON serialization of session rows in `search`/`trace` can no
  longer panic mid-array.

## [0.7.0] - 2026-06-16

### Changed
- Renamed the project from `sessiondex` to **sessionwiki**, reframed around
  "session engineering": a session is a unit of context, and once you have
  hundreds they need curating, not just searching. The data directory migrates
  newest-name-first (`sessiondex` then `session-atlas`), carrying the index and
  curated tags/notes/summaries over.

### Added
- Curation layer (the editable "wiki" part): `tag` / `note`, stored in the
  index and surviving reindexing; the original session files are never touched.
- `related`: sessions about the same thing — same project first, then anything
  sharing a tag — surfaced in `show` and as a "see also" panel in the web UI.
- `projects` and `stats`: per-project and per-tool/per-month rollups.
- Web UI: clickable tag chips and a tag-filter bar, notes, a "see also" panel,
  a node-graph logo, and a globe-button language picker.
- Two demo GIFs: a narrated CLI recording (`docs/demo-cli.gif`) and a web UI
  tour (`docs/demo-web.gif`). `SECURITY.md`.

## [0.6.0] - 2026-06-16

### Added
- Library + binary split (`src/lib.rs`): the adapter and index logic is now a
  reusable crate, not just a CLI.
- Golden-file parser tests for all three adapters, covering boilerplate
  dropping, subagent detection, schema variants, multi-block content, and
  malformed-line tolerance (`tests/parse.rs`, `tests/fixtures/`).
- GitHub Actions: CI (fmt + clippy + test) and a release workflow that builds
  prebuilt binaries for Linux, macOS (x86_64 + arm64), and Windows on tag push.
- `scripts/install.sh` one-line installer that fetches the right prebuilt
  binary for the platform.
- A "how this differs from a single-tool viewer" section in the README.

## [0.5.0] - 2026-06-12

### Changed
- Renamed the project from `session-atlas` to **sessiondex**. The data
  directory migrates automatically on first run, carrying the existing index
  and cached summaries over.

### Added
- Multilingual web UI (English, Korean, Japanese, Chinese), auto-detected from
  the browser locale and switchable from the sidebar.
- Korean README (`README.ko.md`).

## [0.4.0] - 2026-06-12

### Added
- `summarize`: cache 1–2 sentence LLM synopses using your own LLM CLI. Stored
  in a table that survives index schema rebuilds.
- README polish: logo, light/dark adaptive hero, architecture diagram, FAQ.

## [0.3.0] - 2026-06-12

### Added
- Session previews (last assistant message) in the web sidebar.
- `show --outline`: a deterministic digest of a session (every user turn plus
  how it ended), and a clickable outline in the web transcript.

## [0.2.0] - 2026-06-12

### Added
- `resume`: reopen a session in its original tool (`claude --resume` /
  `codex resume`) in the right project directory.
- `brief`: emit a markdown briefing of a session to carry context across tools.

## [0.1.0] - 2026-06-12

### Added
- Initial release: `scan`, `list`, `search`, `show`, and a local web UI over an
  incremental SQLite FTS5 index. Adapters for Claude Code, Codex, and Gemini
  CLI. 100% local, no telemetry.

[0.15.0]: https://github.com/youdie006/sessionwiki/compare/v0.14.0...v0.15.0
[0.14.0]: https://github.com/youdie006/sessionwiki/compare/v0.13.0...v0.14.0
[0.13.0]: https://github.com/youdie006/sessionwiki/compare/v0.12.0...v0.13.0
[0.12.0]: https://github.com/youdie006/sessionwiki/compare/v0.11.0...v0.12.0
[0.11.0]: https://github.com/youdie006/sessionwiki/compare/v0.10.0...v0.11.0
[0.10.0]: https://github.com/youdie006/sessionwiki/compare/v0.9.0...v0.10.0
[0.9.0]: https://github.com/youdie006/sessionwiki/compare/v0.8.0...v0.9.0
[0.8.0]: https://github.com/youdie006/sessionwiki/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/youdie006/sessionwiki/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/youdie006/sessionwiki/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/youdie006/sessionwiki/releases/tag/v0.5.0
[0.4.0]: https://github.com/youdie006/sessionwiki/releases/tag/v0.4.0
[0.3.0]: https://github.com/youdie006/sessionwiki/releases/tag/v0.3.0
[0.2.0]: https://github.com/youdie006/sessionwiki/releases/tag/v0.2.0
[0.1.0]: https://github.com/youdie006/sessionwiki/releases/tag/v0.1.0
