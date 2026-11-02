//! The space's commands as agent tools (docs/spaces.md "Tools the agent
//! gets"): what the `space-tools` MCP server calls. Only the configured
//! commands, never a shell; a repo command the user hasn't allowed is
//! refused before anything runs.
//!
//! ```text
//! run_check ─▶ gate(check) ─▶ run_space_command(check) ─▶ CheckOutcome
//! start_app ─▶ gate(run)   ─▶ space_run(start)         ─▶ AppStatus
//! stop_app  ─▶ cancel run  ─▶ wait ≤ STOP_WAIT         ─▶ AppStatus
//! app_status · read_logs · space_info                   (read only)
//! ```

use std::path::Path;

use theodore_contract::{
    ProcessStatus, Project, Space, SpaceCommandKind, SpaceRunRequest, SpaceStatus,
};
use theodore_domain::{AgentId, PrId};
use theodore_ports::{
    AppStatus, CheckOutcome, CommandInfo, LogTail, McpEndpoint, SpaceCommandName, SpaceInfo,
    SpaceToolError, local_urls,
};

use crate::core::Core;
use crate::deps::CodeTools;
use crate::usecases::space_commands::{STOP_WAIT, SpaceConfig, describe_failure, kind_name};

fn kind_of(name: SpaceCommandName) -> SpaceCommandKind {
    match name {
        SpaceCommandName::Setup => SpaceCommandKind::Setup,
        SpaceCommandName::Run => SpaceCommandKind::Run,
        SpaceCommandName::Check => SpaceCommandKind::Check,
        SpaceCommandName::Archive => SpaceCommandKind::Archive,
    }
}

/// The script of `kind` when it is configured and allowed; otherwise why
/// not, without running anything.
fn gated(cfg: &SpaceConfig, kind: SpaceCommandKind) -> Result<String, SpaceToolError> {
    let key = kind_name(kind);
    match cfg.command(kind) {
        None => Err(SpaceToolError::Unavailable(format!(
            "no `spaces.{key}` command is configured for this project"
        ))),
        Some(c) if !c.allowed => Err(SpaceToolError::NotAllowed(format!(
            "`spaces.{key}` comes from the repo's config and the user hasn't allowed the \
             repo's commands in this space; ask them to allow them"
        ))),
        Some(c) => Ok(c.script),
    }
}

fn unavailable(e: impl std::fmt::Display) -> SpaceToolError {
    SpaceToolError::Unavailable(e.to_string())
}

impl Core {
    async fn tool_space(&self, id: &str) -> Result<(Space, Project, SpaceConfig), SpaceToolError> {
        let space = self.space_row(id).await.map_err(unavailable)?;
        let project = self.space_project(&space).await.map_err(unavailable)?;
        let cfg = self.space_config_of(&space, &project).await;
        Ok((space, project, cfg))
    }

    /// `run_check`: the space's `check`, now. The turn `run_id`'s Stop
    /// stops it.
    pub async fn space_tool_check(
        &self,
        space: &str,
        run_id: &str,
    ) -> Result<CheckOutcome, SpaceToolError> {
        let (space, project, cfg) = self.tool_space(space).await?;
        if space.status == SpaceStatus::Archived {
            return Err(unavailable("this space is archived"));
        }
        let script = gated(&cfg, SpaceCommandKind::Check)?;
        let cancel = self
            .0
            .runs
            .read()
            .await
            .get(run_id)
            .map(|h| h.cancel.child_token())
            .unwrap_or_default();
        let env = self.space_command_env(&space, &project, &cfg);
        let timeout = cfg.timeout(SpaceCommandKind::Check);
        let (exit, output) = self
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
        Ok(CheckOutcome {
            passed: exit.success(),
            exit_code: exit.code,
            timed_out: exit.timed_out,
            summary: describe_failure(SpaceCommandKind::Check, &exit, timeout)
                .unwrap_or_else(|| "check passed".into()),
            output,
        })
    }

    /// `start_app`: the space's `run`, unless it runs already.
    pub async fn space_tool_start(&self, space: &str) -> Result<AppStatus, SpaceToolError> {
        let (row, _, cfg) = self.tool_space(space).await?;
        if row.status == SpaceStatus::Archived {
            return Err(unavailable("this space is archived"));
        }
        gated(&cfg, SpaceCommandKind::Run)?;
        self.space_run(SpaceRunRequest {
            space: row.id.clone(),
            start: true,
        })
        .await
        .map_err(unavailable)?;
        self.space_tool_status(&row.id).await
    }

