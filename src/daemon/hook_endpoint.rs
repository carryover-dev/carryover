//! HTTP hook endpoint — receives transcript-update notifications from the
//! AI tools' hook stubs and queues work for the pipeline worker.
//!
//! Binds to 127.0.0.1:47823 only. The endpoint is the LOWER-LATENCY signal
//! for transcript writes; the fs watcher is the BACKUP that catches anything
//! the hook stub misses (tool crashes mid-write, hook misconfigured, etc).
//!
//! See decisions.md D3 (axum 0.8 `{param}` syntax) and the research finding
//! that uses `UnboundedSender` for symmetry with notify's EventHandler.
//!
//! ## Defense layers
//!
//! 1. **Loopback bind only.** `bind_loopback` hardcodes 127.0.0.1; there is
//!    no env-var or config knob to override.
//! 2. **DNS-rebinding guard.** A middleware rejects any request whose `Host`
//!    header is not `127.0.0.1:<port>` or `localhost:<port>`. Without this,
//!    a browser tab on `evil.com` could resolve `evil.example.com` to
//!    127.0.0.1 and POST hook events into the daemon.
//! 3. **Body size cap.** The router applies a 64 KiB request-body limit so
//!    a malicious local process cannot exhaust memory by POSTing large
//!    payloads.
//! 4. **Tool allowlist.** Only known tool names are accepted; anything else
//!    returns 400 BAD REQUEST so future consumers cannot be tricked into
//!    interpreting attacker-controlled URL params as filesystem paths.
//! 5. **transcript_path scrubbing.** Any `..` component in the path is
//!    rejected at the boundary even though the adapter layer also performs
//!    a containment check at file-open time.

use axum::{
    extract::{DefaultBodyLimit, Path as AxPath, State},
    http::{Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::{Component, PathBuf};
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::mpsc::UnboundedSender;

pub const LOOPBACK_PORT: u16 = 47823;

/// Maximum request body size accepted by the hook endpoint. A real hook
/// envelope is well under 1 KiB; 64 KiB is generous slack. Anything larger
/// is rejected with 413 PAYLOAD TOO LARGE.
pub const MAX_BODY_BYTES: usize = 64 * 1024;

/// Tools recognized by v0.1. Adding a new tool means updating this list AND
/// the ToolSpec table in `src/toolspec/specs.rs`.
pub const KNOWN_TOOLS: &[&str] = &["claude", "cursor", "codex"];

#[derive(Debug, Error)]
pub enum HookEndpointError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("port {0} is already in use")]
    PortInUse(u16),
}

/// Pipeline event published by the endpoint when a hook fires.
/// The worker (future PR) consumes these and dispatches to the right adapter.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HookEvent {
    pub tool: String,
    pub event: String,
    pub transcript_path: Option<PathBuf>,
    pub session_id: Option<String>,
    /// Tool-specific extra metadata (kept opaque so the schema can grow per tool).
    #[serde(default, flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Inbound JSON envelope on POST /hook/{tool}/{event}. Mirrors HookEvent
/// minus the tool/event fields (those come from the URL path).
#[derive(Debug, Deserialize)]
pub struct HookPayload {
    pub transcript_path: Option<PathBuf>,
    pub session_id: Option<String>,
    #[serde(default, flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone)]
struct AppState {
    tx: Arc<UnboundedSender<HookEvent>>,
}

/// Build the axum Router with all routes, body-size limit, and the
/// loopback-host middleware.
pub fn router(tx: UnboundedSender<HookEvent>) -> Router {
    let state = AppState { tx: Arc::new(tx) };
    Router::new()
        .route("/health", get(health_handler))
        .route("/hook/{tool}/{event}", post(hook_handler))
        .layer(middleware::from_fn(require_loopback_host))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state)
}

/// Bind to 127.0.0.1:LOOPBACK_PORT. Returns the bound TcpListener.
/// Returns `PortInUse` if the port is already occupied.
pub async fn bind_loopback() -> Result<tokio::net::TcpListener, HookEndpointError> {
    let addr = SocketAddr::from(([127, 0, 0, 1], LOOPBACK_PORT));
    tokio::net::TcpListener::bind(addr).await.map_err(|e| {
        if matches!(e.kind(), std::io::ErrorKind::AddrInUse) {
            HookEndpointError::PortInUse(LOOPBACK_PORT)
        } else {
            HookEndpointError::Io(e)
        }
    })
}

/// Convenience for tests: bind to 127.0.0.1:0 (any free port). Returns the
/// listener AND the resolved address so the test can know where to POST.
pub async fn bind_test() -> Result<(tokio::net::TcpListener, SocketAddr), HookEndpointError> {
    let addr = SocketAddr::from(([127, 0, 0, 1], 0));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    Ok((listener, local))
}

