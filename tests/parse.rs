//! Golden-file tests for the session adapters.
//!
//! These guard the most fragile part of the project: parsing three different
//! JSONL/JSON schemas that drift between tool versions. Each fixture under
//! `tests/fixtures/<tool>/` is a small, realistic session including the edge
//! cases the parsers are supposed to handle (boilerplate dropping, subagent
//! detection, malformed lines, multi-block content, skipped roles).
//!
//! When a tool changes its format, a fixture from the new version plus an
//! assertion here is the cleanest possible bug report and regression test.

use sessionwiki::adapters;
use sessionwiki::model::Session;
use std::path::PathBuf;

fn fixture(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(rel)
}

fn parse(tool: &str, rel: &str) -> Session {
    let adapter = adapters::by_name(tool).expect("adapter exists");
    adapter.parse(&fixture(rel)).expect("parse succeeds")
}

fn roles(s: &Session) -> Vec<&'static str> {
    s.messages.iter().map(|m| m.role.label()).collect()
}

#[test]
fn claude_main_session() {
    let s = parse(
        "claude-code",
        "claude-code/proj-a/0a000000-0000-4000-8000-000000000001.jsonl",
    );

    // Title comes from the summary line, not the first user message.
    assert_eq!(s.title, "Fix CORS preflight failing on /auth routes");
    assert_eq!(s.tool, "claude-code");
    assert_eq!(s.project, "/home/dev/proj-a");
    assert!(!s.subagent);

    // A malformed line and the isMeta:true boilerplate user turn are dropped;
    // tool_use (Bash, two Edits, a Write) and tool_result all become Tool
    // messages.
    assert_eq!(
        roles(&s),
        [
            "user",
            "assistant",
            "tool",
            "tool",
            "tool",
            "tool",
            "tool",
            "assistant"
        ]
    );

    // The dropped boilerplate must not leak into any message.
    assert!(s
        .messages
        .iter()
        .all(|m| !m.text.contains("harness boilerplate")));

    // First user prompt and final assistant message survive intact.
    assert!(s.messages[0].text.contains("Preflight requests"));
    assert!(s.messages.iter().any(|m| m.text.contains("14 tests pass")));

    // Provenance: Edit/Write file_path is extracted, the repeated edit to
    // mod.rs is de-duplicated, and the read-only Bash call contributes nothing.
    assert_eq!(
        s.touched,
        [
            "/home/dev/proj-a/src/middleware/mod.rs",
            "/home/dev/proj-a/tests/cors_preflight.rs"
        ]
    );

    assert_eq!(
        s.started.map(|t| t.to_rfc3339()),
        Some("2026-06-09T14:01:00+00:00".to_string())
    );
}

#[test]
fn claude_subagent_is_flagged() {
    let s = parse(
        "claude-code",
        "claude-code/proj-a/0a000000-0000-4000-8000-000000000001/subagents/agent-deadbeef.jsonl",
    );
    assert!(s.subagent, "files under /subagents/ must be flagged");
    assert_eq!(roles(&s), ["user", "assistant"]);
    assert_eq!(s.title, "Search the codebase for all CORS references.");
}

#[test]
fn codex_session_with_schema_variants() {
    let s = parse(
        "codex",
        "codex/rollout-2026-06-11T13-00-00-019eb9b2-1466-7e93-8b85-5b596295e96b.jsonl",
    );

    assert_eq!(s.tool, "codex");
    assert_eq!(s.project, "/home/dev/api-server");
    assert_eq!(s.title, "Write property-based tests for the rate limiter.");

    // environment_context boilerplate dropped; both function_calls (the shell
    // test run and the apply_patch) kept as Tool; function_call_output and
    // reasoning dropped; both response_item and event_msg message shapes parse.
    assert_eq!(
        roles(&s),
        ["user", "tool", "tool", "assistant", "user", "assistant"]
    );
    assert!(s
        .messages
        .iter()
        .all(|m| !m.text.contains("environment_context")));
    assert!(s.messages.iter().all(|m| !m.text.contains("cases passed"))); // function_call_output excluded
    assert!(s.messages.iter().any(|m| m.text.contains("2.1M ops/sec")));

    // Provenance: apply_patch file headers (Update File / Add File) are pulled
    // from the call arguments even though the patch is double-JSON-escaped.
    assert_eq!(
        s.touched,
        ["src/rate_limiter.rs", "tests/rate_limiter_props.rs"]
    );
}

