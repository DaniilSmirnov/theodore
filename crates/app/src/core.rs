//! `Core`: the headless app object. Owns the sync log's broadcast, the
//! revision cache, running agents, and the outbox/poller background tasks
//! (plan §2.1, §3).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use theodore_contract::{Ephemeral, SyncFrame, models};
use theodore_domain::{PrId, PrRef, PrSummary, Revision, Sha};
use theodore_ports::{NewAction, StoreResult, SyncAction};
use tokio::sync::{Mutex, Notify, RwLock, broadcast};
use tokio_util::sync::CancellationToken;

use crate::deps::Deps;
use crate::wire::to_wire_action;

/// A cached revision plus what's needed to re-derive it (git paths) without
/// re-fetching from the host.
pub(crate) struct RevisionEntry {
    pub revision: Arc<Revision>,
    pub repo_path: PathBuf,
    pub worktree_path: PathBuf,
    pub pr_ref: PrRef,
    /// Where the repo layer of the review's config is read.
    pub base: crate::usecases::review_config::ConfigBase,
}

impl RevisionEntry {
    /// Where this review's config is read.
    pub fn config_source(&self) -> crate::usecases::review_config::ReviewSource<'_> {
        crate::usecases::review_config::ReviewSource {
            repo: &self.pr_ref.repo,
            repo_path: &self.repo_path,
            worktree: &self.worktree_path,
            base: &self.base,
        }
    }
}

pub(crate) struct RunHandle {
    pub cancel: CancellationToken,
}

pub(crate) struct Inner {
    pub deps: Deps,
    pub sync_tx: broadcast::Sender<SyncFrame>,
    pub ephemeral_tx: broadcast::Sender<Ephemeral>,
    /// Serializes every `Store::apply` + broadcast so sync frames are
    /// published in the same order sync ids were assigned, with no gaps.
    pub apply_lock: Mutex<()>,
    pub last_broadcast: AtomicU64,
    pub revisions: RwLock<HashMap<(PrId, Sha), Arc<RevisionEntry>>>,
    pub current_head: RwLock<HashMap<PrId, Sha>>,
    pub etags: RwLock<HashMap<PrId, String>>,
    pub runs: RwLock<HashMap<String, RunHandle>>,
    /// Wakes the outbox worker as soon as an item is pushed, so it doesn't
    /// wait for its idle poll (which is paced by `Clock`, not wall time).
    pub outbox_notify: Notify,
    pub shutdown: CancellationToken,
    /// Serializes PR opens so two callers never clone/fetch the same PR twice.
    pub open_lock: tokio::sync::Mutex<()>,
    /// Serializes shipping spaces (Open PR / Push / Merge locally): two
    /// clicks never squash and push the same space twice.
    pub ship_lock: tokio::sync::Mutex<()>,
    /// PRs whose grouping is running, so a PR is never grouped twice at
    /// once; `true` when another run was asked for meanwhile (a push), to
    /// run on the then-current head once this one ends
    /// (`usecases::grouping::GroupingSlot`).
    pub grouping_inflight: std::sync::Mutex<HashMap<PrId, bool>>,
    /// Last host health the UI was told about (`Ephemeral::HostStatus`):
    /// online, rate-limit reset, token accepted.
    pub host_health: std::sync::Mutex<(bool, Option<i64>, bool)>,
    /// While the host is unreachable the pollers make no host calls; this
    /// paces the one probe that notices it's back (`poller::probe_host`).
    pub host_probe: std::sync::Mutex<crate::poller::HostProbe>,
    /// The device flow's `device_code` + poll interval between `login_start`
    /// and the login finishing (plan "login"): kept in memory only, never
    /// persisted — a restart just means starting over.
    pub login: Mutex<Option<crate::usecases::host::LoginSession>>,
    /// `linear_issues` results per PR, for about 5 minutes (plan "Linear"):
    /// Peek and the Review header both ask on every render, and Linear's
    /// API has no conditional-GET story to make repeat lookups free.
    pub linear_cache: std::sync::Mutex<HashMap<PrId, (i64, Vec<theodore_contract::LinearIssue>)>>,
    /// The Theodore account: session, sign-in in progress, refresher.
    pub account: crate::usecases::account::AccountState,
    /// Per space: its commands' last log lines and the ones in flight.
    pub space_rt: std::sync::Mutex<HashMap<String, crate::usecases::space_commands::SpaceRuntime>>,
    /// Serializes port reservation, so two creates never take one port.
    pub ports_lock: tokio::sync::Mutex<()>,
    /// Serializes changes to a space's setup stage (so the goal starts once).
    pub setup_lock: tokio::sync::Mutex<()>,
    /// Config problems already shown (`file\0key`): warned once per session.
    pub config_warned: std::sync::Mutex<std::collections::HashSet<String>>,
    /// Merged project configs by a hash of their layers' text
    /// (`usecases::review_config`).
    pub review_config: std::sync::Mutex<HashMap<String, Arc<crate::config::Merged>>>,
    /// A host project's cache path, by project id, once the config editor
    /// fetched it: checking as you type reads it without fetching again
    /// (`usecases::config_editor`).
    pub config_repos: std::sync::Mutex<HashMap<String, PathBuf>>,
    /// Serializes the review config's bookkeeping: the ignored-findings
    /// count on a skill run, the head `review.auto_skills` last ran at.
    pub review_lock: tokio::sync::Mutex<()>,
    /// `review.auto_skills` runs at all (`Core::set_auto_skills`).
    pub auto_skills: std::sync::atomic::AtomicBool,
    /// Worktrees open on the code-intelligence port, and the install
    /// suggestions already made (`usecases::code_intel`).
    pub code_intel: crate::usecases::code_intel::CodeIntelState,
    /// A space's open terminals (docs/spaces.md "Terminal"), by terminal
    /// id: purely in memory, like `space_rt` — never synced, gone on
    /// restart (the PTYs themselves don't survive one either).
    pub term_rt: std::sync::Mutex<HashMap<String, crate::usecases::space_terminal::TerminalEntry>>,
    /// Spaces' send queues and the turns "Send now" can steer into.
    pub space_queue: crate::usecases::space_queue::QueueState,
    /// The app's URL scheme a space's PR links back with
    /// (`Core::set_space_link_scheme`).
    pub space_link_scheme: std::sync::Mutex<Option<String>>,
    /// `ask_user` questions waiting for your answer, by question id
    /// (`usecases::space_questions`).
    pub questions:
        std::sync::Mutex<HashMap<String, crate::usecases::space_questions::PendingQuestion>>,
}

