//! Filesystem watcher — backup signal that catches transcript writes the
//! hook endpoint missed (tool crashed mid-write, hook misconfigured, etc).
//!
//! Subscribes to every configured tool's transcript root via the ToolSpec
//! table. Uses notify-debouncer-full so rename-pair (tmp → final) writes
//! coalesce into a single event. Always checks `event.need_rescan()` and
//! triggers a full re-scan on true: that flag is the only signal that
//! FSEvents (macOS) has coalesced events or inotify (Linux) has overflowed
//! its queue. Skipping it = silent data loss.
//!
//! See decisions.md research findings on notify 8.2.

use notify::event::EventKind;
use notify::RecursiveMode;
use notify_debouncer_full::{new_debouncer, DebounceEventHandler, DebounceEventResult};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use thiserror::Error;
use tokio::sync::mpsc::UnboundedSender;

/// Debounce window. Cursor's SQLite writes are atomic-rename pairs and the
/// adapters all read whole files; 250 ms collapses a tmp → final write
/// into a single event without delaying real updates noticeably.
pub const DEBOUNCE_WINDOW_MS: u64 = 250;

#[derive(Debug, Error)]
pub enum WatcherError {
    #[error("notify: {0}")]
    Notify(#[from] notify::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("watch root not found: {0}")]
    WatchRootMissing(PathBuf),
}

/// Event emitted when a transcript write is detected. The worker (future
/// PR) maps `path` to a tool via the ToolSpec table.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WatchEvent {
    pub path: PathBuf,
    /// True when notify reported `need_rescan()` — the consumer MUST
    /// re-scan all watched directories rather than treating this as a
    /// single-file event. Without that, FSEvents coalescing or inotify
    /// queue overflow silently drops events.
    pub rescan: bool,
}

/// Bridge from notify's internal thread to a tokio mpsc channel.
struct Bridge {
    tx: UnboundedSender<WatchEvent>,
}

impl DebounceEventHandler for Bridge {
    fn handle_event(&mut self, result: DebounceEventResult) {
        match result {
            Ok(events) => {
                for ev in events {
                    let needs_rescan = ev.need_rescan();
                    let kind = ev.kind;
                    if !is_relevant(&kind) && !needs_rescan {
                        continue;
                    }
                    for path in ev.paths.iter() {
                        // UnboundedSender::send returns Err only when the
                        // receiver is dropped; the daemon is shutting down
                        // and we ignore.
                        let _ = self.tx.send(WatchEvent {
                            path: path.clone(),
                            rescan: needs_rescan,
                        });
                    }
                    // need_rescan with no paths still needs to surface so
                    // the worker knows to do a full re-scan.
                    if needs_rescan && ev.paths.is_empty() {
                        let _ = self.tx.send(WatchEvent {
                            path: PathBuf::new(),
                            rescan: true,
                        });
                    }
                }
            }
            Err(_errors) => {
                // notify reports per-watch errors here. For v0.1 we surface
                // them via a sentinel rescan event so the worker triggers
                // full re-detection of the watched roots; the daemon log
                // (future PR) will wrap this in structured event output.
                let _ = self.tx.send(WatchEvent {
                    path: PathBuf::new(),
                    rescan: true,
                });
            }
        }
    }
}

fn is_relevant(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
    )
}

/// A live watcher. Drop it to stop watching.
pub struct FsWatcher {
    /// Hold the debouncer so its background thread keeps running.
    /// `_` prefix is intentional — we never read it back.
    _debouncer: notify_debouncer_full::Debouncer<
        notify::RecommendedWatcher,
        notify_debouncer_full::RecommendedCache,
    >,
    /// The roots we're watching, for diagnostics.
    pub roots: Vec<PathBuf>,
}

impl FsWatcher {
    /// Spawn a watcher on every existing path in `roots`. Missing roots
    /// are skipped silently — a tool that isn't installed doesn't need a
    /// watcher. Returns the live watcher; events flow into `tx`.
    pub fn spawn(
        roots: Vec<PathBuf>,
        tx: UnboundedSender<WatchEvent>,
    ) -> Result<Self, WatcherError> {
        let bridge = Bridge { tx };
        let mut debouncer = new_debouncer(Duration::from_millis(DEBOUNCE_WINDOW_MS), None, bridge)?;

        let mut watched = Vec::new();
        for root in &roots {
            if !root.exists() {
                continue;
            }
            debouncer
                .watch(root, RecursiveMode::Recursive)
                .map_err(WatcherError::Notify)?;
            watched.push(root.clone());
        }

        if watched.is_empty() {
            // No tool installed at all — surface as missing to the caller.
            return Err(WatcherError::WatchRootMissing(
                roots.into_iter().next().unwrap_or_default(),
            ));
        }

        Ok(Self {
            _debouncer: debouncer,
            roots: watched,
        })
    }

