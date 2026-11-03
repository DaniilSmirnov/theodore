//! Spaces (docs/spaces.md): the stage before a pull request. A space is a
//! detached worktree of a project's cache with its checkpoints on
//! `refs/theodore/spaces/<id>`; each chat message runs one agent turn in
//! it, the turn ends in a checkpoint commit, and the branch review of
//! `base...checkpoint` (the `open_branch` plumbing) is the live diff and the
//! grouped Review.

use std::path::{Path, PathBuf};

use theodore_contract::{
    ChatMessage, CreateSpaceRequest, Ephemeral, OpenPrResponse, Project, Space, SpaceMode,
    SpaceReviewRequest, SpaceSendRequest, SpaceStatus, SpaceTurn, ToastLevel, models,
};
use theodore_domain::{AgentId, Author, PrId, PrRef};
use theodore_ports::{
    AgentAttachment, AgentEvent, AgentMode, AgentRequest, NewAction, SPACE_REF_PREFIX,
    SyncActionCode, space_ref,
};
use tokio_util::sync::CancellationToken;

use crate::core::{Core, RunHandle};
use crate::deps::CodeTools;
use crate::error::{AppError, AppResult};
use crate::usecases::attachments::{agent_attachments, with_attachments};
use crate::usecases::live::LiveRun;
use crate::usecases::projects::stable_hash_u32;
use crate::usecases::skills::{expand_skill, space_skill_content};
use crate::usecases::space_queue::TurnEnd;

/// The `chat_message.pr` key a space's transcript is stored under.
pub fn space_chat_key(space: &str) -> PrId {
    PrId(format!("space:{space}"))
}

/// Messages of history replayed into a turn that starts a new agent session
/// (docs/spaces.md §5 "One session per space").
pub(crate) const HISTORY_MESSAGES: usize = 12;
/// Per replayed message.
const HISTORY_CHARS: usize = 2_000;