#[test]
fn gemini_session() {
    let s = parse(
        "gemini",
        "gemini/myproject/chats/session-2026-06-08T10-00-abcd1234.json",
    );

    assert_eq!(s.tool, "gemini");
    assert_eq!(s.project, "myproject");
    // String content and array-of-blocks content both parse; unknown role
    // ("system") is skipped.
    assert_eq!(roles(&s), ["user", "assistant"]);
    assert!(s.messages[0].text.contains("iframe"));
    assert!(s.messages[1].text.contains("1급 스코프")); // CJK survives intact
    assert_eq!(
        s.ended.map(|t| t.to_rfc3339()),
        Some("2026-06-08T10:00:30+00:00".to_string())
    );
}

#[test]
fn opencode_multi_file_session() {
    // OpenCode splits a session across session/message/part JSON files; the
    // adapter is handed the session file and joins the rest from the store.
    let s = parse("opencode", "opencode/storage/session/proj1/ses_aaa.json");

    assert_eq!(s.tool, "opencode");
    assert_eq!(s.title, "Add retry to the HTTP client"); // from session.title
    assert_eq!(s.project, "/home/dev/myapp"); // session.directory
    assert!(!s.subagent);

    // reasoning parts are dropped; text parts and the edit tool become messages
    // in id order; the malformed part file is skipped without panicking; the
    // patch part contributes no message (only provenance).
    assert_eq!(roles(&s), ["user", "assistant", "tool"]);
    assert!(s.messages[0]
        .text
        .contains("retry with exponential backoff"));
    assert!(s
        .messages
        .iter()
        .any(|m| m.text.contains("3-attempt retry")));
    assert!(s
        .messages
        .iter()
        .all(|m| !m.text.contains("thinking about"))); // reasoning dropped

    // Provenance: edit tool's state.input.filePath + the patch's files list.
    assert_eq!(
        s.touched,
        [
            "/home/dev/myapp/src/http/client.ts",
            "/home/dev/myapp/src/util.ts"
        ]
    );

    // epoch-millis timestamp is parsed (not an RFC3339 string).
    assert_eq!(
        s.started,
        chrono::DateTime::from_timestamp_millis(1718630400000)
    );
}

#[test]
fn cline_xml_and_native_tool_edits() {
    // Cline keeps each task as api_conversation_history.json (the Anthropic
    // Messages array) plus ui_messages.json (timestamps + the title).
    let s = parse(
        "cline",
        "cline/tasks/1749477660000/api_conversation_history.json",
    );

    assert_eq!(s.tool, "cline");
    assert_eq!(s.title, "Add a hello function to src/main.py"); // ui_messages say:"task"
    assert_eq!(s.project, "/home/dev/app"); // from <environment_details>
    assert!(!s.subagent);

    // The <task>/<environment_details> wrappers and the tool-result feedback
    // turn ("[... ] Result:") are stripped; XML and native tool calls both
    // become Tool messages.
    assert_eq!(
        roles(&s),
        [
            "user",
            "assistant",
            "tool",
            "assistant",
            "tool",
            "assistant"
        ]
    );
    assert_eq!(s.messages[0].text, "Add a hello function to src/main.py");
    assert!(s
        .messages
        .iter()
        .all(|m| !m.text.contains("environment_details")));
    assert!(s.messages.iter().all(|m| !m.text.contains("] Result:")));
    assert!(s
        .messages
        .iter()
        .all(|m| !m.text.contains("successfully saved")));

    // Provenance from BOTH the XML <write_to_file><path> and the native
    // tool_use input.path.
    assert_eq!(s.touched, ["src/main.py", "tests/test_main.py"]);

    assert_eq!(
        s.started,
        chrono::DateTime::from_timestamp_millis(1749477660000)
    );
    assert_eq!(
        s.ended,
        chrono::DateTime::from_timestamp_millis(1749477700000)
    );
}