/// Inbound streams the shell (Tauri or the CLI) forwards to the UI.
pub struct Receivers {
    pub sync: broadcast::Receiver<SyncFrame>,
    pub ephemeral: broadcast::Receiver<Ephemeral>,
}

#[derive(Clone)]
pub struct Core(pub(crate) Arc<Inner>);

impl Core {
    /// Starts the core: an initial `last_sync_id` read from the store, and
    /// the outbox + poller background loops.
    pub fn start(deps: Deps) -> (Core, Receivers) {
        let (sync_tx, sync_rx) = broadcast::channel(1024);
        let (ephemeral_tx, ephemeral_rx) = broadcast::channel(1024);
        let last = deps.store.last_sync_id().unwrap_or(0);
        let open = crate::usecases::account::gate_open_at_start(&deps);

        let inner = Arc::new(Inner {
            deps,
            sync_tx,
            ephemeral_tx,
            apply_lock: Mutex::new(()),
            last_broadcast: AtomicU64::new(last),
            revisions: RwLock::new(HashMap::new()),
            current_head: RwLock::new(HashMap::new()),
            etags: RwLock::new(HashMap::new()),
            runs: RwLock::new(HashMap::new()),
            outbox_notify: Notify::new(),
            shutdown: CancellationToken::new(),
            open_lock: tokio::sync::Mutex::new(()),
            ship_lock: tokio::sync::Mutex::new(()),
            grouping_inflight: std::sync::Mutex::new(HashMap::new()),
            host_health: std::sync::Mutex::new((true, None, true)),
            host_probe: std::sync::Mutex::new(crate::poller::HostProbe::default()),
            login: Mutex::new(None),
            linear_cache: std::sync::Mutex::new(HashMap::new()),
            account: crate::usecases::account::AccountState::new(open),
            space_rt: std::sync::Mutex::new(HashMap::new()),
            ports_lock: tokio::sync::Mutex::new(()),
            setup_lock: tokio::sync::Mutex::new(()),
            config_warned: std::sync::Mutex::new(std::collections::HashSet::new()),
            review_config: crate::usecases::review_config::new_cache(),
            config_repos: std::sync::Mutex::new(HashMap::new()),
            review_lock: tokio::sync::Mutex::new(()),
            auto_skills: std::sync::atomic::AtomicBool::new(true),
            code_intel: Default::default(),
            term_rt: std::sync::Mutex::new(HashMap::new()),
            space_queue: Default::default(),
            space_link_scheme: std::sync::Mutex::new(None),
            questions: std::sync::Mutex::new(HashMap::new()),
        });

        let core = Core(inner.clone());
        {
            let core = core.clone();
            tokio::spawn(async move {
                core.fail_interrupted_runs().await;
                core.fail_interrupted_spaces().await;
            });
        }
        // Host traffic waits for the account gate (`account_hold`): the
        // outbox and the poller hold on every tick, the start-up inbox
        // refresh and clones before they begin. Free when sign-in isn't
        // required; else they start once signed in.
        tokio::spawn(crate::outbox::run(inner.clone()));
        tokio::spawn(crate::poller::run(inner.clone()));
        tokio::spawn(crate::sync::compactor::run(inner.clone()));
        {
            let core = core.clone();
            tokio::spawn(async move {
                if core.account_hold().await {
                    let _ = core.refresh_inbox().await;
                }
            });
        }
        {
            let core = core.clone();
            tokio::spawn(async move {
                if core.account_hold().await {
                    let _ = core.resume_cloning_projects().await;
                }
            });
        }
        {
            let core = core.clone();
            tokio::spawn(async move { core.account_resume().await });
        }

        (
            core,
            Receivers {
                sync: sync_rx,
                ephemeral: ephemeral_rx,
            },
        )
    }

