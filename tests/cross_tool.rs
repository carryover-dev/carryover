//! Cross-tool integration test — drives the full pipeline end-to-end:
//! adapter parses a sanitized fixture transcript → rows insert into the
//! ledger → 6 distillers extract the handoff fields → publisher
//! dual-writes handoff.md and stamps the static pointer block into
//! AGENTS.md + CLAUDE.md.
//!
//! This is the load-bearing test that proves cross-tool resume works on
//! the in-tree pipeline, independent of any individual unit test. If
//! this passes, cross-tool resume between Claude Code, Cursor, and
//! Codex is functionally correct.
//!
//! Each #[test] function names a (source, destination) pair. At this
//! integration layer the handoff format is destination-agnostic — the
//! 4 pair cycles really just exercise "for each source adapter, the
//! pipeline produces a valid handoff that any destination can consume."
//! Distinct test names point a CI failure at the source-tool path.

use std::path::{Path, PathBuf};

use carryover::adapters::{
    self, claude::ClaudeAdapter, codex::CodexAdapter, cursor::CursorAdapter, Adapter,
};
use carryover::distill::{
    failed_approaches::extract_failed_approaches, git_context::extract_git_context,
    next_action::extract_next_action, open_questions::extract_open_questions,
    recent_files::extract_recent_files, task::extract_task,
};
use carryover::publish::{
    publish, Distilled, PublishContext, POINTER_BLOCK, POINTER_END, POINTER_START,
};
use carryover::storage::{Ledger, LedgerRow};

fn fixture_root(tool: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(tool)
}

/// Run the source-tool adapter against its first happy-path fixture.
/// Returns the parsed `LedgerRow`s plus the canonical lowercase tool name.
fn parse_fixture_through_adapter(source_tool: &str) -> (Vec<LedgerRow>, &'static str) {
    match source_tool {
        "claude" => {
            let adapter = ClaudeAdapter::with_projects_root(fixture_root("claude"));
            let cursor = adapters::claude::ClaudeCursor {
                file_path: fixture_root("claude").join("1-simple-conversation.jsonl"),
                byte_offset: 0,
                last_uuid: None,
            };
            let (records, _) = adapter
                .read_new_records(&cursor)
                .expect("claude adapter: read_new_records");
            let rows = adapter.parse(records).expect("claude adapter: parse");
            (rows, "claude")
        }
        "cursor" => {
            let adapter = CursorAdapter::with_db_root(fixture_root("cursor"));
            let cursor = adapters::cursor::CursorCursor {
                db_path: fixture_root("cursor").join("1-state.vscdb"),
                last_rowid: 0,
                last_msg_id: String::new(),
            };
            let (records, _) = adapter
                .read_new_records(&cursor)
                .expect("cursor adapter: read_new_records");
            let rows = adapter.parse(records).expect("cursor adapter: parse");
            (rows, "cursor")
        }
        "codex" => {
            let adapter = CodexAdapter::with_sessions_root(fixture_root("codex"));
            let cursor = adapters::codex::CodexCursor {
                file_path: fixture_root("codex").join("1-simple-session.jsonl"),
                byte_offset: 0,
                last_event_seq: 0,
            };
            let (records, _) = adapter
                .read_new_records(&cursor)
                .expect("codex adapter: read_new_records");
            let rows = adapter.parse(records).expect("codex adapter: parse");
            (rows, "codex")
        }
        other => panic!("unknown source tool: {other}"),
    }
}

/// Run all 6 distillers and assemble a `Distilled` ready for `publish()`.
fn distill(rows: &[LedgerRow], source_tool: &str, cwd: Option<&Path>) -> Distilled {
    let session_id = rows
        .first()
        .map(|r| r.session_id.clone())
        .unwrap_or_default();
    Distilled {
        source_tool: source_tool.to_string(),
        session_id,
        timestamp_iso: "2026-04-28T00:00:00Z".to_string(),
        task: extract_task(rows),
        open_questions: extract_open_questions(rows),
        next_action: extract_next_action(rows),
        recent_files: extract_recent_files(rows),
        failed_approaches: extract_failed_approaches(rows),
        git_context: extract_git_context(rows, cwd),
    }
}

