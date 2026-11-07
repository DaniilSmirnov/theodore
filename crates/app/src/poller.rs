//! Poller (plan §6.6): ETag polling of open PRs, cursor polling of the
//! inbox. A new head runs the push pipeline: diff, classify, clear reads of
//! changed hunks, dismiss their files, regroup (queued behind a run already
//! going), wake `NextPush` snoozes.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use theodore_contract::{Ephemeral, HunkRead, InboxState, ToastLevel, models};
use theodore_domain::anchors::Remapped;
use theodore_domain::snooze::PrEvent;
use theodore_domain::{Anchor, HunkId, PrDetail, PrId, PrRef, Revision, Sha, ViewedState};
use theodore_ports::{DiffOptions, NewAction, SyncActionCode};

use crate::core::{Core, Inner, RevisionEntry};
use crate::usecases::review_config::{ConfigBase, ReviewSource};

const OPEN_PR_POLL_MS: i64 = 15_000;
const INBOX_ERROR_RETRY_MS: i64 = 30_000;
const DEFAULT_INBOX_POLL_MS: i64 = 15_000;
/// Your own open PRs: one search a minute, plus a conditional fetch for each
/// one it stopped returning.
const AUTHORED_POLL_MS: i64 = 60_000;
/// Real-time tick for noticing a `Clock`-measured deadline is due. Kept
/// short and wall-clock-based (not through `Clock`) so a test's single
/// `advance()` jump is never missed while this task is busy elsewhere —
/// unlike a freshly armed `Clock::sleep`, a recurring real timer doesn't
/// need to be re-armed exactly before the jump lands.
const REAL_TICK_MS: u64 = 20;
/// A host project's page data is refreshed in the background once it's
/// this old.
const PROJECT_REFRESH_AFTER_MS: i64 = 10 * 60_000;
/// At most one background project refresh per this interval: projects
/// that all went stale together are spread out, not fetched in a burst.
const PROJECT_STAGGER_MS: i64 = 30_000;

/// The first probe after the host stops answering, doubling per failed
/// probe up to `PROBE_MAX_MS`.
pub(crate) const PROBE_FIRST_MS: i64 = 30_000;
pub(crate) const PROBE_MAX_MS: i64 = 5 * 60_000;

pub(crate) async fn run(inner: Arc<Inner>) {
    tokio::join!(
        poll_open_prs(inner.clone()),
        poll_inbox(inner.clone()),
        poll_authored(inner.clone()),
        poll_projects(inner.clone()),
        poll_spaces(inner.clone()),
        probe_host(inner)
    );
}

/// When the next "is the host back?" probe is due. Armed by `note_host`
/// when health goes offline, cleared when it comes back.
///
/// ```text
/// online ──read fails (Network)──▶ offline: pollers skip host calls
///                                      │ probe at +30 s
///                                      ├─ fails ─▶ next at +60 s, +120 s, … ≤ 5 min
///                                      └─ answers ─▶ note_host flips online ─▶ polling resumes
/// ```
#[derive(Debug, Default)]
pub(crate) struct HostProbe {
    due: Option<i64>,
    delay_ms: i64,
}

impl HostProbe {
    /// Health just went offline: first probe in `PROBE_FIRST_MS`.
    pub(crate) fn arm(&mut self, now: i64) {
        self.delay_ms = PROBE_FIRST_MS;
        self.due = Some(now + self.delay_ms);
    }

    /// Health is back: nothing to probe.
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    /// Whether a probe is due at `now` (taking it).
    fn take_due(&mut self, now: i64) -> bool {
        match self.due {
            Some(due) if now >= due => {
                self.due = None;
                true
            }
            // Offline with nothing armed (it started offline): arm now.
            None if self.delay_ms == 0 => {
                self.arm(now);
                false
            }
            _ => false,
        }
    }

    /// The probe failed: try again after twice as long, up to the cap.
    pub(crate) fn failed(&mut self, now: i64) {
        self.delay_ms = (self.delay_ms * 2).clamp(PROBE_FIRST_MS, PROBE_MAX_MS);
        self.due = Some(now + self.delay_ms);
    }

    #[cfg(test)]
    pub(crate) fn delay_ms(&self) -> i64 {
        self.delay_ms
    }
}