impl Core {
    pub async fn create_space(&self, mut req: CreateSpaceRequest) -> AppResult<Space> {
        let project = self
            .project_row(&req.project)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("project {}", req.project)))?;
        // From a host issue (rewrites `req.goal`), an existing branch, or an
        // existing PR (docs/spaces.md §3 "New space") — `None` for the
        // common case, a fresh branch off the project's default.
        let start = self.resolve_space_start(&project, &mut req).await?;
        let base = req
            .base
            .clone()
            .or_else(|| project.default_branch.clone())
            .ok_or_else(|| {
                AppError::Invalid(format!("project {} has no default branch yet", project.id))
            })?;
        // Whether the dialog's own name/branch wins over the agent's later
        // proposal (`space_naming::apply`, docs/spaces.md §3 "New space").
        let user_named = req
            .name
            .as_deref()
            .map(str::trim)
            .is_some_and(|n| !n.is_empty());
        // An adopted branch/PR is never renamed by the later proposal
        // either — it's not this space's to rename, and `fork_sha` tracks
        // it by this exact name.
        let user_branched = req.branch.is_some() || start.adopt.is_some();
        let name = match req.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
            Some(n) => n.to_string(),
            // Named after the goal; a second "fix login" in the project
            // becomes `fix-login-2`, and so does its branch.
            None => {
                let siblings = self.list_spaces(Some(project.id.clone())).await?;
                let taken: Vec<&str> = siblings.iter().map(|s| s.name.as_str()).collect();
                unique_name(&slug(&req.goal), &taken)
            }
        };
        let branch = match (&start.adopt, req.branch.clone()) {
            // An adopted branch/PR keeps its own name — never renamed into
            // `<login>/<slug>`, since shipping pushes to it directly.
            (Some(a), _) => a.branch.clone(),
            (None, Some(b)) => b,
            // Named after you on the host (docs/spaces.md §12); a local
            // folder, or a host we can't reach, gets `space/<slug>`.
            (None, None) => crate::usecases::space_naming::branch_for(self, &project, &name).await,
        };
        if !is_valid_branch(&branch) {
            return Err(AppError::Invalid(format!("bad branch name {branch:?}")));
        }
        let repo_path = self.ensure_project_cache(&project).await?;
        let base_sha = self.0.deps.git.resolve_ref(&repo_path, &base).await?;
        // The worktree starts on the adopted branch's tip when there is
        // one; `base_sha` stays the diff base (docs/spaces.md §3 "the diff
        // is the whole branch vs its base").
        let fork_sha = start.adopt.as_ref().map_or(&base_sha, |a| &a.fork_sha);
        // The project's config (docs/config.md), as of the space's base.
        let layers = self.project_layers(&project, Some(&base_sha)).await;
        let cfg = self.config_with_trust(&layers, &project, false).await;
        self.apply_review_agents(&cfg.merged, &project.repo).await;

        let agent = match req.agent {
            Some(a) => a,
            None => {
                let configured = self
                    .setting_agent_scoped(
                        "agents.chat",
                        Some(&project.repo.slug()),
                        AgentId::Claude,
                    )
                    .await;
                self.effective_agent(configured).await
            }
        };

        let id = new_space_id();
        let wt = self
            .0
            .deps
            .git
            .space_worktree(&repo_path, &id, fork_sha)
            .await?;

        // Files from the checkout, then ports for the space's life.
        let mut notes = Vec::new();
        let copy = cfg.merged.config.spaces.effective_copy();
        match (&project.path, copy.is_empty()) {
            (_, true) => {}
            (Some(root), false) => {
                let (root, dest) = (PathBuf::from(root), wt.path.clone());
                notes.extend(
                    tokio::task::spawn_blocking(move || {
                        crate::usecases::space_commands::copy_into(&root, &dest, &copy)
                    })
                    .await
                    .map_err(AppError::join)?,
                );
            }
            (None, false) => notes
                .push("copy: skipped, this project has no local checkout to copy from".to_string()),
        }
        let ports_guard = self.0.ports_lock.lock().await;
        let ports = self
            .reserve_ports(cfg.merged.config.spaces.port_count(), &id)
            .await
            .unwrap_or_else(|e| {
                notes.push(format!("ports: {e}"));
                Vec::new()
            });
        let has_goal = !req.goal.trim().is_empty();
        let setup = crate::usecases::space_hooks::initial_setup(&cfg, has_goal, notes);
        let adopted_pr = start.adopt.as_ref().and_then(|a| a.pr.clone());
        let fork_sha = start.adopt.map(|a| a.fork_sha);

        let now = self.0.deps.clock.now();
        let space = Space {
            id: id.clone(),
            project: project.id.clone(),
            name,
            goal: req.goal.trim().to_string(),
            branch,
            base,
            base_sha,
            head_sha: wt.head,
            worktree: wt.path.display().to_string(),
            status: SpaceStatus::Idle,
            agent,
            model: req.model.clone(),
            mode: SpaceMode::Edit,
            review_pr: None,
            pr: adopted_pr.as_ref().map(|(r, _)| r.id()),
            turns: 0,
            run: None,
            error: None,
            shipped: None,
            done_at: None,
            turn_log: Vec::new(),
            ports,
            setup,
            process: None,
            commands_denied: false,
            agent_session: None,
            sent_feedback: Vec::new(),
            created_at: now,
            updated_at: now,
            title: None,
            handoff_summary: None,
            progress: None,
            updating: None,
            rounds: Vec::new(),
            continue_note: None,
            fork_sha,
            issue: start.issue,
            pinned: false,
            build_mode: None,
            question: None,
            mirror: None,
        };
        self.apply(vec![NewAction {
            model: models::SPACE.into(),
            model_id: id.clone(),
            action: SyncActionCode::I,
            data: Some(to_json(&space)?),
        }])
        .await?;
        drop(ports_guard);
        self.spawn_space_code(&space, &project);
        theodore_telemetry::product(theodore_telemetry::ProductEvent::SpaceCreated {
            mode: crate::usecases::telemetry::wire_str(&space.mode),
        });
        // An adopted PR (`start_from: Pr`, or a `Branch` that already has
        // one): store what's known now, fetch the rest, so the space opens
        // "In review" without waiting for the next poll.
        match adopted_pr {
            Some((_, Some(detail))) => {
                let _ = self.apply_pr_detail(&detail).await;
            }
            Some((pr_ref, None)) => {
                self.refresh_pr(&pr_ref.id(), &pr_ref).await;
            }
            None => {}
        }

        // The empty diff exists from the start, so the page has a review id.
        self.space_review(SpaceReviewRequest {
            space: id.clone(),
            group: false,
        })
        .await?;

        let setup_status = space.setup.as_ref().map(|s| s.status);
        if setup_status == Some(theodore_contract::SetupStatus::Running) {
            // The goal waits for setup (`goal_pending`).
            self.spawn_setup(&id);
        } else if !space.goal.is_empty() && !space.setup.as_ref().is_some_and(|s| s.goal_pending) {
            self.space_send(SpaceSendRequest {
                space: id.clone(),
                text: space.goal.clone(),
                agent: None,
                model: None,
                mode: None,
                attachments: None,
            })
            .await?;
        }
        // A better name and a chat title, from the chat agent (never turn
        // 1's own, which the dialog may have overridden): backgrounded, so
        // it never delays turn 1 (`space_naming`).
        if !space.goal.is_empty() {
            let configured = self
                .setting_agent_scoped("agents.chat", Some(&project.repo.slug()), AgentId::Claude)
                .await;
            let chat_agent = self.effective_agent(configured).await;
            let core = self.clone();
            let (id2, goal, cwd) = (id.clone(), space.goal.clone(), wt.path.clone());
            tokio::spawn(async move {
                crate::usecases::space_naming::apply(
                    &core,
                    id2,
                    chat_agent,
                    goal,
                    cwd,
                    user_named,
                    user_branched,
                )
                .await;
            });
        }
        self.space_row(&id).await
    }

    /// Every space, or one project's; newest activity first.
    pub async fn list_spaces(&self, project: Option<String>) -> AppResult<Vec<Space>> {
        let store = self.0.deps.store.clone();
        let rows = tokio::task::spawn_blocking(move || store.all(models::SPACE))
            .await
            .map_err(AppError::join)??;
        let mut spaces: Vec<Space> = rows
            .into_iter()
            .filter_map(|r| serde_json::from_value(r.data).ok())
            .filter(|s: &Space| project.as_ref().is_none_or(|p| &s.project == p))
            .collect();
        spaces.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
        Ok(spaces)
    }

    /// Stores the message and starts one agent turn in the space's worktree;
    /// while a turn runs it is queued for its own turn instead
    /// (docs/spaces.md §8). Returns the message id; the reply arrives as a
    /// `chat_message` row.
    pub async fn space_send(&self, req: SpaceSendRequest) -> AppResult<String> {
        let _queue = self.space_queue_lock(&req.space).await;
        self.space_send_as(req, None).await
    }

    /// `space_send`, with `prompt` (when set) as the turn's ask instead of
    /// the message as typed (an approved plan, `space_plan`). Caller holds
    /// the space's queue lock.
    pub(crate) async fn space_send_as(
        &self,
        req: SpaceSendRequest,
        prompt: Option<String>,
    ) -> AppResult<String> {
        let space = self.space_row(&req.space).await?;
        if space.status == SpaceStatus::Archived {
            return Err(AppError::Invalid("this space is archived".into()));
        }
        let running = space.status == SpaceStatus::Running;
        if !running {
            self.check_setup_done(&space)?;
        }
        let text = req.text.trim().to_string();
        // Checked before anything is stored: an unknown id fails the send.
        let attachments = self.resolve_attachments(req.attachments.as_deref()).await?;
        if text.is_empty() && attachments.is_empty() {
            return Err(AppError::Invalid("empty message".into()));
        }
        if running {
            if prompt.is_some() {
                // Its ask isn't the message: it can't wait in the queue.
                return Err(AppError::Invalid(
                    "the agent is still working in this space".into(),
                ));
            }
            return self.queue_message(&space, &req, text, attachments).await;
        }
        let message = ChatMessage {
            id: uuid::Uuid::new_v4().to_string(),
            pr: space_chat_key(&space.id),
            author: Author::Me,
            text,
            refs: vec![],
            at: self.0.deps.clock.now(),
            failed: None,
            attachments,
            queued: None,
        };
        self.start_turn(space, &req, message, false, prompt).await
    }

    /// Refused while the space sets up: the goal goes first.
    pub(crate) fn check_setup_done(&self, space: &Space) -> AppResult<()> {
        if let Some(setup) = &space.setup
            && matches!(
                setup.status,
                theodore_contract::SetupStatus::Running | theodore_contract::SetupStatus::Waiting
            )
        {
            return Err(AppError::Invalid("the space is still setting up".into()));
        }
        Ok(())
    }

    /// Stores `message` (new, or a queued one now sent: `stored`) and
    /// starts its turn with `req`'s picks; `prompt`, when set, is the
    /// turn's ask instead of the message (an approved plan). Caller holds
    /// the space's queue lock; the space isn't running.
    pub(crate) async fn start_turn(
        &self,
        space: Space,
        req: &SpaceSendRequest,
        message: ChatMessage,
        stored: bool,
        prompt: Option<String>,
    ) -> AppResult<String> {
        let project = self.space_project(&space).await?;
        let (text, attachments) = (message.text.clone(), message.attachments.clone());

        let agent = req.agent.unwrap_or(space.agent);
        let model = req.model.clone().or_else(|| space.model.clone());
        let mode = req.mode.unwrap_or(space.mode);

        // A worktree removed by hand comes back from the checkpoint ref.
        let repo_path = self.ensure_project_cache(&project).await?;
        let wt = self
            .0
            .deps
            .git
            .space_worktree(&repo_path, &space.id, &space.base_sha)
            .await?;

        let history = self.space_history(&space).await?;
        let message_id = message.id.clone();
        let now = message.at;
        let run_id = format!("space:{}:{message_id}", space.id);
        let mut patch = serde_json::json!({
            "status": SpaceStatus::Running,
            "run": run_id,
            "agent": agent,
            "model": model,
            "mode": mode,
            "error": null,
            // Said once, in this turn's prompt.
            "continue_note": null,
            "updated_at": now,
        });
        if matches!(mode, SpaceMode::Edit | SpaceMode::Run) {
            // What an approved plan is built in (docs/spaces.md §5).
            patch["build_mode"] = serde_json::json!(mode);
        }
        self.apply(vec![
            NewAction {
                model: models::CHAT_MESSAGE.into(),
                model_id: message_id.clone(),
                // A queued one is rewritten: it stops being queued.
                action: if stored {
                    SyncActionCode::U
                } else {
                    SyncActionCode::I
                },
                data: Some(to_json(&message)?),
            },
            NewAction {
                model: models::SPACE.into(),
                model_id: space.id.clone(),
                action: SyncActionCode::C,
                data: Some(patch),
            },
        ])
        .await?;

        let cancel = CancellationToken::new();
        self.0.runs.write().await.insert(
            run_id.clone(),
            RunHandle {
                cancel: cancel.clone(),
            },
        );
        // `/<skill> task` runs one of your or the repo's skills as this
        // turn's instructions, and the attached files are listed after it;
        // the transcript keeps the text as typed.
        let claude_home = &self.0.deps.claude_home;
        let ask = prompt.unwrap_or_else(|| {
            expand_skill(&text, |id| space_skill_content(id, &wt.path, claude_home))
                .unwrap_or_else(|| text.clone())
        });
        let ask = with_attachments(ask, &attachments);
        let ultracode = agent == AgentId::Claude
            && self.setting_string("agents.claude.effort", "").await.trim() == ULTRACODE;
        let wrap = |prompt: String| {
            let prompt = crate::usecases::space_pr::with_continue_note(&space, prompt);
            with_ultracode(with_failed_check(&space, prompt), ultracode)
        };
        // The same agent in the same worktree continues its session and
        // hears only the new turn; anything else starts a new session from
        // the replayed transcript.
        let resume = resumable_session(&space, agent, &wt.path);
        // Can't resume: a cached, once-generated summary of everything
        // before the replayed window stands in for the full transcript
        // (`space_handoff`). The rarer case — a resume that turns out to
        // be gone mid-run — keeps today's plain replay; it's already
        // failing once, so it doesn't also pay for a summarizer call.
        let (prompt, replay) = match &resume {
            Some(_) => (wrap(ask.clone()), Some(wrap(turn_prompt(&history, &ask)))),
            None => (
                wrap(
                    crate::usecases::space_handoff::replay_prompt(
                        self, &space, agent, &wt.path, &history, &ask,
                    )
                    .await,
                ),
                None,
            ),
        };
        // Opens the worktree on code intelligence; the turn's mode decides
        // whether its tools may edit.
        let code = match self.0.deps.tools {
            Some(_) => {
                let at = Space {
                    worktree: wt.path.display().to_string(),
                    ..space.clone()
                };
                self.space_code_tools(&at, &project, mode).await
            }
            None => CodeTools::Off,
        };
        let turn = Turn {
            run_id,
            message: message_id.clone(),
            space: space.clone(),
            agent,
            model,
            mode,
            prompt,
            resume,
            replay,
            system: self
                .space_review_rules(&space, &project, &wt.path)
                .await
                .with_system(Some(crate::usecases::space_plan::with_plan_rules(
                    system_prompt(
                        &space,
                        self.0.deps.tools.is_some(),
                        code,
                        self.user_mcp_allowed().await,
                    ),
                    mode,
                )))
                .unwrap_or_default(),
            env: self.space_agent_env(&space).await,
            attachments: agent_attachments(&attachments),
            cwd: wt.path,
            cancel,
            code,
        };
        let core = self.clone();
        let span = tracing::info_span!("space_turn", space = %space.id, agent = agent.as_str());
        tokio::spawn(tracing::Instrument::instrument(
            async move { drive_turn(&core, turn).await },
            span,
        ));
        Ok(message_id)
    }

    /// Cancels the turn in flight; its edits are still checkpointed.
    pub async fn space_stop(&self, space: &str) -> AppResult<()> {
        let space = self.space_row(space).await?;
        if let Some(run) = &space.run
            && let Some(handle) = self.0.runs.write().await.remove(run)
        {
            handle.cancel.cancel();
        }
        Ok(())
    }

    /// Opens (or refreshes, after a new checkpoint) the branch review of
    /// `base_sha...checkpoint`; `group` also groups it by meaning.
    pub async fn space_review(&self, req: SpaceReviewRequest) -> AppResult<OpenPrResponse> {
        let space = self.space_row(&req.space).await?;
        let response = {
            let _guard = self.0.open_lock.lock().await;
            self.open_space_review(&space, req.group).await?
        };
        let pr_id = response.pr.clone();
        if space.review_pr.as_ref() != Some(&pr_id) {
            self.apply(vec![NewAction {
                model: models::SPACE.into(),
                model_id: space.id.clone(),
                action: SyncActionCode::C,
                data: Some(serde_json::json!({ "review_pr": pr_id })),
            }])
            .await?;
        }
        Ok(response)
    }

    /// Checkpoints uncommitted edits, removes the worktree and the review;
    /// the checkpoint ref and the transcript stay.
    pub async fn archive_space(&self, space: &str) -> AppResult<()> {
        // Not while a turn hands over to the next queued message.
        let _queue = self.space_queue_lock(space).await;
        let space = self.space_row(space).await?;
        if space.status == SpaceStatus::Running {
            return Err(AppError::Invalid(
                "stop the agent before archiving this space".into(),
            ));
        }
        if space.status == SpaceStatus::Archived {
            return Ok(());
        }
        let project = self.space_project(&space).await?;
        let repo_path = self.ensure_project_cache(&project).await?;
        // docs/config.md: stop `run`, checkpoint, `archive`, then remove.
        self.stop_space_commands(&space.id).await;
        // docs/spaces.md "Terminal": a shell must not keep writing into a
        // worktree about to be removed.
        self.close_space_terminals(&space.id).await;
        let worktree_exists = Path::new(&space.worktree).exists();
        if worktree_exists {
            let saved = self
                .0
                .deps
                .git
                .space_checkpoint(
                    Path::new(&space.worktree),
                    &space.id,
                    &format!("{}: uncommitted changes at archive", space.name),
                )
                .await;
            match saved {
                // Its `.git` is gone: nothing git can save, and the remove
                // below moves the directory aside instead of deleting it.
                Ok(_) | Err(theodore_ports::GitError::NotAWorktree(_)) => {}
                Err(e) => return Err(e.into()),
            }
        }
        self.run_archive_command(&space, &project, worktree_exists)
            .await;
        self.0
            .deps
            .git
            .remove_space_worktree(&repo_path, &space.id)
            .await?;
        self.close_code_root(Path::new(&space.worktree)).await;
        if let Some(review) = &space.review_pr {
            // Best-effort: a review that was never stored is already gone.
            let _ = self.close_branch_review(review).await;
        }
        self.apply(vec![NewAction {
            model: models::SPACE.into(),
            model_id: space.id.clone(),
            action: SyncActionCode::C,
            data: Some(serde_json::json!({
                "status": SpaceStatus::Archived,
                "review_pr": null,
                // Released: another space may take them now.
                "ports": [],
                "updated_at": self.0.deps.clock.now(),
            })),
        }])
        .await?;
        Ok(())
    }

    /// Undo last turn (docs/spaces.md §7): the worktree and the checkpoint
    /// ref go back to where the turn started; the transcript keeps the turn,
    /// marked undone, and later prompts leave it out. Refused while a turn
    /// runs, on an archived space, and once the turn was shipped.
    pub async fn undo_space_turn(&self, space: &str) -> AppResult<Space> {
        // Not while a ship reads the checkpoint.
        let _guard = self.0.ship_lock.lock().await;
        let _queue = self.space_queue_lock(space).await;
        let space = self.space_row(space).await?;
        let i = undoable_turn(&space).map_err(AppError::Invalid)?;
        let turn = space.turn_log[i].clone();
        let project = self.space_project(&space).await?;
        let repo_path = self.ensure_project_cache(&project).await?;
        let wt = self
            .0
            .deps
            .git
            .space_worktree(&repo_path, &space.id, &space.base_sha)
            .await?;
        self.0
            .deps
            .git
            .space_reset(&wt.path, &space.id, &turn.before)
            .await
            .map_err(|e| match e {
                theodore_ports::GitError::Busy(busy) => AppError::Invalid(format!(
                    "can't undo: {busy} in the space's worktree; finish or abort it first"
                )),
                e => e.into(),
            })?;
        let mut log = space.turn_log.clone();
        log[i].undone = true;
        let mut data = serde_json::json!({
            "status": SpaceStatus::Idle,
            "head_sha": turn.before,
            "error": null,
            "turn_log": log,
            // The agent's session still holds the undone turn: the next
            // one starts over from the transcript, which leaves it out.
            "agent_session": null,
            "updated_at": self.0.deps.clock.now(),
        });
        // The turn completed an update from base: the old base is back.
        if let Some(base) = &turn.base_before {
            data["base_sha"] = serde_json::json!(base);
        }
        self.apply(vec![NewAction {
            model: models::SPACE.into(),
            model_id: space.id.clone(),
            action: SyncActionCode::C,
            data: Some(data),
        }])
        .await?;
        if turn.before != turn.after {
            self.space_review(SpaceReviewRequest {
                space: space.id.clone(),
                group: false,
            })
            .await?;
        }
        self.space_row(&space.id).await
    }

    /// Brings an archived space back: its worktree is recreated from the
    /// checkpoint ref and the live diff reopened. A space that was done
    /// starts its auto-archive countdown again.
    pub async fn unarchive_space(&self, space: &str) -> AppResult<Space> {
        let space = self.space_row(space).await?;
        if space.status != SpaceStatus::Archived {
            return Ok(space);
        }
        let project = self.space_project(&space).await?;
        let repo_path = self.ensure_project_cache(&project).await?;
        let wt = self
            .0
            .deps
            .git
            .space_worktree(&repo_path, &space.id, &space.base_sha)
            .await?;
        // Ports were released on archive: reserve fresh ones.
        let cfg = self.space_config_of(&space, &project).await;
        let ports_guard = self.0.ports_lock.lock().await;
        let ports = self
            .reserve_ports(cfg.merged.config.spaces.port_count(), &space.id)
            .await
            .unwrap_or_else(|e| {
                self.emit(Ephemeral::Toast {
                    level: ToastLevel::Warn,
                    text: format!("{}: {e}", space.name),
                });
                Vec::new()
            });
        let now = self.0.deps.clock.now();
        let mut patch = serde_json::json!({
            "status": SpaceStatus::Idle,
            "worktree": wt.path.display().to_string(),
            "error": null,
            "ports": ports,
            "updated_at": now,
        });
        if wt.head != space.head_sha {
            // The checkpoint ref is gone (deleted outside Theodore): the
            // worktree starts over at the base. Say so instead of showing
            // an empty diff as if nothing had happened.
            patch["status"] = serde_json::json!(SpaceStatus::Failed);
            patch["head_sha"] = serde_json::json!(wt.head);
            patch["error"] = serde_json::json!(format!(
                "the checkpoints were gone; the space restarted from {}",
                space.base
            ));
            // Its session talks about work that is no longer there.
            patch["agent_session"] = serde_json::Value::Null;
        }
        if space.done_at.is_some() {
            patch["done_at"] = serde_json::json!(now);
        }
        self.apply(vec![NewAction {
            model: models::SPACE.into(),
            model_id: space.id.clone(),
            action: SyncActionCode::C,
            data: Some(patch),
        }])
        .await?;
        drop(ports_guard);
        self.space_review(SpaceReviewRequest {
            space: space.id.clone(),
            group: false,
        })
        .await?;
        let space = self.space_row(&space.id).await?;
        self.spawn_space_code(&space, &project);
        Ok(space)
    }

    /// Pin/unpin a space (docs/spaces.md §3): pinned spaces sort first in
    /// the sidebar and ⌘K. Works on an archived space too; it keeps no
    /// worktree state.
    pub async fn set_space_pinned(&self, space: &str, pinned: bool) -> AppResult<Space> {
        let row = self.space_row(space).await?;
        if row.pinned == pinned {
            return Ok(row);
        }
        self.apply(vec![NewAction {
            model: models::SPACE.into(),
            model_id: row.id.clone(),
            action: SyncActionCode::C,
            data: Some(serde_json::json!({ "pinned": pinned })),
        }])
        .await?;
        self.space_row(space).await
    }

    /// Turns left `running` by a previous process can never finish
    /// (docs/spaces.md §7 "Crash recovery"). Their worktree keeps whatever
    /// the agent wrote; the next turn or archive checkpoints it.
    pub(crate) async fn fail_interrupted_spaces(&self) {
        let Ok(spaces) = self.list_spaces(None).await else {
            return;
        };
        let now = self.0.deps.clock.now();
        // Turns, and a space's setup, `run` and `check` (docs/config.md).
        let mut actions: Vec<NewAction> = Vec::new();
        for s in spaces.clone() {
            let reaped = self.reap_space_process(&s).await;
            let Some(patch) = crate::usecases::space_hooks::interrupted_patch(&s, now, reaped)
            else {
                continue;
            };
            actions.push(NewAction {
                model: models::SPACE.into(),
                model_id: s.id,
                action: SyncActionCode::C,
                data: Some(patch),
            });
        }
        let _ = self.apply(actions).await;
        self.hold_queues_after_restart(&spaces).await;
    }

    /// Reopens a space's branch review by its id (`ensure_open` after a
    /// restart, which already holds `open_lock`): the space knows its fork
    /// point and display names.
    pub(crate) async fn reopen_space_review(&self, pr: &PrId) -> AppResult<()> {
        let space = self
            .list_spaces(None)
            .await?
            .into_iter()
            .find(|s| s.review_pr.as_ref() == Some(pr))
            .ok_or_else(|| AppError::NotFound(format!("space of {pr}")))?;
        self.open_space_review(&space, true).await.map(|_| ())
    }

    /// The review of `base_sha...refs/theodore/spaces/<id>`. Caller holds
    /// `open_lock`.
    async fn open_space_review(&self, space: &Space, group: bool) -> AppResult<OpenPrResponse> {
        let project = self.space_project(space).await?;
        let repo_path = self.ensure_project_cache(&project).await?;
        let refname = space_ref(&space.id);
        let pr_ref = PrRef {
            repo: project.repo.clone(),
            number: stable_hash_u32(&refname) as u64,
            host_id: Some(refname.clone()),
        };
        let (mut detail, revision) = self
            .0
            .deps
            .git
            .describe_branch(
                &repo_path,
                &project.repo,
                pr_ref.number,
                &refname,
                space.base_sha.as_str(),
            )
            .await?;
        detail.summary.title = space.name.clone();
        detail.summary.head_ref = space.branch.clone();
        detail.summary.base_ref = space.base.clone();
        let head = detail.summary.head_sha.clone();
        let worktree = self.0.deps.git.worktree(&pr_ref, &head).await?;
        self.finish_open_with(pr_ref, detail, revision, repo_path, worktree.path, group)
            .await
    }

    pub(crate) async fn space_row(&self, id: &str) -> AppResult<Space> {
        let data = self
            .get_row(models::SPACE, id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("space {id}")))?;
        serde_json::from_value(data).map_err(|e| AppError::Invalid(e.to_string()))
    }

    pub(crate) async fn space_project(&self, space: &Space) -> AppResult<Project> {
        self.project_row(&space.project)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("project {}", space.project)))
    }

    /// The transcript so far, oldest first, without failed placeholders.
    async fn space_history(&self, space: &Space) -> AppResult<Vec<ChatMessage>> {
        let store = self.0.deps.store.clone();
        let key = space_chat_key(&space.id).0;
        let undone: Vec<&str> = space
            .turn_log
            .iter()
            .filter(|t| t.undone)
            .flat_map(|t| {
                [t.message.as_str(), t.reply.as_str()]
                    .into_iter()
                    .chain(t.steered.iter().map(String::as_str))
            })
            .collect();
        let rows =
            tokio::task::spawn_blocking(move || store.query(models::CHAT_MESSAGE, "pr", &key))
                .await
                .map_err(AppError::join)??;
        let mut messages: Vec<ChatMessage> = rows
            .into_iter()
            .filter_map(|r| serde_json::from_value(r.data).ok())
            .filter(|m: &ChatMessage| m.failed.is_none())
            .filter(|m| !crate::usecases::space_queue::is_queued(m))
            .filter(|m| !undone.contains(&m.id.as_str()))
            .collect();
        messages.sort_by_key(|m| m.at);
        Ok(messages)
    }
}

