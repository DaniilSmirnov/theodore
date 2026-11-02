//! The space commands' lifecycle (docs/config.md): setup before the first
//! turn, the trust prompt, Run/Stop, check after a turn, logs, and the
//! config views. The config and runner plumbing is in `space_commands`.

use std::path::Path;
use std::time::Duration;

use theodore_contract::{
    EffectiveConfig, GlobalConfigInfo, ProcessStatus, SetupAction, SetupStatus, Space, SpaceCheck,
    SpaceCheckStatus, SpaceCommandKind, SpaceConfigView, SpaceLogLine, SpaceMode, SpaceProcess,
    SpaceRunRequest, SpaceSendRequest, SpaceSetup, SpaceSetupRequest, SpaceStatus,
    SpaceTrustRequest, ToastLevel, models,
};
use theodore_ports::{NewAction, SyncActionCode};
use tokio_util::sync::CancellationToken;

use crate::config;
use crate::core::Core;
use crate::error::{AppError, AppResult};
use crate::usecases::space_commands::{STOP_WAIT, SpaceConfig, describe_failure};

/// The setup stage a new space starts in, from its config and the notes
/// copying left. `None`: nothing to show, the goal runs at once.
pub(crate) fn initial_setup(
    cfg: &SpaceConfig,
    has_goal: bool,
    notes: Vec<String>,
) -> Option<SpaceSetup> {
    let (status, goal_pending) = match cfg.command(SpaceCommandKind::Setup) {
        None if notes.is_empty() => return None,
        None => (SetupStatus::Done, false),
        Some(c) if c.allowed => (SetupStatus::Running, has_goal),
        Some(_) => (SetupStatus::Waiting, has_goal),
    };
    Some(SpaceSetup {
        status,
        goal_pending,
        error: None,
        notes,
        finished_at: None,
    })
}

/// The patch that settles what a previous process left running
/// (docs/spaces.md §7 "Crash recovery"), or `None` when nothing did.
/// `process_reaped`: whether `reap_space_process` found the space's `run`
/// still alive under its recorded pgid and killed it — picks the message
/// a stopped `process` gets.
pub(crate) fn interrupted_patch(
    space: &Space,
    now: i64,
    process_reaped: bool,
) -> Option<serde_json::Value> {
    const WHY: &str = "interrupted: Theodore was closed";
    const REAPED: &str = "stopped when Theodore restarted";
    let mut patch = serde_json::Map::new();
    if space.status == SpaceStatus::Running {
        patch.insert("status".into(), serde_json::json!(SpaceStatus::Failed));
        patch.insert("run".into(), serde_json::Value::Null);
        patch.insert("error".into(), serde_json::json!(WHY));
    }
    if let Some(p) = &space.process
        && p.status == ProcessStatus::Running
    {
        let mut p = p.clone();
        p.status = ProcessStatus::Stopped;
        p.ended_at = Some(now);
        p.error = Some(if process_reaped { REAPED } else { WHY }.into());
        p.pgid = None;
        p.script = None;
        patch.insert("process".into(), serde_json::json!(p));
    }
    if let Some(s) = &space.setup
        && s.status == SetupStatus::Running
    {
        let mut s = s.clone();
        s.status = SetupStatus::Failed;
        s.error = Some(WHY.into());
        patch.insert("setup".into(), serde_json::json!(s));
    }
    if space.turn_log.iter().any(|t| {
        t.check
            .as_ref()
            .is_some_and(|c| c.status == SpaceCheckStatus::Running)
    }) {
        let mut log = space.turn_log.clone();
        for t in &mut log {
            if let Some(c) = &mut t.check
                && c.status == SpaceCheckStatus::Running
            {
                c.status = SpaceCheckStatus::Failed;
                c.output.push(WHY.into());
                c.finished_at = Some(now);
            }
        }
        patch.insert("turn_log".into(), serde_json::json!(log));
    }
    (!patch.is_empty()).then_some(serde_json::Value::Object(patch))
}