/// While the host is unreachable, the only host call the pollers make: one
/// cheap read on the `HostProbe` schedule. Its outcome goes through
/// `note_host`, so an answer flips health back online and the pollers pick
/// up where they left off.
async fn probe_host(inner: Arc<Inner>) {
    let core = Core(inner.clone());
    loop {
        // Held while signed out where sign-in is required (no host calls).
        if !core.account_hold().await {
            return;
        }
        if !core.host_online() {
            let now = inner.deps.clock.now();
            let due = inner
                .host_probe
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take_due(now);
            if due {
                probe_once(&core).await;
                if !core.host_online() {
                    let now = inner.deps.clock.now();
                    let mut probe = inner.host_probe.lock().unwrap_or_else(|p| p.into_inner());
                    probe.failed(now);
                    tracing::debug!(next_in_ms = probe.delay_ms, "host still unreachable");
                }
            }
        }
        tokio::select! {
            _ = inner.shutdown.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_millis(REAL_TICK_MS)) => {},
        }
    }
}

/// One probe: the inbox's `changed_since` (what polling would ask anyway;
/// the cursor isn't advanced, so the inbox poller still sees the changes),
/// else — a host without it — a conditional fetch of an open PR, its etag
/// left as is so the PR poller still gets the change.
async fn probe_once(core: &Core) {
    let store = core.0.deps.store.clone();
    let cursor: Option<String> = tokio::task::spawn_blocking(move || store.kv_get("inbox:cursor"))
        .await
        .ok()
        .and_then(|r| r.ok())
        .flatten()
        .and_then(|v| v.as_str().map(str::to_string));
    let result = core.0.deps.host.changed_since(cursor.as_deref()).await;
    if !matches!(result, Err(theodore_ports::HostError::Unsupported(_))) {
        core.note_host(&result);
        return;
    }
    let prs: Vec<PrId> = { core.0.current_head.read().await.keys().cloned().collect() };
    for pr in prs {
        let Some(pr_ref) = core.pr_ref(&pr).await else {
            continue;
        };
        if pr_ref.repo.host == theodore_domain::HostKind::Local {
            continue;
        }
        let etag = core.0.etags.read().await.get(&pr).cloned();
        let fetched = core.0.deps.host.get_pr(&pr_ref, etag.as_deref()).await;
        core.note_host(&fetched);
        return;
    }
}

/// Shipped spaces (docs/spaces.md §1): notices a space's PR merging and
/// archives done spaces after `spaces.auto_archive_days`.
const SPACES_SWEEP_MS: i64 = 60_000;

async fn poll_spaces(inner: Arc<Inner>) {
    let core = Core(inner.clone());
    let mut next_due = inner.deps.clock.now() + SPACES_SWEEP_MS;
    loop {
        // Held while signed out where sign-in is required (no host calls).
        if !core.account_hold().await {
            return;
        }
        let now = inner.deps.clock.now();
        if now >= next_due {
            next_due = now + SPACES_SWEEP_MS;
            core.sweep_spaces().await;
        }
        tokio::select! {
            _ = inner.shutdown.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_millis(REAL_TICK_MS)) => {},
        }
    }
}

/// Keeps host projects' PR lists fresh without the page open: every
/// `PROJECT_STAGGER_MS`, the stalest project older than
/// `PROJECT_REFRESH_AFTER_MS` gets one quiet refresh. Skipped while
/// offline — the banner already says so, and it would only fail.
async fn poll_projects(inner: Arc<Inner>) {
    let core = Core(inner.clone());
    let mut next_due = inner.deps.clock.now() + PROJECT_STAGGER_MS;
    loop {
        // Held while signed out where sign-in is required (no host calls).
        if !core.account_hold().await {
            return;
        }
        let now = inner.deps.clock.now();
        if now >= next_due {
            next_due = now + PROJECT_STAGGER_MS;
            let online = inner
                .host_health
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .0;
            if online
                && let Some(id) = core
                    .next_stale_host_project(now - PROJECT_REFRESH_AFTER_MS)
                    .await
            {
                let _ = core.refresh_project_quietly(&id).await;
            }
        }
        tokio::select! {
            _ = inner.shutdown.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_millis(REAL_TICK_MS)) => {},
        }
    }
}