#[test]
fn gajae_jsonl_session() {
    // gajae-code (and upstream Pi) store one JSONL transcript per session: a
    // header line then message lines, with toolCall/arguments content blocks.
    let s = parse(
        "gajae-code",
        "gajae-code/--home--dev--proj--/2025-12-09T00-53-29-825Z_ffae836b-9420-4060-ac13-7745215f90ff.jsonl",
    );

    assert_eq!(s.tool, "gajae-code");
    assert_eq!(s.title, "refactor the parser"); // header title
    assert_eq!(s.project, "/home/dev/proj"); // header cwd
    assert!(!s.subagent);

    // thinking blocks dropped; toolResult -> tool; the malformed line is skipped
    // without panicking.
    assert_eq!(
        roles(&s),
        [
            "user",
            "assistant",
            "tool",
            "tool",
            "assistant",
            "tool",
            "assistant",
            "tool",
            "assistant"
        ]
    );
    assert!(s
        .messages
        .iter()
        .all(|m| !m.text.contains("considering the structure")));

    // Provenance: write (arguments.path) + ast_edit (arguments.paths[]) with the
    // repeat de-duplicated; read is excluded.
    assert_eq!(s.touched, ["src/parse.ts", "src/helper.ts"]);

    // started = header RFC3339; ended = last entry's RFC3339.
    assert_eq!(
        s.started.map(|t| t.to_rfc3339()),
        Some("2025-12-09T00:53:29.825+00:00".to_string())
    );
    assert_eq!(
        s.ended.map(|t| t.to_rfc3339()),
        Some("2025-12-09T00:53:40+00:00".to_string())
    );
}

#[test]
fn continue_session_with_tool_edit() {
    let s = parse(
        "continue",
        "continue/sessions/5f1d2c9a-8b34-4e21-9d6e-7a0c1b2e3f44.json",
    );

    assert_eq!(s.tool, "continue");
    assert_eq!(s.title, "Add retry to the HTTP client");
    assert_eq!(s.project, "/home/alex/projects/acme-api");
    assert!(!s.subagent);

    assert_eq!(roles(&s), ["user", "assistant", "tool", "tool"]);

    // Provenance from toolCallStates[].parsedArgs.filepath.
    assert_eq!(s.touched, ["src/http.ts"]);

    // The session file has no timestamps; started comes from sessions.json's
    // dateCreated (epoch-ms as a string); there is no reliable ended.
    assert_eq!(
        s.started,
        chrono::DateTime::from_timestamp_millis(1718600000000)
    );
    assert_eq!(s.ended, None);
}

#[test]
fn gptme_session() {
    let s = parse(
        "gptme",
        "gptme/fix-cors-bug-20260608-abcd/conversation.jsonl",
    );

    assert_eq!(s.tool, "gptme");
    // Project label is the session directory name (the slug), not a cwd.
    assert_eq!(s.project, "fix-cors-bug-20260608-abcd");

    // Pinned system-prompt line and bare system role are both dropped;
    // the malformed JSON line is silently skipped.
    assert_eq!(roles(&s), ["user", "assistant", "user", "assistant"]);

    // Naive timestamp fallback (no UTC offset) is parsed and assumed UTC.
    assert_eq!(
        s.started,
        chrono::DateTime::parse_from_rfc3339("2026-06-08T10:00:01Z")
            .ok()
            .map(|t| t.with_timezone(&chrono::Utc))
    );

    // Title comes from first user message.
    assert!(s.messages[0].text.contains("CORS preflight"));
}

#[test]
fn gptme_malformed_lines_do_not_panic() {
    // A fixture with a bad line must not panic — it is silently skipped.
    let adapter = adapters::by_name("gptme").unwrap();
    let result = adapter.parse(&fixture(
        "gptme/fix-cors-bug-20260608-abcd/conversation.jsonl",
    ));
    assert!(result.is_ok());
}

#[test]
fn missing_file_errors_without_panicking() {
    let adapter = adapters::by_name("codex").unwrap();
    assert!(adapter
        .parse(&fixture("codex/does-not-exist.jsonl"))
        .is_err());
}

