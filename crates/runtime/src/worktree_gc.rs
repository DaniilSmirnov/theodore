//! Worktree garbage collection. `GitPort::gc_worktrees` already exists and
//! is already correct (it drops everything not in a `keep` list), but
//! nothing in the running app ever called it, so a PR's worktree outlived
//! every review of it. This wires a periodic sweep around it.
//!
//! The one thing a sweep must never do is remove a worktree a run is
//! actively using (an agent mid-review, a commit-and-push in flight):
//! `WorktreeLocks` is a refcount `Core`/the agent runner can hold one of
//! for exactly that long, always unioned into the sweep's `keep` list.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use theodore_app::Core;
use theodore_contract::models;
use theodore_domain::{PrRef, PrState, PrSummary};
use theodore_ports::{GitPort, GitResult, Store, WorktreeLease};

/// Refcounts worktrees currently in use, so a periodic sweep never removes
/// one out from under whatever is holding it. Cheap to share: clone the
/// `Arc`, not the lock table.
#[derive(Debug, Default)]
pub struct WorktreeLocks {
    counts: Mutex<HashMap<PrRef, u32>>,
}

/// Releases its PR's hold when dropped.
pub struct WorktreeGuard {
    locks: Arc<WorktreeLocks>,
    pr: PrRef,
}

impl Drop for WorktreeGuard {
    fn drop(&mut self) {
        self.locks.release(&self.pr);
    }
}

impl WorktreeLocks {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Holds `pr`'s worktree until the returned guard drops. Several
    /// concurrent holds on the same PR stack (refcounted); the worktree is
    /// only eligible for a sweep once every guard has dropped.
    pub fn acquire(self: &Arc<Self>, pr: PrRef) -> WorktreeGuard {
        let mut counts = self.counts.lock().unwrap_or_else(|p| p.into_inner());
        *counts.entry(pr.clone()).or_insert(0) += 1;
        WorktreeGuard {
            locks: self.clone(),
            pr,
        }
    }

    fn release(&self, pr: &PrRef) {
        let mut counts = self.counts.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(n) = counts.get_mut(pr) {
            *n -= 1;
            if *n == 0 {
                counts.remove(pr);
            }
        }
    }

    /// Every PR currently held by at least one guard.
    pub fn snapshot(&self) -> Vec<PrRef> {
        let counts = self.counts.lock().unwrap_or_else(|p| p.into_inner());
        counts.keys().cloned().collect()
    }
}

/// Adapts `WorktreeLocks` to the `app`-facing `WorktreeLease` port: `app`
/// depends only on the trait, never on `runtime`, so this thin wrapper is
/// where the two sides meet.
pub struct WorktreeLeaseAdapter(Arc<WorktreeLocks>);

impl WorktreeLeaseAdapter {
    pub fn new(locks: Arc<WorktreeLocks>) -> Self {
        Self(locks)
    }
}

impl WorktreeLease for WorktreeLeaseAdapter {
    fn acquire(&self, pr: PrRef) -> Box<dyn std::any::Any + Send> {
        Box::new(self.0.acquire(pr))
    }
}

/// PRs to keep a worktree for: every `pr` row in `store` whose last known
/// state is `Open`/`Draft` (a closed/merged PR's worktree is swept),
/// unioned with whatever `locks` currently holds (an agent run in flight on
/// one, say, even if its PR has since closed underneath it) and with
/// `open` — every PR currently open in this session (`Core::open_pr_refs`),
/// so the worktree behind a tab the user is looking at is never swept just
/// because the host now reports it closed/merged. `Store` is sync
/// (rusqlite) — call this through `spawn_blocking` from async code.
pub fn keep_list(store: &dyn Store, locks: &WorktreeLocks, open: &[PrRef]) -> Vec<PrRef> {
    let mut keep: Vec<PrRef> = store
        .all(models::PR)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|row| serde_json::from_value::<PrSummary>(row.data).ok())
        .filter(|s| matches!(s.state, PrState::Open | PrState::Draft))
        .map(|s| s.pr)
        .collect();
    keep.extend(locks.snapshot());
    keep.extend_from_slice(open);
    keep
}