/// Every `AUTHORED_POLL_MS`, your own open PRs (`refresh_authored`): the
/// host sends no notification for your own actions, so the inbox poll
/// alone leaves a PR you opened or closed elsewhere stale. Skipped while
/// offline; the startup inbox pass covers the first minute.
async fn poll_authored(inner: Arc<Inner>) {
    let core = Core(inner.clone());
    let mut next_due = inner.deps.clock.now() + AUTHORED_POLL_MS;
    loop {
        // Held while signed out where sign-in is required (no host calls).
        if !core.account_hold().await {
            return;
        }
        let now = inner.deps.clock.now();
        if now >= next_due && core.host_online() {
            next_due = now + AUTHORED_POLL_MS;
            if let Err(e) = core.refresh_authored().await {
                tracing::debug!(error = %e, "authored PRs poll failed");
            }
        }
        tokio::select! {
            _ = inner.shutdown.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_millis(REAL_TICK_MS)) => {},
        }
    }
}

/// Every `OPEN_PR_POLL_MS`, each PR open this session. While the host is
/// offline only local branch reviews are polled (`probe_host` watches for
/// it coming back); the skipped ones are polled as soon as it does.
async fn poll_open_prs(inner: Arc<Inner>) {
    let core = Core(inner.clone());
    let mut next_due = inner.deps.clock.now();
    let mut skipped = false;
    loop {
        // Held while signed out where sign-in is required (no host calls).
        if !core.account_hold().await {
            return;
        }
        let online = core.host_online();
        if inner.deps.clock.now() >= next_due || (online && skipped) {
            skipped = false;
            let prs: Vec<PrId> = { inner.current_head.read().await.keys().cloned().collect() };
            for pr in prs {
                skipped |= !poll_one_pr(&core, &pr, online).await;
            }
            next_due = inner.deps.clock.now() + OPEN_PR_POLL_MS;
        }
        tokio::select! {
            _ = inner.shutdown.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_millis(REAL_TICK_MS)) => {},
        }
    }
}

/// Polls `pr`; `false` when it was skipped because the host is offline.
async fn poll_one_pr(core: &Core, pr: &PrId, online: bool) -> bool {
    let Some(pr_ref) = core.pr_ref(pr).await else {
        return true;
    };
    if !online && pr_ref.repo.host != theodore_domain::HostKind::Local {
        return false;
    }
    poll_pr_ref(core, pr, &pr_ref).await;
    true
}

async fn poll_pr_ref(core: &Core, pr: &PrId, pr_ref: &PrRef) {
    // `refresh_pr` applies `pr`/`pr_detail` whenever the host reports a
    // change, head moved or not — comments, threads, checks and labels can
    // all change between pushes, and must not be dropped just because this
    // tick isn't a push.
    let Some(value) = core.refresh_pr(pr, pr_ref).await else {
        return;
    };
    let new_head = value.summary.head_sha.clone();
    let old_head = core.0.current_head.read().await.get(pr).cloned();
    if old_head.as_ref() == Some(&new_head) {
        return;
    }
    handle_push(core, pr, pr_ref, old_head, new_head, value).await;
}

