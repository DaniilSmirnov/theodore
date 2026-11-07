//! `regroup` (plan §5.3): re-run the grouper against the cached revision,
//! continuing from the previous grouping when there is one.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use theodore_contract::{Ephemeral, ToastLevel, models};
use theodore_domain::grouping::{
    CO_CHANGE_MAX_COMMITS, CO_CHANGE_MAX_FILES, RepoContext, SymbolLink,
};
use theodore_domain::review_rules::ReviewRules;
use theodore_domain::{AgentId, FileFlag, FileStatus, Grouping, LineKind, PrId, Revision};
use theodore_ports::{
    CodeIntelError, CodeIntelPort, CodeSymbol, CodeSymbolKind, NewAction, SyncActionCode,
};
use tracing::Instrument as _;

use crate::core::Core;
use crate::error::{AppError, AppResult};

impl Core {
    /// An explicit regroup (the UI's button): refused while a run is in
    /// flight. A push that lands during it queues one more run, on the
    /// new head, after it (`group_until_settled`).
    pub async fn regroup(&self, pr: PrId) -> AppResult<()> {
        self.ensure_open(&pr).await?;
        let Some(mut slot) = self.claim_grouping(&pr, false) else {
            return Err(AppError::Invalid("grouping is already running".into()));
        };
        let result = self.group_head(&mut slot, true).await;
        if slot.next(result.as_ref().err().map(|e| e.to_string())) {
            tokio::spawn(self.clone().group_until_settled(slot).in_current_span());
        }
        result
    }

    /// Regroup `pr` at its current head after a push. Never two runs at
    /// once: while one is in flight this only queues one more, run once it
    /// ends — however many pushes land meanwhile.
    ///
    /// ```text
    /// push #1 ─▶ slot free ─▶ claim ─▶ run @head1 ─────────┐
    /// push #2 ─▶ slot taken ─▶ queued ┐                    │ ends
    /// push #3 ─▶ slot taken ─▶ queued ┘ (one flag)         ▼
    ///                                  queued? ─yes─▶ run @head3 (latest) ─▶ …
    ///                                     └─no──▶ release, running: false
    /// ```
    pub(crate) fn regroup_latest(&self, pr: &PrId) {
        if let Some(slot) = self.claim_grouping(pr, true) {
            tokio::spawn(self.clone().group_until_settled(slot).in_current_span());
        }
    }

    /// Claims `pr`'s grouping slot. `None` when a run holds it; `queue`
    /// then asks that run's owner for one more run after it.
    pub(crate) fn claim_grouping(&self, pr: &PrId, queue: bool) -> Option<GroupingSlot> {
        let mut inflight = self
            .0
            .grouping_inflight
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(queued) = inflight.get_mut(pr) {
            *queued |= queue;
            return None;
        }
        inflight.insert(pr.clone(), false);
        Some(GroupingSlot {
            core: self.clone(),
            pr: pr.clone(),
            announced: false,
            released: false,
        })
    }

    /// Groups `pr`'s current head while holding its slot, again for as
    /// long as another run was queued meanwhile, then releases the slot.
    pub(crate) async fn group_until_settled(self, mut slot: GroupingSlot) {
        loop {
            let error = match self.group_head(&mut slot, false).await {
                Ok(()) => None,
                Err(e) => {
                    self.emit(Ephemeral::Toast {
                        level: ToastLevel::Warn,
                        text: format!("regrouping after push failed: {e}"),
                    });
                    Some(e.to_string())
                }
            };
            if !slot.next(error) {
                return;
            }
        }
    }

    /// One grouping run of `slot`'s PR at its current head. `explicit`: a
    /// user's regroup, run even when the stored grouping is current (and
    /// an error when the PR isn't open); otherwise a current grouping is
    /// left alone and a closed PR is nothing to do.
    async fn group_head(&self, slot: &mut GroupingSlot, explicit: bool) -> AppResult<()> {
        let pr = slot.pr.clone();
        let head = self.0.current_head.read().await.get(&pr).cloned();
        let Some(head) = head else {
            return if explicit {
                Err(AppError::Invalid("pr is not open".into()))
            } else {
                Ok(())
            };
        };
        let entry = self
            .0
            .revisions
            .read()
            .await
            .get(&(pr.clone(), head.clone()))
            .cloned();
        let Some(entry) = entry else {
            return Err(AppError::Invalid("revision not cached".into()));
        };
        let detail = self.load_pr_detail(&pr).await?;

        // Held for the whole run (plan "worktree locks"): see `agents::run_skill`.
        let _lease = self.0.deps.worktree_lease.acquire(entry.pr_ref.clone());

        let pr_s = pr.0.clone();
        let store = self.0.deps.store.clone();
        let prev_row = tokio::task::spawn_blocking(move || store.get(models::GROUPING, &pr_s))
            .await
            .map_err(AppError::join)??;
        let rules = self.review_rules_at(entry.config_source()).await;
        let hash = rules.grouping_hash();
        if !explicit
            && prev_row
                .as_ref()
                .is_some_and(|r| row_is_current(&r.data, &head, hash.as_deref()))
        {
            return Ok(());
        }
        let prior = prev_row.as_ref().map(|r| r.data.clone());
        // Continue from the previous grouping only after a push to a newer
        // head, and only if the current algorithm made it; an explicit
        // regroup of the same head starts fresh (incremental regroup keeps
        // the previous groups' shape, including their fragmentation).
        // A changed review config (`hide`, `skim`, `checklist`) starts
        // fresh too: the previous groups were cut under other rules.
        let prev: Option<Grouping> = prev_row.and_then(|r| {
            let algo = theodore_domain::grouping::row_algo(&r.data);
            let same_rules = theodore_domain::grouping::row_review_hash(&r.data) == hash.as_deref();
            serde_json::from_value::<Grouping>(r.data)
                .ok()
                .filter(|g| {
                    g.rev != head
                        && !g.in_progress
                        && algo >= theodore_domain::grouping::ALGO_VERSION
                        && same_rules
                })
                // Its catch-all isn't a group to place hunks in.
                .map(|mut g| {
                    g.groups.retain(|g| g.id.0 != UNGROUPED);
                    g
                })
        });
        let repo_scope = detail.summary.pr.repo.slug();
        let configured = self
            .setting_agent_scoped("agents.grouping", Some(&repo_scope), AgentId::Claude)
            .await;
        let agent = self.effective_agent(configured).await;

        slot.announce();
        let span = grouping_span(&pr, agent, prev.is_some());
        let started = std::time::Instant::now();
        let writer = PartialWriter::start(
            self,
            pr.clone(),
            entry.revision.clone(),
            rules.clone(),
            prior,
        );
        let result = self
            .regroup_run(&prev, &entry, &detail, agent, rules, &|g| writer.send(g))
            .instrument(span)
            .await;
        writer.finish(result.is_ok()).await;
        if let Err(
            e @ AppError::Grouper(
                crate::error::GrouperError::Offline(_) | crate::error::GrouperError::SignedOut(_),
            ),
        ) = &result
        {
            // The agent couldn't be reached or isn't signed in: the network
            // or the user's to fix, not a bug.
            tracing::info!(
                error.code = e.code(),
                pr = %pr.0,
                agent = agent.as_str(),
                duration_ms = started.elapsed().as_millis() as u64,
                error = %e,
                "grouping couldn't run"
            );
        } else if let Err(e) = &result {
            tracing::warn!(
                error.code = e.code(),
                pr = %pr.0,
                agent = agent.as_str(),
                duration_ms = started.elapsed().as_millis() as u64,
                error = %e,
                "grouping failed"
            );
            theodore_telemetry::product(theodore_telemetry::ProductEvent::GroupingFailed {
                code: e.code().to_string(),
            });
        }
        let (grouping, ctx) = result?;
        let data = grouping_row(grouping, &entry.revision, &ctx)
            .map_err(|e| AppError::Invalid(e.to_string()))?;
        self.write_grouping_row(&pr, &head, data).await?;
        Ok(())
    }