struct Turn {
    run_id: String,
    /// Your message that started it.
    message: String,
    space: Space,
    agent: AgentId,
    model: Option<String>,
    mode: SpaceMode,
    prompt: String,
    /// The agent session this turn continues; `prompt` is then only the
    /// new turn.
    resume: Option<String>,
    /// With `resume`: the transcript-replaying prompt, for when the
    /// session turns out to be gone.
    replay: Option<String>,
    system: String,
    env: Vec<(String, String)>,
    attachments: Vec<AgentAttachment>,
    cwd: PathBuf,
    cancel: CancellationToken,
    /// The code tools this turn's `space-tools` lists.
    code: CodeTools,
}

/// The stored session the next turn continues: same agent, same worktree.
fn resumable_session(space: &Space, agent: AgentId, cwd: &Path) -> Option<String> {
    space
        .agent_session
        .as_ref()
        .filter(|s| s.agent == agent && Path::new(&s.cwd) == cwd)
        .map(|s| s.id.clone())
}

/// What a resumed run fails with when its session no longer exists:
/// Claude Code's result `errors`, Codex's stderr.
const SESSION_GONE: [&str; 2] = ["no conversation found", "no rollout found"];

/// One agent run of a turn, streamed to the UI.
#[derive(Default)]
struct Streamed {
    text: String,
    error: Option<String>,
    /// `error` says the resumed session is gone.
    session_gone: bool,
    /// The last session id the run reported.
    session: Option<String>,
    /// The plan a Plan turn handed in (`AgentEvent::Plan`).
    plan: Option<String>,
}