/// Reject any request whose `Host` header is not `127.0.0.1:*` or
/// `localhost:*`. This blocks DNS-rebinding attacks where a malicious page
/// resolves a hostile name to 127.0.0.1 and submits cross-origin POSTs.
async fn require_loopback_host(req: Request<axum::body::Body>, next: Next) -> Response {
    let host = req
        .headers()
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let host_only = host.split(':').next().unwrap_or("");
    if host_only == "127.0.0.1" || host_only == "localhost" {
        next.run(req).await
    } else {
        StatusCode::FORBIDDEN.into_response()
    }
}

async fn health_handler() -> impl IntoResponse {
    (StatusCode::OK, Json(serde_json::json!({"status": "ok"})))
}

async fn hook_handler(
    AxPath((tool, event)): AxPath<(String, String)>,
    State(state): State<AppState>,
    Json(payload): Json<HookPayload>,
) -> Response {
    // Tool allowlist. Returning 400 here means a future consumer that uses
    // HookEvent.tool to look up filesystem paths cannot be tricked into
    // accepting traversal-style components like `..` or absolute paths.
    if !KNOWN_TOOLS.contains(&tool.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "unknown tool", "allowed": KNOWN_TOOLS})),
        )
            .into_response();
    }

    // Defensive: scrub `..` from transcript_path even though the adapter
    // layer enforces a containment check at file-open time. Belt and braces.
    let transcript_path = match payload.transcript_path {
        Some(p) if path_has_parent_component(&p) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "transcript_path may not contain `..` components",
                })),
            )
                .into_response();
        }
        other => other,
    };

    let evt = HookEvent {
        tool,
        event,
        transcript_path,
        session_id: payload.session_id,
        extra: payload.extra,
    };
    // UnboundedSender::send only fails when the receiver is dropped (channel closed).
    // In that case the daemon is shutting down; we still return 200 so the
    // hook stub doesn't retry into a vanishing endpoint.
    let _ = state.tx.send(evt);
    (StatusCode::OK, Json(serde_json::json!({"queued": true}))).into_response()
}

