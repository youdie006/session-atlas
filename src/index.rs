use crate::adapters::{self, Adapter};
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

/// The index lives outside the session stores and never touches them.
/// Default: ~/.local/share/sessionwiki/index.db (platform equivalent).
pub fn db_path() -> Result<PathBuf> {
    let dir = std::env::var_os("SESSIONWIKI_DATA")
        .map(PathBuf::from)
        .or_else(|| dirs::data_dir().map(|d| d.join("sessionwiki")))
        .context("cannot determine a data directory")?;
    // One-time migration from earlier names, newest first. This carries over
    // the existing index AND the curated tags/notes/summaries, which are not
    // rebuildable. The project was session-atlas, then sessiondex.
    if !dir.exists() {
        if let Some(parent) = dirs::data_dir() {
            for old_name in ["sessiondex", "session-atlas"] {
                let old = parent.join(old_name);
                if old.exists() {
                    // A failed rename must not pass silently: the user would
                    // get a fresh empty index while their curation sits
                    // stranded in the old directory. Re-check after failure,
                    // though - a concurrent first run (hook + CLI) may have
                    // migrated it already, which is success, not failure.
                    if let Err(e) = std::fs::rename(&old, &dir) {
                        if !dir.exists() && old.exists() {
                            eprintln!(
                                "warning: could not migrate {} -> {} ({e}); \
                                 starting a fresh index. Move it manually to \
                                 keep your tags, notes, and archive.",
                                old.display(),
                                dir.display()
                            );
                        }
                    }
                    break;
                }
            }
        }
    }
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("index.db"))
}

/// `user_version` versions the disposable cache: a mismatch drops and rebuilds
/// the derived tables (files/messages/msgs/touched) instead of migrating. The
/// durable tables (summaries, tags, notes, archive) hold what cannot be
/// re-derived - LLM output, user curation, and sessions whose originals the tool
/// deleted - and are versioned separately by `meta.durable_version` via forward,
/// additive-only migrations that never drop, so they survive every upgrade. The
/// two counters are independent and must never gate each other.
const SCHEMA_VERSION: i64 = 5;

/// Version of the durable schema this binary ships. The durable CREATE
/// statements are frozen at this shape; every later durable change is a
/// migration in DURABLE_MIGRATIONS. Independent of SCHEMA_VERSION (the cache).
const BASELINE_DURABLE_VERSION: i64 = 1;

/// Read meta.durable_version, seeding the baseline when absent. Absence covers
/// both a fresh DB (durables just created at baseline) and an existing
/// pre-feature index (durables already at baseline) - both correctly start at
/// BASELINE. INSERT OR IGNORE is safe under a concurrent first-open.
fn read_or_init_durable_version(conn: &Connection) -> Result<i64> {
    conn.execute(
        "INSERT OR IGNORE INTO meta(key, value) VALUES ('durable_version', ?1)",
        params![BASELINE_DURABLE_VERSION.to_string()],
    )?;
    let v: String = conn.query_row(
        "SELECT value FROM meta WHERE key = 'durable_version'",
        [],
        |r| r.get(0),
    )?;
    Ok(v.parse().unwrap_or(BASELINE_DURABLE_VERSION))
}

/// One migration step: plain DDL, or Rust code for transformations SQL cannot
/// express (e.g. unicode normalization). Both run inside the same gated
/// transaction and must stay additive-and-repair-only - never drop durable data.
enum MigrationStep {
    // The natural shape for most future migrations (ALTER/CREATE); only data
    // repairs need `Fix`. Allowed while no registered migration uses it.
    #[allow(dead_code)]
    Sql(&'static str),
    Fix(fn(&Connection) -> Result<()>),
}

struct Migration {
    version: i64,
    step: MigrationStep,
}

/// Forward-only, additive-only durable migrations applied in order. ALTER ADD
/// COLUMN / CREATE / data repair only - never DROP/RENAME a durable column or
/// table.
const DURABLE_MIGRATIONS: &[Migration] = &[
    // v2: tags written by pre-0.17 binaries were lowercased but not
    // NFC-normalized, so a decomposed-form tag (macOS IME) is unreachable by
    // the normalized lookups. Re-normalize stored rows once.
    Migration {
        version: 2,
        step: MigrationStep::Fix(normalize_stored_tags),
    },
];

/// Durable migration v2: converge every stored tag on `norm_tag` form. An NFC
/// twin that already exists absorbs the row (INSERT OR IGNORE + DELETE).
fn normalize_stored_tags(conn: &Connection) -> Result<()> {
    let rows: Vec<(String, String)> = conn
        .prepare("SELECT session_id, tag FROM tags")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (sid, tag) in rows {
        let norm = norm_tag(&tag);
        if norm != tag {
            conn.execute(
                "INSERT OR IGNORE INTO tags(session_id, tag) VALUES (?1, ?2)",
                params![sid, norm],
            )?;
            conn.execute(
                "DELETE FROM tags WHERE session_id = ?1 AND tag = ?2",
                params![sid, tag],
            )?;
        }
    }
    Ok(())
}

/// Apply migrations whose version exceeds the stored durable_version, in one
/// IMMEDIATE transaction (re-reading the version inside it so a concurrent
/// process that already migrated makes this a no-op). DDL + version bump are
/// atomic. Version-gating is the only thing that makes re-running safe, since
/// SQLite ALTER ADD COLUMN is not idempotent.
fn run_durable_migrations(conn: &Connection, migrations: &[Migration]) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let outcome = (|| -> Result<()> {
        let current: i64 = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'durable_version'",
                [],
                |r| r.get::<_, String>(0),
            )?
            .parse()
            .unwrap_or(BASELINE_DURABLE_VERSION);
        for m in migrations.iter().filter(|m| m.version > current) {
            match m.step {
                MigrationStep::Sql(sql) => conn.execute_batch(sql)?,
                MigrationStep::Fix(f) => f(conn)?,
            }
            conn.execute(
                "UPDATE meta SET value = ?1 WHERE key = 'durable_version'",
                params![m.version.to_string()],
            )?;
        }
        Ok(())
    })();
    match outcome {
        Ok(()) => conn.execute_batch("COMMIT")?,
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(e);
        }
    }
    Ok(())
}

#[cfg(test)]
mod migration_tests {
    use super::*;