    /// Stops the background loops. Idempotent.
    /// Runs left `running` by a previous process can never finish: mark them
    /// failed so the UI doesn't show them spinning forever.
    async fn fail_interrupted_runs(&self) {
        use theodore_contract::models;
        let store = self.0.deps.store.clone();
        let Ok(Ok(rows)) = tokio::task::spawn_blocking(move || store.all(models::SKILL_RUN)).await
        else {
            return;
        };
        let actions: Vec<theodore_ports::NewAction> = rows
            .into_iter()
            .filter(|r| r.data.get("status").and_then(|s| s.as_str()) == Some("running"))
            .map(|r| theodore_ports::NewAction {
                model: models::SKILL_RUN.into(),
                model_id: r.id,
                action: theodore_ports::SyncActionCode::C,
                data: Some(serde_json::json!({ "status": "failed", "error": "interrupted: Theodore was closed" })),
            })
            .collect();
        if !actions.is_empty() {
            let _ = self.apply(actions).await;
        }
    }

    pub fn shutdown(&self) {
        self.0.shutdown.cancel();
    }

    pub fn deps(&self) -> &Deps {
        &self.0.deps
    }

    /// Every PR open in this session (plan "worktree locks"): the GC
    /// keep-list must never drop one of these, even if its host state has
    /// gone to closed/merged underneath it since it was opened.
    pub async fn open_pr_refs(&self) -> Vec<PrRef> {
        let current = self.0.current_head.read().await;
        let revisions = self.0.revisions.read().await;
        current
            .iter()
            .filter_map(|(pr, head)| revisions.get(&(pr.clone(), head.clone())))
            .map(|e| e.pr_ref.clone())
            .collect()
    }

    /// Whether `err` is about the connection itself (shown by a banner),
    /// rather than about one request.
    pub(crate) fn is_connection_error(err: &theodore_ports::HostError) -> bool {
        use theodore_ports::HostError;
        matches!(
            err,
            HostError::Auth | HostError::RateLimited { .. } | HostError::Network(_)
        )
    }

    /// Whether the host answered the last read (host health's `online`).
    pub(crate) fn host_online(&self) -> bool {
        self.0
            .host_health
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .0
    }