    /// Stores a finished run's grouping of `head` as `pr`'s row — unless
    /// the PR was closed while the agent worked (no orphaned row), or the
    /// stored row is already a finished grouping of the PR's current head
    /// and this run was for an older one (a stale run never overwrites a
    /// newer grouping).
    pub(crate) async fn write_grouping_row(
        &self,
        pr: &PrId,
        head: &theodore_domain::Sha,
        data: serde_json::Value,
    ) -> AppResult<()> {
        if self.grouping_is_stale(pr, head).await {
            tracing::info!(pr = %pr.0, rev = %head, "grouping of an old head dropped");
            return Ok(());
        }
        self.apply(vec![NewAction {
            model: models::GROUPING.into(),
            model_id: pr.0.clone(),
            action: SyncActionCode::U,
            data: Some(data),
        }])
        .await?;
        Ok(())
    }

    /// A grouping of `rev` mustn't be written for `pr`: the PR is closed,
    /// or it moved on from `rev` and its row is a finished grouping of the
    /// head it's at now.
    async fn grouping_is_stale(&self, pr: &PrId, rev: &theodore_domain::Sha) -> bool {
        let Some(current) = self.0.current_head.read().await.get(pr).cloned() else {
            return true;
        };
        if &current == rev {
            return false;
        }
        let store = self.0.deps.store.clone();
        let id = pr.0.clone();
        let row = tokio::task::spawn_blocking(move || store.get(models::GROUPING, &id))
            .await
            .ok()
            .and_then(|r| r.ok())
            .flatten();
        row.is_some_and(|r| {
            r.data.get("rev").and_then(|v| v.as_str()) == Some(current.as_str())
                && !r
                    .data
                    .get("in_progress")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
        })
    }

    async fn regroup_run(
        &self,
        prev: &Option<Grouping>,
        entry: &crate::core::RevisionEntry,
        detail: &theodore_domain::PrDetail,
        agent: AgentId,
        rules: ReviewRules,
        on_partial: crate::deps::OnPartial<'_>,
    ) -> AppResult<(Grouping, RepoContext)> {
        let mut ctx = self
            .grouping_context(&entry.worktree_path, &entry.revision)
            .await;
        ctx.review = rules;
        let agent = crate::deps::GroupingAgent {
            id: agent,
            // Grouping never uses the user's MCP servers: starting
            // them on every chunk would only slow it down.
            user_mcp: false,
        };
        let grouping = match prev {
            Some(p) => {
                self.0
                    .deps
                    .grouper
                    .regroup(
                        p,
                        &entry.revision,
                        detail,
                        &ctx,
                        &entry.worktree_path,
                        agent,
                    )
                    .await?
            }
            None => {
                self.0
                    .deps
                    .grouper
                    .group(
                        &entry.revision,
                        detail,
                        &ctx,
                        &entry.worktree_path,
                        agent,
                        on_partial,
                    )
                    .await?
            }
        };
        Ok((grouping, ctx))
    }