    fn mem() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE notes(session_id TEXT PRIMARY KEY, note TEXT NOT NULL, updated TEXT NOT NULL);
             INSERT INTO meta VALUES('durable_version','1');
             INSERT INTO notes VALUES('s1','keep me','t');",
        )
        .unwrap();
        c
    }

    #[test]
    fn runner_applies_gated_and_is_idempotent() {
        let c = mem();
        let migs = [Migration {
            version: 2,
            step: MigrationStep::Sql("ALTER TABLE notes ADD COLUMN pinned INTEGER"),
        }];
        run_durable_migrations(&c, &migs).unwrap();
        let cols: Vec<String> = c
            .prepare("SELECT name FROM pragma_table_info('notes')")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert!(cols.contains(&"pinned".to_string()), "column added");
        let v: String = c
            .query_row(
                "SELECT value FROM meta WHERE key='durable_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(v, "2", "version advanced");
        let note: String = c
            .query_row("SELECT note FROM notes WHERE session_id='s1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(note, "keep me", "existing durable row preserved");
        // re-run is a clean no-op (ADD COLUMN would otherwise error duplicate column)
        run_durable_migrations(&c, &migs).unwrap();
    }
}

/// A read-only handle for serving processes (the MCP server): it can never
/// create the schema, migrate, VACUUM, or write durable data. Fails if no
/// index exists yet (unlike `open`, which creates one).
pub fn open_readonly() -> Result<Connection> {
    use rusqlite::OpenFlags;
    let conn = Connection::open_with_flags(
        db_path()?,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    Ok(conn)
}

pub fn open() -> Result<Connection> {
    let conn = Connection::open(db_path()?)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    let bumped = version != SCHEMA_VERSION;
    if bumped {
        // Drop only the derived cache. The durable tables (summaries, tags,
        // notes, archive) are never dropped: rebuilding the index is cheap,
        // re-running an LLM or recovering a session the tool already deleted is
        // not. Archived sessions are rehydrated into the cache below.
        conn.execute_batch(
            "DROP TABLE IF EXISTS msgs;
             DROP TABLE IF EXISTS messages;
             DROP TABLE IF EXISTS touched;
             DROP TABLE IF EXISTS files;",
        )?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS files(
            path       TEXT PRIMARY KEY,
            mtime      INTEGER NOT NULL,
            size       INTEGER NOT NULL,
            session_id TEXT NOT NULL,
            tool       TEXT NOT NULL,
            project    TEXT NOT NULL DEFAULT '',
            title      TEXT NOT NULL DEFAULT '',
            started    TEXT,
            ended      TEXT,
            msg_count  INTEGER NOT NULL DEFAULT 0,
            kind       TEXT NOT NULL DEFAULT 'main',
            -- Set when the tool deleted the original session file but we kept
            -- the indexed copy (archive mode). NULL for live sessions.
            archived_at TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_files_session ON files(session_id);
        -- Plain rows + external-content FTS. Deleting a session is an
        -- indexed lookup here; with session_id stored UNINDEXED inside the
        -- FTS table it was a full scan per file, which made re-index runs
        -- quadratic in practice.
        CREATE TABLE IF NOT EXISTS messages(
            id         INTEGER PRIMARY KEY,
            session_id TEXT NOT NULL,
            role       TEXT NOT NULL,
            text       TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id);
        CREATE VIRTUAL TABLE IF NOT EXISTS msgs USING fts5(
            text,
            content='messages',
            content_rowid='id',
            tokenize='trigram'
        );
        CREATE TABLE IF NOT EXISTS summaries(
            session_id TEXT PRIMARY KEY,
            summary    TEXT NOT NULL,
            created    TEXT NOT NULL
        );
        -- Curation layer (the editable 'wiki' part). Like summaries, these
        -- are user-authored and survive index rebuilds: only files/messages/
        -- msgs are dropped on a schema bump, never these.
        CREATE TABLE IF NOT EXISTS tags(
            session_id TEXT NOT NULL,
            tag        TEXT NOT NULL,
            PRIMARY KEY (session_id, tag)
        );
        CREATE INDEX IF NOT EXISTS idx_tags_tag ON tags(tag);
        CREATE TABLE IF NOT EXISTS notes(
            session_id TEXT PRIMARY KEY,
            note       TEXT NOT NULL,
            updated    TEXT NOT NULL
        );
        -- Archive (durable, never dropped on a schema bump). When the tool
        -- deletes a session's original file, we keep a self-contained copy
        -- here - the distilled transcript and provenance plus the metadata
        -- needed to reconstruct the files row. This is the only table that is
        -- not re-derivable from disk, so on a schema bump the cache tables are
        -- rehydrated from it. Live sessions are NOT stored here.
        CREATE TABLE IF NOT EXISTS archive(
            session_id  TEXT PRIMARY KEY,
            path        TEXT NOT NULL,
            mtime       INTEGER NOT NULL,
            size        INTEGER NOT NULL,
            tool        TEXT NOT NULL,
            project     TEXT NOT NULL DEFAULT '',
            title       TEXT NOT NULL DEFAULT '',
            started     TEXT,
            ended       TEXT,
            msg_count   INTEGER NOT NULL DEFAULT 0,
            kind        TEXT NOT NULL DEFAULT 'main',
            transcript  TEXT NOT NULL,  -- JSON [[role,text],...] in order
            touched     TEXT NOT NULL,  -- JSON [path,...]
            archived_at TEXT NOT NULL
        );
        -- Provenance: which files each session edited or created, from its
        -- tool calls. Rebuilt from the sessions on sync, so it is dropped on a
        -- schema bump like messages - not curated. The path index powers
        -- `trace` (sessions for a file) and shared-file relatedness.
        CREATE TABLE IF NOT EXISTS touched(
            session_id TEXT NOT NULL,
            path       TEXT NOT NULL,
            PRIMARY KEY (session_id, path)
        );
        CREATE INDEX IF NOT EXISTS idx_touched_path ON touched(path);
        -- Durable key/value scratchpad. Holds `durable_version` (the durable-
        -- schema version, separate from user_version). Never dropped.
        CREATE TABLE IF NOT EXISTS meta(
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );",
    )?;
    // Durable-table versioning, independent of user_version (the cache). `meta`
    // exists from the CREATE batch above.
    let durable = read_or_init_durable_version(&conn)?;
    let latest = DURABLE_MIGRATIONS
        .iter()
        .map(|m| m.version)
        .max()
        .unwrap_or(BASELINE_DURABLE_VERSION);
    if durable < latest {
        // Back up the irreplaceable durable data before the first migration runs.
        let bak = db_path()?.with_extension(format!("db.bak-v{durable}"));
        let _ = std::fs::remove_file(&bak);
        conn.execute("VACUUM INTO ?1", params![bak.to_string_lossy()])?;
        run_durable_migrations(&conn, DURABLE_MIGRATIONS)?;
    }
    // Replay archived sessions into the cache whenever any are missing from it
    // - after a schema bump (which dropped the cache) or if the cache was
    // cleared some other way. Gated on a count so a normal open does nothing.
    let arch_total: i64 = conn.query_row("SELECT count(*) FROM archive", [], |r| r.get(0))?;
    let arch_live: i64 = conn.query_row(
        "SELECT count(*) FROM files WHERE archived_at IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    if arch_total > arch_live {
        rehydrate_archive(&conn)?;
    }
    Ok(conn)
}

/// After a schema bump drops the cache tables, replay archived sessions back
/// into them from the durable `archive` table, so search, `trace`, and reading
/// keep working for sessions whose originals the tool deleted. This is what
/// makes archive survive a rebuild; without it a version bump would silently
/// lose exactly the data that cannot be re-derived from disk.
fn rehydrate_archive(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT session_id, path, tool, project, title, started, ended,
                kind, transcript, touched, archived_at FROM archive",
    )?;
    let rows: Vec<ArchiveRow> = stmt
        .query_map([], |r| {
            Ok(ArchiveRow {
                session_id: r.get(0)?,
                path: r.get(1)?,
                tool: r.get(2)?,
                project: r.get(3)?,
                title: r.get(4)?,
                started: r.get(5)?,
                ended: r.get(6)?,
                kind: r.get(7)?,
                transcript: r.get(8)?,
                touched: r.get(9)?,
                archived_at: r.get(10)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;

    for a in rows {
        // The transcript is the durable backup; if it will not deserialize,
        // skip the session rather than rehydrate an empty shell that claims to
        // have content - that would be silent data loss disguised as success.
        let msgs: Vec<(String, String)> = match serde_json::from_str(&a.transcript) {
            Ok(m) => m,
            Err(e) => {
                eprintln!(
                    "archive: skipping {} - unreadable transcript ({e})",
                    a.session_id
                );
                continue;
            }
        };
        let paths: Vec<String> = serde_json::from_str(&a.touched).unwrap_or_else(|e| {
            eprintln!("archive: {} has unreadable provenance ({e})", a.session_id);
            Vec::new()
        });

        // Idempotent: clear any existing cache rows for this session first, so
        // re-running rehydrate never duplicates messages/FTS rows.
        delete_session_msgs(conn, &a.session_id)?;
        conn.execute(
            "DELETE FROM touched WHERE session_id = ?1",
            params![a.session_id],
        )?;
        conn.execute(
            "DELETE FROM files WHERE session_id = ?1",
            params![a.session_id],
        )?;

        // mtime/size are forced to 0 so that if this file ever reappears on
        // disk, the next sync always sees a mismatch and re-parses it, clearing
        // archived_at. msg_count comes from the actual transcript, never the
        // stored count, so the displayed count can never outrun the content.
        conn.execute(
            "INSERT INTO files
             (path, mtime, size, session_id, tool, project, title, started, ended,
              msg_count, kind, archived_at)
             VALUES (?1,0,0,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                a.path,
                a.session_id,
                a.tool,
                crate::util::nfc(&a.project),
                a.title,
                a.started,
                a.ended,
                msgs.len() as i64,
                a.kind,
                a.archived_at,
            ],
        )?;
        {
            let mut ins_row = conn
                .prepare_cached("INSERT INTO messages(session_id, role, text) VALUES (?1,?2,?3)")?;
            let mut ins_fts =
                conn.prepare_cached("INSERT INTO msgs(rowid, text) VALUES (?1,?2)")?;
            for (role, text) in &msgs {
                // Re-normalize on rehydrate: pre-fix archives hold raw/NFD JSON,
                // so this is where archived Korean sessions become NFC again.
                let text = crate::util::nfc(text);
                ins_row.execute(params![a.session_id, role, text])?;
                ins_fts.execute(params![conn.last_insert_rowid(), text])?;
            }
        }
        let mut ins_touched =
            conn.prepare_cached("INSERT OR IGNORE INTO touched(session_id, path) VALUES (?1,?2)")?;
        for p in &paths {
            ins_touched.execute(params![a.session_id, crate::util::nfc(p)])?;
        }
    }
    Ok(())
}

struct ArchiveRow {
    session_id: String,
    path: String,
    tool: String,
    project: String,
    title: String,
    started: Option<String>,
    ended: Option<String>,
    kind: String,
    transcript: String,
    touched: String,
    archived_at: String,
}

/// Bring the index up to date with what is on disk. Only files whose
/// (mtime, size) changed since the last run are re-parsed.
/// Insert one parsed session into the cache tables (files, messages, msgs,
/// touched) and tag it if an oh-my-* harness drove it. `key` is the stored
/// path/identity, `mtime` the change-token, `size` the byte size (0 for
/// shared-store sessions). Shared by the file-per-session and shared-store paths.
fn index_one(
    tx: &rusqlite::Transaction,
    session: &crate::model::Session,
    key: &str,
    mtime: i64,
    size: i64,
) -> Result<()> {
    delete_session_msgs(tx, &session.id)?;
    tx.execute(
        "DELETE FROM touched WHERE session_id = ?1",
        params![session.id],
    )?;
    // A path that was archived (the tool deleted it, now it is back) is live
    // again: this INSERT clears archived_at, and the durable copy is dropped.
    tx.execute(
        "DELETE FROM archive WHERE session_id = ?1",
        params![session.id],
    )?;
    tx.execute(
        "INSERT OR REPLACE INTO files
         (path, mtime, size, session_id, tool, project, title, started, ended, msg_count, kind)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            key,
            mtime,
            size,
            session.id,
            session.tool,
            crate::util::nfc(&session.project),
            session.title,
            session.started.map(|t| t.to_rfc3339()),
            session.ended.map(|t| t.to_rfc3339()),
            session.messages.len() as i64,
            if session.subagent { "sub" } else { "main" },
        ],
    )?;
    // Contract: messages are inserted in transcript order within one
    // transaction, so the autoincrement messages.id is a monotonic proxy for
    // order (preview + the web transcript rely on it). Keep this sequential.
    {
        let mut ins_row =
            tx.prepare_cached("INSERT INTO messages(session_id, role, text) VALUES (?1,?2,?3)")?;
        let mut ins_fts = tx.prepare_cached("INSERT INTO msgs(rowid, text) VALUES (?1,?2)")?;
        for m in &session.messages {
            // Normalize once and reuse for the plain row and the external-content
            // FTS row: they MUST be byte-identical or delete_session_msgs corrupts.
            let text = crate::util::nfc(&m.text);
            ins_row.execute(params![session.id, m.role.label(), text])?;
            ins_fts.execute(params![tx.last_insert_rowid(), text])?;
        }
        let mut ins_touched =
            tx.prepare_cached("INSERT OR IGNORE INTO touched(session_id, path) VALUES (?1,?2)")?;
        for p in &session.touched {
            ins_touched.execute(params![session.id, crate::util::nfc(p)])?;
        }
    }
    // Tag sessions an oh-my-* harness drove (it wraps Claude Code / Codex /
    // OpenCode) so they are filterable; recomputed on every reindex.
    if matches!(session.tool, "claude-code" | "codex" | "opencode") {
        if let Some(h) = crate::adapters::harness::detect(&session.project) {
            add_tag(tx, &session.id, h)?;
        }
    }
    Ok(())
}

/// The end-of-adapter sync line. Honest about failures: "indexed 12/14
/// (2 failed to parse)" rather than pretending everything landed.
fn report_indexed(tool: &str, total: usize, failed: usize) {
    if failed > 0 {
        eprintln!(
            "\r[{tool}] indexed {}/{total} ({failed} failed to parse)    ",
            total - failed
        );
    } else {
        eprintln!("\r[{tool}] indexed {total}/{total}    ");
    }
}

pub fn sync(conn: &mut Connection, only_tool: Option<&str>) -> Result<()> {
    let adapters: Vec<Box<dyn Adapter>> = match only_tool {
        Some(t) => adapters::by_name(t).into_iter().collect(),
        None => adapters::all(),
    };

    let mut known: HashMap<String, (i64, i64)> = HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT path, mtime, size FROM files")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                (r.get::<_, i64>(1)?, r.get::<_, i64>(2)?),
            ))
        })?;
        for row in rows {
            let (path, ms) = row?;
            known.insert(path, ms);
        }
    }

    let mut archived_total = 0usize;
    for adapter in &adapters {
        // `store_present` tells "the tool pruned some sessions" (root exists,
        // those gone) apart from "the whole store vanished" (uninstall,
        // unmounted) - we must not mass-archive on the latter.
        let tool = adapter.name();
        let store_present = adapter.root().is_some_and(|r| r.exists());

        // Shared store (e.g. OpenCode's SQLite db): enumerate sessions by key +
        // change-token and re-parse only the changed ones, bypassing the
        // file-per-session path. The token is stored in the `mtime` column
        // (size 0), so the same change comparison works.
        if let Some(store) = adapter.store() {
            let mut seen: Vec<String> = Vec::with_capacity(store.keys.len());
            let mut pending: Vec<String> = Vec::new();
            for (key, token) in &store.keys {
                if known.get(key) != Some(&(*token, 0)) {
                    pending.push(key.clone());
                }
                seen.push(key.clone());
            }
            // Reconcile deletions only when the whole store was read this run.
            // If a backing db was present but unreadable (locked, half-written),
            // `seen` is partial - pruning off it would archive the whole corpus
            // on a transient hiccup, so skip reconciliation until a clean read.
            if !store.had_error {
                archived_total += archive_or_prune(conn, tool, &seen, store_present)?;
            }

            if !pending.is_empty() {
                let token_of: HashMap<&str, i64> =
                    store.keys.iter().map(|(k, t)| (k.as_str(), *t)).collect();
                let total = pending.len();
                let mut failed = 0usize;
                let tx = conn.transaction()?;
                for (i, key) in pending.iter().enumerate() {
                    eprint!("\r[{tool}] indexing {}/{total}", i + 1);
                    std::io::stderr().flush().ok();
                    // A failed parse is warned, not silently dropped: the user
                    // must know a session is missing from the corpus.
                    let session = match adapter.parse_key(key) {
                        Ok(s) => s,
                        Err(e) => {
                            failed += 1;
                            eprintln!("\r[{tool}] failed to parse {key}: {e:#}");
                            continue;
                        }
                    };
                    let token = token_of.get(key.as_str()).copied().unwrap_or(0);
                    index_one(&tx, &session, key, token, 0)?;
                }
                tx.commit()?;
                report_indexed(tool, total, failed);
            }
            continue;
        }

        let discovered = adapter.discover();
        let mut seen: Vec<String> = Vec::with_capacity(discovered.files.len());
        let mut pending: Vec<(PathBuf, i64, i64)> = Vec::new();

        for f in discovered.files {
            let meta = match f.metadata() {
                Ok(m) => m,
                Err(e) => {
                    // The file was just discovered, so it exists: a failed
                    // stat must keep it in `seen` (dropping it would let
                    // reconciliation archive a live session) and be warned,
                    // not swallowed.
                    eprintln!("\r[{tool}] failed to stat {}: {e}", f.display());
                    seen.push(f.to_string_lossy().into_owned());
                    continue;
                }
            };
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let size = meta.len() as i64;
            let key = f.to_string_lossy().into_owned();
            if known.get(&key) != Some(&(mtime, size)) {
                pending.push((f, mtime, size));
            }
            seen.push(key);
        }

        // Same guard the shared-store path has: a partial walk (an unreadable
        // directory) means `seen` is incomplete - reconciling deletions off it
        // would archive live sessions, so wait for a clean walk.
        if discovered.had_error {
            eprintln!(
                "[{tool}] some session directories could not be read; \
                 skipping deletion reconciliation this run"
            );
        } else {
            archived_total += archive_or_prune(conn, tool, &seen, store_present)?;
        }

        if pending.is_empty() {
            continue;
        }
        let total = pending.len();
        let mut done = 0usize;
        let mut failed = 0usize;
        let tx = conn.transaction()?;
        for (path, mtime, size) in pending {
            done += 1;
            eprint!("\r[{tool}] indexing {done}/{total}");
            std::io::stderr().flush().ok();

            // A failed parse is warned, not silently dropped: the user must
            // know a session is missing from the corpus.
            let session = match adapter.parse(&path) {
                Ok(s) => s,
                Err(e) => {
                    failed += 1;
                    eprintln!("\r[{tool}] failed to parse {}: {e:#}", path.display());
                    continue;
                }
            };
            let key = path.to_string_lossy();
            index_one(&tx, &session, &key, mtime, size)?;
        }
        tx.commit()?;
        report_indexed(tool, total, failed);
    }

    // The one passive signal that archive is earning its keep: how many
    // sessions we kept this run that the tool deleted, and the running total.
    if archived_total > 0 {
        let kept: i64 = conn.query_row(
            "SELECT count(*) FROM files WHERE archived_at IS NOT NULL",
            [],
            |r| r.get(0),
        )?;
        eprintln!(
            "archived {archived_total} session(s) the tool removed ({kept} kept that your tools have deleted)"
        );
    }
    Ok(())
}