    /// Spawn watchers for every transcript path resolved by ToolSpec.
    /// Convenience for the daemon entrypoint.
    pub fn spawn_for_all_tools(tx: UnboundedSender<WatchEvent>) -> Result<Self, WatcherError> {
        let home = dirs::home_dir()
            .ok_or_else(|| WatcherError::WatchRootMissing(PathBuf::from("$HOME")))?;

        let mut roots = Vec::new();
        for spec in crate::toolspec::specs::ALL_TOOLS {
            for path_spec in spec.transcript_paths {
                if let Some(path) = path_spec.resolve_first_existing(&home) {
                    // For files (Cursor's state.vscdb), watch the parent dir.
                    let to_watch = if path.is_file() {
                        path.parent().map(Path::to_path_buf).unwrap_or(path)
                    } else {
                        path
                    };
                    if !roots.contains(&to_watch) {
                        roots.push(to_watch);
                    }
                }
            }
        }

        Self::spawn(roots, tx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::sync::mpsc::unbounded_channel;

    // ---------------------------------------------------------------------------
    // Unit tests — no filesystem I/O
    // ---------------------------------------------------------------------------

    #[test]
    fn is_relevant_filters_metadata_only_events() {
        use notify::event::{AccessKind, CreateKind, ModifyKind, RemoveKind};

        // Access events must be filtered out.
        assert!(!is_relevant(&EventKind::Access(AccessKind::Any)));
        assert!(!is_relevant(&EventKind::Access(AccessKind::Open(
            notify::event::AccessMode::Any
        ))));

        // Modify, Create, Remove must pass through.
        assert!(is_relevant(&EventKind::Modify(ModifyKind::Any)));
        assert!(is_relevant(&EventKind::Modify(ModifyKind::Data(
            notify::event::DataChange::Content
        ))));
        assert!(is_relevant(&EventKind::Create(CreateKind::Any)));
        assert!(is_relevant(&EventKind::Create(CreateKind::File)));
        assert!(is_relevant(&EventKind::Remove(RemoveKind::Any)));
        assert!(is_relevant(&EventKind::Remove(RemoveKind::File)));

        // Other is not relevant.
        assert!(!is_relevant(&EventKind::Other));
    }

    #[test]
    fn bridge_handles_error_with_rescan_sentinel() {
        let (tx, mut rx) = unbounded_channel::<WatchEvent>();
        let mut bridge = Bridge { tx };

        // Simulate the error path by constructing a Vec<notify::Error>.
        let fake_error = notify::Error::new(notify::ErrorKind::Generic("test error".to_string()));
        bridge.handle_event(Err(vec![fake_error]));

        let event = rx.try_recv().expect("sentinel rescan event must be sent");
        assert!(event.rescan, "error path must set rescan=true");
        assert_eq!(event.path, PathBuf::new(), "error sentinel has empty path");
    }

    // ---------------------------------------------------------------------------
    // Error-path tests — no existing roots
    // ---------------------------------------------------------------------------

    #[test]
    fn spawn_with_no_existing_roots_errors() {
        let (tx, _rx) = unbounded_channel::<WatchEvent>();
        let nonexistent = vec![
            PathBuf::from("/tmp/carryover_nonexistent_a_xyz123"),
            PathBuf::from("/tmp/carryover_nonexistent_b_xyz456"),
        ];
        let result = FsWatcher::spawn(nonexistent.clone(), tx);
        assert!(
            matches!(result, Err(WatcherError::WatchRootMissing(_))),
            "expected WatchRootMissing, got: {:?}",
            result.err()
        );
    }

    // ---------------------------------------------------------------------------
    // Integration tests — require filesystem
    // ---------------------------------------------------------------------------

    #[test]
    fn spawn_skips_missing_roots_but_keeps_existing() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, _rx) = unbounded_channel::<WatchEvent>();

        let roots = vec![
            dir.path().to_path_buf(),
            PathBuf::from("/tmp/carryover_nonexistent_skip_xyz"),
        ];

        let watcher =
            FsWatcher::spawn(roots, tx).expect("spawn must succeed with one existing root");
        assert_eq!(watcher.roots.len(), 1, "only the existing root is watched");
        assert_eq!(watcher.roots[0], dir.path());
    }

    /// macOS reports paths under `/private/var/folders/...` while
    /// `tempfile::tempdir()` returns `/var/folders/...`. Canonicalize both
    /// sides before comparing so FSEvents canonicalization doesn't break
    /// the assertion. Linux is unaffected (canonical path = the same path).
    fn canonical(p: &std::path::Path) -> PathBuf {
        std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
    }

    /// FSEvents on macOS has higher and more variable latency than inotify
    /// on Linux. 5 s is generous enough to cover macOS-14 GitHub runners
    /// while still failing fast in normal Linux runs.
    const EVENT_TIMEOUT_SECS: u64 = 5;