    /// What the grouper can use beyond the diff: head contents of the
    /// changed text files (imports outside the hunks), which of them
    /// changed together before this PR (`GitPort::co_change_commits` from
    /// the base) and, with code intelligence on the PR's worktree, which
    /// changed hunks use a symbol another one defines ([`symbol_links`],
    /// alongside the history scan, within its own time budget).
    /// Best-effort: anything unreadable is simply left out.
    pub(crate) async fn grouping_context(&self, worktree: &Path, rev: &Revision) -> RepoContext {
        let mut ctx = RepoContext::default();
        let mut budget = HEAD_FILES_MAX_TOTAL_BYTES;
        for f in rev.files.iter().take(HEAD_FILES_MAX_COUNT) {
            if !is_linkable(f) {
                continue;
            }
            let path = worktree.join(&f.path);
            let Ok(meta) = tokio::fs::metadata(&path).await else {
                continue;
            };
            let size = meta.len() as usize;
            if !meta.is_file() || size > HEAD_FILE_MAX_BYTES || size > budget {
                continue;
            }
            if let Ok(text) = tokio::fs::read_to_string(&path).await {
                budget -= size;
                ctx.head_files.insert(f.path.clone(), text);
            }
        }
        let paths: Vec<String> = rev.files.iter().map(|f| f.path.clone()).collect();
        let history = self.0.deps.git.co_change_commits(
            worktree,
            &rev.base,
            &paths,
            CO_CHANGE_MAX_COMMITS,
            CO_CHANGE_MAX_FILES,
        );
        let links = async {
            let Some(port) = self.0.deps.code_intel.clone() else {
                return Vec::new();
            };
            let started = std::time::Instant::now();
            let links = symbol_links(port, worktree, rev, SymbolLinkLimits::default()).await;
            tracing::debug!(
                links = links.len(),
                duration_ms = started.elapsed().as_millis() as u64,
                "grouping symbol links"
            );
            links
        };
        let (history, links) = tokio::join!(history, links);
        ctx.co_change_commits = history.unwrap_or_default();
        ctx.symbol_links = links;
        ctx
    }
}

/// The span a grouping run goes in. The grouper records when the first
/// group was shown on it (`first_group_ms`).
/// The id of the catch-all group `carry_grouping` adds.
pub(crate) const UNGROUPED: &str = "ungrouped";

impl Core {
    /// A branch review moved to `head` without a regroup (a space's turn,
    /// docs/spaces.md "Review of a space"): the stored grouping keeps its
    /// groups and gets the hunks none of them holds in a trailing "Not
    /// grouped yet" group, so no changed file is in none. Its `rev` stays,
    /// so opening Review or Regroup still groups the new head; no agent runs.
    pub(crate) async fn carry_grouping(&self, pr: &PrId, head: &theodore_domain::Sha) {
        let entry = self
            .0
            .revisions
            .read()
            .await
            .get(&(pr.clone(), head.clone()))
            .cloned();
        let Some(entry) = entry else {
            return;
        };
        let store = self.0.deps.store.clone();
        let id = pr.0.clone();
        let row = tokio::task::spawn_blocking(move || store.get(models::GROUPING, &id))
            .await
            .ok()
            .and_then(|r| r.ok())
            .flatten();
        let Some(row) = row else {
            return;
        };
        let Ok(mut grouping) = serde_json::from_value::<Grouping>(row.data.clone()) else {
            return;
        };
        if grouping.in_progress {
            return;
        }
        let before = grouping.groups.clone();
        grouping.groups.retain(|g| g.id.0 != UNGROUPED);
        if grouping.groups.is_empty() {
            return;
        }
        let taken: BTreeSet<theodore_domain::HunkId> = grouping
            .groups
            .iter()
            .flat_map(|g| g.hunks.iter().cloned())
            .collect();
        let rest: Vec<theodore_domain::HunkId> = entry
            .revision
            .hunks()
            .map(|h| h.id.clone())
            .filter(|h| !taken.contains(h))
            .collect();
        if !rest.is_empty() {
            grouping.groups.push(theodore_domain::Group {
                id: theodore_domain::GroupId::new(UNGROUPED),
                title: "Not grouped yet".into(),
                why: "Changed since the last grouping. Regroup places them.".into(),
                kind: theodore_domain::GroupKind::Normal,
                risk: None,
                check: vec![],
                hunks: rest,
                outliers: vec![],
            });
        }
        if grouping.groups == before {
            return;
        }
        let mut data = row.data;
        let Ok(groups) = serde_json::to_value(&grouping.groups) else {
            return;
        };
        data["groups"] = groups;
        let _ = self
            .apply(vec![NewAction {
                model: models::GROUPING.into(),
                model_id: pr.0.clone(),
                action: SyncActionCode::U,
                data: Some(data),
            }])
            .await;
    }
}

pub(crate) fn grouping_span(pr: &PrId, agent: AgentId, incremental: bool) -> tracing::Span {
    tracing::info_span!(
        "grouping",
        pr = %pr.0,
        agent = agent.as_str(),
        incremental,
        first_group_ms = tracing::field::Empty,
    )
}

/// The grouping as stored: the config's checklist items added to its
/// groups, then the row with the algorithm version and the hash of the
/// review rules it was made under.
pub(crate) fn grouping_row(
    grouping: Grouping,
    rev: &Revision,
    ctx: &RepoContext,
) -> serde_json::Result<serde_json::Value> {
    grouping_row_under(grouping, rev, &ctx.review)
}

fn grouping_row_under(
    mut grouping: Grouping,
    rev: &Revision,
    rules: &ReviewRules,
) -> serde_json::Result<serde_json::Value> {
    rules.apply_checklist(&mut grouping, rev);
    theodore_domain::grouping::to_row_with(&grouping, rules.grouping_hash().as_deref())
}

/// A stored grouping row is still good for `head` under rules hashing to
/// `hash`: same head, current algorithm, same review rules, and finished
/// (a run that failed or was cut short mid-stream left `in_progress` set).
pub(crate) fn row_is_current(
    data: &serde_json::Value,
    head: &theodore_domain::Sha,
    hash: Option<&str>,
) -> bool {
    use theodore_domain::grouping::{ALGO_VERSION, row_algo, row_review_hash};
    data.get("rev").and_then(|v| v.as_str()) == Some(head.as_str())
        && row_algo(data) >= ALGO_VERSION
        && row_review_hash(data) == hash
        && !data
            .get("in_progress")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
}

/// A PR's claim on its one grouping run at a time (`Inner::grouping_inflight`),
/// from `Core::claim_grouping`. Its owner runs, then calls `next`: another
/// run if one was queued meanwhile, else the slot is released. Dropped
/// without that (a panic, a cancelled task) it still releases the slot
/// and says the run stopped, so neither the PR nor its spinner is ever
/// left "grouping" with nothing running.
///
/// ```text
/// claim ─▶ announce (running: true) ─▶ run ─▶ next(error)
///                                               ├─ queued ─▶ run again (no flicker)
///                                               └─ else ──▶ release + running: false
/// drop before next ─▶ release + running: false, error
/// ```
pub(crate) struct GroupingSlot {
    core: Core,
    pr: PrId,
    /// `Ephemeral::Grouping { running: true }` went out for this claim.
    announced: bool,
    released: bool,
}