/// External-content FTS5 requires handing back the old rows on delete.
fn delete_session_msgs(conn: &Connection, session_id: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO msgs(msgs, rowid, text)
         SELECT 'delete', id, text FROM messages WHERE session_id = ?1",
        params![session_id],
    )?;
    conn.execute(
        "DELETE FROM messages WHERE session_id = ?1",
        params![session_id],
    )?;
    Ok(())
}

/// Reconcile the index with a tool's store after discovery. Sessions whose
/// original file disappeared are **archived** (kept in the durable `archive`
/// table and flagged in `files`, with messages/touched left in place so
/// search and trace keep working) instead of deleted - unless
/// `SESSIONWIKI_NO_ARCHIVE` is set or the session has no indexed content, in
/// which case they are pruned as before. Returns how many were newly archived.
///
/// Guard: if the store root is gone (uninstalled, unmounted), do not touch its
/// sessions - that is "the whole store vanished", not "the tool pruned some".
/// An existing-but-empty store is a legitimate prune-everything and proceeds.
fn archive_or_prune(
    conn: &Connection,
    tool: &str,
    seen: &[String],
    store_present: bool,
) -> Result<usize> {
    let no_archive = std::env::var_os("SESSIONWIKI_NO_ARCHIVE").is_some();
    let seen_set: std::collections::HashSet<&str> = seen.iter().map(String::as_str).collect();

    let mut stmt =
        conn.prepare("SELECT path, session_id FROM files WHERE tool = ?1 AND archived_at IS NULL")?;
    let live: Vec<(String, String)> = stmt
        .query_map(params![tool], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let gone: Vec<(String, String)> = live
        .into_iter()
        .filter(|(p, _)| !seen_set.contains(p.as_str()))
        .collect();
    if gone.is_empty() {
        return Ok(0);
    }
    if !store_present {
        eprintln!(
            "[{tool}] store not found - skipping ({} indexed session(s) left untouched, not archived)",
            gone.len()
        );
        return Ok(0);
    }
    // The root exists but discovery returned nothing while we still had live
    // sessions: could be a legitimate prune-everything, but also a transient
    // read failure (permissions, a half-mounted network FS). Archiving keeps
    // the data (reversible on the next good sync), but say so loudly.
    if seen.is_empty() {
        eprintln!(
            "[{tool}] no sessions found on disk but {} were indexed - archiving them; \
             if the store is just unreadable right now, they will un-archive on the next sync",
            gone.len()
        );
    }

    let mut archived = 0usize;
    for (path, sid) in gone {
        if no_archive {
            conn.execute("DELETE FROM files WHERE path = ?1", params![path])?;
            delete_session_msgs(conn, &sid)?;
            conn.execute("DELETE FROM touched WHERE session_id = ?1", params![sid])?;
        } else {
            archive_session(conn, &path, &sid)?;
            archived += 1;
        }
    }
    Ok(archived)
}

/// Copy a session whose original file is gone into the durable `archive` table
/// and flag its `files` row. The messages/msgs/touched rows are left in place
/// so search and `trace` keep working; the archive copy is the rebuild-survival
/// backup (replayed by `rehydrate_archive` after a schema bump).
fn archive_session(conn: &Connection, path: &str, sid: &str) -> Result<()> {
    let mut s =
        conn.prepare("SELECT role, text FROM messages WHERE session_id = ?1 ORDER BY id")?;
    let transcript: Vec<(String, String)> = s
        .query_map(params![sid], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    drop(s);
    let mut s = conn.prepare("SELECT path FROM touched WHERE session_id = ?1 ORDER BY rowid")?;
    let touched: Vec<String> = s
        .query_map(params![sid], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(s);
    let transcript_json = serde_json::to_string(&transcript)?;
    let touched_json = serde_json::to_string(&touched)?;
    conn.execute(
        "INSERT OR REPLACE INTO archive
         (session_id, path, mtime, size, tool, project, title, started, ended,
          msg_count, kind, transcript, touched, archived_at)
         SELECT session_id, path, mtime, size, tool, project, title, started, ended,
                msg_count, kind, ?2, ?3, datetime('now')
         FROM files WHERE path = ?1",
        params![path, transcript_json, touched_json],
    )?;
    conn.execute(
        "UPDATE files SET archived_at = datetime('now') WHERE path = ?1",
        params![path],
    )?;
    Ok(())
}

/// Serializes to the agent-facing JSON contract: snake_case keys matching the
/// web API (`id`, `msgs`, tags as an array), and the absolute `path` is skipped
/// so it never leaks into agent-consumed output.
#[derive(Serialize)]
pub struct SessionRow {
    #[serde(rename = "id")]
    pub session_id: String,
    pub tool: String,
    #[serde(skip)]
    pub path: String,
    pub project: String,
    pub title: String,
    pub started: Option<String>,
    #[serde(rename = "msgs")]
    pub msg_count: i64,
    pub kind: String,
    /// Tail of the conversation (last assistant message), so a list can show
    /// how the session ended without opening it.
    pub preview: Option<String>,
    /// Cached LLM synopsis, if `summarize` has been run for this session.
    pub summary: Option<String>,
    /// Comma-joined user tags, if any. Serialized as a string array (or null).
    #[serde(serialize_with = "ser_tags")]
    pub tags: Option<String>,
    /// True if the tool deleted the original and we kept the indexed copy.
    pub archived: bool,
    /// The swapdex account profile active when this session started, when a
    /// swapdex switch timeline exists on the machine. Null otherwise - a
    /// missing badge, never a guess.
    pub account: Option<String>,
}

/// Tags are stored comma-joined but the JSON contract is an array (matching the
/// web API). Null when there are no tags.
fn ser_tags<S: serde::Serializer>(tags: &Option<String>, s: S) -> Result<S::Ok, S::Error> {
    match tags {
        Some(t) => s.collect_seq(t.split(',')),
        None => s.serialize_none(),
    }
}

/// Correlated subquery for the preview column; messages.id preserves
/// insertion order, which is message order.
const PREVIEW_SQL: &str = "(SELECT substr(m2.text, 1, 280) FROM messages m2
    WHERE m2.session_id = f.session_id AND m2.role = 'assistant'
    ORDER BY m2.id DESC LIMIT 1)";

const SUMMARY_SQL: &str = "(SELECT s.summary FROM summaries s WHERE s.session_id = f.session_id)";

const TAGS_SQL: &str =
    "(SELECT group_concat(t.tag, ',') FROM tags t WHERE t.session_id = f.session_id)";

pub fn recent(
    conn: &Connection,
    limit: usize,
    tool: Option<&str>,
    project: Option<&str>,
    tag: Option<&str>,
    include_subagents: bool,
) -> Result<Vec<SessionRow>> {
    let mut sql = format!(
        "SELECT session_id, tool, path, project, title, started, msg_count, kind, {PREVIEW_SQL}, {SUMMARY_SQL}, {TAGS_SQL}, (archived_at IS NOT NULL)
         FROM files f WHERE 1=1",
    );
    let mut args: Vec<String> = Vec::new();
    // A tag filter is an explicit ask for *those* sessions; don't hide subagent
    // hits behind the main-only default (the tag cloud counts every kind, so a
    // sub-only tag would otherwise show in the cloud but return nothing here).
    if !include_subagents && tag.is_none() {
        sql.push_str(" AND kind = 'main'");
    }
    if let Some(t) = tool {
        sql.push_str(" AND tool = ?");
        args.push(t.to_string());
    }
    if let Some(p) = project {
        sql.push_str(" AND project LIKE ?");
        args.push(format!("%{}%", crate::util::nfc(p)));
    }
    if let Some(t) = tag {
        sql.push_str(
            " AND EXISTS (SELECT 1 FROM tags g WHERE g.session_id = f.session_id AND g.tag = ?)",
        );
        args.push(norm_tag(t)); // stored tags are normalized; match their form
    }
    sql.push_str(&format!(" ORDER BY started DESC LIMIT {limit}"));

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(args), |r| {
        Ok(SessionRow {
            session_id: r.get(0)?,
            tool: r.get(1)?,
            path: r.get(2)?,
            project: r.get(3)?,
            title: r.get(4)?,
            started: r.get(5)?,
            msg_count: r.get(6)?,
            kind: r.get(7)?,
            preview: r.get(8)?,
            summary: r.get(9)?,
            tags: r.get(10)?,
            archived: r.get(11)?,
            account: None,
        })
    })?;
    let mut out = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    crate::account_link::annotate(out.iter_mut());
    Ok(out)
}

/// Recent main sessions whose launch project is EXACTLY this directory (for the
/// SessionStart recall hook). Exact equality - never the substring `--project`
/// filter, which over-matches sibling/child paths. Newest first, stable.
pub fn project_brief(conn: &Connection, project: &str, limit: usize) -> Result<Vec<SessionRow>> {
    let p = crate::util::nfc(project.trim_end_matches('/'));
    let mut stmt = conn.prepare(&format!(
        "SELECT session_id, tool, path, project, title, started, msg_count, kind, {PREVIEW_SQL}, {SUMMARY_SQL}, {TAGS_SQL}, (archived_at IS NOT NULL)
         FROM files f
         WHERE f.project = ?1 AND f.kind = 'main'
         ORDER BY f.started DESC, f.session_id
         LIMIT ?2"
    ))?;
    let rows = stmt.query_map(params![p, limit as i64], |r| {
        Ok(SessionRow {
            session_id: r.get(0)?,
            tool: r.get(1)?,
            path: r.get(2)?,
            project: r.get(3)?,
            title: r.get(4)?,
            started: r.get(5)?,
            msg_count: r.get(6)?,
            kind: r.get(7)?,
            preview: r.get(8)?,
            summary: r.get(9)?,
            tags: r.get(10)?,
            archived: r.get(11)?,
            account: None,
        })
    })?;
    let mut out = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    crate::account_link::annotate(out.iter_mut());
    Ok(out)
}

pub struct Hit {
    pub row: SessionRow,
    pub role: String,
    pub snippet: String,
}

/// Full-text search, best match per session. The trigram tokenizer gives
/// substring matching, which also makes CJK text searchable.
pub fn search(
    conn: &Connection,
    query: &str,
    limit: usize,
    tool: Option<&str>,
    project: Option<&str>,
) -> Result<Vec<Hit>> {
    // A plain quoted string disables FTS5 operator parsing: users type
    // text, not query syntax.
    // Normalize the query to NFC so it lines up with the NFC-normalized indexed
    // text, then quote (the quoting is FTS5 syntax we add, not user content).
    let fts_query = format!("\"{}\"", crate::util::nfc(query).replace('"', "\"\""));

    // snippet()/rank only work in a plain FTS5 query context, not under
    // joins or GROUP BY, so match in a subquery and attach metadata outside.
    //
    // Tradeoff: we take the top 1000 message hits by rank, then group to
    // sessions. For a very common term this can miss sessions whose only hits
    // fall past rank 1000 - a deliberate choice that keeps the query fast on a
    // multi-million-message index. Narrow the query to surface the long tail.
    let mut sql = String::from(
        "SELECT f.session_id, f.tool, f.path, f.project, f.title, f.started, f.msg_count, f.kind,
                m.role, x.snip, min(x.rank) AS best, (f.archived_at IS NOT NULL)
         FROM (SELECT rowid AS mid,
                      snippet(msgs, 0, char(2), char(3), char(8230), 18) AS snip,
                      rank
               FROM msgs WHERE msgs MATCH ? ORDER BY rank LIMIT 4000) x
         JOIN messages m ON m.id = x.mid
         JOIN files f ON f.session_id = m.session_id
         WHERE 1=1",
    );
    let mut args: Vec<String> = vec![fts_query];
    if let Some(t) = tool {
        sql.push_str(" AND f.tool = ?");
        args.push(t.to_string());
    }
    if let Some(p) = project {
        sql.push_str(" AND f.project LIKE ?");
        args.push(format!("%{}%", crate::util::nfc(p)));
    }
    sql.push_str(&format!(
        " GROUP BY f.session_id ORDER BY best LIMIT {limit}"
    ));

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(args), |r| {
        Ok(Hit {
            row: SessionRow {
                session_id: r.get(0)?,
                tool: r.get(1)?,
                path: r.get(2)?,
                project: r.get(3)?,
                title: r.get(4)?,
                started: r.get(5)?,
                msg_count: r.get(6)?,
                kind: r.get(7)?,
                preview: None,
                summary: None,
                tags: None,
                archived: r.get(11)?,
                account: None,
            },
            role: r.get(8)?,
            snippet: r.get(9)?,
        })
    })?;
    let mut out = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    crate::account_link::annotate(out.iter_mut().map(|h| &mut h.row));
    Ok(out)
}

