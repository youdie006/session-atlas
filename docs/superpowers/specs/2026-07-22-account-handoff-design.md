# Account handoff — continue a conversation on another account — design spec

**Status:** design, awaiting review. A joint swapdex + sessionwiki feature.

**Goal:** when an AI coding session hits its usage limit on account A, continue
the SAME conversation on account B with one command — swapdex switches the
account, sessionwiki carries the transcript so B can resume it.

**Why it's a differentiator:** deja-vu's `handoff` moves context to another
*agent/tool*; Claude Code just makes you wait when you hit the 5-hour limit.
"continue the same conversation on a different *account*" is something neither
does — and it only works because swapdex (accounts) and sessionwiki (sessions)
sit on one machine. It targets the real "quota wall" pain directly.

## The key insight (why it's possible)

A conversation is a LOCAL file and is account-agnostic:
`~/.claude/projects/<cwd>/<uuid>.jsonl` holds the user/assistant/tool-call
history; the account (credential + oauthAccount) only provides API quota.
`claude --resume <uuid>` replays that local transcript and re-sends the context
to the API — so ANY account with quota can continue it. The only real barrier is
making the transcript visible to account B.

## Two swapdex models, two difficulty levels

- **Classic snapshot model** (one `~/.claude`, swapdex swaps only the credential
  + oauthAccount): the `projects/` transcripts are SHARED across accounts. So
  `swapdex use B` then `claude --resume <uuid>` just works — B's quota, same
  transcript, zero copying.
- **Slot model** (0.26.0, each account its own `CLAUDE_CONFIG_DIR`, `projects/`
  included): transcripts are isolated per slot. Continuing on B needs the
  transcript carried into B's slot — this is exactly where sessionwiki earns its
  place, crossing the account-isolation boundary the slot model deliberately
  builds.

## Division of labor

- **swapdex** — knows which account has quota (`usage` already breaks down per
  account: `@bsgong 5h: 606k / @rnd 5h: 1.2M`) and switches (`use`). "which B,
  and switch to it".
- **sessionwiki** — knows the session (file, project, tool) and can `migrate`
  (copy a transcript into a target store), `resume` (reopen in-tool), and `brief`
  (markdown context if a full resume isn't possible). "make this conversation
  continue on B".

## Command shape

`swapdex continue` (or `sessionwiki handoff --account B`), one command:

1. Identify the session to continue — the most recent session in the current
   project, or an explicit id (sessionwiki: `recent_sessions` / by cwd).
2. Pick account B — the same tool's account with the most remaining quota
   (swapdex `usage`/`quota`), or an explicit `--account`.
3. Switch to B (swapdex `use`).
4. Slot model only: carry the transcript into B's `CLAUDE_CONFIG_DIR` store
   (sessionwiki `migrate`, taught to target a specific config dir). Classic
   model: skip.
5. Print/exec the resume command for B (`claude --resume <uuid>`).

## Hard parts (and how to handle them)

- **A running session can't be swapped under itself** — swapdex's running-session
  guard refuses a switch while the tool is live (rotation-logout protection). So
  the flow is: exit the current Claude → handoff → resume in a fresh Claude. Not
  true mid-stream streaming; practically fine ("you hit the wall, so you're
  stopping anyway").
- **`migrate` must target B's slot config dir** — today `migrate` copies into a
  project dir in the default store. Slot-model handoff needs it to accept a
  `CLAUDE_CONFIG_DIR` (or swapdex hands sessionwiki the slot's path). New
  parameter, not new machinery.
- **Quota detection** — v1 is manual (you run `continue` when Claude says you're
  limited). A later hook/wrapper could detect the limit message and offer it.
- **oauthAccount correctness** — the handoff MUST land B's identity correctly, or
  it reproduces the identity-poisoning class (see the swapdex Claude identity
  notes). Depends on the account being cleanly switched first.

## Feasibility to verify BEFORE building (the one real unknown)

Does `claude --resume <transcript>` continue on a DIFFERENT account without a
server-side org/account binding on the session? The transcript is local and
resume re-sends context, so it SHOULD be account-agnostic — but confirm with a
real two-account test (resume an account-A transcript while account B is active,
check it continues and bills B). If Claude Code binds a session to its creating
org server-side, the classic-model path still works within one org, and
cross-org handoff would fall back to `brief` (a fresh session seeded with a
markdown summary) rather than a true resume.

## Slices (build order, after the feasibility check passes)

1. **Feasibility test** — the two-account resume check above. Gate everything on
   it.
2. **Classic-model handoff** — `swapdex continue`: pick B by quota, `use` B,
   print `claude --resume <uuid>` for the current project's latest session. No
   transcript copy needed. Ship this first — it's the whole feature for
   classic-model users.
3. **Slot-model carry** — teach `migrate` to target a slot's `CLAUDE_CONFIG_DIR`;
   `continue` uses it when the account is slotted.
4. **`brief` fallback** — when a full resume isn't possible (cross-org, or a tool
   without `--resume`), emit `brief <id>` into a fresh session on B.
5. **(later) auto-offer** — detect the limit and prompt, instead of manual.

## Out of scope (v1)

- Mid-stream / live streaming swap (the running-session guard makes this a
  stop-then-resume, not a seamless splice).
- Automatic quota detection (manual trigger first).
- Non-Claude tools beyond what each tool's own resume supports (codex `resume`,
  etc. — same pattern, per-adapter).