async fn stream_run(
    core: &Core,
    req: AgentRequest,
    cancel: &CancellationToken,
    run_id: &str,
) -> Streamed {
    let agent = req.agent;
    let resumed = req.resume.is_some();
    let mut out = Streamed::default();
    let mut run = match core.0.deps.agents.run_steerable(req).await {
        Ok((run, steer)) => {
            // "Send now" writes into this run while it works.
            core.steer_run(run_id, steer);
            run
        }
        Err(e) => {
            out.error = Some(e.to_string());
            return out;
        }
    };
    let mut live = LiveRun::new(run_id, agent);
    let _unsteer = Unsteer { core, run: run_id };
    loop {
        tokio::select! {
            _ = cancel.cancelled() => { run.cancel.cancel(); break; }
            event = run.events.recv() => {
                let Some(event) = event else { break };
                // Quiet because it waits on your answer, not stuck.
                if !(event == AgentEvent::Stalled && core.asking(run_id)) {
                    live.show(core, &event);
                }
                match event {
                    AgentEvent::Text { text: t } => out.text.push_str(&t),
                    AgentEvent::Session { id } => out.session = Some(id),
                    AgentEvent::Plan { text } => out.plan = Some(text),
                    AgentEvent::Done { final_text, .. } => {
                        if !final_text.is_empty() {
                            out.text = final_text;
                        }
                        break;
                    }
                    AgentEvent::Error { message, stderr_tail } => {
                        let said = format!("{message}\n{stderr_tail}").to_lowercase();
                        out.session_gone = resumed && SESSION_GONE.iter().any(|m| said.contains(m));
                        out.error = Some(message);
                        break;
                    }
                    AgentEvent::TextDelta { .. }
                    | AgentEvent::Thinking { .. }
                    | AgentEvent::ToolCall { .. }
                    | AgentEvent::ToolResult { .. }
                    | AgentEvent::Stalled
                    | AgentEvent::Usage { .. } => {}
                }
            }
        }
    }
    out
}