/// Substring search for queries too short for the trigram FTS index (1-2
/// chars, e.g. the Korean words 회사 / 검색). The trigram tokenizer needs >=3
/// chars, so these terms are unindexable; we fall back to a LIKE scan of
/// messages.text. Returns the same `Hit` shape as `search` so callers are
/// agnostic to which path ran.
///
/// Perf: this is a table scan, used ONLY for short queries (the >=3 path stays
/// on FTS). We cap the candidate rows scanned (SCAN_CAP) ordered newest-first
/// so a very common 2-char term cannot walk an unbounded table; the tradeoff is
/// that a session whose only match is older than the newest SCAN_CAP hits can be
/// missed. Narrow to a >=3-char term to use the exact FTS path instead. LIKE has
/// no rank, so results are ordered by recency (newest session first).
pub fn search_like(
    conn: &Connection,
    query: &str,
    limit: usize,
    tool: Option<&str>,
    project: Option<&str>,
) -> Result<Vec<Hit>> {
    const SCAN_CAP: i64 = 50_000;

    // NFC so a decomposed query (macOS Korean) matches NFC-stored text, then
    // escape LIKE metacharacters ('\' first so an escape char is literal).
    let q = crate::util::nfc(query.trim());
    let pattern = format!(
        "%{}%",
        q.replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    );

    let mut sql = String::from(
        "SELECT f.session_id, f.tool, f.path, f.project, f.title, f.started, f.msg_count, f.kind,
                x.role, x.text, (f.archived_at IS NOT NULL)
         FROM (SELECT m.session_id AS sid, m.role AS role, m.text AS text, m.id AS mid
               FROM messages m
               WHERE m.text LIKE ?1 ESCAPE '\\'
               ORDER BY m.id DESC LIMIT ?2) x
         JOIN files f ON f.session_id = x.sid
         WHERE 1=1",
    );
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(pattern), Box::new(SCAN_CAP)];
    if let Some(t) = tool {
        sql.push_str(" AND f.tool = ?");
        args.push(Box::new(t.to_string()));
    }
    if let Some(p) = project {
        sql.push_str(" AND f.project LIKE ?");
        args.push(Box::new(format!("%{}%", crate::util::nfc(p))));
    }
    // One row per session (its newest matching message), sessions newest-first.
    sql.push_str(&format!(
        " GROUP BY f.session_id ORDER BY max(x.mid) DESC LIMIT {limit}"
    ));

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        rusqlite::params_from_iter(args.iter().map(|b| b.as_ref())),
        |r| {
            let role: String = r.get(8)?;
            let text: String = r.get(9)?;
            Ok(Hit {
                row: SessionRow {
                    session_id: r.get(0)?,
                    tool: r.get(1)?,
                    path: r.get(2)?,
                    project: r.get(3)?,
                    title: r.get(4)?,
                    started: r.get(5)?,
                    msg_count: r.get(6)?,
                    kind: r.get(7)?,
                    preview: None,
                    summary: None,
                    tags: None,
                    archived: r.get(10)?,
                    account: None,
                },
                role,
                snippet: snippet_around(&text, &q),
            })
        },
    )?;
    let mut out = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    crate::account_link::annotate(out.iter_mut().map(|h| &mut h.row));
    Ok(out)
}