pub(crate) async fn handle_push(
    core: &Core,
    pr: &PrId,
    pr_ref: &PrRef,
    old_head: Option<Sha>,
    new_head: Sha,
    detail: PrDetail,
) {
    let repo_path = {
        let revisions = core.0.revisions.read().await;
        old_head.as_ref().and_then(|h| {
            revisions
                .get(&(pr.clone(), h.clone()))
                .map(|e| e.repo_path.clone())
        })
    };
    let repo_path = match repo_path {
        Some(p) => p,
        None => {
            let fallback = if pr_ref.repo.host == theodore_domain::HostKind::Local {
                core.ensure_local_project_cache(&pr_ref.repo).await
            } else {
                core.0.deps.git.ensure_repo(&pr_ref.repo).await
            };
            match fallback {
                Ok(p) => p,
                Err(_) => return,
            }
        }
    };
    // The new head isn't in the local clone yet: fetch it before checking
    // it out (a local branch review has no host to fetch from).
    if pr_ref.repo.host != theodore_domain::HostKind::Local {
        let _ = core.0.deps.git.fetch_pr(pr_ref).await;
    }
    let worktree = match core.0.deps.git.worktree(pr_ref, &new_head).await {
        Ok(w) => w,
        Err(_) => return,
    };
    let base = ConfigBase::of(pr_ref, &detail);
    let source = ReviewSource {
        repo: &pr_ref.repo,
        repo_path: &repo_path,
        worktree: &worktree.path,
        base: &base,
    };
    core.load_repo_settings(source).await;
    let new_rev = match core
        .0
        .deps
        .git
        .diff(
            &repo_path,
            &detail.base_sha,
            &new_head,
            &DiffOptions::default(),
        )
        .await
    {
        Ok(r) => r,
        Err(_) => return,
    };

    let old_rev = {
        let revisions = core.0.revisions.read().await;
        old_head.as_ref().and_then(|h| {
            revisions
                .get(&(pr.clone(), h.clone()))
                .map(|e| e.revision.clone())
        })
    };

    if let Some(old_rev) = &old_rev {
        let classes = theodore_domain::interdiff::classify(old_rev, &new_rev);
        // Reads are recorded against the *old* hunk id; only a `Changed`
        // hunk has one (a `New` hunk never had a prior read to clear).
        let mut changed_new_ids: BTreeSet<HunkId> = BTreeSet::new();
        let mut stale_old_ids: BTreeSet<HunkId> = BTreeSet::new();
        for (new_id, change) in &classes {
            match change {
                theodore_domain::interdiff::HunkChange::Unchanged => {}
                theodore_domain::interdiff::HunkChange::New => {
                    changed_new_ids.insert(new_id.clone());
                }
                theodore_domain::interdiff::HunkChange::Changed { previous } => {
                    changed_new_ids.insert(new_id.clone());
                    stale_old_ids.insert(previous.clone());
                }
            }
        }
        // `classify` only classifies *new*-side hunks, so an old hunk with
        // no counterpart at all in the new revision — the push dropped it
        // outright, with nothing landing to match it against — never shows
        // up as anyone's `previous` and would otherwise keep its stale
        // `hunk_read` forever. Catch those directly: any old hunk id gone
        // from the new revision, whether or not `classify` ever saw it.
        let new_ids: BTreeSet<&HunkId> = new_rev.hunks().map(|h| &h.id).collect();
        let mut vanished_paths: BTreeSet<&str> = BTreeSet::new();
        for h in old_rev.hunks() {
            if !new_ids.contains(&h.id) {
                stale_old_ids.insert(h.id.clone());
                vanished_paths.insert(h.path.as_str());
            }
        }
        clear_reads(core, pr, &stale_old_ids).await;
        dismiss_files(core, pr, &new_rev, &changed_new_ids, &vanished_paths).await;
    }

    if let Some(old_head) = &old_head {
        remap_anchors_after_push(core, pr, &repo_path, old_head, &new_head).await;
    }

    // The new head is the PR's head from here on — before regrouping, so
    // the regroup (and a run already going for the old head, when it
    // ends) sees it, and the poller doesn't wait on an agent run.
    {
        core.0.revisions.write().await.insert(
            (pr.clone(), new_head.clone()),
            Arc::new(RevisionEntry {
                revision: Arc::new(new_rev),
                repo_path,
                worktree_path: worktree.path,
                base,
                pr_ref: pr_ref.clone(),
            }),
        );
    }
    {
        core.0
            .current_head
            .write()
            .await
            .insert(pr.clone(), new_head.clone());
    }

    // In the background, and never alongside another run for this PR: a
    // push while one is going queues one more run, on the latest head.
    core.regroup_latest(pr);
    wake_next_push(core, pr).await;

    // `pr`/`pr_detail` were already written by `poll_one_pr`'s `refresh_pr`
    // call, from this same fetch — nothing left to apply here.

    core.emit(Ephemeral::Toast {
        level: ToastLevel::Info,
        text: format!("{} pushed {new_head}", pr_ref.repo.slug()),
    });
}