impl GroupingSlot {
    /// Tells the UI a run is going, once per claim.
    pub(crate) fn announce(&mut self) {
        if !self.announced {
            self.announced = true;
            self.core.emit(Ephemeral::Grouping {
                pr: self.pr.clone(),
                running: true,
                error: None,
            });
        }
    }

    /// A run ended with `error`. `true`: another was queued meanwhile, the
    /// slot is kept for it. `false`: the slot is released, and the UI told
    /// so — under the lock, so a `running: false` can never land after the
    /// next claim's `running: true`.
    pub(crate) fn next(&mut self, error: Option<String>) -> bool {
        let mut inflight = self
            .core
            .0
            .grouping_inflight
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(queued) = inflight.get_mut(&self.pr)
            && *queued
        {
            *queued = false;
            return true;
        }
        inflight.remove(&self.pr);
        self.released = true;
        if self.announced {
            self.core.emit(Ephemeral::Grouping {
                pr: self.pr.clone(),
                running: false,
                error,
            });
        }
        false
    }
}

impl Drop for GroupingSlot {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let mut inflight = self
            .core
            .0
            .grouping_inflight
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        inflight.remove(&self.pr);
        if self.announced {
            self.core.emit(Ephemeral::Grouping {
                pr: self.pr.clone(),
                running: false,
                error: Some("grouping stopped before it finished".into()),
            });
        }
    }
}

/// Writes a run's provisional groupings (`Grouper::group`'s `on_partial`)
/// as the PR's grouping row while the run is in flight, so the UI shows
/// groups as the agent writes them:
///
/// ```text
/// grouper ─on_partial─▶ channel ─▶ writer task ─▶ grouping row (U, in_progress)
///                                  └─ several queued: only the newest
/// run returns ─▶ finish(ok): channel closed, last write awaited
///    ok     ─▶ caller writes the final row after it
///    failed ─▶ the row from before the run is put back, if there was one;
///              else the in_progress row stays (shown as unfinished)
/// ```
pub(crate) struct PartialWriter {
    tx: Option<tokio::sync::mpsc::UnboundedSender<Grouping>>,
    /// Resolves to whether any row was written.
    task: tokio::task::JoinHandle<bool>,
    core: Core,
    pr: PrId,
    /// The PR's grouping row from before the run.
    prior: Option<serde_json::Value>,
}

impl PartialWriter {
    pub(crate) fn start(
        core: &Core,
        pr: PrId,
        rev: impl Into<std::sync::Arc<Revision>>,
        rules: ReviewRules,
        prior: Option<serde_json::Value>,
    ) -> Self {
        let rev: std::sync::Arc<Revision> = rev.into();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Grouping>();
        let task = tokio::spawn({
            let core = core.clone();
            let pr = pr.clone();
            async move {
                let mut wrote = false;
                while let Some(mut grouping) = rx.recv().await {
                    while let Ok(newer) = rx.try_recv() {
                        grouping = newer;
                    }
                    // Closed while the agent worked (no orphaned row), or
                    // a newer head's grouping is already stored.
                    if core.grouping_is_stale(&pr, &rev.head).await {
                        continue;
                    }
                    grouping.in_progress = true;
                    let Ok(data) = grouping_row_under(grouping, &rev, &rules) else {
                        continue;
                    };
                    wrote |= core
                        .apply(vec![NewAction {
                            model: models::GROUPING.into(),
                            model_id: pr.0.clone(),
                            action: SyncActionCode::U,
                            data: Some(data),
                        }])
                        .await
                        .is_ok();
                }
                wrote
            }
            .in_current_span()
        });
        Self {
            tx: Some(tx),
            task,
            core: core.clone(),
            pr,
            prior,
        }
    }

    /// Queue `grouping` for writing (the grouper's `on_partial`).
    pub(crate) fn send(&self, grouping: Grouping) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(grouping);
        }
    }

    /// Wait for the queued writes, so a final row written next lands last.
    /// A failed run (`!ok`) that wrote any puts the row from before it
    /// back.
    pub(crate) async fn finish(self, ok: bool) {
        let Self {
            tx,
            task,
            core,
            pr,
            prior,
        } = self;
        drop(tx);
        let wrote = task.await.unwrap_or(false);
        if ok || !wrote {
            return;
        }
        let Some(data) = prior else { return };
        if !core.0.current_head.read().await.contains_key(&pr) {
            return;
        }
        let _ = core
            .apply(vec![NewAction {
                model: models::GROUPING.into(),
                model_id: pr.0.clone(),
                action: SyncActionCode::U,
                data: Some(data),
            }])
            .await;
    }
}

/// Caps of [`symbol_links`]: grouping waits for it, so it stays well under
/// a second or two whatever the PR's size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SymbolLinkLimits {
    /// Wall time for the whole computation; links found by then are kept.
    pub budget: Duration,
    /// Changed files asked for their symbols.
    pub max_files: usize,
    /// Symbols asked for their references.
    pub max_symbols: usize,
    /// Links kept per symbol (a name used in 40 hunks is a rollout, which
    /// the graph sees on its own).
    pub max_links_per_symbol: usize,
    pub max_links: usize,
    /// Port calls in flight at once.
    pub concurrency: usize,
}

impl Default for SymbolLinkLimits {
    fn default() -> Self {
        Self {
            budget: Duration::from_secs(2),
            max_files: 60,
            max_symbols: 40,
            max_links_per_symbol: 8,
            max_links: 200,
            concurrency: 4,
        }
    }
}

/// How often, and how far apart, `symbol_links` asks again for a worktree
/// that isn't open yet (the PR's open races the first grouping).
const OPEN_RETRIES: u32 = 3;
const OPEN_RETRY_EVERY: Duration = Duration::from_millis(100);