/// Build a snippet for a LIKE hit: a window around the first (case-insensitive,
/// NFC) match of `needle` in `text`, with the match wrapped in \u{2}..\u{3} -
/// the same delimiters snippet() emits - so the CLI's ANSI swap and the web UI
/// render LIKE hits identically to FTS hits. Matching is on chars, not bytes,
/// so multibyte CJK is never sliced mid-codepoint.
fn snippet_around(text: &str, needle: &str) -> String {
    const WINDOW: usize = 36;
    let hay_chars: Vec<char> = text.to_lowercase().chars().collect();
    let nee_chars: Vec<char> = needle.to_lowercase().chars().collect();
    let chars: Vec<char> = text.chars().collect();

    let match_at = if nee_chars.is_empty() {
        None
    } else {
        hay_chars
            .windows(nee_chars.len())
            .position(|w| w == nee_chars.as_slice())
    };
    let Some(start) = match_at else {
        return chars
            .iter()
            .take(WINDOW * 2)
            .collect::<String>()
            .replace('\n', " ");
    };
    let end = start + nee_chars.len();
    let lo = start.saturating_sub(WINDOW);
    let hi = (end + WINDOW).min(chars.len());

    let mut out = String::new();
    if lo > 0 {
        out.push('\u{2026}');
    }
    out.extend(&chars[lo..start]);
    out.push('\u{2}');
    out.extend(&chars[start..end]);
    out.push('\u{3}');
    out.extend(&chars[end..hi]);
    if hi < chars.len() {
        out.push('\u{2026}');
    }
    out.replace('\n', " ")
}