/// Moves (or outdates) every open `draft`/`finding`/`task` anchor of `pr`
/// across the push (plan §4): diffs `old_head..new_head` and runs
/// `domain::anchors::remap` over each anchor still on file. A draft whose
/// anchor comes back `Outdated` is marked so (never anchor-less: an
/// outdated draft is still shown, just not submittable — `submit_review`
/// skips it).
async fn remap_anchors_after_push(
    core: &Core,
    pr: &PrId,
    repo_path: &Path,
    old_head: &Sha,
    new_head: &Sha,
) {
    let between = match core
        .0
        .deps
        .git
        .diff(repo_path, old_head, new_head, &DiffOptions::default())
        .await
    {
        Ok(r) => r,
        Err(_) => return,
    };
    for model in [models::DRAFT, models::FINDING, models::TASK] {
        remap_model_anchors(core, pr, model, &between, new_head).await;
    }
}

async fn remap_model_anchors(
    core: &Core,
    pr: &PrId,
    model: &str,
    between: &Revision,
    new_head: &Sha,
) {
    let store = core.0.deps.store.clone();
    let pr_s = pr.0.clone();
    let model_owned = model.to_string();
    let rows =
        match tokio::task::spawn_blocking(move || store.query(&model_owned, "pr", &pr_s)).await {
            Ok(Ok(r)) => r,
            _ => return,
        };
    let mut actions = Vec::new();
    for row in rows {
        // Archived: a submitted draft, a closed finding: nothing to move.
        if row.archived
            || row
                .data
                .get("outdated")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        {
            continue; // already outdated; nothing more to remap
        }
        let Some(anchor_val) = row.data.get("anchor").filter(|v| !v.is_null()) else {
            continue; // task with no anchor
        };
        let Ok(anchor) = serde_json::from_value::<Anchor>(anchor_val.clone()) else {
            continue;
        };
        match theodore_domain::anchors::remap(&anchor, between, new_head) {
            Remapped::Moved(new_anchor) if new_anchor != anchor => {
                actions.push(NewAction {
                    model: model.into(),
                    model_id: row.id,
                    action: SyncActionCode::C,
                    data: Some(serde_json::json!({ "anchor": new_anchor })),
                });
            }
            Remapped::Moved(_) => {}
            Remapped::Outdated => {
                actions.push(NewAction {
                    model: model.into(),
                    model_id: row.id,
                    action: SyncActionCode::C,
                    data: Some(serde_json::json!({ "outdated": true })),
                });
            }
        }
    }
    let _ = core.apply(actions).await;
}

async fn clear_reads(core: &Core, pr: &PrId, changed: &BTreeSet<HunkId>) {
    if changed.is_empty() {
        return;
    }
    let store = core.0.deps.store.clone();
    let pr_s = pr.0.clone();
    let rows = match tokio::task::spawn_blocking(move || {
        store.query(models::HUNK_READ, "pr", &pr_s)
    })
    .await
    {
        Ok(Ok(r)) => r,
        _ => return,
    };
    let mut actions = Vec::new();
    for row in rows {
        if let Ok(hr) = serde_json::from_value::<HunkRead>(row.data.clone())
            && changed.contains(&hr.hunk)
        {
            actions.push(NewAction {
                model: models::HUNK_READ.into(),
                model_id: row.id,
                action: SyncActionCode::D,
                data: None,
            });
        }
    }
    let _ = core.apply(actions).await;
}

async fn dismiss_files(
    core: &Core,
    pr: &PrId,
    new_rev: &Revision,
    changed: &BTreeSet<HunkId>,
    extra_changed_paths: &BTreeSet<&str>,
) {
    let mut changed_paths: BTreeSet<&str> = new_rev
        .files
        .iter()
        .filter(|f| f.hunks.iter().any(|h| changed.contains(&h.id)))
        .map(|f| f.path.as_str())
        .collect();
    changed_paths.extend(extra_changed_paths.iter().copied());
    if changed_paths.is_empty() {
        return;
    }
    let store = core.0.deps.store.clone();
    let pr_s = pr.0.clone();
    let rows =
        match tokio::task::spawn_blocking(move || store.query(models::FILE_VIEWED, "pr", &pr_s))
            .await
        {
            Ok(Ok(r)) => r,
            _ => return,
        };
    let mut actions = Vec::new();
    for row in rows {
        if let Ok(fv) = serde_json::from_value::<theodore_contract::FileViewedRow>(row.data.clone())
            && changed_paths.contains(fv.path.as_str())
            && fv.state == ViewedState::Viewed
        {
            let data = serde_json::to_value(theodore_contract::FileViewedRow {
                pr: pr.clone(),
                path: fv.path.clone(),
                state: ViewedState::Dismissed,
            })
            .expect("serializable");
            actions.push(NewAction {
                model: models::FILE_VIEWED.into(),
                model_id: row.id,
                action: SyncActionCode::U,
                data: Some(data),
            });
        }
    }
    let _ = core.apply(actions).await;
}