impl Core {
    async fn patch_space(&self, id: &str, patch: serde_json::Value) -> AppResult<()> {
        self.apply(vec![NewAction {
            model: models::SPACE.into(),
            model_id: id.to_string(),
            action: SyncActionCode::C,
            data: Some(patch),
        }])
        .await?;
        Ok(())
    }

    /// Reads the space, changes its setup with `f`, writes it back.
    async fn update_setup(&self, id: &str, f: impl FnOnce(&mut SpaceSetup)) -> AppResult<Space> {
        let _guard = self.0.setup_lock.lock().await;
        let space = self.space_row(id).await?;
        let mut setup = space.setup.clone().unwrap_or(SpaceSetup {
            status: SetupStatus::Skipped,
            goal_pending: false,
            error: None,
            notes: vec![],
            finished_at: None,
        });
        f(&mut setup);
        self.patch_space(
            id,
            serde_json::json!({ "setup": setup, "updated_at": self.0.deps.clock.now() }),
        )
        .await?;
        self.space_row(id).await
    }

    /// Ends the setup stage with `status`; the goal waiting on it starts.
    async fn finish_setup(
        &self,
        id: &str,
        status: SetupStatus,
        note: Option<String>,
    ) -> AppResult<Space> {
        let now = self.0.deps.clock.now();
        let pending = {
            let _guard = self.0.setup_lock.lock().await;
            let space = self.space_row(id).await?;
            let Some(mut setup) = space.setup.clone() else {
                return Ok(space);
            };
            let pending = setup.goal_pending && !space.goal.is_empty();
            setup.status = status;
            setup.goal_pending = false;
            setup.finished_at = Some(now);
            setup.notes.extend(note);
            self.patch_space(id, serde_json::json!({ "setup": setup, "updated_at": now }))
                .await?;
            pending.then_some(space.goal)
        };
        if let Some(goal) = pending
            && let Err(e) = self
                .space_send(SpaceSendRequest {
                    space: id.to_string(),
                    text: goal,
                    agent: None,
                    model: None,
                    mode: None,
                    attachments: None,
                })
                .await
        {
            self.emit(theodore_contract::Ephemeral::Toast {
                level: ToastLevel::Error,
                text: format!("the first turn didn't start: {e}"),
            });
        }
        self.space_row(id).await
    }

    pub(crate) fn spawn_setup(&self, id: &str) {
        let core = self.clone();
        let id = id.to_string();
        tokio::spawn(async move { core.run_setup(&id).await });
    }

    async fn run_setup(&self, id: &str) {
        let Ok(space) = self.space_row(id).await else {
            return;
        };
        let Ok(project) = self.space_project(&space).await else {
            return;
        };
        let cfg = self.space_config_of(&space, &project).await;
        let Some(script) = cfg.runnable(SpaceCommandKind::Setup) else {
            let _ = self
                .finish_setup(
                    id,
                    SetupStatus::Skipped,
                    Some("setup was not run: no allowed setup command".into()),
                )
                .await;
            return;
        };
        let token = CancellationToken::new();
        self.with_runtime(id, |rt| rt.setup = Some(token.clone()));
        let _ = self
            .update_setup(id, |s| {
                s.status = SetupStatus::Running;
                s.error = None;
                s.finished_at = None;
            })
            .await;
        let env = self.space_command_env(&space, &project, &cfg);
        let timeout = cfg.timeout(SpaceCommandKind::Setup);
        let (exit, _) = self
            .run_space_command(
                id,
                SpaceCommandKind::Setup,
                &script,
                Path::new(&space.worktree),
                env,
                timeout,
                token.clone(),
            )
            .await;
        self.with_runtime(id, |rt| rt.setup = None);
        if token.is_cancelled() {
            // "Continue without setup" already settled the stage.
            return;
        }
        match describe_failure(SpaceCommandKind::Setup, &exit, timeout) {
            None => {
                let _ = self.finish_setup(id, SetupStatus::Done, None).await;
            }
            Some(error) => {
                let now = self.0.deps.clock.now();
                let _ = self
                    .update_setup(id, |s| {
                        s.status = SetupStatus::Failed;
                        s.error = Some(error);
                        s.finished_at = Some(now);
                    })
                    .await;
            }
        }
    }