/// Nothing more is steered into a run once its events stop.
struct Unsteer<'a> {
    core: &'a Core,
    run: &'a str,
}

impl Drop for Unsteer<'_> {
    fn drop(&mut self) {
        self.core.unsteer_run(self.run);
    }
}

async fn drive_turn(core: &Core, turn: Turn) {
    let Turn {
        run_id,
        message,
        space,
        agent,
        model,
        mode,
        prompt,
        resume,
        replay,
        system,
        env,
        attachments,
        cwd,
        cancel,
        code,
    } = turn;
    // The space's commands as tools, for this turn only.
    let mcp = core
        .0
        .deps
        .tools
        .as_ref()
        .map(|t| t.register_space(&run_id, &space.id, agent, code));
    let tools_guard = crate::usecases::agents::ToolsGuard {
        core,
        mcp: mcp.clone(),
    };
    let resumed = resume.is_some();
    let mut req = AgentRequest {
        agent,
        prompt,
        system: Some(system),
        cwd: cwd.clone(),
        mode: match mode {
            SpaceMode::Ask => AgentMode::ReadOnly,
            SpaceMode::Edit => AgentMode::Edit,
            SpaceMode::Run => AgentMode::Run,
            SpaceMode::Plan => AgentMode::Plan,
        },
        mcp,
        model,
        effort: None,
        stall_after_ms: core.stall_after_ms().await,
        env,
        user_mcp: core.user_mcp_allowed().await,
        attachments,
        resume,
        stream_text: true,
    };
    let started = std::time::Instant::now();
    let mut streamed = stream_run(core, req.clone(), &cancel, &run_id).await;
    let session_gone = streamed.session_gone && !cancel.is_cancelled();
    if session_gone && let Some(replay) = replay {
        // The CLI lost it (cleaned up, another machine): once more as a new
        // session that hears the transcript.
        tracing::info!(
            space = %space.id,
            agent = agent.as_str(),
            "agent session gone, replaying the transcript"
        );
        req.resume = None;
        req.prompt = replay;
        streamed = stream_run(core, req, &cancel, &run_id).await;
    }
    let Streamed {
        text,
        mut error,
        session,
        plan,
        ..
    } = streamed;
    // Questions still waiting go with the run that asked them.
    core.end_space_questions(&run_id);
    // A Plan turn's reply is its plan (the card's text, and what a later
    // turn's replayed transcript carries).
    let plan = (mode == SpaceMode::Plan && error.is_none() && !cancel.is_cancelled())
        .then(|| crate::usecases::space_plan::plan_of(plan, &text))
        .flatten();
    let text = plan.clone().unwrap_or(text);
    // The session this turn ran in is the one the next turn continues. A
    // resumed run that reported none (it failed to start) leaves it as is;
    // a new one that reported none (Gemini) means there is none to continue.
    let agent_session = match session {
        Some(id) => Some(theodore_contract::AgentSession {
            agent,
            id,
            cwd: cwd.display().to_string(),
        }),
        None if resumed && !session_gone => space.agent_session.clone(),
        None => None,
    };
    // The agent is done: its token goes before the after-turn check.
    drop(tools_guard);
    let cancelled = cancel.is_cancelled();
    let ms = started.elapsed().as_millis() as u64;
    if let (Some(e), false) = (&error, cancelled)
        && theodore_ports::offline_message_in(e).is_some()
    {
        // The agent's servers were out of reach: the network, not a bug.
        tracing::info!(
            error.code = theodore_ports::AGENT_OFFLINE_CODE,
            space = %space.id,
            agent = agent.as_str(),
            duration_ms = ms,
            "space turn couldn't reach the agent"
        );
    } else if let (Some(e), false) = (&error, cancelled) {
        tracing::warn!(
            error.code = "space.turn_failed",
            space = %space.id,
            agent = agent.as_str(),
            duration_ms = ms,
            error = %e,
            "space turn failed"
        );
    }
    theodore_telemetry::product(theodore_telemetry::ProductEvent::SpaceTurn {
        ms,
        mode: crate::usecases::telemetry::wire_str(&mode),
        ok: error.is_none() && !cancelled,
    });
    if cancelled && error.is_none() {
        error = Some("stopped".into());
    }

    // Whatever the agent wrote is kept, even on a failed or stopped turn.
    let turn_no = space.turns + 1;
    let suffix = if cancelled {
        " (stopped)"
    } else if error.is_some() {
        " (failed)"
    } else {
        ""
    };
    // While an update from base resolves, Theodore commits its merge here.
    let saved = crate::usecases::space_update::turn_checkpoint(
        core,
        &space,
        &cwd,
        &format!("{}: turn {turn_no}{suffix}", space.name),
    )
    .await;
    // A rebase or conflict left in the worktree: the answer stands, the
    // snapshot waits for the next turn, and the space says why.
    let mut skipped = None;
    let head = match saved.result {
        Ok(head) => Some(head),
        Err(theodore_ports::GitError::Busy(busy)) => {
            skipped = Some(
                saved
                    .skipped
                    .unwrap_or_else(|| format!("checkpoint skipped: {busy}")),
            );
            None
        }
        Err(e) => {
            error.get_or_insert_with(|| format!("checkpoint failed: {e}"));
            None
        }
    };

    let key = space_chat_key(&space.id);
    let reply = ChatMessage {
        id: uuid::Uuid::new_v4().to_string(),
        pr: key,
        author: Author::Agent(agent),
        text: if error.is_some() { String::new() } else { text },
        refs: vec![],
        at: core.0.deps.clock.now(),
        failed: error.clone(),
        attachments: vec![],
        queued: None,
    };
    let changed = head.as_ref().is_some_and(|h| *h != space.head_sha);
    // `check` (docs/config.md) is part of the turn: the space stays
    // running, and Stop stops it too.
    let checking = changed && !cancelled && core.has_check(&space).await;
    let status = if (error.is_some() && !cancelled) || skipped.is_some() {
        SpaceStatus::Failed
    } else {
        SpaceStatus::Idle
    };
    let space_error = match (cancelled, &error) {
        (false, Some(e)) => Some(e.clone()),
        _ => skipped.clone(),
    };
    let mut patch = serde_json::json!({
        "status": if checking { SpaceStatus::Running } else { status },
        "run": if checking { Some(run_id.clone()) } else { None },
        "turns": turn_no,
        "error": space_error,
        "agent_session": agent_session,
        // The tracker is the turn's: it goes when the agent is done.
        "progress": null,
        "question": null,
        "updated_at": core.0.deps.clock.now(),
    });
    if let Some(extra) = saved.patch {
        crate::usecases::space_update::merge_fields(&mut patch, extra);
    }
    if let Some(head) = &head {
        patch["head_sha"] = serde_json::json!(head);
        // Nothing else writes the log while a turn runs (undo is refused),
        // but read it fresh rather than trust the snapshot from the send.
        let mut log = core
            .space_row(&space.id)
            .await
            .map(|s| s.turn_log)
            .unwrap_or_else(|_| space.turn_log.clone());
        log.push(SpaceTurn {
            message,
            reply: reply.id.clone(),
            before: space.head_sha.clone(),
            after: head.clone(),
            undone: false,
            steered: core.take_steered(&run_id),
            check: checking.then(|| theodore_contract::SpaceCheck {
                status: theodore_contract::SpaceCheckStatus::Running,
                exit_code: None,
                timed_out: false,
                output: vec![],
                started_at: core.0.deps.clock.now(),
                finished_at: None,
            }),
            base_before: saved.base_before.clone(),
            plan,
        });
        patch["turn_log"] = serde_json::json!(log);
    }
    let mut actions = Vec::new();
    if let Ok(data) = serde_json::to_value(&reply) {
        actions.push(NewAction {
            model: models::CHAT_MESSAGE.into(),
            model_id: reply.id.clone(),
            action: SyncActionCode::I,
            data: Some(data),
        });
    }
    actions.push(NewAction {
        model: models::SPACE.into(),
        model_id: space.id.clone(),
        action: SyncActionCode::C,
        data: Some(patch),
    });
    // Stopped or failed: whatever is queued waits for you.
    let end = if cancelled || error.is_some() {
        TurnEnd::Interrupted
    } else {
        TurnEnd::Done
    };
    if checking {
        let _ = core.apply(actions).await;
    } else {
        core.0.runs.write().await.remove(&run_id);
        core.end_turn(&space.id, actions, end).await;
    }

    if changed && let Some(after) = &head {
        core.spawn_space_code_changed(cwd.clone(), space.head_sha.clone(), after.clone());
    }
    if changed {
        // The live diff follows the new checkpoint; grouping waits for Review.
        if let Err(e) = core
            .space_review(SpaceReviewRequest {
                space: space.id.clone(),
                group: false,
            })
            .await
        {
            core.emit(Ephemeral::Toast {
                level: ToastLevel::Warn,
                text: format!("{}: diff refresh failed: {e}", space.name),
            });
        }
        // Mirroring into a local folder (docs/spaces.md "Mirror into a
        // local folder") re-syncs here, after every turn; a no-op for the
        // common case of a space that doesn't mirror.
        core.sync_mirror_after_turn(&space).await;
    }

    if checking {
        let check = core.run_check(&space, cancel.clone()).await;
        core.0.runs.write().await.remove(&run_id);
        let mut log = core
            .space_row(&space.id)
            .await
            .map(|s| s.turn_log)
            .unwrap_or_default();
        if let Some(t) = log.iter_mut().rev().find(|t| t.reply == reply.id) {
            t.check = check;
        }
        // A failing check isn't a failed turn: the next one hears it. Stop
        // during the check is a stop.
        let end = if cancel.is_cancelled() {
            TurnEnd::Interrupted
        } else {
            end
        };
        core.end_turn(
            &space.id,
            vec![NewAction {
                model: models::SPACE.into(),
                model_id: space.id.clone(),
                action: SyncActionCode::C,
                data: Some(serde_json::json!({
                    "status": status,
                    "run": null,
                    "turn_log": log,
                    "updated_at": core.0.deps.clock.now(),
                })),
            }],
            end,
        )
        .await;
    }
}