/// Hunk-to-hunk links from code intelligence (docs/code-intel.md §7.1):
/// the symbols defined in changed hunks, and which other changed hunks use
/// them. No LLM call.
///
/// ```text
/// changed files ─▶ file_symbols ─▶ symbols whose range covers an added line
///                                  (innermost first; names on an added line first)
///               ─▶ references(name) ─▶ hits on another hunk's added line
///               ─▶ SymbolLink { from: defining hunk, to: using hunk, symbol }
/// ```
///
/// Best effort: a worktree that isn't open, an error, or the time budget
/// running out leaves links out, never fails grouping.
pub async fn symbol_links(
    port: Arc<dyn CodeIntelPort>,
    root: &Path,
    rev: &Revision,
    limits: SymbolLinkLimits,
) -> Vec<SymbolLink> {
    let deadline = tokio::time::Instant::now() + limits.budget;
    let lines = AddedLines::of(rev, limits.max_files);
    if lines.by_file.is_empty() {
        return Vec::new();
    }
    let root: Arc<PathBuf> = Arc::new(root.to_path_buf());

    // 1. Symbols of each changed file. A worktree not open yet (the PR's
    // open races its first grouping) is asked again shortly.
    let paths: Vec<String> = lines.by_file.keys().cloned().collect();
    let mut symbols: Vec<CodeSymbol> = Vec::new();
    for attempt in 0..=OPEN_RETRIES {
        let answers = bounded(paths.clone(), limits.concurrency, deadline, {
            let (port, root) = (port.clone(), root.clone());
            move |path: String| {
                let (port, root) = (port.clone(), root.clone());
                async move { port.file_symbols(&root, &path).await }
            }
        })
        .await;
        let not_open = !answers.is_empty()
            && answers
                .iter()
                .all(|a| matches!(a, Err(CodeIntelError::NotOpen(_))));
        let retry_at = tokio::time::Instant::now() + OPEN_RETRY_EVERY;
        if not_open && attempt < OPEN_RETRIES && retry_at < deadline {
            tokio::time::sleep_until(retry_at).await;
            continue;
        }
        symbols = answers
            .into_iter()
            .filter_map(Result::ok)
            .flat_map(|a| a.items)
            .collect();
        break;
    }
    let picked = lines.defining(symbols, limits.max_symbols);
    if picked.is_empty() {
        return Vec::new();
    }

    // 2. Where each is used.
    let hits = bounded(picked, limits.concurrency, deadline, {
        let (port, root) = (port.clone(), root.clone());
        move |(hunk, sym): (usize, CodeSymbol)| {
            let (port, root) = (port.clone(), root.clone());
            async move {
                let hits = port
                    .references(&root, &sym.path, sym.name_range.start)
                    .await
                    .map(|a| a.items)
                    .unwrap_or_default();
                (hunk, sym.name, hits)
            }
        }
    })
    .await;

    let mut seen: BTreeSet<(usize, usize)> = BTreeSet::new();
    let mut found: Vec<(usize, usize, String)> = Vec::new();
    for (from, name, hits) in hits {
        let mut kept = 0;
        // A text hit is only the same word: a column `name` in a migration
        // says nothing about a function `name()`.
        for h in hits
            .iter()
            .filter(|h| !h.is_definition && h.source != theodore_ports::Source::Text)
        {
            if kept >= limits.max_links_per_symbol {
                break;
            }
            let Some(to) = lines.hunk_at(&h.location.path, h.location.range.start.line) else {
                continue;
            };
            if to != from && seen.insert((from, to)) {
                found.push((from, to, name.clone()));
                kept += 1;
            }
        }
    }
    found.sort();
    found.truncate(limits.max_links);
    found
        .into_iter()
        .map(|(from, to, symbol)| SymbolLink {
            from: lines.hunks[from].clone(),
            to: lines.hunks[to].clone(),
            symbol,
        })
        .collect()
}

/// Runs `call` over `items`, at most `concurrency` at once, until
/// `deadline`: the results in by then (in completion order); the calls
/// still running are aborted.
async fn bounded<I, O, F, Fut>(
    items: Vec<I>,
    concurrency: usize,
    deadline: tokio::time::Instant,
    call: F,
) -> Vec<O>
where
    I: Send + 'static,
    O: Send + 'static,
    F: Fn(I) -> Fut,
    Fut: std::future::Future<Output = O> + Send + 'static,
{
    let mut queue = items.into_iter();
    let mut set = tokio::task::JoinSet::new();
    let mut out = Vec::new();
    loop {
        while set.len() < concurrency.max(1) {
            match queue.next() {
                Some(item) => {
                    set.spawn(call(item));
                }
                None => break,
            }
        }
        match tokio::time::timeout_at(deadline, set.join_next()).await {
            Ok(Some(Ok(o))) => out.push(o),
            Ok(Some(Err(_))) => {}
            Ok(None) | Err(_) => break,
        }
    }
    out
}

/// The added (new-side) lines of a revision's linkable files, by hunk.
struct AddedLines {
    /// Hunks in revision order.
    hunks: Vec<theodore_domain::HunkId>,
    /// path → new line number → index into `hunks`.
    by_file: BTreeMap<String, BTreeMap<u32, usize>>,
}

impl AddedLines {
    fn of(rev: &Revision, max_files: usize) -> Self {
        let mut out = Self {
            hunks: Vec::new(),
            by_file: BTreeMap::new(),
        };
        for f in rev.files.iter().filter(|f| is_linkable(f)).take(max_files) {
            let mut lines = BTreeMap::new();
            for h in &f.hunks {
                let i = out.hunks.len();
                out.hunks.push(h.id.clone());
                for l in &h.lines {
                    if let (LineKind::Add, Some(n)) = (l.kind, l.new_no) {
                        lines.insert(n, i);
                    }
                }
            }
            if !lines.is_empty() {
                out.by_file.insert(f.path.clone(), lines);
            }
        }
        out
    }

    fn hunk_at(&self, path: &str, line: u32) -> Option<usize> {
        self.by_file.get(path)?.get(&line).copied()
    }

