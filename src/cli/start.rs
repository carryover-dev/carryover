//! `carryover start` — runs the daemon in the foreground.
//!
//! Spawns the hook endpoint (axum::serve on bind_loopback), the fs
//! watcher (notify across all configured tool transcript roots), and a
//! pipeline-stub worker that just drains the channels. The full
//! pipeline (writes ledger rows, runs distillers, calls publish())
//! lands in a follow-up PR; for v0.1 the stub keeps the channels
//! healthy so the daemon survives the systemd-managed lifetime.
//!
//! Integration smoke tests belong in `tests/cross_tool.rs` (future PR)
//! since they require binding a real port and spawning long-lived tasks.

use anyhow::{Context, Result};
use tokio::signal;
use tokio::sync::mpsc::unbounded_channel;

use crate::daemon::{fs_watcher::FsWatcher, hook_endpoint};

pub async fn run() -> Result<()> {
    println!("Carryover daemon starting...");

    // Hook endpoint listener (bind 127.0.0.1:47823).
    let listener = hook_endpoint::bind_loopback()
        .await
        .context("bind hook endpoint")?;
    let local = listener.local_addr().context("local_addr")?;
    println!("Hook endpoint listening on {local}");

    // Worker channels.
    let (hook_tx, mut hook_rx) = unbounded_channel::<hook_endpoint::HookEvent>();
    let (watcher_tx, mut watcher_rx) = unbounded_channel::<crate::daemon::fs_watcher::WatchEvent>();

    // Spawn fs watcher (best-effort — if no tool installed, log + continue).
    let watcher = match FsWatcher::spawn_for_all_tools(watcher_tx) {
        Ok(w) => {
            println!("fs watcher subscribed to {} root(s)", w.roots.len());
            Some(w)
        }
        Err(e) => {
            eprintln!("fs watcher: not started ({e}); hook endpoint still active");
            None
        }
    };

    // Pipeline stub: drain both channels until shutdown. Future PRs
    // wire this into adapters → ledger → distillers → publish.
    let drain = tokio::spawn(async move {
        loop {
            tokio::select! {
                Some(_evt) = hook_rx.recv() => {
                    // TODO(daemon-pipeline): adapter dispatch + ledger write.
                }
                Some(_evt) = watcher_rx.recv() => {
                    // TODO(daemon-pipeline): same as above for fs events.
                }
                else => break,
            }
        }
    });

    // axum serve, with graceful shutdown on Ctrl-C / SIGTERM.
    let app = hook_endpoint::router(hook_tx);
    let server = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal());
    if let Err(e) = server.await {
        eprintln!("hook endpoint server error: {e}");
    }

    drop(watcher);
    drain.abort();
    println!("Carryover daemon stopped.");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c().await.ok();
    };

    #[cfg(unix)]
    {
        use signal::unix::{signal, SignalKind};
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => {
                ctrl_c.await;
                return;
            }
        };
        tokio::select! {
            _ = ctrl_c => {}
            _ = sigterm.recv() => {}
        }
    }

    #[cfg(not(unix))]
    {
        ctrl_c.await;
    }
}
