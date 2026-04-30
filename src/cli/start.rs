//! `carryover start` — runs the daemon in the foreground.
//!
//! Spawns the hook endpoint (axum::serve on bind_loopback), the fs
//! watcher (notify across all configured tool transcript roots), and the
//! pipeline worker that ingests events → ledger → distillers → handoff.

use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::signal;
use tokio::sync::mpsc::unbounded_channel;

use crate::cli::config::Config;
use crate::daemon::{fs_watcher::FsWatcher, hook_endpoint, pipeline::Pipeline};
use crate::storage::Ledger;

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

    // Open ledger and build pipeline.
    let ledger_path = Ledger::default_path().context("resolve ledger path")?;
    let ledger = Ledger::open(&ledger_path).context("open ledger")?;
    let config_path = Config::default_path().context("resolve config path")?;
    let config = Config::load_or_default(&config_path).context("load config")?;
    let home_dir = dirs::home_dir().context("resolve home directory")?;
    let pipeline = Arc::new(Pipeline::build(&config, ledger, home_dir));

    // Refresh the preamble of every known handoff so template/rule updates
    // propagate even when no new prompts arrive after a daemon restart.
    pipeline.refresh_all_preambles();

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

    // Pipeline drain: process hook and fs events until shutdown.
    let pl = pipeline.clone();
    let drain = tokio::spawn(async move {
        loop {
            tokio::select! {
                Some(evt) = hook_rx.recv() => {
                    pl.process_hook(&evt);
                }
                Some(evt) = watcher_rx.recv() => {
                    pl.process_watch(&evt);
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