    /// Retry a failed setup, or continue without it.
    pub async fn space_setup(&self, req: SpaceSetupRequest) -> AppResult<Space> {
        let space = self.space_row(&req.space).await?;
        let status = space.setup.as_ref().map(|s| s.status);
        match req.action {
            SetupAction::Retry => {
                if !matches!(status, Some(SetupStatus::Failed | SetupStatus::Skipped)) {
                    return Err(AppError::Invalid("setup isn't failed".into()));
                }
                let project = self.space_project(&space).await?;
                let cfg = self.space_config_of(&space, &project).await;
                if cfg.runnable(SpaceCommandKind::Setup).is_none() {
                    return Err(AppError::Invalid(
                        "there is no allowed setup command to run".into(),
                    ));
                }
                let space = self
                    .update_setup(&space.id, |s| {
                        s.status = SetupStatus::Running;
                        s.error = None;
                    })
                    .await?;
                self.spawn_setup(&space.id);
                Ok(space)
            }
            SetupAction::Skip => {
                if !matches!(
                    status,
                    Some(SetupStatus::Failed | SetupStatus::Running | SetupStatus::Waiting)
                ) {
                    return Ok(space);
                }
                if let Some(t) = self.with_runtime(&space.id, |rt| rt.setup.take()) {
                    t.cancel();
                }
                self.finish_setup(
                    &space.id,
                    SetupStatus::Skipped,
                    Some("continued without setup".into()),
                )
                .await
            }
        }
    }

    /// "Set up with agent" (docs/config.md §2, §8): the project has no
    /// `spaces:` commands yet, so send the space's own agent a turn to
    /// look at the repo and write them. Edit mode, so it can actually
    /// write the file — not the space's sticky mode, which may be Ask.
    pub async fn space_setup_assist(&self, space: &str) -> AppResult<Space> {
        self.space_send(SpaceSendRequest {
            space: space.to_string(),
            text: crate::usecases::space_setup_assist::setup_assist_prompt(),
            agent: None,
            model: None,
            mode: Some(SpaceMode::Edit),
            attachments: None,
        })
        .await?;
        self.space_row(space).await
    }

    /// "Fix with agent", next to Retry / Continue without setup: setup's
    /// `Failed` already lets the composer send (only `running` /
    /// `waiting` block it, in `space_send`), so this just sends the
    /// diagnostic turn — the setup command, how it failed, and its log
    /// tail — asking the agent to fix the cause. Setup itself is left
    /// `failed`: Retry and Continue without setup stay exactly where they
    /// were, and a Retry that later succeeds still fires the goal it was
    /// waiting on.
    pub async fn space_fix_setup(&self, space: &str) -> AppResult<Space> {
        let row = self.space_row(space).await?;
        let failure = row
            .setup
            .as_ref()
            .filter(|s| s.status == SetupStatus::Failed)
            .and_then(|s| s.error.clone())
            .ok_or_else(|| AppError::Invalid("setup hasn't failed".into()))?;
        let project = self.space_project(&row).await?;
        let cfg = self.space_config_of(&row, &project).await;
        let script = cfg
            .command(SpaceCommandKind::Setup)
            .map(|c| c.script)
            .unwrap_or_default();
        let tail = self.with_runtime(space, |rt| {
            rt.logs
                .iter()
                .filter(|l| l.kind == SpaceCommandKind::Setup)
                .map(|l| l.line.clone())
                .collect::<Vec<_>>()
        });
        self.space_send(SpaceSendRequest {
            space: space.to_string(),
            text: crate::usecases::space_setup_assist::fix_setup_prompt(&script, &failure, &tail),
            agent: None,
            model: None,
            mode: Some(SpaceMode::Edit),
            attachments: None,
        })
        .await?;
        self.space_row(space).await
    }