fn path_has_parent_component(p: &std::path::Path) -> bool {
    p.components().any(|c| matches!(c, Component::ParentDir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn make_router() -> (Router, tokio::sync::mpsc::UnboundedReceiver<HookEvent>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (router(tx), rx)
    }

    /// Build a request with the loopback Host header set so the middleware
    /// accepts it. Tests that want to exercise the rebinding guard set
    /// the Host header explicitly.
    fn loopback_req(method: &str, uri: &str, body: Body) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "127.0.0.1:47823")
            .header("content-type", "application/json")
            .body(body)
            .unwrap()
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let (app, _rx) = make_router();
        let response = app
            .oneshot(loopback_req("GET", "/health", Body::empty()))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ok");
    }

    #[tokio::test]
    async fn hook_route_accepts_path_params_and_pushes_event() {
        let (app, mut rx) = make_router();
        let response = app
            .oneshot(loopback_req(
                "POST",
                "/hook/claude/SessionStart",
                Body::from(r#"{"session_id": "abc123"}"#),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Drain the channel: the event MUST have arrived. Using `_rx`
        // would silently swallow a regression where send() fails because
        // the receiver was dropped.
        let evt = rx.try_recv().expect("hook event must be queued");
        assert_eq!(evt.tool, "claude");
        assert_eq!(evt.event, "SessionStart");
        assert_eq!(evt.session_id.as_deref(), Some("abc123"));
    }

    #[tokio::test]
    async fn hook_route_uses_axum08_curly_braces() {
        // Verify {tool}/{event} syntax works — colon syntax would produce a 404
        // because axum 0.8 treats `:tool` as a literal character, not a capture.
        let (app, mut rx) = make_router();
        let response = app
            .oneshot(loopback_req(
                "POST",
                "/hook/cursor/sessionStart",
                Body::from(r#"{}"#),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let evt = rx.try_recv().expect("hook event must be queued");
        assert_eq!(evt.tool, "cursor");
        assert_eq!(evt.event, "sessionStart");
    }

    #[tokio::test]
    async fn hook_event_pushed_to_channel() {
        let (app, mut rx) = make_router();
        app.oneshot(loopback_req(
            "POST",
            "/hook/codex/SessionStart",
            Body::from(r#"{"transcript_path": "/tmp/abc.jsonl", "session_id": "s1"}"#),
        ))
        .await
        .unwrap();

        let evt = rx.try_recv().expect("hook event must be queued");
        assert_eq!(evt.tool, "codex");
        assert_eq!(evt.event, "SessionStart");
        assert_eq!(evt.transcript_path, Some(PathBuf::from("/tmp/abc.jsonl")));
        assert_eq!(evt.session_id.as_deref(), Some("s1"));
    }

    #[tokio::test]
    async fn hook_event_extra_fields_preserved() {
        let (app, mut rx) = make_router();
        app.oneshot(loopback_req(
            "POST",
            "/hook/claude/PreCompact",
            Body::from(r#"{"cwd": "/synthetic/path/1", "version": "2.0.0"}"#),
        ))
        .await
        .unwrap();

        let evt = rx.try_recv().expect("hook event must be queued");
        assert_eq!(evt.extra.get("cwd").unwrap(), "/synthetic/path/1");
        assert_eq!(evt.extra.get("version").unwrap(), "2.0.0");
    }

    #[tokio::test]
    async fn unknown_tool_is_rejected() {
        let (app, mut rx) = make_router();
        let response = app
            .oneshot(loopback_req(
                "POST",
                "/hook/evil-tool/SessionStart",
                Body::from(r#"{}"#),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        // No event should reach the channel.
        assert!(rx.try_recv().is_err(), "no event for rejected tool");
    }

    #[tokio::test]
    async fn parent_dir_in_transcript_path_is_rejected() {
        let (app, mut rx) = make_router();
        let response = app
            .oneshot(loopback_req(
                "POST",
                "/hook/claude/SessionStart",
                Body::from(r#"{"transcript_path": "/tmp/../etc/passwd"}"#),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(rx.try_recv().is_err(), "no event for traversal attempt");
    }

    #[tokio::test]
    async fn dns_rebinding_request_is_rejected() {
        // Browser-style cross-origin attack: malicious page resolves
        // evil.example.com to 127.0.0.1 and POSTs. Without the Host check
        // the request would land in the channel.
        let (app, mut rx) = make_router();
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/hook/claude/SessionStart")
                    .header("host", "evil.example.com")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(rx.try_recv().is_err(), "no event for rebinding attempt");
    }

    #[tokio::test]
    async fn body_size_limit_rejects_oversized_payload() {
        let (app, mut rx) = make_router();
        // 128 KiB > MAX_BODY_BYTES (64 KiB)
        let huge = format!(
            r#"{{"session_id":"{}"}}"#,
            "a".repeat(MAX_BODY_BYTES + 1024)
        );
        let response = app
            .oneshot(loopback_req(
                "POST",
                "/hook/claude/SessionStart",
                Body::from(huge),
            ))
            .await
            .unwrap();
        // axum returns 413 for body limit; either 413 or 400 is acceptable as
        // long as it's NOT 200.
        assert_ne!(response.status(), StatusCode::OK);
        assert!(rx.try_recv().is_err(), "no event for oversized payload");
    }

    #[tokio::test]
    async fn bind_test_returns_random_port() {
        let (a, addr_a) = bind_test().await.unwrap();
        let (b, addr_b) = bind_test().await.unwrap();
        assert_ne!(addr_a.port(), addr_b.port());
        // both are 127.0.0.1
        assert!(addr_a.ip().is_loopback());
        assert!(addr_b.ip().is_loopback());
        drop(a);
        drop(b);
    }

    #[tokio::test]
    async fn port_in_use_returns_specific_error() {
        // Bind once on port 47823 (or skip if it's already in use by another
        // test/process — rare but possible). Then a second bind_loopback
        // must return the typed PortInUse variant from the actual function
        // under test, not a hand-rolled error.
        let first = match bind_loopback().await {
            Ok(l) => l,
            Err(_) => return, // port already taken externally; can't run this test
        };
        let second = bind_loopback().await;
        match second {
            Err(HookEndpointError::PortInUse(p)) => assert_eq!(p, LOOPBACK_PORT),
            Ok(_) => panic!("second bind unexpectedly succeeded"),
            Err(e) => panic!("expected PortInUse, got {e:?}"),
        }
        drop(first);
    }

    #[tokio::test]
    async fn unknown_route_returns_404() {
        let (app, _rx) = make_router();
        let response = app
            .oneshot(loopback_req("POST", "/nope", Body::empty()))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn health_path_only_accepts_get() {
        let (app, _rx) = make_router();
        let response = app
            .oneshot(loopback_req("POST", "/health", Body::empty()))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[test]
    fn loopback_constant_is_47823() {
        assert_eq!(LOOPBACK_PORT, 47823);
    }
}