/// `agents.claude.effort = "ultracode"` (Claude only, spaces only): the
/// keyword opens the turn's prompt, so Claude Code works it with several
/// agents. The run itself goes at `--effort max` (`claude.rs`).
const ULTRACODE: &str = "ultracode";

fn with_ultracode(prompt: String, on: bool) -> String {
    if on {
        format!("{ULTRACODE}\n\n{prompt}")
    } else {
        prompt
    }
}

/// A failing `check` after the last turn goes into the next prompt, so
/// the agent sees what broke.
fn with_failed_check(space: &Space, prompt: String) -> String {
    let failed = space
        .turn_log
        .iter()
        .rev()
        .find(|t| !t.undone)
        .and_then(|t| t.check.as_ref())
        .filter(|c| c.status == theodore_contract::SpaceCheckStatus::Failed);
    match failed {
        None => prompt,
        Some(c) => format!(
            "The project's check failed after your last turn. Its last output:\n```\n{}\n```\n\n{prompt}",
            c.output.join("\n")
        ),
    }
}

fn to_json<T: serde::Serialize>(v: &T) -> AppResult<serde_json::Value> {
    serde_json::to_value(v).map_err(|e| AppError::Invalid(e.to_string()))
}

/// 12 hex chars: short enough for a path, unique enough per machine.
fn new_space_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..12].to_string()
}