    /// "Run commands from this repo?": Allow (stored for the project
    /// against `hash`) or Not now (skipped in this space).
    pub async fn space_trust(&self, req: SpaceTrustRequest) -> AppResult<Space> {
        let space = self.space_row(&req.space).await?;
        let project = self.space_project(&space).await?;
        let cfg = self.space_config_of(&space, &project).await;
        let repo = cfg
            .repo
            .as_ref()
            .ok_or_else(|| AppError::Invalid("this repo's config has no commands".into()))?;
        if repo.hash != req.hash {
            return Err(AppError::Invalid(
                "the repo's commands changed; review them again".into(),
            ));
        }
        let waiting = |s: &Space| {
            s.setup
                .as_ref()
                .is_some_and(|x| x.status == SetupStatus::Waiting)
        };
        if req.allow {
            self.store_trust(&project.id, &repo.hash).await?;
            self.patch_space(&space.id, serde_json::json!({ "commands_denied": false }))
                .await?;
            // Every space of the project waiting on this answer goes on.
            for s in self.list_spaces(Some(project.id.clone())).await? {
                if waiting(&s) && (s.id == space.id || !s.commands_denied) {
                    let _ = self
                        .update_setup(&s.id, |x| x.status = SetupStatus::Running)
                        .await;
                    self.spawn_setup(&s.id);
                }
            }
            return self.space_row(&space.id).await;
        }
        self.patch_space(&space.id, serde_json::json!({ "commands_denied": true }))
            .await?;
        if waiting(&space) {
            return self
                .finish_setup(
                    &space.id,
                    SetupStatus::Skipped,
                    Some("commands from the repo were not run".into()),
                )
                .await;
        }
        self.space_row(&space.id).await
    }

    /// Starts or stops the space's `run` command.
    pub async fn space_run(&self, req: SpaceRunRequest) -> AppResult<Space> {
        let space = self.space_row(&req.space).await?;
        if !req.start {
            if let Some((cancel, _)) = self.with_runtime(&space.id, |rt| rt.run.clone()) {
                cancel.cancel();
            }
            return Ok(space);
        }
        if space.status == SpaceStatus::Archived {
            return Err(AppError::Invalid("this space is archived".into()));
        }
        let project = self.space_project(&space).await?;
        let cfg = self.space_config_of(&space, &project).await;
        let script = match cfg.command(SpaceCommandKind::Run) {
            None => return Err(AppError::Invalid("no `spaces.run` command is set".into())),
            Some(c) if !c.allowed => {
                return Err(AppError::Invalid(
                    "the repo's commands aren't allowed for this space".into(),
                ));
            }
            Some(c) => c.script,
        };
        let cancel = CancellationToken::new();
        let done = CancellationToken::new();
        let started = self.with_runtime(&space.id, |rt| {
            if rt.run.is_some() {
                return false;
            }
            rt.run = Some((cancel.clone(), done.clone()));
            true
        });
        if !started {
            return Ok(space);
        }
        // Defensive: a port the space reserved may still be held by a
        // process from before (docs/spaces.md "Crash recovery") — report it
        // in the run log instead of letting the command crash on it.
        if let Some(port) = space
            .ports
            .iter()
            .copied()
            .find(|p| !config::ports::is_free(*p))
        {
            self.with_runtime(&space.id, |rt| rt.run = None);
            let message = format!("port {port} is already in use by another process");
            self.push_log(&space.id, SpaceCommandKind::Run, message.clone(), true);
            let now = self.0.deps.clock.now();
            self.patch_space(
                &space.id,
                serde_json::json!({
                    "process": SpaceProcess {
                        status: ProcessStatus::Failed,
                        started_at: now,
                        ended_at: Some(now),
                        exit_code: None,
                        error: Some(message),
                        pgid: None,
                        script: None,
                    },
                }),
            )
            .await?;
            return self.space_row(&space.id).await;
        }
        let env = self.space_command_env(&space, &project, &cfg);
        let now = self.0.deps.clock.now();
        // Spawned here (not inside the task below) so the pid is known
        // before the first write of `process`: a space's `run` is recorded
        // as `running` with its pgid from the start, never with a gap a
        // concurrent restart could race.
        let run = self
            .spawn_space_command(
                &space.id,
                SpaceCommandKind::Run,
                &script,
                Path::new(&space.worktree),
                env,
                None,
            )
            .await;
        let pgid = run.pid;
        self.patch_space(
            &space.id,
            serde_json::json!({
                "process": SpaceProcess {
                    status: ProcessStatus::Running,
                    started_at: now,
                    ended_at: None,
                    exit_code: None,
                    error: None,
                    pgid,
                    script: Some(script.clone()),
                },
            }),
        )
        .await?;
        let core = self.clone();
        let id = space.id.clone();
        tokio::spawn(async move {
            let (exit, _) = core
                .drive_space_command(&id, SpaceCommandKind::Run, run, None, cancel.clone())
                .await;
            core.with_runtime(&id, |rt| rt.run = None);
            let status = if exit.error.is_some() {
                ProcessStatus::Failed
            } else if exit.cancelled || cancel.is_cancelled() {
                ProcessStatus::Stopped
            } else {
                ProcessStatus::Exited
            };
            let _ = core
                .patch_space(
                    &id,
                    serde_json::json!({
                        "process": SpaceProcess {
                            status,
                            started_at: now,
                            ended_at: Some(core.0.deps.clock.now()),
                            exit_code: exit.code,
                            error: exit.error.clone(),
                            pgid: None,
                            script: None,
                        },
                    }),
                )
                .await;
            done.cancel();
        });
        self.space_row(&space.id).await
    }

