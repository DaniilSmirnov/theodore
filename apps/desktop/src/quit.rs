//! Quitting: stopping every space's commands and flushing telemetry, each
//! with a time limit, so the app exits even when a step hangs. A quit that
//! waits forever leaves the process running, and the next launch is a
//! second instance on the same data dir.
//!
//! ```text
//! ⌘Q / handle.exit ──▶ ExitRequested ──▶ stop spaces (≤ 4 s, once)
//!                      Exit          ──▶ telemetry (≤ 3 s, once)
//! SIGTERM / SIGINT ──▶ stop spaces (≤ 4 s) ──▶ telemetry (≤ 3 s) ──▶ handle.exit
//!                                                                  └─ 2 s ──▶ process::exit
//! second signal    ──▶ process::exit now
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use crate::CoreApi;

/// The most quitting waits for space commands and terminals to stop.
pub const STOP_SPACES_BUDGET: Duration = Duration::from_secs(4);
/// The most quitting waits for telemetry to send and flush.
pub const TELEMETRY_BUDGET: Duration = Duration::from_secs(3);
/// After a signal's cleanup: how long the event loop gets to exit on its
/// own before the process exits without it.
const EXIT_GRACE: Duration = Duration::from_secs(2);

/// Runs `f` on its own thread and waits up to `budget` for it. Returns
/// whether it finished; one that didn't keeps running, detached, until the
/// process exits. A thread, not a timeout around a future: a step that
/// blocks its thread outright (a lock, a join) never yields to a timer.
pub fn run_bounded(budget: Duration, f: impl FnOnce() + Send + 'static) -> bool {
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("theodore-quit".into())
        .spawn(move || {
            f();
            let _ = tx.send(());
        });
    if spawned.is_err() {
        return false;
    }
    rx.recv_timeout(budget).is_ok()
}

/// `run_bounded` for a future: drives it on the async runtime from its own
/// thread, up to `budget`.
pub fn block_on_bounded(budget: Duration, fut: impl Future<Output = ()> + Send + 'static) -> bool {
    run_bounded(budget, move || tauri::async_runtime::block_on(fut))
}

/// Stops every space's `run`, setup and terminal, within
/// `STOP_SPACES_BUDGET`. Left to `kill_on_drop` alone, a process that
/// exits without running destructors orphans their process groups to
/// PID 1 (docs/spaces.md "Crash recovery").
/// Once per process: `ExitRequested` and `Exit` both call it, and after a
/// signal so does the exit that follows.
pub fn stop_spaces(core: Arc<dyn CoreApi>) {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let done = block_on_bounded(STOP_SPACES_BUDGET, async move {
        core.stop_all_space_commands().await;
    });
    if !done {
        tracing::warn!(
            budget_s = STOP_SPACES_BUDGET.as_secs(),
            "space commands didn't stop in time; quitting anyway"
        );
    }
}

/// Sends what the remote sinks still hold and flushes the log file, within
/// `TELEMETRY_BUDGET`. Once per process, like `stop_spaces`.
pub fn shutdown_telemetry() {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let Some(t) = theodore_telemetry::global() else {
        return;
    };
    tracing::info!("Theodore quitting");
    if !run_bounded(TELEMETRY_BUDGET, move || t.shutdown()) {
        tracing::warn!(
            budget_s = TELEMETRY_BUDGET.as_secs(),
            "telemetry didn't shut down in time; quitting anyway"
        );
    }
}

/// SIGTERM and SIGINT quit like ⌘Q: the same bounded cleanup, then the
/// app exits. A second signal exits at once. Without this, a signal ends
/// the process on the spot, and no space's commands are stopped.
#[cfg(unix)]
pub fn exit_on_signal(handle: tauri::AppHandle, core: Arc<dyn CoreApi>) {
    use tokio::signal::unix::{SignalKind, signal};
    // Spawned first: registering a signal needs the runtime's context.
    tauri::async_runtime::spawn(async move {
        let (Ok(mut term), Ok(mut int)) = (
            signal(SignalKind::terminate()),
            signal(SignalKind::interrupt()),
        ) else {
            tracing::warn!("couldn't listen for SIGTERM/SIGINT");
            return;
        };
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
        }
        tracing::info!("quit signal received");
        // Off the runtime: the cleanup blocks its thread.
        std::thread::spawn(move || {
            stop_spaces(core);
            shutdown_telemetry();
            handle.exit(0);
            // The event loop may be the thing that's stuck.
            std::thread::sleep(EXIT_GRACE);
            std::process::exit(0);
        });
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
        }
        std::process::exit(1);
    });
}

#[cfg(not(unix))]
pub fn exit_on_signal(_handle: tauri::AppHandle, _core: Arc<dyn CoreApi>) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn a_finished_step_reports_done() {
        assert!(run_bounded(Duration::from_secs(5), || {}));
        assert!(block_on_bounded(Duration::from_secs(5), async {}));
    }

    #[test]
    fn a_hung_blocking_step_returns_within_the_budget() {
        let start = Instant::now();
        let done = run_bounded(Duration::from_millis(200), || {
            loop {
                std::thread::sleep(Duration::from_secs(60));
            }
        });
        assert!(!done);
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "{:?}",
            start.elapsed()
        );
    }

    #[test]
    fn a_hung_async_step_returns_within_the_budget() {
        let start = Instant::now();
        let done = block_on_bounded(Duration::from_millis(200), std::future::pending());
        assert!(!done);
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "{:?}",
            start.elapsed()
        );
    }
}
