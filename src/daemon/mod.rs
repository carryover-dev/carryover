//! Daemon lifecycle: hook endpoint on `127.0.0.1:47823` + fs watcher + pipeline worker.
//!
//! See ARCHITECTURE.md L2 for the three-process model.
//! axum 0.8 route syntax uses `{param}` (not `:param`) — see decisions.md gotcha #1.
//! The fs-watcher → worker bridge uses `tokio::sync::mpsc::UnboundedSender` to avoid
//! silent event drop on a full bounded channel (decisions.md gotcha #6).