    /// Stops the space's setup and `run`, waiting (a while) for `run` to
    /// exit: archive must not remove a worktree a server still runs in.
    pub(crate) async fn stop_space_commands(&self, id: &str) {
        let (setup, run) = self.with_runtime(id, |rt| (rt.setup.take(), rt.run.clone()));
        if let Some(t) = setup {
            t.cancel();
        }
        if let Some((cancel, done)) = run {
            cancel.cancel();
            let _ = tokio::time::timeout(STOP_WAIT, done.cancelled()).await;
        }
    }

    /// Stops every space's running `run` (and in-flight `setup`) and every
    /// open terminal, waiting briefly for each — called on a clean app exit
    /// so quitting normally never orphans a space's server (or a
    /// terminal's shell) the way an unclean exit can (docs/spaces.md
    /// "Crash recovery").
    pub async fn stop_all_running_spaces(&self) {
        let ids: Vec<String> = {
            let rt = self.0.space_rt.lock().unwrap_or_else(|p| p.into_inner());
            rt.iter()
                .filter(|(_, r)| r.run.is_some() || r.setup.is_some())
                .map(|(id, _)| id.clone())
                .collect()
        };
        for id in ids {
            self.stop_space_commands(&id).await;
        }
        self.stop_all_space_terminals().await;
    }

    /// Reaps a `run` an earlier, uncleanly-exited Theodore left behind
    /// (docs/spaces.md §7 "Crash recovery"): if `space.process` is
    /// `running` with a recorded pgid, and that group is still alive and
    /// still running the script Theodore started there — never a pid
    /// reused by something else — it's killed (SIGTERM, grace, SIGKILL).
    /// Returns whether it found and killed one, for `interrupted_patch`'s
    /// message. Called once at startup, before any space command runs.
    pub(crate) async fn reap_space_process(&self, space: &Space) -> bool {
        const GRACE: Duration = Duration::from_secs(5);
        let Some(p) = &space.process else {
            return false;
        };
        if p.status != ProcessStatus::Running {
            return false;
        }
        let (Some(pgid), Some(script)) = (p.pgid, &p.script) else {
            return false;
        };
        if !self.0.deps.commands.group_matches(pgid, script).await {
            return false;
        }
        self.0.deps.commands.reap_group(pgid, GRACE).await;
        true
    }