/// Any refresh about to write `pr`'s row (inbox, project, one PR) calls
/// this first: a head that moved from the stored one is a push, which wakes
/// a `NextPush` snooze, whether or not the PR is open this session.
pub(crate) async fn wake_if_pushed(core: &Core, pr: &PrId, new_head: &Sha) {
    let store = core.0.deps.store.clone();
    let pr_s = pr.0.clone();
    let stored_head = tokio::task::spawn_blocking(move || store.get(models::PR, &pr_s))
        .await
        .ok()
        .and_then(|r| r.ok())
        .flatten()
        .and_then(|r| {
            r.data
                .get("head_sha")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        });
    if stored_head.is_some_and(|h| h != new_head.as_str()) {
        wake_next_push(core, pr).await;
    }
}

async fn wake_next_push(core: &Core, pr: &PrId) {
    let store = core.0.deps.store.clone();
    let pr_s = pr.0.clone();
    let row = tokio::task::spawn_blocking(move || store.get(models::INBOX_STATE, &pr_s))
        .await
        .ok()
        .and_then(|r| r.ok())
        .flatten();
    let Some(row) = row else { return };
    let Ok(state) = serde_json::from_value::<InboxState>(row.data.clone()) else {
        return;
    };
    let Some(snooze) = &state.snooze else { return };
    if theodore_domain::snooze::wakes(snooze, &PrEvent::Pushed) {
        let data = serde_json::to_value(InboxState {
            pr: pr.clone(),
            done: state.done,
            snooze: None,
        })
        .expect("serializable");
        let action = NewAction {
            model: models::INBOX_STATE.into(),
            model_id: pr.0.clone(),
            action: SyncActionCode::U,
            data: Some(data),
        };
        let _ = core.apply(vec![action]).await;
    }
}

async fn poll_inbox(inner: Arc<Inner>) {
    let core = Core(inner.clone());
    let store = inner.deps.store.clone();
    let mut cursor: Option<String> =
        tokio::task::spawn_blocking(move || store.kv_get("inbox:cursor"))
            .await
            .ok()
            .and_then(|r| r.ok())
            .flatten()
            .and_then(|v| v.as_str().map(str::to_string));
    let mut next_due = inner.deps.clock.now();

    loop {
        // Held while signed out where sign-in is required (no host calls).
        if !core.account_hold().await {
            return;
        }
        // Offline, `probe_host` asks instead; once it's back, this is due.
        if inner.deps.clock.now() >= next_due && core.host_online() {
            let result = inner.deps.host.changed_since(cursor.as_deref()).await;
            core.note_host(&result);
            match result {
                Ok(changes) => {
                    cursor = Some(changes.cursor.clone());
                    let store = inner.deps.store.clone();
                    let value = serde_json::json!(changes.cursor);
                    let _ =
                        tokio::task::spawn_blocking(move || store.kv_set("inbox:cursor", &value))
                            .await;
                    if !changes.prs.is_empty() {
                        core.emit(Ephemeral::Toast {
                            level: ToastLevel::Info,
                            text: match changes.prs.len() {
                                1 => "1 PR updated".to_string(),
                                n => format!("{n} PRs updated"),
                            },
                        });
                        let _ = core.refresh_inbox().await;
                    }
                    let wait = changes
                        .poll_after_ms
                        .unwrap_or(DEFAULT_INBOX_POLL_MS)
                        .max(1_000);
                    next_due = inner.deps.clock.now() + wait;
                }
                Err(_) => {
                    next_due = inner.deps.clock.now() + INBOX_ERROR_RETRY_MS;
                }
            }
        }
        tokio::select! {
            _ = inner.shutdown.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_millis(REAL_TICK_MS)) => {},
        }
    }
}