#[test]
fn aider_golden() {
    // aider is a shared store: drive parse_key with `<path>\u{1f}<run-index>`.
    let path = fixture("aider/myrepo/.aider.chat.history.md");
    let adapter = adapters::by_name("aider").unwrap();
    let s = adapter
        .parse_key(&format!("{}\u{1f}0", path.to_string_lossy()))
        .unwrap();
    assert_eq!(s.tool, "aider");
    assert_eq!(s.title, "add a retry to the webhook");
    let roles: Vec<&str> = s.messages.iter().map(|m| m.role.label()).collect();
    assert_eq!(roles, vec!["user", "assistant", "tool"]);
    assert_eq!(s.touched, vec!["src/webhook.py", "src/retry.py"]); // dry-run ignored
    assert!(s.started.is_some());
    assert!(s.messages.iter().all(|m| m.ts.is_none()));
    // a bad run index errors, never panics
    assert!(adapter
        .parse_key(&format!("{}\u{1f}99", path.to_string_lossy()))
        .is_err());
}

#[test]
fn every_adapter_is_addressable_by_name() {
    for tool in [
        "claude-code",
        "codex",
        "gemini",
        "opencode",
        "cline",
        "roo-code",
        "kilo-code",
        "gajae-code",
        "continue",
        "gptme",
        "aider",
    ] {
        assert!(adapters::by_name(tool).is_some(), "{tool} should resolve");
    }
    assert!(adapters::by_name("nonexistent").is_none());
}

#[test]
fn prodex_consult_task_becomes_a_two_message_session() {
    let s = parse(
        "prodex",
        "prodex/repo-a/.bridge/tasks/task_20260707_090000_gpt-pro-consult.json",
    );
    assert_eq!(s.tool, "prodex");
    // sha256("task_20260707_090000_gpt-pro-consult")[..12] - the uniform
    // short-id shape every tool gets in the index.
    assert_eq!(s.id, "567f374d7a70");
    // The question is the title - prodex auto-titles every consult
    // "GPT Pro consult", which would make a list of them indistinguishable.
    assert!(s.title.starts_with("Should the retry loop"), "{}", s.title);
    assert_eq!(roles(&s), ["user", "assistant"]);
    assert!(s.messages[0].text.contains("exponential backoff"), "prompt");
    // The full pro-consult artifact wins over the truncated summary.
    assert!(
        s.messages[1].text.contains("thundering herds"),
        "artifact text used as the answer: {}",
        s.messages[1].text
    );
    assert!(s.started.is_some(), "claimed_at parsed");
    // Project = the repo that owns the .bridge.
    assert!(s.project.ends_with("repo-a"), "{}", s.project);
}

#[test]
fn prodex_pending_task_is_a_one_message_session() {
    let s = parse(
        "prodex",
        "prodex/repo-a/.bridge/tasks/task_20260707_100000_open-question.json",
    );
    assert_eq!(roles(&s), ["user"]);
    assert!(s.started.is_some(), "falls back to the id timestamp");
}

#[test]
fn prodex_thread_url_comes_from_the_bridge_session() {
    let task = fixture("prodex/repo-a/.bridge/tasks/task_20260707_090000_gpt-pro-consult.json");
    let url = sessionwiki::adapters::prodex_thread_url(&task);
    assert_eq!(
        url.as_deref(),
        Some("https://chatgpt.com/c/aaaa1111-2222-3333-4444-555566667777")
    );
    // A non-chatgpt URL in a tampered file is never surfaced.
    let dir = tempfile::tempdir().unwrap();
    let tasks = dir.path().join(".bridge/tasks");
    let sessions = dir.path().join(".bridge/sessions");
    std::fs::create_dir_all(&tasks).unwrap();
    std::fs::create_dir_all(&sessions).unwrap();
    let t = tasks.join("task_x.json");
    std::fs::write(&t, "{}").unwrap();
    std::fs::write(
        sessions.join("s.json"),
        r#"{"thread":"file:///etc/passwd"}"#,
    )
    .unwrap();
    assert_eq!(sessionwiki::adapters::prodex_thread_url(&t), None);
}

// The crash path the durable ledger exists for: the answer artifact was
// written but the result JSON was not. The answer must still be indexed.
#[test]
fn prodex_artifact_without_result_is_still_the_answer() {
    let s = parse(
        "prodex",
        "prodex/repo-a/.bridge/tasks/task_20260707_110000_crash-path.json",
    );
    assert_eq!(roles(&s), ["user", "assistant"]);
    assert!(
        s.messages[1].text.contains("logical shards"),
        "{}",
        s.messages[1].text
    );
}