    /// `archive` before the worktree is removed. A failure is a toast; the
    /// archive goes on.
    pub(crate) async fn run_archive_command(
        &self,
        space: &Space,
        project: &theodore_contract::Project,
        worktree_exists: bool,
    ) {
        let cfg = self.space_config_of(space, project).await;
        let Some(script) = cfg.runnable(SpaceCommandKind::Archive) else {
            return;
        };
        if !worktree_exists {
            return;
        }
        let env = self.space_command_env(space, project, &cfg);
        let timeout = cfg.timeout(SpaceCommandKind::Archive);
        let (exit, _) = self
            .run_space_command(
                &space.id,
                SpaceCommandKind::Archive,
                &script,
                Path::new(&space.worktree),
                env,
                timeout,
                CancellationToken::new(),
            )
            .await;
        if let Some(why) = describe_failure(SpaceCommandKind::Archive, &exit, timeout) {
            self.emit(theodore_contract::Ephemeral::Toast {
                level: ToastLevel::Warn,
                text: format!("{}: {why}; archived anyway", space.name),
            });
        }
    }

    /// `check` after a turn, when there is an allowed one; `cancel` (the
    /// turn's Stop) stops it.
    pub(crate) async fn run_check(
        &self,
        space: &Space,
        cancel: CancellationToken,
    ) -> Option<SpaceCheck> {
        let project = self.space_project(space).await.ok()?;
        let cfg = self.space_config_of(space, &project).await;
        let script = cfg.runnable(SpaceCommandKind::Check)?;
        let started_at = self.0.deps.clock.now();
        let env = self.space_command_env(space, &project, &cfg);
        let timeout = cfg.timeout(SpaceCommandKind::Check);
        let (exit, mut output) = self
            .run_space_command(
                &space.id,
                SpaceCommandKind::Check,
                &script,
                Path::new(&space.worktree),
                env,
                timeout,
                cancel,
            )
            .await;
        if let Some(why) = describe_failure(SpaceCommandKind::Check, &exit, timeout) {
            output.push(why);
        }
        Some(SpaceCheck {
            status: if exit.success() {
                SpaceCheckStatus::Passed
            } else {
                SpaceCheckStatus::Failed
            },
            exit_code: exit.code,
            timed_out: exit.timed_out,
            output,
            started_at,
            finished_at: Some(self.0.deps.clock.now()),
        })
    }

    /// Whether a check would run after a turn of `space`.
    pub(crate) async fn has_check(&self, space: &Space) -> bool {
        let Ok(project) = self.space_project(space).await else {
            return false;
        };
        self.space_config_of(space, &project)
            .await
            .runnable(SpaceCommandKind::Check)
            .is_some()
    }

    /// The environment for an agent turn in the space.
    pub(crate) async fn space_agent_env(&self, space: &Space) -> Vec<(String, String)> {
        let Ok(project) = self.space_project(space).await else {
            return vec![];
        };
        let cfg = self.space_config_of(space, &project).await;
        self.space_command_env(space, &project, &cfg)
    }

    /// What the space will run, where each command comes from, and
    /// whether the repo's commands are allowed.
    pub async fn space_config(&self, space: &str) -> AppResult<SpaceConfigView> {
        let space = self.space_row(space).await?;
        let project = self.space_project(&space).await?;
        let cfg = self.space_config_of(&space, &project).await;
        let commands = [
            SpaceCommandKind::Setup,
            SpaceCommandKind::Run,
            SpaceCommandKind::Check,
            SpaceCommandKind::Archive,
        ]
        .into_iter()
        .filter_map(|k| cfg.command(k))
        .collect();
        Ok(SpaceConfigView {
            commands,
            ports: space.ports.clone(),
            repo_commands: cfg.repo.clone(),
            denied: cfg.denied,
        })
    }

    /// The last lines of every command of the space, oldest first.
    pub async fn space_logs(&self, space: &str) -> AppResult<Vec<SpaceLogLine>> {
        self.space_row(space).await?;
        Ok(self.with_runtime(space, |rt| rt.logs.iter().cloned().collect()))
    }