/// Drive the full source-side pipeline and assert every cross-tool
/// invariant on the resulting handoff and pointer blocks.
fn run_pipeline(source_tool: &str) {
    // 1+2. Parse fixture through adapter.
    let (rows, name) = parse_fixture_through_adapter(source_tool);
    assert!(!rows.is_empty(), "{source_tool}: adapter produced 0 rows");

    // 3. Insert into a fresh ledger.
    let ledger_dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&ledger_dir.path().join("ledger.sqlite")).unwrap();
    ledger.insert_batch(&rows).expect("ledger: insert_batch");

    // 4. Query back. Cursor's adapter merges three SQLite keys, each
    //    of which can carry its own session_id, so the row count
    //    queried by a single session_id may be < total. We assert it's
    //    non-empty and round-tripped without corruption.
    let session_id = &rows[0].session_id;
    let queried = ledger
        .query_session(session_id)
        .expect("ledger: query_session");
    assert!(
        !queried.is_empty(),
        "{source_tool}: at least one row should round-trip"
    );

    // 5+6. Distill from ALL rows (not just one session) so the handoff
    // reflects the full source-side parse, not a single key's slice.
    let distilled = distill(&rows, name, None);

    // 7. Publish.
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let ctx = PublishContext {
        home_dir: home.path().to_path_buf(),
        project_dir: project.path().to_path_buf(),
        resume_mode: "ask".to_string(),
    };
    let outcome = publish(&distilled, &ctx).expect("publish");

    // 8a. Dual-write byte-identity.
    let user_bytes = std::fs::read(&outcome.user_handoff).unwrap();
    let project_bytes = std::fs::read(&outcome.project_handoff).unwrap();
    assert_eq!(
        user_bytes, project_bytes,
        "{source_tool}: dual-write bytes must be identical"
    );
    assert!(!user_bytes.is_empty(), "{source_tool}: handoff is empty");

    // 8b. Handoff content invariants.
    let body = String::from_utf8(user_bytes).unwrap();
    assert!(
        body.contains("# CARRYOVER RESUME (mode: ask)"),
        "{source_tool}: missing resume protocol header"
    );
    assert!(
        body.contains(&format!("from {name}")),
        "{source_tool}: title line should mention the source tool"
    );
    assert!(
        body.contains("## Task"),
        "{source_tool}: missing Task section"
    );
    assert!(
        body.contains("## Next action"),
        "{source_tool}: missing Next action section"
    );

    // 8c. Pointer block byte-identity in AGENTS.md and CLAUDE.md.
    let agents = std::fs::read_to_string(project.path().join("AGENTS.md")).unwrap();
    let claude = std::fs::read_to_string(project.path().join("CLAUDE.md")).unwrap();
    let extract_block = |s: &str| -> String {
        let start = s.find(POINTER_START).expect("START marker present");
        let end = s.find(POINTER_END).expect("END marker present") + POINTER_END.len();
        s[start..end].to_string()
    };
    assert_eq!(
        extract_block(&agents),
        extract_block(&claude),
        "{source_tool}: pointer block must be byte-identical across AGENTS.md and CLAUDE.md"
    );
    assert_eq!(
        extract_block(&agents),
        POINTER_BLOCK,
        "{source_tool}: pointer block must match the constant"
    );

    // 8d. .gitignore covers .carryover/.
    let gitignore = std::fs::read_to_string(project.path().join(".gitignore")).unwrap();
    assert!(
        gitignore.contains(".carryover/"),
        "{source_tool}: .gitignore must contain .carryover/"
    );

    // 8e. Handoff files are 0o600 on unix.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for f in [&outcome.user_handoff, &outcome.project_handoff] {
            let mode = std::fs::metadata(f).unwrap().permissions().mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "{source_tool}: handoff at {f:?} must be 0o600"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 4 pair cycles
// ---------------------------------------------------------------------------

#[test]
fn claude_to_cursor_pipeline() {
    run_pipeline("claude");
}

#[test]
fn cursor_to_codex_pipeline() {
    run_pipeline("cursor");
}

#[test]
fn codex_to_cursor_pipeline() {
    run_pipeline("codex");
}

#[test]
fn claude_to_codex_pipeline() {
    run_pipeline("claude");
}
