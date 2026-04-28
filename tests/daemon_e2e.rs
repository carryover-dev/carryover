//! End-to-end daemon integration test.
//!
//! Uses `tower::ServiceExt::oneshot` to send a synthetic HTTP request
//! directly into the axum router — no TCP socket needed. Verifies the
//! full path: HTTP request → hook channel → pipeline ingest → ledger
//! rows → handoff.md written.

use std::collections::HashMap;

use axum::body::Body;
use carryover::adapters::mock::{MockAdapter, MockCursor};
use carryover::adapters::{AdapterKind, RawRecord};
use carryover::daemon::hook_endpoint;
use carryover::daemon::pipeline::build_for_test;
use carryover::storage::{Ledger, LedgerRow};
use axum::http::Request;
use tokio::sync::mpsc::unbounded_channel;
use tower::ServiceExt as _;

fn mock_record(offset: u64) -> RawRecord {
    let row = LedgerRow {
        session_id: "e2e-session".to_string(),
        tool: "claude".to_string(),
        ts: offset as i64 * 1000,
        role: "user".to_string(),
        content: format!("e2e turn {offset}"),
        tool_calls_json: None,
        files_touched_json: None,
        parent_id: None,
    };
    RawRecord {
        tool: "claude".to_string(),
        payload: serde_json::to_vec(&row).unwrap(),
        offset,
    }
}

#[tokio::test]
async fn hook_post_produces_ledger_rows_and_handoff() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&dir.path().join("ledger.sqlite")).unwrap();

    // Register MockAdapter under "claude" so the hook endpoint's KNOWN_TOOLS
    // check passes, while still using synthetic in-memory records.
    let adapter = MockAdapter {
        name: "claude",
        binary: None,
        records: vec![mock_record(1), mock_record(2), mock_record(3)],
    };
    let mut adapters = HashMap::new();
    adapters.insert("claude".to_string(), AdapterKind::Mock(adapter));
    let pipeline = build_for_test(adapters, ledger.clone(), dir.path().to_path_buf());

    // Build the axum router with a channel.
    let (hook_tx, mut hook_rx) = unbounded_channel::<hook_endpoint::HookEvent>();
    let app = hook_endpoint::router(hook_tx);

    // Send a POST /hook/mock/SessionEnd directly via oneshot (no TCP needed).
    let request = Request::builder()
        .method("POST")
        .uri("/hook/claude/SessionEnd")
        .header("host", "127.0.0.1:47823")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"session_id":"e2e-session"}"#))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert!(
        response.status().is_success(),
        "hook endpoint should return 2xx, got {}",
        response.status()
    );

    // Receive the event from the channel (the router sends it synchronously).
    let evt = hook_rx
        .try_recv()
        .expect("hook event should be in channel after successful POST");
    assert_eq!(evt.tool, "claude");
    assert_eq!(evt.event, "SessionEnd");

    // Drive the pipeline directly.
    pipeline.process_hook(&evt);

    // Assert ledger received rows.
    let rows = ledger.query_recent("claude", 100).unwrap();
    assert!(
        !rows.is_empty(),
        "expected ledger rows after process_hook, got 0"
    );

    // Assert every row has required fields.
    for row in &rows {
        assert_eq!(row.tool, "claude");
        assert!(!row.session_id.is_empty());
        assert!(!row.role.is_empty());
        assert!(row.ts > 0);
    }

    // Assert cursor was persisted.
    let cursor_json = ledger
        .load_cursor("claude", "e2e-session")
        .unwrap();
    assert!(cursor_json.is_some(), "cursor should be persisted");
    let _: MockCursor = serde_json::from_str(&cursor_json.unwrap())
        .expect("cursor should deserialize as MockCursor");

    // Assert handoff.md was written and has the protocol header.
    let handoff = dir.path().join(".carryover").join("handoff.md");
    assert!(handoff.exists(), "handoff.md should be written after ingest");
    let body = std::fs::read_to_string(&handoff).unwrap();
    assert!(
        body.contains("# [CARRYOVER]"),
        "handoff should have protocol title"
    );
    assert!(
        body.contains("claude"),
        "handoff should mention source tool"
    );
}

#[tokio::test]
async fn hook_health_endpoint_returns_ok() {
    let (hook_tx, _hook_rx) = unbounded_channel::<hook_endpoint::HookEvent>();
    let app = hook_endpoint::router(hook_tx);
    let request = Request::builder()
        .method("GET")
        .uri("/health")
        .header("host", "127.0.0.1:47823")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert!(response.status().is_success());
}
