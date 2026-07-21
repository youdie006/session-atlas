# File history page (flagship) + evidence chain — design spec

**Status:** design, awaiting review. Supersedes the earlier "global graph view"
flagship idea after a ChatGPT Pro (prodex) strategy review argued the global
force-directed graph is eye-candy, not a moat.

**Goal:** make sessionwiki the tool that answers *"why does this file look like
this?"* with preserved evidence — deeper than a file-level "which sessions
touched this path" list. The graph survives only as a focused drill-down inside
this page, not as the product's front door.

**Positioning:** deja helps agents remember; sessionwiki helps humans prove why
the code is there, and keeps the evidence after the originating tool deletes it.
This permits coexistence — it does not require beating deja at generic recall.

## The reframe: the evidence chain is the moat, the page is the surface

Current provenance is only `touched(session_id, path)` — the bare fact that a
session touched a path. That is the same depth as deja's file-level blame. A
pretty page over that data would be shallow. The differentiation is the
evidence chain, and most of it is not stored yet.

Feasibility is good: at parse time the Claude adapter already sees each edit
tool call's `name` (Edit / Write / MultiEdit / ...) and `input`
(`file_path`, `old_string` / `new_string` / `content`) — see
`src/adapters/claude_code.rs` `edited_path()` and the `tool_use` branch. Today
that payload is flattened into message `text`; the work is to extract it into a
structured, queryable evidence record.

## Data model (the real build)

Per `(session, file)` edit event, extracted at parse time from the tool-call
payload the adapters already see:

- `kind` — edited (Edit/Write/MultiEdit/apply_patch) vs mentioned (path appears
  but no edit call). The review's "edited" / "mentioned" / "probably
  responsible" distinction.
- `evidence` — the concrete change: for Edit, the `old_string`→`new_string`
  snippet; for Write, that it created/replaced the file; a bounded snippet, not
  the whole blob.
- `ts` — when in the session it happened.

Git correlation stays a query-time join (as `blame` already does): the commit
that last changed the file ↔ session timing, surfaced with an explicit
confidence tier, never asserted as authorship.

New table sketch (additive, derived — safe to drop/rebuild like the other
derived tables): `edits(session_id, path, kind, ts, snippet)`, indexed by path.
`touched` stays as the cheap path index; `edits` is the evidence layer.

## The page

`/file/<path>` in the web UI — assembled from the evidence layer:

- The sessions that edited or discussed the file, newest first, each with its
  concrete edit evidence (the snippet / kind), summary, and tool.
- Relation label per session: edited vs mentioned vs probably-responsible
  (confidence), never a bare authorship claim.
- Git commit correlation where available (the `blame` join), clearly marked
  best-effort.
- Archive/raw status: is the original still on disk, or only the distilled
  archive copy? (Ties directly into the durability work below.)
- A focused, file-centered local graph as a *component*: 1–2 hops (this file →
  its sessions → sibling files/sessions), filtered, deterministic layout,
  common-hub files hidden. A drill-down, not the entry point.

Allowed to use a bundled local graph library — bundling it does not violate the
"no network calls in the codebase" promise, and hand-writing layout physics is
not where solo time should go.

## Sequencing (each slice independently testable, TDD)

1. Evidence extraction: `edits` table + parse-time population from the tool-call
   payload. Tests over fixture sessions asserting kind + snippet per (session,
   file).
2. Query: `evidence_for(path)` assembling the per-file chain (edits + relation +
   git correlation + archive status). Tests over a seeded index.
3. Web: `/api/file?path=` returning the chain as JSON; `/file/<path>` page
   rendering it. Reuses the existing tiny_http + single-file webui.html spine.
4. Focused graph component inside the page (only after 2–3 real uses show a need
   to explore neighbors — per the review, do not build graph on spec alone).

## Parallel tracks (independent of the flagship, review-endorsed)

**Do-now parity (table stakes that reinforce the niche):**
- Index-time secret redaction (API keys / JWTs / private keys). Prerequisite for
  any export/share and mandatory for a tool that retains data longer than the
  originating tools.
- `doctor` — self-diagnosis: store parse state, path discovery, SQLite health,
  filesystem permissions, MCP wiring per agent, archive integrity. A solo
  maintainer's support-load reducer across ~12 drifting formats.
- One-command MCP install across all detected agents (today's auto-recall path
  is Claude-specific).

**Archive durability (the biggest real risk — larger than the star gap):**
- Resolve the doc contradiction: the index cannot be both "the only remaining
  copy after the tool deletes the original" and a "disposable cache." Once
  permanence is a pillar, the archive is primary storage.
- Harden accordingly: lossless tested schema migrations, atomic writes + crash
  recovery, integrity checksums, stable IDs, explicit retention/deletion,
  portable export/import.
- Claim guardrail: do not say "never lose your history" until durability matches
  the claim, and state precisely what the distilled archive preserves vs drops
  (it is not byte-exact; blame is best-effort, not an audit trail).

## Out of scope (cut/demoted by the review)

- Global force-directed graph as the front door.
- Feature-count parity for its own sake; sanitized `share` absent real demand.
- In-binary self-update (breaks the network-free core); semantic search
  (deprioritized — no demonstrated retrieval-quality problem).
- Framing the roadmap as "stealing deja" or "deja structurally cannot follow."

## Highest-EV action (owner: user, not buildable here)

Watch ~10 heavy multi-agent developers use `trace` on a real file in their own
data they do not understand. Do not expand the graph unless several independently
reach for neighbor exploration. Build slices 1–3 (evidence chain + page) and the
parallel tracks meanwhile — all survive regardless of that outcome.
