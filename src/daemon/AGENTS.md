# AGENTS.md — daemon

## Purpose
Manage the daemon lifecycle: axum hook endpoint on `127.0.0.1:47823`, notify fs watcher, and the pipeline worker loop.

## Architectural decisions that constrain this module
- ARCHITECTURE.md L2: three-process model (hook receiver → fs watcher → pipeline worker).
- decisions.md gotcha #1: axum 0.8 route syntax uses `{param}`, not `:param`. CI greps for stale colon syntax.
- decisions.md gotcha #3: `event.need_rescan()` must be checked on every notify event — skipping causes silent data loss on macOS FSEvents coalesce and Linux inotify overflow.
- decisions.md gotcha #6: fs-watcher → worker bridge uses `tokio::sync::mpsc::UnboundedSender` to avoid silent drop on a full bounded channel.

## Files
- `mod.rs` — daemon lifecycle stub; axum router, watcher setup, and worker loop land in Phase 2 (TASK-05, TASK-11)

## Out of scope for v0.1
- TLS / auth on the hook endpoint
- Multi-daemon coordination
- Windows named-pipe transport (v0.4+)