/// Resolve a (possibly abbreviated) session id to its file row.
pub fn resolve(conn: &Connection, id_prefix: &str) -> Result<Vec<SessionRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT session_id, tool, path, project, title, started, msg_count, kind, {SUMMARY_SQL}, {TAGS_SQL}, (archived_at IS NOT NULL)
         FROM files f WHERE session_id LIKE ?1 ESCAPE '\\' LIMIT 10",
    ))?;
    // Escape LIKE metacharacters ('\' first) so an id prefix like "%" or "_"
    // can't turn the prefix match into a wildcard that resolves every session.
    let pattern = format!(
        "{}%",
        id_prefix
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    );
    let rows = stmt.query_map(params![pattern], |r| {
        Ok(SessionRow {
            session_id: r.get(0)?,
            tool: r.get(1)?,
            path: r.get(2)?,
            project: r.get(3)?,
            title: r.get(4)?,
            started: r.get(5)?,
            msg_count: r.get(6)?,
            kind: r.get(7)?,
            preview: None,
            summary: r.get(8)?,
            tags: r.get(9)?,
            archived: r.get(10)?,
            account: None,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Store (or replace) the cached synopsis for a session.
pub fn set_summary(conn: &Connection, session_id: &str, summary: &str) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO summaries(session_id, summary, created)
         VALUES (?1, ?2, datetime('now'))",
        params![session_id, summary],
    )?;
    Ok(())
}

/// Most recent main sessions that have no cached summary yet.
pub fn unsummarized(
    conn: &Connection,
    limit: usize,
    tool: Option<&str>,
) -> Result<Vec<SessionRow>> {
    let mut sql = format!(
        "SELECT session_id, tool, path, project, title, started, msg_count, kind, {PREVIEW_SQL}, {SUMMARY_SQL}, {TAGS_SQL}, (archived_at IS NOT NULL)
         FROM files f
         WHERE kind = 'main' AND NOT EXISTS
               (SELECT 1 FROM summaries s WHERE s.session_id = f.session_id)",
    );
    let mut args: Vec<String> = Vec::new();
    if let Some(t) = tool {
        sql.push_str(" AND tool = ?");
        args.push(t.to_string());
    }
    sql.push_str(&format!(" ORDER BY started DESC LIMIT {limit}"));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(args), |r| {
        Ok(SessionRow {
            session_id: r.get(0)?,
            tool: r.get(1)?,
            path: r.get(2)?,
            project: r.get(3)?,
            title: r.get(4)?,
            started: r.get(5)?,
            msg_count: r.get(6)?,
            kind: r.get(7)?,
            preview: r.get(8)?,
            summary: r.get(9)?,
            tags: r.get(10)?,
            archived: r.get(11)?,
            account: None,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

// --- curation (the editable wiki layer) ---

/// One canonical form per tag: trimmed, lowercased, NFC-normalized - the same
/// normalization every other indexed string gets, so a tag typed in decomposed
/// form (macOS IME, some CJK inputs) matches on add, filter, and remove alike.
fn norm_tag(tag: &str) -> String {
    crate::util::nfc(&tag.trim().to_lowercase())
}

pub fn add_tag(conn: &Connection, session_id: &str, tag: &str) -> Result<()> {
    // Tags are joined with ',' at read time and the JSON/web contract splits on
    // it, so a comma inside a tag would corrupt the array into two elements.
    // Reject it (and the empty tag) at the input boundary.
    let tag = norm_tag(tag);
    if tag.is_empty() || tag.contains(',') {
        anyhow::bail!("a tag must be non-empty and contain no commas");
    }
    conn.execute(
        "INSERT OR IGNORE INTO tags(session_id, tag) VALUES (?1, ?2)",
        params![session_id, tag],
    )?;
    Ok(())
}

pub fn remove_tag(conn: &Connection, session_id: &str, tag: &str) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM tags WHERE session_id = ?1 AND tag = ?2",
        params![session_id, norm_tag(tag)],
    )?)
}

pub fn set_note(conn: &Connection, session_id: &str, note: &str) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO notes(session_id, note, updated)
         VALUES (?1, ?2, datetime('now'))",
        params![session_id, note],
    )?;
    Ok(())
}

pub fn note_for(conn: &Connection, session_id: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT note FROM notes WHERE session_id = ?1",
            params![session_id],
            |r| r.get(0),
        )
        .ok())
}