    /// A project's merged config with each value's file. A host project's
    /// repo file is read at its default branch.
    pub async fn project_config(&self, project: &str) -> AppResult<EffectiveConfig> {
        let project = self
            .project_row(project)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("project {project}")))?;
        let rev = match (&project.path, &project.default_branch) {
            (None, Some(branch)) => match self.ensure_project_cache(&project).await {
                Ok(repo) => self.0.deps.git.resolve_ref(&repo, branch).await.ok(),
                Err(_) => None,
            },
            _ => None,
        };
        let layers = self.project_layers(&project, rev.as_ref()).await;
        let cfg = self.config_with_trust(&layers, &project, false).await;
        Ok(crate::usecases::config_editor::effective(
            &cfg.merged,
            cfg.repo,
        ))
    }

    /// Where `~/.theodore/config.yaml` is, and whether it exists.
    pub async fn global_config(&self) -> AppResult<GlobalConfigInfo> {
        let path = config::global_path(&self.0.deps.data_dir);
        Ok(GlobalConfigInfo {
            exists: tokio::fs::try_exists(&path).await.unwrap_or(false),
            path: path.display().to_string(),
        })
    }

    /// Same, creating the file from a commented template when missing.
    pub async fn ensure_global_config(&self) -> AppResult<GlobalConfigInfo> {
        let path = config::global_path(&self.0.deps.data_dir);
        if let Some(dir) = path.parent() {
            tokio::fs::create_dir_all(dir)
                .await
                .map_err(|e| AppError::Invalid(format!("{}: {e}", dir.display())))?;
        }
        if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
            // `create_new`: never overwrite a file that appeared meanwhile.
            use tokio::io::AsyncWriteExt;
            if let Ok(mut f) = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .await
            {
                f.write_all(config::GLOBAL_TEMPLATE.as_bytes())
                    .await
                    .map_err(|e| AppError::Invalid(format!("{}: {e}", path.display())))?;
            }
        }
        self.global_config().await
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use theodore_contract::ConfigLayer;

    use super::*;
    use crate::config::{LayerText, merge, trust};

    fn cfg(text: &str, trusted: bool, denied: bool) -> SpaceConfig {
        let merged = merge(&[LayerText {
            layer: ConfigLayer::Repo,
            path: PathBuf::from("/r"),
            legacy: false,
            text: Some(text.into()),
        }]);
        let hash = trust::repo_commands(&merged, None).map(|r| r.hash);
        let repo = trust::repo_commands(&merged, hash.as_deref().filter(|_| trusted));
        SpaceConfig {
            merged,
            repo,
            denied,
        }
    }

    #[test]
    fn the_setup_stage_follows_the_config_and_trust() {
        assert_eq!(initial_setup(&cfg("{}", false, false), true, vec![]), None);
        let notes = initial_setup(&cfg("{}", false, false), true, vec!["n".into()]).unwrap();
        assert_eq!(
            (notes.status, notes.goal_pending),
            (SetupStatus::Done, false)
        );
        let waiting = initial_setup(&cfg("spaces: {setup: make}", false, false), true, vec![]);
        assert_eq!(waiting.unwrap().status, SetupStatus::Waiting);
        let running =
            initial_setup(&cfg("spaces: {setup: make}", true, false), true, vec![]).unwrap();
        assert_eq!(running.status, SetupStatus::Running);
        assert!(running.goal_pending);
        let no_goal =
            initial_setup(&cfg("spaces: {setup: make}", true, false), false, vec![]).unwrap();
        assert!(!no_goal.goal_pending);
        let denied = initial_setup(&cfg("spaces: {setup: make}", true, true), true, vec![]);
        assert_eq!(denied.unwrap().status, SetupStatus::Waiting);
    }

    #[test]
    fn untrusted_repo_env_is_left_out() {
        let c = cfg("spaces: {env: {A: x}, run: dev}", false, false);
        assert!(c.env().is_empty());
        assert_eq!(c.runnable(SpaceCommandKind::Run), None);
        let c = cfg("spaces: {env: {A: x}, run: dev}", true, false);
        assert_eq!(c.env().get("A").map(String::as_str), Some("x"));
        assert_eq!(c.runnable(SpaceCommandKind::Run).as_deref(), Some("dev"));
    }
}