/// The index in `turn_log` of the turn "Undo last turn" would undo — the
/// last one not undone yet — or why there is none to undo.
pub(crate) fn undoable_turn(space: &Space) -> Result<usize, String> {
    match space.status {
        SpaceStatus::Running => {
            return Err("the agent is still working; stop it before undoing".into());
        }
        SpaceStatus::Archived => return Err("this space is archived".into()),
        SpaceStatus::Idle | SpaceStatus::Failed => {}
    }
    if space.updating.is_some() {
        return Err("finish or abort the update from base first".into());
    }
    let i = space
        .turn_log
        .iter()
        .rposition(|t| !t.undone)
        .ok_or_else(|| "there is no turn to undo".to_string())?;
    let turn = &space.turn_log[i];
    if turn.after != space.head_sha {
        return Err("the space moved since its last turn".into());
    }
    if let Some(shipped) = &space.shipped {
        // Shipped from a checkpoint at or before this turn's start: the
        // turn's own change hasn't left the space. Anything else (this
        // turn's result, or a checkpoint we can't place) was shipped.
        let mut earlier = space.turn_log[..i]
            .iter()
            .filter(|t| !t.undone)
            .flat_map(|t| [&t.before, &t.after]);
        let before_turn = shipped.head == turn.before
            || shipped.head == space.base_sha
            || earlier.any(|h| *h == shipped.head);
        if !before_turn {
            return Err(match shipped.how {
                theodore_contract::ShipKind::Pr => {
                    "this turn is already pushed; ask the agent to revert it instead"
                }
                theodore_contract::ShipKind::MergedLocally => {
                    "this turn is already merged; ask the agent to revert it instead"
                }
                theodore_contract::ShipKind::Branch => {
                    "this turn is already written as a branch; ask the agent to revert it instead"
                }
            }
            .into());
        }
    }
    Ok(i)
}

/// `true` for a space review's `PrRef::host_id`.
pub(crate) fn is_space_ref(host_id: &str) -> bool {
    host_id.starts_with(SPACE_REF_PREFIX)
}

/// The first few words of `text`, lowercased and dash-joined:
/// "Retry the export on 429s!" → "retry-the-export-on-429s". Cyrillic is
/// transliterated ("исправить логин" → "ispravit-login") and accented
/// Latin loses its accents; any other script separates words, and a goal
/// with nothing left is "space".
pub(crate) fn slug(text: &str) -> String {
    let ascii: String = text.chars().map(to_ascii).collect();
    let words: Vec<String> = ascii
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(5)
        .map(|w| w.to_ascii_lowercase())
        .collect();
    let s = words.join("-");
    let s: String = s.chars().take(40).collect();
    let s = s.trim_end_matches('-').to_string();
    if s.is_empty() { "space".into() } else { s }
}

/// One character of a goal as ASCII: itself, a transliteration, or a
/// space (a word break) for anything without one.
fn to_ascii(c: char) -> std::borrow::Cow<'static, str> {
    use std::borrow::Cow;
    if c.is_ascii() {
        return Cow::Owned(c.to_string());
    }
    let lower: char = c.to_lowercase().next().unwrap_or(c);
    let t: &'static str = match lower {
        // Russian (GOST-like, the way people type names), plus the
        // Ukrainian and Belarusian letters.
        'а' => "a",
        'б' => "b",
        'в' => "v",
        'г' => "g",
        'ґ' => "g",
        'д' => "d",
        'е' | 'ё' | 'э' | 'є' => "e",
        'ж' => "zh",
        'з' => "z",
        'и' | 'і' => "i",
        'ї' => "yi",
        'й' => "y",
        'к' => "k",
        'л' => "l",
        'м' => "m",
        'н' => "n",
        'о' => "o",
        'п' => "p",
        'р' => "r",
        'с' => "s",
        'т' => "t",
        'у' => "u",
        'ў' => "u",
        'ф' => "f",
        'х' => "kh",
        'ц' => "ts",
        'ч' => "ch",
        'ш' => "sh",
        'щ' => "shch",
        'ъ' | 'ь' => "",
        'ы' => "y",
        'ю' => "yu",
        'я' => "ya",
        // Latin with diacritics.
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
        'æ' => "ae",
        'ç' | 'ć' | 'č' => "c",
        'ď' | 'đ' | 'ð' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ė' | 'ę' | 'ě' => "e",
        'ğ' => "g",
        'ì' | 'í' | 'î' | 'ï' | 'ī' | 'į' | 'ı' => "i",
        'ł' | 'ľ' | 'ĺ' => "l",
        'ñ' | 'ń' | 'ň' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ő' => "o",
        'œ' => "oe",
        'ř' | 'ŕ' => "r",
        'ś' | 'š' | 'ş' | 'ș' => "s",
        'ß' => "ss",
        'ť' | 'ţ' | 'ț' => "t",
        'þ' => "th",
        'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' | 'ű' | 'ų' => "u",
        'ý' | 'ÿ' => "y",
        'ź' | 'ż' | 'ž' => "z",
        _ => " ",
    };
    Cow::Borrowed(t)
}

/// `name`, or `name-2`, `name-3`… — the first one no space in `taken` has.
pub(crate) fn unique_name(name: &str, taken: &[&str]) -> String {
    if !taken.contains(&name) {
        return name.to_string();
    }
    (2..)
        .map(|n| format!("{name}-{n}"))
        .find(|candidate| !taken.contains(&candidate.as_str()))
        .expect("an unbounded range has a free suffix")
}

/// A conservative subset of `git check-ref-format --branch`.
pub(crate) fn is_valid_branch(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && !name.starts_with(['-', '/', '.'])
        && !name.ends_with(['/', '.'])
        && !name.ends_with(".lock")
        && !name.contains("..")
        && !name.contains("//")
        && !name.contains("@{")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
}