/// All tags in use, with how many sessions carry each.
pub fn tag_counts(conn: &Connection) -> Result<Vec<(String, i64)>> {
    let mut stmt =
        conn.prepare("SELECT tag, count(*) FROM tags GROUP BY tag ORDER BY count(*) DESC, tag")?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

// --- provenance: sessions <-> the code they produced ---

/// Files a session edited or created, in the order it first touched them.
pub fn files_for(conn: &Connection, session_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT path FROM touched WHERE session_id = ?1 ORDER BY rowid")?;
    let rows = stmt.query_map(params![session_id], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Sessions that touched a file, newest first - the reverse provenance link.
/// Matches either the exact stored path or any stored path ending in the
/// query, so a relative `src/auth.rs` finds `/home/me/proj/src/auth.rs`. The
/// matched stored path is returned alongside each session.
pub fn sessions_for_file(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<(SessionRow, String)>> {
    let q = crate::util::nfc(query.trim().trim_start_matches("./"));
    // Escape LIKE metacharacters: the query is a caller-supplied path (the MCP
    // trace_file arg included), so a bare `%` must match a literal `%`, not act
    // as a wildcard that enumerates the whole index.
    let esc = q
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    let suffix = format!("%/{esc}");
    let mut stmt = conn.prepare(&format!(
        "SELECT f.session_id, f.tool, f.path, f.project, f.title, f.started, f.msg_count, f.kind,
                {SUMMARY_SQL}, {TAGS_SQL}, t.path, (f.archived_at IS NOT NULL)
         FROM touched t JOIN files f ON f.session_id = t.session_id
         WHERE t.path = ?1 OR t.path LIKE ?2 ESCAPE '\\'
         GROUP BY f.session_id
         ORDER BY f.started DESC LIMIT ?3"
    ))?;
    let rows = stmt.query_map(params![q, suffix, limit as i64], |r| {
        Ok((
            SessionRow {
                session_id: r.get(0)?,
                tool: r.get(1)?,
                path: r.get(2)?,
                project: r.get(3)?,
                title: r.get(4)?,
                started: r.get(5)?,
                msg_count: r.get(6)?,
                kind: r.get(7)?,
                preview: None,
                summary: r.get(8)?,
                tags: r.get(9)?,
                archived: r.get(11)?,
                account: None,
            },
            r.get::<_, String>(10)?,
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Like `sessions_for_file` but returns the start/end epoch window and project
/// that blame's commit->session attribution needs. Matches both an exact stored
/// path and any stored path ending in the query suffix, so a repo-relative query
/// (e.g. `src/auth.rs`) catches Claude Code's absolute touched paths and Codex's
/// relative ones alike (NFC-normalized).
pub fn sessions_touching(
    conn: &Connection,
    query: &str,
) -> Result<Vec<crate::blame::TouchingSession>> {
    let q = crate::util::nfc(query.trim().trim_start_matches("./"));
    let suffix = format!("%/{q}");
    let mut stmt = conn.prepare(
        "SELECT f.session_id, f.tool, f.title, f.project, f.started, f.ended, (f.archived_at IS NOT NULL)
         FROM touched t JOIN files f ON f.session_id = t.session_id
         WHERE t.path = ?1 OR t.path LIKE ?2
         GROUP BY f.session_id",
    )?;
    let rows = stmt.query_map(params![q, suffix], |r| {
        let started: Option<String> = r.get(4)?;
        let ended: Option<String> = r.get(5)?;
        Ok(crate::blame::TouchingSession {
            session_id: r.get(0)?,
            tool: r.get(1)?,
            title: r.get(2)?,
            project: r.get(3)?,
            started: started.as_deref().and_then(to_epoch),
            ended: ended.as_deref().and_then(to_epoch),
            archived: r.get(6)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Parse a stored timestamp (RFC3339) to epoch seconds; None if unparseable.
fn to_epoch(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.timestamp())
}

// --- archive: serving and forgetting sessions whose originals are gone ---

/// Reconstruct a `Session` from the index alone, for sessions whose original
/// file the tool deleted (archive mode). It carries the distilled transcript
/// we kept - the same text `show`/`brief` would display for a live session,
/// minus per-message timestamps and full tool I/O, which were never indexed.
pub fn session_from_index(conn: &Connection, row: &SessionRow) -> Result<crate::model::Session> {
    use crate::model::{Message, Role};
    let mut stmt =
        conn.prepare("SELECT role, text FROM messages WHERE session_id = ?1 ORDER BY id")?;
    let messages: Vec<Message> = stmt
        .query_map(params![row.session_id], |r| {
            let role: String = r.get(0)?;
            let text: String = r.get(1)?;
            Ok(Message {
                role: match role.as_str() {
                    "user" => Role::User,
                    "assistant" => Role::Assistant,
                    _ => Role::Tool,
                },
                text,
                ts: None,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let tool = adapters::by_name(&row.tool)
        .map(|a| a.name())
        .unwrap_or("unknown");
    let started = row
        .started
        .as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&chrono::Utc));
    Ok(crate::model::Session {
        id: row.session_id.clone(),
        tool,
        // A shared-store key carries a U+001F separator in the stored path;
        // render it human-readable (`<db>#<id>`) so it never leaks into show /
        // brief / web. Real file paths never contain U+001F, so this is a no-op
        // for every other adapter.
        path: std::path::PathBuf::from(row.path.replace('\u{1f}', "#")),
        project: row.project.clone(),
        started,
        ended: started,
        title: row.title.clone(),
        subagent: row.kind == "sub",
        messages,
        touched: files_for(conn, &row.session_id)?,
    })
}

/// Permanently remove a session from the index AND the archive - the only way
/// to undo archiving for a session the user genuinely wants gone. Curation for
/// it (tags/notes/summary) goes too, since the session no longer exists here.
/// All-or-nothing: a crash mid-forget must not leave the FTS index out of sync
/// with `messages`, nor an `archive` row that would resurrect it on rebuild.
pub fn forget(conn: &mut Connection, session_id: &str) -> Result<()> {
    let tx = conn.transaction()?;
    delete_session_msgs(&tx, session_id)?;
    for table in ["files", "touched", "archive", "summaries", "tags", "notes"] {
        tx.execute(
            &format!("DELETE FROM {table} WHERE session_id = ?1"),
            params![session_id],
        )?;
    }
    tx.commit()?;
    Ok(())
}

// --- related sessions (backlinks) ---

/// Sessions related to `session_id`. A session is most usefully "related" to
/// the others about the same codebase, so same-project sessions are the spine;
/// sessions sharing a user tag are layered on as explicit links. Both are
/// indexed lookups, so this is instant even over a large store - the earlier
/// full-text-on-title approach was both slow and noisy (generic title words
/// like "session" matched everything).
pub fn related(conn: &Connection, session_id: &str, limit: usize) -> Result<Vec<SessionRow>> {
    let Some(target) = resolve(conn, session_id)?.into_iter().next() else {
        return Ok(vec![]);
    };
    let target_tags: Vec<String> = target
        .tags
        .as_deref()
        .map(|t| t.split(',').map(String::from).collect())
        .unwrap_or_default();

    let mut out: Vec<SessionRow> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    seen.insert(target.session_id.clone());

    // 1. same project (exact), most recent first - the same-context spine.
    if !target.project.is_empty() {
        let sql = format!(
            "SELECT session_id, tool, path, project, title, started, msg_count, kind,
                    {PREVIEW_SQL}, {SUMMARY_SQL}, {TAGS_SQL}, (archived_at IS NOT NULL)
             FROM files f
             WHERE kind = 'main' AND project = ?1 AND session_id != ?2
             ORDER BY started DESC LIMIT ?3"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![target.project, target.session_id, limit as i64 + 1],
            map_row,
        )?;
        for row in rows {
            let row = row?;
            if seen.insert(row.session_id.clone()) {
                out.push(row);
            }
        }
    }

    // 2. sessions that edited a file this one also edited - the strongest
    //    signal that two sessions are about the same work, and one no other
    //    session viewer has, since it comes from the provenance link.
    if out.len() < limit {
        let sql = format!(
            "SELECT DISTINCT f.session_id, f.tool, f.path, f.project, f.title, f.started,
                    f.msg_count, f.kind, {PREVIEW_SQL}, {SUMMARY_SQL}, {TAGS_SQL}, (f.archived_at IS NOT NULL)
             FROM touched a
             JOIN touched b ON a.path = b.path AND b.session_id != a.session_id
             JOIN files f ON f.session_id = b.session_id
             WHERE a.session_id = ?1 AND f.kind = 'main'
             ORDER BY f.started DESC LIMIT 50"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![target.session_id], map_row)?;
        for row in rows {
            let row = row?;
            if seen.insert(row.session_id.clone()) {
                out.push(row);
                if out.len() >= limit {
                    break;
                }
            }
        }
    }

    // 3. sessions that share a tag with the target (explicit wiki links).
    if out.len() < limit && !target_tags.is_empty() {
        let placeholders = target_tags
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT DISTINCT f.session_id, f.tool, f.path, f.project, f.title, f.started,
                    f.msg_count, f.kind, {PREVIEW_SQL}, {SUMMARY_SQL}, {TAGS_SQL}, (f.archived_at IS NOT NULL)
             FROM files f JOIN tags t ON t.session_id = f.session_id
             WHERE f.kind = 'main' AND t.tag IN ({placeholders})
             ORDER BY f.started DESC LIMIT 50"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(&target_tags), map_row)?;
        for row in rows {
            let row = row?;
            if seen.insert(row.session_id.clone()) {
                out.push(row);
                if out.len() >= limit {
                    break;
                }
            }
        }
    }

    out.truncate(limit);
    Ok(out)
}

/// Row mapper for the full session-list column set.
fn map_row(r: &rusqlite::Row) -> rusqlite::Result<SessionRow> {
    Ok(SessionRow {
        session_id: r.get(0)?,
        tool: r.get(1)?,
        path: r.get(2)?,
        project: r.get(3)?,
        title: r.get(4)?,
        started: r.get(5)?,
        msg_count: r.get(6)?,
        kind: r.get(7)?,
        preview: r.get(8)?,
        summary: r.get(9)?,
        tags: r.get(10)?,
        archived: r.get(11)?,
        account: None,
    })
}

// --- session engineering: management views ---

pub struct ProjectRow {
    pub project: String,
    pub sessions: i64,
    pub messages: i64,
    pub oldest: Option<String>,
    pub newest: Option<String>,
}

/// One row per project (a wiki "category" page), busiest first.
pub fn projects(conn: &Connection) -> Result<Vec<ProjectRow>> {
    let mut stmt = conn.prepare(
        "SELECT project, count(*), coalesce(sum(msg_count), 0), min(started), max(started)
         FROM files WHERE kind = 'main' AND project != ''
         GROUP BY project ORDER BY count(*) DESC, max(started) DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(ProjectRow {
            project: r.get(0)?,
            sessions: r.get(1)?,
            messages: r.get(2)?,
            oldest: r.get(3)?,
            newest: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub struct Stats {
    pub per_tool: Vec<(String, i64, i64)>, // tool, sessions, messages
    pub per_month: Vec<(String, i64)>,     // YYYY-MM, sessions
    pub total_sessions: i64,
    pub total_messages: i64,
    pub projects: i64,
    pub tags: i64,
    pub summarized: i64,
    /// Distinct files linked to at least one session (provenance coverage).
    pub files: i64,
    /// Sessions kept after the tool deleted their originals (archive mode).
    pub archived: i64,
}

pub fn stats(conn: &Connection) -> Result<Stats> {
    let mut per_tool_stmt = conn.prepare(
        "SELECT tool, count(*), coalesce(sum(msg_count),0) FROM files WHERE kind='main'
         GROUP BY tool ORDER BY count(*) DESC",
    )?;
    let per_tool = per_tool_stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut per_month_stmt = conn.prepare(
        "SELECT substr(started,1,7) AS ym, count(*) FROM files
         WHERE kind='main' AND started IS NOT NULL
         GROUP BY ym ORDER BY ym DESC LIMIT 12",
    )?;
    let per_month = per_month_stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let one = |sql: &str| -> Result<i64> { Ok(conn.query_row(sql, [], |r| r.get(0))?) };
    Ok(Stats {
        per_tool,
        per_month,
        total_sessions: one("SELECT count(*) FROM files WHERE kind='main'")?,
        total_messages: one("SELECT coalesce(sum(msg_count),0) FROM files WHERE kind='main'")?,
        projects: one(
            "SELECT count(DISTINCT project) FROM files WHERE kind='main' AND project!=''",
        )?,
        tags: one("SELECT count(DISTINCT tag) FROM tags")?,
        summarized: one("SELECT count(*) FROM summaries")?,
        files: one("SELECT count(DISTINCT path) FROM touched")?,
        archived: one("SELECT count(*) FROM files WHERE archived_at IS NOT NULL")?,
    })
}
