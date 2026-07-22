use clap::{Parser, Subcommand};
use sessionwiki::{commands, doctor, web};

/// Find, search, and read every AI coding session on your machine - across
/// Claude Code, Codex, Gemini CLI, OpenCode, Cline, and more. 100% local.
#[derive(Parser)]
#[command(name = "sessionwiki", version, about, max_term_width = 100)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Discover session stores on this machine (which tools, where, how much)
    Scan,
    /// List recent sessions across all tools
    List {
        /// Max sessions to show
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
        /// Filter by tool (claude-code, codex, gemini, prodex, ... - `scan` lists all)
        #[arg(long)]
        tool: Option<String>,
        /// Filter by project path substring
        #[arg(long)]
        project: Option<String>,
        /// Filter by tag
        #[arg(long)]
        tag: Option<String>,
        /// Filter by swapdex account profile (the @badge; needs a swapdex switch history)
        #[arg(long)]
        account: Option<String>,
        /// Include subagent transcripts in the listing
        #[arg(long)]
        all: bool,
        /// Emit as a JSON array (agent-friendly, stable field names)
        #[arg(long)]
        json: bool,
        /// Skip the index sync and query what is already indexed (pair with `sync`)
        #[arg(long)]
        no_sync: bool,
    },
    /// Full-text search across every session of every tool
    Search {
        /// Text to look for (substring match, works for CJK too)
        query: String,
        /// Max sessions to show
        #[arg(short = 'n', long, default_value_t = 10)]
        limit: usize,
        /// Filter by tool (claude-code, codex, gemini, prodex, ... - `scan` lists all)
        #[arg(long)]
        tool: Option<String>,
        /// Filter by project path substring
        #[arg(long)]
        project: Option<String>,
        /// Filter by swapdex account profile (the @badge; needs a swapdex switch history)
        #[arg(long)]
        account: Option<String>,
        /// Emit as a JSON array (agent-friendly, stable field names)
        #[arg(long)]
        json: bool,
        /// Skip the index sync and query what is already indexed (pair with `sync`)
        #[arg(long)]
        no_sync: bool,
    },
    /// Recall past work in one step: search, then brief the top match
    Recall {
        /// What to recall - a topic, error text, or identifier (exact phrasing)
        query: String,
        /// How many candidate matches to list (the top one is briefed)
        #[arg(short = 'n', long, default_value_t = 5)]
        limit: usize,
        /// Filter by tool (claude-code, codex, gemini, prodex, ... - `scan` lists all)
        #[arg(long)]
        tool: Option<String>,
        /// Filter by project path substring
        #[arg(long)]
        project: Option<String>,
        /// Budget for the briefed top match
        #[arg(long, default_value_t = 12000)]
        max_chars: usize,
        /// Emit a JSON object { query, top, candidates } instead
        #[arg(long)]
        json: bool,
        /// Skip the index sync and query what is already indexed (pair with `sync`)
        #[arg(long)]
        no_sync: bool,
    },
    /// Browse and search sessions in a local web UI
    Web {
        /// Port to listen on (localhost only)
        #[arg(long, default_value_t = 7575)]
        port: u16,
        /// Do not open the browser automatically
        #[arg(long)]
        no_open: bool,
        /// Refresh the index once before serving (otherwise it reflects the
        /// last `list`/`search`)
        #[arg(long)]
        sync: bool,
    },
    /// Print one session as a readable transcript
    Show {
        /// Session id (prefix is enough), from list/search output
        id: String,
        /// Include full tool inputs/outputs instead of summaries
        #[arg(long)]
        full: bool,
        /// Emit the parsed session as JSON
        #[arg(long)]
        json: bool,
        /// Show a digest instead: every user turn plus how the session ended
        #[arg(long)]
        outline: bool,
        /// Agent-friendly bounded window: the real turns, but tool outputs
        /// folded to head+tail (add --budget to keep only the recent tail)
        #[arg(long)]
        window: bool,
        /// With --window, cap the render to about this many tokens (recent tail)
        #[arg(long)]
        budget: Option<usize>,
        /// Read the session file directly (0-delay tail), bypassing the index
        #[arg(long)]
        live: bool,
        /// Skip the index sync; only sync if the id is not already indexed
        #[arg(long)]
        no_sync: bool,
    },
    /// Reopen a session in its original tool (claude --resume / codex resume)
    Resume {
        /// Session id (prefix is enough), from list/search output
        id: String,
        /// Print the resume command instead of running it
        #[arg(long)]
        print: bool,
        /// Skip the index sync; only sync if the id is not already indexed
        #[arg(long)]
        no_sync: bool,
    },
    /// Copy a session so it can be resumed from a different project directory
    Migrate {
        /// Session id (prefix is enough), from list/search output
        id: String,
        /// Target project directory to make the session resumable from
        dir: String,
        /// Skip the index sync; only sync if the id is not already indexed
        #[arg(long)]
        no_sync: bool,
    },
    /// Generate and cache LLM synopses for sessions (uses your own LLM CLI)
    Summarize {
        /// Session id to summarize; omit to batch over recent sessions
        id: Option<String>,
        /// How many recent unsummarized sessions to process in batch mode
        #[arg(long, default_value_t = 10)]
        recent: usize,
        /// Filter batch mode by tool
        #[arg(long)]
        tool: Option<String>,
        /// Summarizer command reading the session on stdin (default: `claude -p`,
        /// or the SESSIONWIKI_SUMMARIZER environment variable)
        #[arg(long)]
        cmd: Option<String>,
        /// Re-summarize even if a cached summary exists
        #[arg(long)]
        force: bool,
    },
    /// Emit a markdown briefing of a session, ready to paste into a new one
    Brief {
        /// Session id (prefix is enough), from list/search output
        id: String,
        /// Budget for the briefing body; the middle of long sessions is omitted
        #[arg(long, default_value_t = 24000)]
        max_chars: usize,
        /// Include tool calls in the briefing
        #[arg(long)]
        tools: bool,
        /// Emit a JSON object { id, tool, project, title, started, source,
        /// markdown } instead of raw markdown
        #[arg(long)]
        json: bool,
        /// Skip the index sync; only sync if the id is not already indexed
        #[arg(long)]
        no_sync: bool,
    },
    /// Tag a session: `tag <id> <tag>...` adds, `--rm <tag>` removes (no id
    /// lists every tag in use). Tags are positional - there is no `add`
    /// keyword, so `tag <id> add foo` would create the tags `add` and `foo`.
    Tag {
        /// Session id (prefix is enough); omit to list every tag in use
        #[arg(default_value = "")]
        id: String,
        /// Tags to add (positional, space-separated)
        #[arg(value_name = "TAG")]
        add: Vec<String>,
        /// Tags to remove
        #[arg(long = "rm", value_name = "TAG")]
        remove: Vec<String>,
    },
    /// Attach or read a freeform note on a session
    Note {
        /// Session id (prefix is enough)
        id: String,
        /// Note text; omit to print the existing note
        text: Option<String>,
    },
    /// Find sessions related to one (shared project, files, and tags)
    Related {
        /// Session id (prefix is enough)
        id: String,
        /// Max related sessions to show
        #[arg(short = 'n', long, default_value_t = 10)]
        limit: usize,
        /// Emit as a JSON array (agent-friendly, stable field names)
        #[arg(long)]
        json: bool,
    },
    /// List the files a session edited or created
    Files {
        /// Session id (prefix is enough), from list/search output
        id: String,
        /// Emit as a JSON array of paths
        #[arg(long)]
        json: bool,
    },
    /// Trace a file back to the AI sessions that edited it (newest first)
    Trace {
        /// File path as it appears in your editor (e.g. src/auth.rs)
        path: String,
        /// Emit as a JSON array (agent-friendly, stable field names)
        #[arg(long)]
        json: bool,
        /// Skip the index sync and query what is already indexed (pair with `sync`)
        #[arg(long)]
        no_sync: bool,
    },
    /// Attribute each line of a file to the AI session most likely behind it (git blame for the AI era)
    Blame {
        /// File path as it appears in your editor (e.g. src/auth.rs)
        file: String,
        /// Restrict to a line range, e.g. -L 40,80
        #[arg(short = 'L', value_name = "START,END", value_parser = parse_line_range)]
        range: Option<(usize, usize)>,
        /// Emit as a JSON array (agent-friendly, stable field names)
        #[arg(long)]
        json: bool,
        /// Skip the index sync and query what is already indexed (pair with `sync`)
        #[arg(long)]
        no_sync: bool,
    },
    /// Permanently drop an archived (or any) session from the index
    Forget {
        /// Session id (prefix is enough), from list/search output
        id: String,
    },
    /// Build or refresh the index now, so later queries can use `--no-sync`
    Sync {
        /// Limit to one tool (claude-code, codex, gemini, opencode, ...)
        #[arg(long)]
        tool: Option<String>,
    },
    /// Markdown rollup of recent sessions by project (files touched, synopses)
    Digest {
        /// Time window: 7d, 2w, 24h, 90m (a bare number is days)
        #[arg(long, default_value = "7d")]
        since: String,
        /// Limit to one tool
        #[arg(long)]
        tool: Option<String>,
        /// Limit to one project (substring match)
        #[arg(long)]
        project: Option<String>,
        /// Emit JSON instead of markdown
        #[arg(long)]
        json: bool,
        /// Skip the index sync; only sync if the id is not already indexed
        #[arg(long)]
        no_sync: bool,
    },
    /// List projects with session counts (a page per project)
    Projects,
    /// Usage breakdown across tools, projects, and months
    Stats,
    /// Diagnose this machine's setup: which stores exist, index health, version
    Doctor {
        /// Emit the checks as JSON
        #[arg(long)]
        json: bool,
    },
    /// Internal hooks for editor integrations (run by Claude Code, not by hand)
    Hook {
        #[command(subcommand)]
        cmd: HookCmd,
    },
    /// Run as an MCP server over stdio (register: claude mcp add sessionwiki -- sessionwiki mcp)
    Mcp,
}