    /// Reports a host read's outcome as host health, so the UI's offline,
    /// token and rate-limit banners reflect reads (inbox, polling, opening
    /// a PR) and not only outbox writes. Emits only when health changes.
    pub(crate) fn note_host<T>(&self, result: &theodore_ports::HostResult<T>) {
        use theodore_ports::HostError;
        let next = match result {
            Ok(_) => (true, None, true),
            Err(HostError::Auth) => (true, None, false),
            Err(HostError::RateLimited { reset_at }) => (true, Some(*reset_at), true),
            Err(HostError::Network(_)) => (false, None, true),
            // A 404, a validation error, … says nothing about the connection.
            Err(_) => return,
        };
        let (changed, went_offline) = {
            let mut health = self.0.host_health.lock().unwrap_or_else(|p| p.into_inner());
            let changed = *health != next;
            let went_offline = health.0 && !next.0;
            *health = next;
            (changed, went_offline)
        };
        {
            let now = self.0.deps.clock.now();
            let mut probe = self.0.host_probe.lock().unwrap_or_else(|p| p.into_inner());
            if went_offline {
                probe.arm(now);
            } else if next.0 {
                probe.reset();
            }
        }
        // Every failure is re-sent (a window that opened after the first one
        // must still learn of it); recovery only once.
        if changed || result.is_err() {
            self.emit(Ephemeral::HostStatus {
                online: next.0,
                rate_limited_until: next.1,
                auth_ok: next.2,
            });
        }
    }

    pub fn emit(&self, event: Ephemeral) {
        let _ = self.0.ephemeral_tx.send(event);
    }

    /// Applies a batch through `Store::apply` on `spawn_blocking`, then
    /// broadcasts one contiguous `SyncFrame`. The single `apply_lock` keeps
    /// concurrent writers (transactions, the outbox, the poller, agent
    /// runs) from interleaving sync ids out of order.
    pub(crate) async fn apply(&self, actions: Vec<NewAction>) -> StoreResult<Vec<SyncAction>> {
        if actions.is_empty() {
            return Ok(Vec::new());
        }
        let _guard = self.0.apply_lock.lock().await;
        let store = self.0.deps.store.clone();
        let applied = tokio::task::spawn_blocking(move || store.apply(actions))
            .await
            .map_err(|e| theodore_ports::StoreError::Db(e.to_string()))??;
        self.publish(&applied);
        Ok(applied)
    }

    /// Queues an outbox item and wakes the worker immediately (it would
    /// otherwise only notice on its next, `Clock`-paced idle poll).
    pub(crate) async fn enqueue_outbox(
        &self,
        op: &'static str,
        payload: serde_json::Value,
    ) -> theodore_ports::StoreResult<u64> {
        let store = self.0.deps.store.clone();
        let id = tokio::task::spawn_blocking(move || store.outbox_push(op, payload))
            .await
            .map_err(|e| theodore_ports::StoreError::Db(e.to_string()))??;
        self.0.outbox_notify.notify_one();
        Ok(id)
    }

    /// `apply` plus `enqueue_outbox`, atomically (plan "Fix target" accept
    /// flow): a caller that both changes a model row and enqueues a host
    /// write for it must not land one half without the other.
    pub(crate) async fn apply_with_outbox(
        &self,
        actions: Vec<NewAction>,
        op: &'static str,
        payload: serde_json::Value,
    ) -> theodore_ports::StoreResult<Vec<SyncAction>> {
        let _guard = self.0.apply_lock.lock().await;
        let store = self.0.deps.store.clone();
        let (applied, _outbox_id) =
            tokio::task::spawn_blocking(move || store.apply_with_outbox(actions, op, payload))
                .await
                .map_err(|e| theodore_ports::StoreError::Db(e.to_string()))??;
        self.publish(&applied);
        self.0.outbox_notify.notify_one();
        Ok(applied)
    }

    /// Looks up the host-side `PrRef` for a locally known PR (outbox calls
    /// need it; the UI only ever deals in `PrId`).
    pub(crate) async fn pr_ref(&self, pr: &PrId) -> Option<PrRef> {
        if let Some(entry) = self
            .0
            .revisions
            .read()
            .await
            .values()
            .find(|e| &e.pr_ref.id() == pr)
        {
            return Some(entry.pr_ref.clone());
        }
        let store = self.0.deps.store.clone();
        let id = pr.0.clone();
        let row = tokio::task::spawn_blocking(move || store.get(models::PR, &id))
            .await
            .ok()?
            .ok()??;
        let summary: PrSummary = serde_json::from_value(row.data).ok()?;
        Some(summary.pr)
    }

    /// A search result older than the stored row (its `updated_at` is
    /// behind): the search started before a write the core already applied
    /// — a merge, a fresh `get_pr` — and must not roll the row back.
    pub(crate) async fn is_stale_summary(&self, pr_id: &str, incoming_updated_at: i64) -> bool {
        let store = self.0.deps.store.clone();
        let id = pr_id.to_string();
        let stored = tokio::task::spawn_blocking(move || store.get(models::PR, &id))
            .await
            .ok()
            .and_then(|r| r.ok())
            .flatten();
        stored
            .and_then(|r| r.data.get("updated_at").and_then(|v| v.as_i64()))
            .is_some_and(|stored_at| stored_at > incoming_updated_at)
    }