    /// The hunks `s`'s definition covers an added line of.
    fn covered(&self, s: &CodeSymbol) -> BTreeSet<usize> {
        self.by_file
            .get(&s.path)
            .map(|lines| {
                lines
                    .range(s.range.start.line..=s.range.end.line)
                    .map(|(_, &h)| h)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Up to `max` symbols worth asking about, each with the hunk that
    /// defines it, taken round-robin across hunks so one big new file
    /// doesn't use them all. Per hunk: a symbol whose name is on an added
    /// line (new, renamed, re-signed) first, then the innermost; a
    /// container whose own line didn't change gives way to the changed
    /// symbol inside it (a method's callers, not its class's).
    fn defining(&self, symbols: Vec<CodeSymbol>, max: usize) -> Vec<(usize, CodeSymbol)> {
        let mut by_hunk: BTreeMap<usize, Vec<(bool, u32, CodeSymbol)>> = BTreeMap::new();
        for s in symbols {
            if !worth_linking(&s) {
                continue;
            }
            let named_here = self.hunk_at(&s.path, s.name_range.start.line);
            let covered = self.covered(&s);
            let Some(hunk) = named_here.or_else(|| covered.first().copied()) else {
                continue;
            };
            let span = s.range.end.line.saturating_sub(s.range.start.line);
            by_hunk
                .entry(hunk)
                .or_default()
                .push((named_here.is_none(), span, s));
        }
        let lists: Vec<(usize, Vec<CodeSymbol>)> = by_hunk
            .into_iter()
            .map(|(hunk, cands)| {
                let container = |(unnamed, _, outer): &(bool, u32, CodeSymbol)| {
                    *unnamed
                        && cands
                            .iter()
                            .any(|(_, _, s)| s != outer && contains(outer.range, s.range))
                };
                let mut kept: Vec<&(bool, u32, CodeSymbol)> =
                    cands.iter().filter(|c| !container(c)).collect();
                kept.sort_by(|a, b| (a.0, a.1, &a.2.name).cmp(&(b.0, b.1, &b.2.name)));
                let mut names = BTreeSet::new();
                let syms = kept
                    .into_iter()
                    .map(|(_, _, s)| s.clone())
                    .filter(|s| names.insert(s.name.clone()))
                    .collect();
                (hunk, syms)
            })
            .collect();
        let mut out = Vec::new();
        for round in 0.. {
            let before = out.len();
            for (hunk, syms) in &lists {
                if out.len() >= max {
                    return out;
                }
                if let Some(s) = syms.get(round) {
                    out.push((*hunk, s.clone()));
                }
            }
            if out.len() == before {
                break;
            }
        }
        out
    }
}

/// `outer` contains `inner` and is larger.
fn contains(outer: theodore_ports::Range, inner: theodore_ports::Range) -> bool {
    outer != inner && outer.start <= inner.start && inner.end <= outer.end
}

/// A file whose head text code intelligence can answer about.
fn is_linkable(f: &theodore_domain::FileDiff) -> bool {
    f.status != FileStatus::Deleted
        && !f.hunks.is_empty()
        && !f.flags.iter().any(|fl| {
            matches!(
                fl,
                FileFlag::Binary { .. }
                    | FileFlag::Generated { .. }
                    | FileFlag::Lfs
                    | FileFlag::Submodule
            )
        })
        && is_plain_relative(&f.path)
}

/// Names common enough that a by-name match (the syntax layer) links
/// unrelated code.
const COMMON_NAMES: &[&str] = &[
    "new",
    "default",
    "from",
    "into",
    "init",
    "main",
    "test",
    "tests",
    "run",
    "get",
    "set",
    "len",
    "drop",
    "clone",
    "fmt",
    "eq",
    "hash",
    "next",
    "call",
    "apply",
    "build",
    "render",
    "update",
    "handle",
    "parse",
    "load",
    "save",
    "open",
    "close",
    "start",
    "stop",
    "read",
    "write",
    "send",
    "value",
    "data",
    "name",
    "id",
    "key",
    "type",
    "self",
    "this",
    "constructor",
    "toString",
    "__init__",
    "__str__",
    "__repr__",
    "setUp",
    "tearDown",
];

/// A symbol whose uses are worth linking: a definition kind, not a local
/// or a common method name; a field or variable only with a compound name
/// (`retry_after_ms`, `maxRetries`).
fn worth_linking(s: &CodeSymbol) -> bool {
    let n = s.name.as_str();
    if n.chars().count() < 3 || COMMON_NAMES.contains(&n) {
        return false;
    }
    let camel = n.chars().skip(1).any(char::is_uppercase) && n.chars().any(char::is_lowercase);
    let compound = n.contains('_') || camel;
    match s.kind {
        CodeSymbolKind::Module | CodeSymbolKind::Other => false,
        CodeSymbolKind::Variable | CodeSymbolKind::Field => compound,
        _ => true,
    }
}
const HEAD_FILE_MAX_BYTES: usize = 512 * 1024;
const HEAD_FILES_MAX_TOTAL_BYTES: usize = 24 * 1024 * 1024;
const HEAD_FILES_MAX_COUNT: usize = 3000;

/// A diff path that stays inside the worktree (no `..`, not absolute).
fn is_plain_relative(path: &str) -> bool {
    Path::new(path)
        .components()
        .all(|c| matches!(c, Component::Normal(_)))
}

#[cfg(test)]
mod link_tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use theodore_domain::{FileDiff, HunkId, Sha};
    use theodore_ports::{
        Answer, CodeHit, CodeIntelLimits, CodeIntelResult, Diagnostic, Hover, InstallProgressFn,
        LanguageId, LanguageStatus, Location, Position, Range, Source, TextEdit, Workspace,
    };

    use super::*;
    use crate::testkit::{self, line_add, line_ctx, make_hunk};

    /// A port answering `file_symbols` and `references` (per asked
    /// position), nothing else.
    #[derive(Default)]
    struct Port {
        symbols: Vec<CodeSymbol>,
        /// (path, line of the name) → hits.
        refs: HashMap<(String, u32), Vec<CodeHit>>,
        /// `file_symbols` says "not open" this many times first.
        not_open: Mutex<u32>,
        /// `references` of these names never answers in time.
        slow: Vec<String>,
        asked: Mutex<Vec<String>>,
    }

    fn unsupported<T>() -> CodeIntelResult<T> {
        Err(CodeIntelError::Unsupported("test".into()))
    }

    fn answer<T>(items: Vec<T>) -> Answer<T> {
        Answer {
            items,
            source: Source::Syntax,
            index_percent: 100,
            note: None,
        }
    }

    #[async_trait]
    impl CodeIntelPort for Port {
        async fn open(&self, _ws: Workspace) -> CodeIntelResult<()> {
            Ok(())
        }
        async fn close(&self, _root: &Path) {}
        async fn warm(&self, _root: &Path) {}
        async fn set_lsp(&self, _root: &Path, _on: bool) {}
        async fn find_symbol(
            &self,
            _root: &Path,
            _name: &str,
            _kind: Option<CodeSymbolKind>,
        ) -> CodeIntelResult<Answer<CodeSymbol>> {
            unsupported()
        }
        async fn file_symbols(
            &self,
            root: &Path,
            path: &str,
        ) -> CodeIntelResult<Answer<CodeSymbol>> {
            {
                let mut left = self.not_open.lock().unwrap();
                if *left > 0 {
                    *left -= 1;
                    return Err(CodeIntelError::NotOpen(root.to_path_buf()));
                }
            }
            Ok(answer(
                self.symbols
                    .iter()
                    .filter(|s| s.path == path)
                    .cloned()
                    .collect(),
            ))
        }
        async fn symbol_body(
            &self,
            _root: &Path,
            _path: &str,
            _name: &str,
        ) -> CodeIntelResult<(CodeSymbol, String)> {
            unsupported()
        }
        async fn definition(
            &self,
            _root: &Path,
            _path: &str,
            _at: Position,
        ) -> CodeIntelResult<Answer<CodeHit>> {
            unsupported()
        }
        async fn references(
            &self,
            _root: &Path,
            path: &str,
            at: Position,
        ) -> CodeIntelResult<Answer<CodeHit>> {
            let name = self
                .symbols
                .iter()
                .find(|s| s.path == path && s.name_range.start == at)
                .map(|s| s.name.clone())
                .unwrap_or_default();
            self.asked.lock().unwrap().push(name.clone());
            if self.slow.contains(&name) {
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
            Ok(answer(
                self.refs
                    .get(&(path.to_string(), at.line))
                    .cloned()
                    .unwrap_or_default(),
            ))
        }
        async fn diagnostics(
            &self,
            _root: &Path,
            _path: Option<&str>,
        ) -> CodeIntelResult<Answer<Diagnostic>> {
            unsupported()
        }
        async fn rename(
            &self,
            _root: &Path,
            _path: &str,
            _at: Position,
            _new_name: &str,
        ) -> CodeIntelResult<Vec<TextEdit>> {
            unsupported()
        }
        async fn hover(
            &self,
            _root: &Path,
            _path: &str,
            _at: Position,
        ) -> CodeIntelResult<Option<Hover>> {
            Ok(None)
        }
        async fn install(
            &self,
            _language: &LanguageId,
            _progress: Option<InstallProgressFn>,
        ) -> CodeIntelResult<()> {
            Ok(())
        }
        async fn remove(&self, _language: &LanguageId) -> CodeIntelResult<()> {
            Ok(())
        }
        async fn set_limits(&self, _limits: CodeIntelLimits) {}
        async fn file_changed(&self, _root: &Path, _path: &str) {}
        async fn status(&self, _root: &Path) -> CodeIntelResult<Vec<LanguageStatus>> {
            Ok(Vec::new())
        }
    }

    fn range(start: u32, end: u32) -> Range {
        Range {
            start: Position {
                line: start,
                col: 1,
            },
            end: Position { line: end, col: 1 },
        }
    }

    fn sym(path: &str, name: &str, kind: CodeSymbolKind, lines: (u32, u32), at: u32) -> CodeSymbol {
        CodeSymbol {
            name: name.into(),
            kind,
            range: range(lines.0, lines.1),
            name_range: Range {
                start: Position { line: at, col: 5 },
                end: Position {
                    line: at,
                    col: 5 + name.len() as u32,
                },
            },
            path: path.into(),
            container: None,
            source: Source::Syntax,
        }
    }

    fn hit(path: &str, line: u32, is_definition: bool) -> CodeHit {
        CodeHit {
            location: Location {
                path: path.into(),
                range: range(line, line),
            },
            preview: String::new(),
            is_definition,
            source: Source::Syntax,
        }
    }

    fn file(path: &str, hunks: Vec<theodore_domain::Hunk>) -> FileDiff {
        FileDiff {
            path: path.into(),
            old_path: None,
            status: FileStatus::Modified,
            similarity: None,
            old_mode: None,
            new_mode: None,
            flags: vec![],
            additions: 0,
            deletions: 0,
            hunks,
        }
    }

    /// `config.rs`: a hunk adding lines 10-12 (`parse_config`), one adding
    /// line 40. `main.rs`: a hunk adding line 5, with context line 6.
    fn rev() -> (Revision, [HunkId; 3]) {
        let def = make_hunk(
            "src/config.rs",
            9,
            1,
            9,
            4,
            vec![
                line_ctx(9, 9, "impl Config {"),
                line_add(10, "fn parse_config() {"),
                line_add(11, "  read()"),
                line_add(12, "}"),
            ],
        );
        let other = make_hunk(
            "src/config.rs",
            37,
            0,
            40,
            1,
            vec![line_add(40, "const NOTE_MARKER: u8 = 1;")],
        );
        let user = make_hunk(
            "src/main.rs",
            4,
            1,
            5,
            2,
            vec![
                line_add(5, "parse_config();"),
                line_ctx(5, 6, "parse_config();"),
            ],
        );
        let ids = [def.id.clone(), other.id.clone(), user.id.clone()];
        let rev = Revision {
            base: Sha::new("b"),
            head: Sha::new("h"),
            files: vec![
                file("src/config.rs", vec![def, other]),
                file("src/main.rs", vec![user]),
            ],
        };
        (rev, ids)
    }

    fn parse_config() -> CodeSymbol {
        sym(
            "src/config.rs",
            "parse_config",
            CodeSymbolKind::Function,
            (10, 12),
            10,
        )
    }

    async fn links(port: Port, rev: &Revision) -> Vec<SymbolLink> {
        symbol_links(
            Arc::new(port),
            Path::new("/wt"),
            rev,
            SymbolLinkLimits::default(),
        )
        .await
    }

    #[tokio::test]
    async fn the_same_word_found_by_text_search_links_nothing() {
        let (rev, _) = rev();
        let mut port = Port {
            symbols: vec![parse_config()],
            ..Port::default()
        };
        let mut word = hit("src/main.rs", 5, false);
        word.source = Source::Text;
        port.refs.insert(("src/config.rs".into(), 10), vec![word]);
        assert_eq!(links(port, &rev).await, Vec::<SymbolLink>::new());
    }

    #[tokio::test]
    async fn a_changed_definition_links_to_its_use_in_another_changed_hunk() {
        let (rev, [def, _, user]) = rev();
        let mut port = Port {
            symbols: vec![parse_config()],
            ..Port::default()
        };
        port.refs.insert(
            ("src/config.rs".into(), 10),
            vec![
                hit("src/config.rs", 10, true),
                // A use on an added line of another hunk: the link.
                hit("src/main.rs", 5, false),
                // On a context line, outside any hunk, in its own hunk:
                // none of them a use this PR changed.
                hit("src/main.rs", 6, false),
                hit("src/main.rs", 90, false),
                hit("src/config.rs", 11, false),
            ],
        );
        assert_eq!(
            links(port, &rev).await,
            vec![SymbolLink {
                from: def,
                to: user,
                symbol: "parse_config".into(),
            }]
        );
    }

    #[tokio::test]
    async fn an_unchanged_container_gives_way_and_common_names_are_not_asked() {
        let (rev, _) = rev();
        let port = Arc::new(Port {
            symbols: vec![
                // The impl's own line (9) is context: its users aren't
                // what changed.
                sym(
                    "src/config.rs",
                    "Config",
                    CodeSymbolKind::Struct,
                    (9, 30),
                    9,
                ),
                parse_config(),
                sym(
                    "src/config.rs",
                    "new",
                    CodeSymbolKind::Function,
                    (11, 12),
                    11,
                ),
                // Covers no added line.
                sym(
                    "src/config.rs",
                    "untouched_fn",
                    CodeSymbolKind::Function,
                    (20, 30),
                    20,
                ),
                // A plain, local-looking variable.
                sym("src/main.rs", "cfg", CodeSymbolKind::Variable, (5, 5), 5),
            ],
            ..Port::default()
        });
        symbol_links(
            port.clone(),
            Path::new("/wt"),
            &rev,
            SymbolLinkLimits::default(),
        )
        .await;
        assert_eq!(
            *port.asked.lock().unwrap(),
            vec!["parse_config".to_string()]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_worktree_opening_meanwhile_is_asked_again() {
        let (rev, [def, _, user]) = rev();
        let mut port = Port {
            symbols: vec![parse_config()],
            // Both files' first answers.
            not_open: Mutex::new(2),
            ..Port::default()
        };
        port.refs.insert(
            ("src/config.rs".into(), 10),
            vec![hit("src/main.rs", 5, false)],
        );
        let found = links(port, &rev).await;
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!((&found[0].from, &found[0].to), (&def, &user));
    }

    #[tokio::test]
    async fn a_worktree_never_open_gives_no_links() {
        let (rev, _) = rev();
        let port = Port {
            symbols: vec![parse_config()],
            not_open: Mutex::new(u32::MAX),
            ..Port::default()
        };
        assert!(links(port, &rev).await.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn the_time_budget_keeps_what_was_found_by_then() {
        let (rev, _) = rev();
        let mut port = Port {
            symbols: vec![
                parse_config(),
                sym(
                    "src/config.rs",
                    "NOTE_MARKER",
                    CodeSymbolKind::Constant,
                    (40, 40),
                    40,
                ),
            ],
            slow: vec!["NOTE_MARKER".into()],
            ..Port::default()
        };
        port.refs.insert(
            ("src/config.rs".into(), 10),
            vec![hit("src/main.rs", 5, false)],
        );
        let started = tokio::time::Instant::now();
        let found = links(port, &rev).await;
        assert!(started.elapsed() <= SymbolLinkLimits::default().budget);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].symbol, "parse_config");
    }

    /// With code intelligence the app's grouping context carries the links
    /// (without it: `grouping_context_reads_head_files_and_co_change_history`).
    #[tokio::test]
    async fn the_grouping_context_carries_links_from_code_intelligence() {
        let f = testkit::fixtures();
        *f.code_intel.symbols.lock().unwrap() = vec![sym(
            "src/lib.rs",
            "build_widget",
            CodeSymbolKind::Function,
            (2, 2),
            2,
        )];
        *f.code_intel.hits.lock().unwrap() = vec![hit("src/lib.rs", 12, false)];
        let (core, _recv) = crate::Core::start(f.deps_with_code());
        let rev =
            testkit::two_hunk_revision(&Sha::new("base000"), &Sha::new("head111"), "src/lib.rs");
        let wt = tempfile::tempdir().expect("tempdir");
        let ctx = core.grouping_context(wt.path(), &rev).await;
        let ids = testkit::hunk_ids(&rev);
        assert_eq!(
            ctx.symbol_links,
            vec![SymbolLink {
                from: ids[0].clone(),
                to: ids[1].clone(),
                symbol: "build_widget".into(),
            }]
        );
    }
}