/// Told to the agent when its turn has the `space-tools` server.
pub(crate) const SPACE_TOOLS_PROMPT: &str = "Theodore's space-tools MCP server runs this \
     space's configured commands for you: run_check (the project's check; use it after \
     changes), start_app / stop_app / app_status (the app, its ports and http://localhost \
     URLs; start_app to try it), read_logs (setup, run, check or archive output) and \
     space_info. It also acts on the change: when the user asks you to push, open a PR \
     (or a draft), merge or keep it as a branch, call ship and tell them what it returns \
     (the PR link, or why it was refused). get_space_diff shows your change against the \
     base, uncommitted edits included; get_review_comments reads the review's comments \
     and add_review_comment leaves one on a line; set_base changes the base branch when \
     the user asks; update_progress shows your plan above the user's message box, for \
     work with several steps; ask_user asks the user 1-3 multiple-choice questions and \
     waits for the answer, only when you can't go on without their decision.";

/// Told to the agent when its turn has the user's own MCP servers.
const USER_MCP_PROMPT: &str = "The user's own MCP servers (Linear, GitHub, docs and \
     the like, as connected in their agent CLI) are available too: use them when the \
     task points at an issue, a thread or a document there.";

fn system_prompt(space: &Space, tools: bool, code: CodeTools, user_mcp: bool) -> String {
    let mut prompt = base_system_prompt(space);
    if tools {
        prompt.push('\n');
        prompt.push_str(SPACE_TOOLS_PROMPT);
    }
    if let Some(hint) = crate::usecases::code_intel::code_prompt(code) {
        prompt.push('\n');
        prompt.push_str(hint);
    }
    if user_mcp {
        prompt.push('\n');
        prompt.push_str(USER_MCP_PROMPT);
    }
    prompt
}

fn base_system_prompt(space: &Space) -> String {
    format!(
        "You are working in a Theodore space: an isolated git worktree for one change.\n\
         Goal: {goal}\n\
         Base branch: {base}. The change will be shipped as branch {branch}.\n\
         Rules: work only inside the current directory. Shipping goes through Theodore: \
         never run git push, gh pr create or another PR command, and do not switch, create \
         or rebase branches or rewrite history yourself; without the ship tool, point the \
         user to Ship in the space's header. Theodore commits your edits after every turn, \
         so you don't need to. End each turn with a short summary of what you \
         changed and anything left to do.",
        goal = if space.goal.is_empty() {
            &space.name
        } else {
            &space.goal
        },
        base = space.base,
        branch = space.branch,
    )
}

/// The new message, preceded by the recent transcript (each CLI run starts
/// fresh, so this is what carries the conversation across turns, crashes
/// and agent switches).
pub(crate) fn turn_prompt(history: &[ChatMessage], text: &str) -> String {
    let recent = &history[history.len().saturating_sub(HISTORY_MESSAGES)..];
    if recent.is_empty() {
        return text.to_string();
    }
    let mut out = String::from("Conversation so far:\n");
    for m in recent {
        let who = match m.author {
            Author::Me => "user".to_string(),
            Author::Agent(a) => a.as_str().to_string(),
        };
        let body: String = m.text.chars().take(HISTORY_CHARS).collect();
        out.push_str(&format!("\n[{who}]\n{body}\n"));
        // Named only: the files went with that turn, not this one.
        if !m.attachments.is_empty() {
            let names: Vec<&str> = m.attachments.iter().map(|a| a.name.as_str()).collect();
            out.push_str(&format!("(attached: {})\n", names.join(", ")));
        }
    }
    out.push_str(&format!("\n---\n\nNew message from the user:\n{text}"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ultracode_opens_the_prompt_only_when_on() {
        assert_eq!(with_ultracode("Fix it".into(), true), "ultracode\n\nFix it");
        assert_eq!(with_ultracode("Fix it".into(), false), "Fix it");
    }

    #[test]
    fn slug_takes_the_first_words() {
        assert_eq!(
            slug("Retry the export job on 429s with backoff"),
            "retry-the-export-job-on"
        );
        assert_eq!(slug("  Fix: login/redirect!! "), "fix-login-redirect");
        assert_eq!(slug("¿?"), "space");
    }

    #[test]
    fn cyrillic_goals_are_transliterated() {
        assert_eq!(slug("тес"), "tes");
        assert_eq!(slug("исправить логин"), "ispravit-login");
        assert_eq!(slug("Починить ЭКСПОРТ в CSV"), "pochinit-eksport-v-csv");
        assert_eq!(slug("щука, жёлтый объём"), "shchuka-zheltyy-obem");
        assert_eq!(slug("Ґанок і їжак"), "ganok-i-yizhak");
    }

    #[test]
    fn accents_are_dropped_and_other_scripts_break_words() {
        assert_eq!(slug("Crème brûlée für Straße"), "creme-brulee-fur-strasse");
        assert_eq!(slug("fix 登录 page"), "fix-page");
        assert_eq!(slug("修复登录"), "space");
        assert_eq!(slug("emoji 🚀 launch"), "emoji-launch");
    }

    #[test]
    fn taken_names_get_a_number() {
        assert_eq!(unique_name("tes", &[]), "tes");
        assert_eq!(unique_name("tes", &["other"]), "tes");
        assert_eq!(unique_name("tes", &["tes"]), "tes-2");
        assert_eq!(unique_name("tes", &["tes", "tes-2", "tes-3"]), "tes-4");
    }

    #[test]
    fn branch_names_are_checked() {
        for ok in ["theo/fix-login", "feature/a.b_c", "x"] {
            assert!(is_valid_branch(ok), "{ok}");
        }
        for bad in [
            "", "-x", "/x", "x/", "a..b", "a//b", "a.lock", "a b", "a@{1}", "a~1",
        ] {
            assert!(!is_valid_branch(bad), "{bad:?}");
        }
    }

    #[test]
    fn space_refs_are_recognised() {
        assert!(is_space_ref(&space_ref("abc")));
        assert!(!is_space_ref("feature/x"));
    }

    fn msg(author: Author, text: &str, at: i64) -> ChatMessage {
        ChatMessage {
            id: at.to_string(),
            pr: space_chat_key("s"),
            author,
            text: text.into(),
            refs: vec![],
            at,
            failed: None,
            attachments: vec![],
            queued: None,
        }
    }

    #[test]
    fn the_first_turn_is_just_the_message() {
        assert_eq!(turn_prompt(&[], "do it"), "do it");
    }

    #[test]
    fn later_turns_replay_the_recent_transcript() {
        let history: Vec<_> = (0..20)
            .map(|i| msg(Author::Me, &format!("m{i}"), i))
            .chain([msg(Author::Agent(AgentId::Codex), "done", 99)])
            .collect();
        let p = turn_prompt(&history, "next");
        assert!(p.starts_with("Conversation so far:"));
        assert!(!p.contains("[user]\nm8\n"), "older messages are dropped");
        assert!(p.contains("[user]\nm19"));
        assert!(p.contains("[codex]\ndone"));
        assert!(p.ends_with("New message from the user:\nnext"));
    }

    #[test]
    fn earlier_attachments_are_named_not_attached_again() {
        let mut first = msg(Author::Me, "see the screenshot", 1);
        first.attachments = vec![theodore_contract::Attachment {
            id: "a1".into(),
            name: "shot.png".into(),
            mime: "image/png".into(),
            size: 3,
            path: "/data/attachments/a1/shot.png".into(),
        }];
        let p = turn_prompt(&[first], "next");
        assert!(p.contains("[user]\nsee the screenshot\n(attached: shot.png)\n"));
        assert!(!p.contains("/data/attachments"));
    }
}