    /// `reasons` comes from the inbox searches, not from fetching one PR or
    /// a project's PR list: those carry none, and writing them as-is drops
    /// a PR out of the inbox. Keep what the inbox last said.
    pub(crate) async fn keep_inbox_reasons(&self, pr_id: &str, summary: &mut serde_json::Value) {
        let empty = summary
            .get("reasons")
            .and_then(|v| v.as_array())
            .is_none_or(|a| a.is_empty());
        if !empty {
            return;
        }
        let store = self.0.deps.store.clone();
        let id = pr_id.to_string();
        let stored = tokio::task::spawn_blocking(move || store.get(models::PR, &id))
            .await
            .ok()
            .and_then(|r| r.ok())
            .flatten();
        if let Some(reasons) = stored
            .as_ref()
            .and_then(|r| r.data.get("reasons"))
            .filter(|v| v.as_array().is_some_and(|a| !a.is_empty()))
        {
            summary["reasons"] = reasons.clone();
        }
    }

    /// Writes `pr` + `pr_detail` from an already-fetched `PrDetail` (plan
    /// §3/§6): the one place every path that has a fresh `PrDetail` —
    /// opening a PR, a push, a host-write follow-up refresh — turns it
    /// into sync actions, so the shape can't drift between them.
    pub(crate) async fn apply_pr_detail(
        &self,
        detail: &theodore_domain::PrDetail,
    ) -> crate::error::AppResult<()> {
        let pr_s = detail.summary.pr.id().0.clone();
        let mut summary_data = serde_json::to_value(&detail.summary)
            .map_err(|e| crate::error::AppError::Invalid(e.to_string()))?;
        self.keep_inbox_reasons(&pr_s, &mut summary_data).await;
        let detail_data = serde_json::to_value(detail)
            .map_err(|e| crate::error::AppError::Invalid(e.to_string()))?;
        let actions = vec![
            NewAction {
                model: models::PR.into(),
                model_id: pr_s.clone(),
                action: theodore_ports::SyncActionCode::U,
                data: Some(summary_data),
            },
            NewAction {
                model: models::PR_DETAIL.into(),
                model_id: pr_s,
                action: theodore_ports::SyncActionCode::U,
                data: Some(detail_data),
            },
        ];
        self.apply(actions).await?;
        Ok(())
    }

    /// Conditionally fetches `pr` by its etag and, if the host reports it
    /// modified, applies the fresh `pr_detail` — regardless of whether the
    /// head moved (plan §6.6): comments, threads, checks and labels can all
    /// change without a push, and must not be silently dropped. Returns the
    /// detail so a caller (the poller) can keep using it without fetching
    /// twice.
    pub(crate) async fn refresh_pr(
        &self,
        pr: &PrId,
        pr_ref: &PrRef,
    ) -> Option<theodore_domain::PrDetail> {
        let etag = self.0.etags.read().await.get(pr).cloned();
        let fetched = self.0.deps.host.get_pr(pr_ref, etag.as_deref()).await;
        // A local branch review's "host" is this machine: it says nothing
        // about the network.
        if pr_ref.repo.host != theodore_domain::HostKind::Local {
            self.note_host(&fetched);
        }
        let theodore_ports::Fetched::Modified { etag, value } = fetched.ok()? else {
            return None;
        };
        if let Some(etag) = etag {
            self.0.etags.write().await.insert(pr.clone(), etag);
        }
        self.apply_pr_detail(&value).await.ok()?;
        Some(value)
    }

    fn publish(&self, actions: &[SyncAction]) {
        if actions.is_empty() {
            return;
        }
        let from = self.0.last_broadcast.load(Ordering::SeqCst);
        let last = actions.last().expect("non-empty").id;
        let frame = SyncFrame {
            from_sync_id: from,
            last_sync_id: last,
            actions: actions.iter().cloned().map(to_wire_action).collect(),
            resync_required: false,
        };
        self.0.last_broadcast.store(last, Ordering::SeqCst);
        let _ = self.0.sync_tx.send(frame);
    }
}