#[derive(Subcommand)]
enum HookCmd {
    /// Claude Code SessionStart: print a fenced recall brief for the launch project
    SessionStart,
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Scan => commands::scan(),
        Command::List {
            limit,
            tool,
            project,
            tag,
            account,
            all,
            json,
            no_sync,
        } => commands::list(
            limit,
            tool.as_deref(),
            project.as_deref(),
            tag.as_deref(),
            account.as_deref(),
            all,
            json,
            no_sync,
        ),
        Command::Search {
            query,
            limit,
            tool,
            project,
            account,
            json,
            no_sync,
        } => commands::search(
            &query,
            limit,
            tool.as_deref(),
            project.as_deref(),
            account.as_deref(),
            json,
            no_sync,
        ),
        Command::Recall {
            query,
            limit,
            tool,
            project,
            max_chars,
            json,
            no_sync,
        } => commands::recall(
            &query,
            limit,
            tool.as_deref(),
            project.as_deref(),
            max_chars,
            json,
            no_sync,
        ),
        Command::Web {
            port,
            no_open,
            sync,
        } => web::serve(port, no_open, sync),
        Command::Show {
            id,
            full,
            json,
            outline,
            window,
            budget,
            live,
            no_sync,
        } => commands::show(&id, full, json, outline, window, budget, live, no_sync),
        Command::Resume { id, print, no_sync } => commands::resume_cmd(&id, print, no_sync),
        Command::Migrate { id, dir, no_sync } => commands::migrate_cmd(&id, &dir, no_sync),
        Command::Summarize {
            id,
            recent,
            tool,
            cmd,
            force,
        } => commands::summarize(
            id.as_deref(),
            recent,
            tool.as_deref(),
            cmd.as_deref(),
            force,
        ),
        Command::Brief {
            id,
            max_chars,
            tools,
            json,
            no_sync,
        } => commands::brief(&id, max_chars, tools, json, no_sync),
        Command::Tag { id, add, remove } => commands::tag(&id, &add, &remove),
        Command::Note { id, text } => commands::note(&id, text.as_deref()),
        Command::Related { id, limit, json } => commands::related(&id, limit, json),
        Command::Files { id, json } => commands::files(&id, json),
        Command::Trace {
            path,
            json,
            no_sync,
        } => commands::trace(&path, json, no_sync),
        Command::Blame {
            file,
            range,
            json,
            no_sync,
        } => commands::blame(&file, range, json, no_sync),
        Command::Forget { id } => commands::forget(&id),
        Command::Sync { tool } => commands::sync_cmd(tool.as_deref()),
        Command::Digest {
            since,
            tool,
            project,
            json,
            no_sync,
        } => commands::digest(&since, tool.as_deref(), project.as_deref(), json, no_sync),
        Command::Projects => commands::projects(),
        Command::Stats => commands::stats(),
        Command::Doctor { json } => doctor::run(json),
        Command::Hook {
            cmd: HookCmd::SessionStart,
        } => {
            // Always exit 0 with whatever was printed; never the exit(1) path.
            sessionwiki::hook::session_start();
            return;
        }
        Command::Mcp => {
            sessionwiki::mcp::serve();
            return;
        }
    };
    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

/// Parse a `-L START,END` line range into validated integers (so the raw token
/// is never forwarded to git). 1-based, inclusive, START <= END.
fn parse_line_range(s: &str) -> Result<(usize, usize), String> {
    let (a, b) = s.split_once(',').ok_or("expected START,END")?;
    let start: usize = a.trim().parse().map_err(|_| "START must be a number")?;
    let end: usize = b.trim().parse().map_err(|_| "END must be a number")?;
    if start == 0 || end < start {
        return Err("require 1 <= START <= END".into());
    }
    Ok((start, end))
}