    /// `stop_app`: stops `run` and waits a while for it to exit.
    pub async fn space_tool_stop(&self, space: &str) -> Result<AppStatus, SpaceToolError> {
        let row = self.space_row(space).await.map_err(unavailable)?;
        if let Some((cancel, done)) = self.with_runtime(&row.id, |rt| rt.run.clone()) {
            cancel.cancel();
            let _ = tokio::time::timeout(STOP_WAIT, done.cancelled()).await;
        }
        self.space_tool_status(&row.id).await
    }

    /// `app_status`: whether `run` runs, its ports and URLs, how it last
    /// ended.
    pub async fn space_tool_status(&self, space: &str) -> Result<AppStatus, SpaceToolError> {
        let (row, _, cfg) = self.tool_space(space).await?;
        let running = self.with_runtime(&row.id, |rt| rt.run.is_some());
        let command = cfg.command(SpaceCommandKind::Run);
        let last = row.process.as_ref().filter(|_| !running);
        Ok(AppStatus {
            configured: command.is_some(),
            allowed: command.is_some_and(|c| c.allowed),
            running,
            urls: local_urls(&row.ports),
            ports: row.ports.clone(),
            last_status: last.map(|p| {
                match p.status {
                    ProcessStatus::Running => "running",
                    ProcessStatus::Exited => "exited",
                    ProcessStatus::Stopped => "stopped",
                    ProcessStatus::Failed => "failed",
                }
                .to_string()
            }),
            exit_code: last.and_then(|p| p.exit_code),
            error: last.and_then(|p| p.error.clone()),
        })
    }

    /// `read_logs`: the last `tail` lines of one command (the MCP server
    /// bounds `tail`).
    pub async fn space_tool_logs(
        &self,
        space: &str,
        kind: SpaceCommandName,
        tail: usize,
    ) -> Result<LogTail, SpaceToolError> {
        let row = self.space_row(space).await.map_err(unavailable)?;
        let want = kind_of(kind);
        let lines: Vec<String> = self.with_runtime(&row.id, |rt| {
            rt.logs
                .iter()
                .filter(|l| l.kind == want)
                .map(|l| {
                    if l.stderr {
                        format!("! {}", l.line)
                    } else {
                        l.line.clone()
                    }
                })
                .collect()
        });
        let available = lines.len();
        let lines = lines[available.saturating_sub(tail)..].to_vec();
        Ok(LogTail {
            kind,
            lines,
            available,
        })
    }

    /// `space_info`: the space, its ports, the names (never the values)
    /// of the variables its commands get, and its commands.
    pub async fn space_tool_info(&self, space: &str) -> Result<SpaceInfo, SpaceToolError> {
        let (row, project, cfg) = self.tool_space(space).await?;
        let env = self
            .space_command_env(&row, &project, &cfg)
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        let commands = SpaceCommandName::ALL
            .into_iter()
            .map(|kind| {
                let c = cfg.command(kind_of(kind));
                CommandInfo {
                    kind,
                    configured: c.is_some(),
                    allowed: c.is_some_and(|c| c.allowed),
                }
            })
            .collect();
        Ok(SpaceInfo {
            name: row.name.clone(),
            branch: row.branch.clone(),
            base: row.base.clone(),
            worktree: row.worktree.clone(),
            urls: local_urls(&row.ports),
            ports: row.ports.clone(),
            env,
            commands,
        })
    }

    /// The space whose PR `pr` is (its branch review or its shipped PR).
    pub(crate) async fn space_of_pr(&self, pr: &PrId) -> Option<String> {
        self.list_spaces(None)
            .await
            .ok()?
            .into_iter()
            .find(|s| s.review_pr.as_ref() == Some(pr) || s.pr.as_ref() == Some(pr))
            .map(|s| s.id)
    }

    /// `review-tools` for a PR run, with the read-only space tools when
    /// the PR is a space's and the read-only code tools when code
    /// intelligence is on for it (its worktree is opened here). `None`
    /// without an MCP server.
    pub(crate) async fn pr_tools(
        &self,
        run_id: &str,
        pr: &PrId,
        agent: AgentId,
    ) -> (Option<McpEndpoint>, CodeTools) {
        let Some(tools) = self.0.deps.tools.as_ref() else {
            return (None, CodeTools::Off);
        };
        let space = self.space_of_pr(pr).await;
        let code = self.review_code_tools(pr).await;
        (
            Some(tools.register(run_id, pr, agent, space.as_deref(), code)),
            code,
        )
    }
}