/// One GC sweep, safe to call from async code: `keep_list`'s `Store` read
/// (sync, rusqlite) runs inside `spawn_blocking` so it never blocks the
/// calling worker thread. Returns the number of worktrees removed.
pub async fn sweep(
    git: &dyn GitPort,
    store: Arc<dyn Store>,
    locks: Arc<WorktreeLocks>,
    open: Vec<PrRef>,
) -> GitResult<u32> {
    let keep = match tokio::task::spawn_blocking(move || keep_list(store.as_ref(), &locks, &open))
        .await
    {
        Ok(keep) => keep,
        // A panicked lookup must never be treated as "nothing to keep" —
        // that would make this sweep remove every worktree, including
        // ones in active use. Skip this sweep instead; the next one
        // tries again.
        Err(e) => {
            tracing::warn!(error = %e, "worktree gc: keep-list lookup panicked, skipping sweep");
            return Ok(0);
        }
    };
    git.gc_worktrees(&keep).await
}

/// Spawns a task that sweeps every `interval` for as long as the returned
/// handle isn't dropped/aborted. `core` provides the live "every PR open
/// right now" list on each tick (plan "worktree locks").
pub fn spawn_periodic(
    git: Arc<dyn GitPort>,
    store: Arc<dyn Store>,
    locks: Arc<WorktreeLocks>,
    core: Core,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            let open = core.open_pr_refs().await;
            if let Err(e) = sweep(git.as_ref(), store.clone(), locks.clone(), open).await {
                tracing::warn!(error = %e, "worktree gc sweep failed");
            }
            // Code intelligence lets go of the worktrees the sweep removed.
            core.code_intel_sweep().await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use theodore_domain::{CheckRollup, HostKind, Millis, Person, RepoRef, Sha};
    use theodore_ports::{NewAction, SyncActionCode};

    fn pr_ref(number: u64) -> PrRef {
        PrRef {
            repo: RepoRef {
                host: HostKind::Github,
                owner: "acme".into(),
                name: "widgets".into(),
            },
            number,
            host_id: None,
        }
    }

    fn summary(pr: PrRef, state: PrState) -> PrSummary {
        PrSummary {
            pr,
            title: "t".into(),
            author: Person {
                login: "a".into(),
                name: None,
                avatar_url: None,
            },
            state,
            base_ref: "main".into(),
            head_ref: "feature".into(),
            head_sha: Sha::new("deadbeef"),
            additions: 0,
            deletions: 0,
            changed_files: 0,
            reasons: Vec::new(),
            checks: CheckRollup::None,
            approvals: 0,
            required_approvals: None,
            updated_at: 0 as Millis,
            url: String::new(),
            review_decision: None,
            conflicts: false,
            created_at: None,
            stack: None,
        }
    }

    fn store_with(rows: Vec<PrSummary>) -> theodore_store::SqliteStore {
        let store = theodore_store::SqliteStore::open_in_memory().unwrap();
        let actions = rows
            .into_iter()
            .map(|s| NewAction {
                model: models::PR.into(),
                model_id: s.pr.id().0.clone(),
                action: SyncActionCode::U,
                data: Some(serde_json::to_value(&s).unwrap()),
            })
            .collect();
        store.apply(actions).unwrap();
        store
    }

    #[test]
    fn keep_list_keeps_open_and_draft_drops_closed_and_merged() {
        let open = pr_ref(1);
        let draft = pr_ref(2);
        let closed = pr_ref(3);
        let merged = pr_ref(4);
        let store = store_with(vec![
            summary(open.clone(), PrState::Open),
            summary(draft.clone(), PrState::Draft),
            summary(closed.clone(), PrState::Closed),
            summary(merged.clone(), PrState::Merged),
        ]);
        let locks = WorktreeLocks::new();

        let keep = keep_list(&store, &locks, &[]);

        assert!(keep.contains(&open));
        assert!(keep.contains(&draft));
        assert!(!keep.contains(&closed));
        assert!(!keep.contains(&merged));
    }

    #[test]
    fn keep_list_keeps_a_locked_pr_even_when_closed() {
        let closed = pr_ref(5);
        let store = store_with(vec![summary(closed.clone(), PrState::Closed)]);
        let locks = WorktreeLocks::new();
        let guard = locks.acquire(closed.clone());

        assert!(
            keep_list(&store, &locks, &[]).contains(&closed),
            "a run holding the worktree must block the sweep even though the PR is closed"
        );

        drop(guard);
        assert!(
            !keep_list(&store, &locks, &[]).contains(&closed),
            "once released, a closed PR's worktree is eligible for the sweep again"
        );
    }

    #[test]
    fn keep_list_keeps_a_currently_open_pr_even_when_the_host_reports_it_closed() {
        // A PR the user has open in a tab, whose host state flipped to
        // closed/merged after it was opened — `Core::open_pr_refs` still
        // reports it, and it must never lose its worktree out from under
        // the open tab.
        let closed = pr_ref(9);
        let store = store_with(vec![summary(closed.clone(), PrState::Merged)]);
        let locks = WorktreeLocks::new();

        assert!(
            !keep_list(&store, &locks, &[]).contains(&closed),
            "sanity: without `open`, a merged PR isn't kept"
        );
        assert!(
            keep_list(&store, &locks, std::slice::from_ref(&closed)).contains(&closed),
            "a PR in `open` must be kept even if the host reports it merged"
        );
    }

    #[test]
    fn locks_refcount_several_concurrent_holders_of_the_same_pr() {
        let pr = pr_ref(6);
        let locks = WorktreeLocks::new();
        let a = locks.acquire(pr.clone());
        let b = locks.acquire(pr.clone());

        drop(a);
        assert!(
            locks.snapshot().contains(&pr),
            "one remaining guard must still hold the lock"
        );
        drop(b);
        assert!(!locks.snapshot().contains(&pr));
    }

    /// Records the `keep` list it was called with instead of touching a
    /// real repo — enough to prove `sweep` wires `keep_list`'s result
    /// through to `GitPort::gc_worktrees` unchanged.
    #[derive(Default)]
    struct RecordingGit {
        last_keep: Mutex<Option<Vec<PrRef>>>,
    }

    #[async_trait::async_trait]
    impl GitPort for RecordingGit {
        async fn ensure_repo(&self, _repo: &RepoRef) -> GitResult<std::path::PathBuf> {
            unimplemented!()
        }
        async fn fetch_pr(&self, _pr: &PrRef) -> GitResult<Sha> {
            unimplemented!()
        }
        async fn worktree(&self, _pr: &PrRef, _head: &Sha) -> GitResult<theodore_ports::Worktree> {
            unimplemented!()
        }
        async fn remove_worktree(&self, _pr: &PrRef) -> GitResult<()> {
            unimplemented!()
        }
        async fn gc_worktrees(&self, keep: &[PrRef]) -> GitResult<u32> {
            *self.last_keep.lock().unwrap() = Some(keep.to_vec());
            Ok(keep.len() as u32)
        }
        async fn merge_base(&self, _repo: &std::path::Path, _a: &Sha, _b: &Sha) -> GitResult<Sha> {
            unimplemented!()
        }
        async fn diff(
            &self,
            _repo: &std::path::Path,
            _base: &Sha,
            _head: &Sha,
            _opts: &theodore_ports::DiffOptions,
        ) -> GitResult<theodore_domain::Revision> {
            unimplemented!()
        }
        async fn show(
            &self,
            _repo: &std::path::Path,
            _rev: &Sha,
            _path: &str,
        ) -> GitResult<Option<Vec<u8>>> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn sweep_passes_keep_list_through_to_gc_worktrees() {
        let open_pr = pr_ref(7);
        let closed = pr_ref(8);
        let store: Arc<dyn Store> = Arc::new(store_with(vec![
            summary(open_pr.clone(), PrState::Open),
            summary(closed.clone(), PrState::Closed),
        ]));
        let locks = WorktreeLocks::new();
        let git = RecordingGit::default();

        let removed = sweep(&git, store, locks, vec![]).await.unwrap();

        let last_keep = git.last_keep.lock().unwrap().clone().unwrap();
        assert!(last_keep.contains(&open_pr));
        assert!(!last_keep.contains(&closed));
        assert_eq!(removed, last_keep.len() as u32);
    }

    #[tokio::test]
    async fn sweep_unions_the_live_open_list_into_gc_worktrees() {
        let closed_but_open_in_a_tab = pr_ref(10);
        let store: Arc<dyn Store> = Arc::new(store_with(vec![summary(
            closed_but_open_in_a_tab.clone(),
            PrState::Closed,
        )]));
        let locks = WorktreeLocks::new();
        let git = RecordingGit::default();

        sweep(&git, store, locks, vec![closed_but_open_in_a_tab.clone()])
            .await
            .unwrap();

        let last_keep = git.last_keep.lock().unwrap().clone().unwrap();
        assert!(last_keep.contains(&closed_but_open_in_a_tab));
    }

    #[test]
    fn worktree_lease_adapter_forwards_to_the_underlying_locks() {
        let locks = WorktreeLocks::new();
        let adapter = WorktreeLeaseAdapter::new(locks.clone());
        let pr = pr_ref(11);

        let guard = WorktreeLease::acquire(&adapter, pr.clone());
        assert!(locks.snapshot().contains(&pr));

        drop(guard);
        assert!(!locks.snapshot().contains(&pr));
    }
}
