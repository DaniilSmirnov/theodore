//! A space's diff while its turn runs (docs/spaces.md §3, §6): when the
//! worktree changed, a snapshot no ref points at becomes the review's
//! head. No checkpoint, no ref; the turn's end replaces it with the real
//! checkpoint.
//!
//! ```text
//! every LIVE_EVERY ─▶ space_fingerprint ── same ──▶ wait
//!                          └─ changed ─▶ space_snapshot ── same commit ──▶ wait
//!                                            └─ new ─▶ review head = snapshot
//! turn ends ─▶ stop (waits for a refresh in flight) ─▶ checkpoint ─▶ review head = it
//! ```

use std::path::{Path, PathBuf};
use std::time::Duration;

use theodore_contract::SpaceStatus;
use theodore_domain::{PrId, Sha};
use theodore_ports::GitError;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::core::Core;
use crate::error::AppResult;

/// How often the worktree is looked at; a refresh needs it to have changed.
const LIVE_EVERY: Duration = if cfg!(test) {
    Duration::from_millis(20)
} else {
    Duration::from_secs(3)
};

/// The live diff of one running turn.
pub(crate) struct LiveDiff {
    stop: CancellationToken,
    task: JoinHandle<bool>,
}

impl LiveDiff {
    /// Stops it, waiting for a refresh in flight. `true`: the review shows
    /// a live snapshot now, which the turn's end must replace.
    pub(crate) async fn stop(self) -> bool {
        self.stop.cancel();
        self.task.await.unwrap_or(false)
    }
}

pub(crate) fn start(core: &Core, space: &str, cwd: &Path) -> LiveDiff {
    let stop = CancellationToken::new();
    let task = tokio::spawn(follow(
        core.clone(),
        space.to_string(),
        cwd.to_path_buf(),
        stop.clone(),
    ));
    LiveDiff { stop, task }
}

async fn follow(core: Core, space: String, cwd: PathBuf, stop: CancellationToken) -> bool {
    let git = core.0.deps.git.clone();
    // What the worktree looks like as the turn starts: nothing to show yet.
    let mut seen = match git.space_fingerprint(&cwd).await {
        Ok(f) => f,
        // A port without it (fakes) or a worktree git won't touch.
        Err(GitError::Unsupported(_) | GitError::NotAWorktree(_)) => return false,
        Err(_) => String::new(),
    };
    let mut shown: Vec<(PrId, Sha)> = Vec::new();
    loop {
        tokio::select! {
            _ = stop.cancelled() => break,
            _ = tokio::time::sleep(LIVE_EVERY) => {}
        }
        let Ok(now) = git.space_fingerprint(&cwd).await else {
            continue;
        };
        if now == seen {
            continue;
        }
        seen = now;
        let Ok(sha) = git.space_snapshot(&cwd).await else {
            continue;
        };
        if shown.last().is_some_and(|(_, s)| *s == sha) || stop.is_cancelled() {
            continue;
        }
        match show(&core, &space, &sha).await {
            Ok(Some(pr)) => shown.push((pr, sha)),
            Ok(None) => break,
            Err(e) => tracing::debug!(space = %space, error = %e, "live diff refresh failed"),
        }
        // Keep the last two in memory (the UI may still be loading the one
        // before); older live revisions are never asked for again.
        if shown.len() > 2 {
            let old = shown.remove(0);
            core.0.revisions.write().await.remove(&old);
        }
    }
    !shown.is_empty()
}

/// Points the space's review at `sha`. `None`: the turn is over.
async fn show(core: &Core, space: &str, sha: &Sha) -> AppResult<Option<PrId>> {
    let row = core.space_row(space).await?;
    if row.status != SpaceStatus::Running {
        return Ok(None);
    }
    let _guard = core.0.open_lock.lock().await;
    let opened = core.open_space_review_at(&row, false, Some(sha)).await?;
    Ok(Some(opened.pr))
}