    /// Drain events from `rx` until one is found whose path is under
    /// `dir`. Returns the first matching event, or panics on timeout.
    async fn await_event_under(
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<WatchEvent>,
        dir: &std::path::Path,
        label: &str,
    ) -> WatchEvent {
        let dir_canon = canonical(dir);
        let deadline = std::time::Instant::now() + Duration::from_secs(EVENT_TIMEOUT_SECS);
        loop {
            let remaining = deadline
                .checked_duration_since(std::time::Instant::now())
                .unwrap_or(Duration::from_millis(0));
            let evt = tokio::time::timeout(remaining, rx.recv())
                .await
                .unwrap_or_else(|_| panic!("timeout waiting for {label} under {dir_canon:?}"))
                .expect("channel closed");
            let evt_canon = canonical(&evt.path);
            if evt_canon == dir_canon || evt_canon.starts_with(&dir_canon) {
                return evt;
            }
            // Otherwise the event is unrelated noise (e.g. parent dir
            // touch from tempdir setup); keep waiting.
        }
    }

    #[tokio::test]
    async fn detects_file_creation() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = unbounded_channel::<WatchEvent>();
        let _watcher = FsWatcher::spawn(vec![dir.path().to_path_buf()], tx).expect("spawn failed");

        // Wait briefly for the watcher to be ready (notify ~50ms Linux,
        // FSEvents ~250ms macOS).
        tokio::time::sleep(Duration::from_millis(300)).await;

        let new_file = dir.path().join("hello.txt");
        std::fs::write(&new_file, "hi").unwrap();

        let _evt = await_event_under(&mut rx, dir.path(), "creation").await;
    }

    #[tokio::test]
    async fn detects_file_modification() {
        let dir = tempfile::tempdir().unwrap();
        let existing_file = dir.path().join("existing.txt");
        // Create the file BEFORE spawning the watcher.
        std::fs::write(&existing_file, "initial").unwrap();

        let (tx, mut rx) = unbounded_channel::<WatchEvent>();
        let _watcher = FsWatcher::spawn(vec![dir.path().to_path_buf()], tx).expect("spawn failed");

        tokio::time::sleep(Duration::from_millis(300)).await;

        // Modify after spawn.
        std::fs::write(&existing_file, "modified").unwrap();

        let _evt = await_event_under(&mut rx, dir.path(), "modification").await;
    }

    #[tokio::test]
    async fn detects_file_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = unbounded_channel::<WatchEvent>();
        let _watcher = FsWatcher::spawn(vec![dir.path().to_path_buf()], tx).expect("spawn failed");

        tokio::time::sleep(Duration::from_millis(300)).await;

        let file = dir.path().join("will_be_deleted.txt");
        std::fs::write(&file, "bye").unwrap();

        // Wait for the create event, then trigger the delete.
        let _ = await_event_under(&mut rx, dir.path(), "delete-precursor-create").await;
        std::fs::remove_file(&file).unwrap();

        // At least one further event (the delete) must arrive.
        let _evt = await_event_under(&mut rx, dir.path(), "deletion").await;
    }

    #[tokio::test]
    async fn debounce_coalesces_rapid_writes() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = unbounded_channel::<WatchEvent>();
        let _watcher = FsWatcher::spawn(vec![dir.path().to_path_buf()], tx).expect("spawn failed");

        tokio::time::sleep(Duration::from_millis(100)).await;

        let file = dir.path().join("burst.txt");
        for i in 0u8..5 {
            std::fs::write(&file, [i]).unwrap();
        }

        // Collect events for slightly longer than the debounce window.
        let mut count = 0usize;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(700);
        loop {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(_)) => count += 1,
                Ok(None) => break,
                Err(_) => break, // deadline elapsed
            }
        }

        assert!(
            count < 5,
            "debounce should coalesce rapid writes: got {count} events, expected < 5"
        );
        assert!(count >= 1, "at least one event must have arrived");
    }

    /// Triggering `need_rescan()` deliberately requires overflowing an inotify
    /// queue or hitting FSEvents coalescing — not feasible in a unit test.
    /// The Bridge error path (which emits a rescan sentinel) is covered by
    /// `bridge_handles_error_with_rescan_sentinel` above.
    #[ignore = "need_rescan() can only be triggered by OS-level queue overflow or FSEvents coalesce; not reliably reproducible in a unit test environment"]
    #[tokio::test]
    async fn rescan_flag_propagates() {
        // Covered indirectly via bridge_handles_error_with_rescan_sentinel.
    }

    /// Relies on the test machine NOT having Claude/Cursor/Codex installed at
    /// their default paths. Fragile on developer machines where one or more
    /// tools are installed; run in CI only.
    #[ignore = "depends on no AI tools being installed at default paths; fragile on dev machines"]
    #[tokio::test]
    async fn spawn_for_all_tools_returns_error_when_no_tool_installed() {
        let (tx, _rx) = unbounded_channel::<WatchEvent>();
        let result = FsWatcher::spawn_for_all_tools(tx);
        assert!(
            matches!(result, Err(WatcherError::WatchRootMissing(_))),
            "expected WatchRootMissing when no tools installed, got: {:?}",
            result.err()
        );
    }
}
