//! `run_skill`, `ask`, `cancel_run`, `add_finding` (plan §8.4): agents are
//! spawned per the `AgentPort`, their stream becomes ephemeral events, and
//! only the final result is synced.

use std::path::{Path, PathBuf};

use theodore_contract::{AskRequest, ChatMessage, RunSkillRequest, RunStatus, SkillRun, models};
use theodore_domain::{AgentId, Author, Finding, FindingId, Fix, PrId};
use theodore_ports::{
    AgentAttachment, AgentEvent, AgentMode, AgentRequest, NewAction, SyncActionCode,
};
use tokio_util::sync::CancellationToken;
use tracing::Instrument as _;

use crate::core::{Core, RunHandle};
use crate::error::{AppError, AppResult};
use crate::usecases::attachments::{agent_attachments, with_attachments};
use crate::usecases::code_intel::code_prompt;
use crate::usecases::live::LiveRun;

impl Core {
    pub async fn run_skill(&self, req: RunSkillRequest) -> AppResult<String> {
        self.ensure_open(&req.pr).await?;
        let repo_scope = self.repo_scope_of(&req.pr).await;
        let agents = if req.agents.is_empty() {
            self.setting_agents_scoped(
                "agents.skills",
                repo_scope.as_deref(),
                vec![AgentId::Claude],
            )
            .await
        } else {
            req.agents.clone()
        };
        // Skip agents that aren't installed instead of failing the whole run
        // after the installed ones already spent tokens.
        let installed: Vec<AgentId> = self
            .0
            .deps
            .agents
            .detect()
            .await
            .into_iter()
            .filter(|a| a.installed)
            .map(|a| a.id)
            .collect();
        let mut agents_filtered = Vec::new();
        for a in agents {
            if installed.contains(&a) && self.agent_enabled(a).await {
                agents_filtered.push(a);
            }
        }
        let agents = agents_filtered;
        if agents.is_empty() {
            return Err(AppError::Invalid(
                "none of the selected agents is installed and enabled".into(),
            ));
        }
        let run_id = uuid::Uuid::new_v4().to_string();
        let started_at = self.0.deps.clock.now();
        let row = SkillRun {
            id: run_id.clone(),
            pr: req.pr.clone(),
            skill: req.skill.clone(),
            agents: agents.clone(),
            status: RunStatus::Running,
            error: None,
            started_at,
            hidden_findings: 0,
        };
        let data = serde_json::to_value(&row).map_err(|e| AppError::Invalid(e.to_string()))?;
        self.apply(vec![NewAction {
            model: models::SKILL_RUN.into(),
            model_id: run_id.clone(),
            action: SyncActionCode::I,
            data: Some(data),
        }])
        .await?;

        let cancel = CancellationToken::new();
        self.0.runs.write().await.insert(
            run_id.clone(),
            RunHandle {
                cancel: cancel.clone(),
            },
        );

        // Held for the whole run (plan "worktree locks"): the GC sweep must
        // never remove this worktree out from under a skill run in flight.
        let lease = self
            .pr_ref(&req.pr)
            .await
            .map(|r| self.0.deps.worktree_lease.acquire(r));

        let core = self.clone();
        let pr = req.pr.clone();
        let skill = req.skill.clone();
        let run_id2 = run_id.clone();
        // In the command's span, so the run's records keep its `op_id`.
        tokio::spawn(
            async move {
                let _lease = lease;
                drive_skill_run(&core, run_id2, pr, skill, agents, cancel).await;
            }
            .in_current_span(),
        );
        Ok(run_id)
    }

    pub async fn ask(&self, req: AskRequest) -> AppResult<String> {
        self.ensure_open(&req.pr).await?;
        // Checked before the question is stored: an unknown id fails it.
        let attachments = self.resolve_attachments(req.attachments.as_deref()).await?;
        let message_id = uuid::Uuid::new_v4().to_string();
        let now = self.0.deps.clock.now();
        let question = ChatMessage {
            id: message_id.clone(),
            pr: req.pr.clone(),
            author: Author::Me,
            text: req.question.clone(),
            refs: req.about.clone().into_iter().collect(),
            at: now,
            failed: None,
            attachments: attachments.clone(),
            queued: None,
        };
        let data = serde_json::to_value(&question).map_err(|e| AppError::Invalid(e.to_string()))?;
        self.apply(vec![NewAction {
            model: models::CHAT_MESSAGE.into(),
            model_id: message_id.clone(),
            action: SyncActionCode::I,
            data: Some(data),
        }])
        .await?;

        let agent = match req.agent {
            Some(a) => a,
            None => {
                let repo_scope = self.repo_scope_of(&req.pr).await;
                let configured = self
                    .setting_agent_scoped("agents.chat", repo_scope.as_deref(), AgentId::Claude)
                    .await;
                self.effective_agent(configured).await
            }
        };
        let cwd = {
            let head = self.0.current_head.read().await.get(&req.pr).cloned();
            let Some(head) = head else {
                return Ok(message_id);
            };
            let revisions = self.0.revisions.read().await;
            match revisions.get(&(req.pr.clone(), head)) {
                Some(e) => e.worktree_path.clone(),
                None => return Ok(message_id),
            }
        };

        let run_id = format!("ask:{message_id}");
        let cancel = CancellationToken::new();
        self.0.runs.write().await.insert(
            run_id.clone(),
            RunHandle {
                cancel: cancel.clone(),
            },
        );

        // Held for the whole run (plan "worktree locks"): see `run_skill`.
        let lease = self
            .pr_ref(&req.pr)
            .await
            .map(|r| self.0.deps.worktree_lease.acquire(r));

        let scope = if req.about.is_some() {
            "hunk"
        } else if req.group.is_some() {
            "group"
        } else {
            "pr"
        };
        theodore_telemetry::product(theodore_telemetry::ProductEvent::ChatAsked {
            scope: scope.into(),
        });
        let core = self.clone();
        let pr = req.pr.clone();
        tokio::spawn(
            async move {
                let _lease = lease;
                let question = Question {
                    prompt: with_attachments(req.question, &attachments),
                    attachments: agent_attachments(&attachments),
                };
                drive_ask(&core, run_id, pr, agent, question, cwd, cancel).await;
            }
            .in_current_span(),
        );
        Ok(message_id)
    }

    /// `configured` unless it's disabled, in which case the first enabled
    /// installed agent (plan "enable/disable agents"): falls back instead
    /// of silently running a CLI the user turned off. Returns `configured`
    /// unchanged if every installed agent is disabled — a hard failure
    /// here would be worse than running the one the setting already named.
    pub(crate) async fn effective_agent(&self, configured: AgentId) -> AgentId {
        if self.agent_enabled(configured).await {
            return configured;
        }
        let installed: Vec<AgentId> = self
            .0
            .deps
            .agents
            .detect()
            .await
            .into_iter()
            .filter(|a| a.installed)
            .map(|a| a.id)
            .collect();
        self.first_enabled(&installed).await.unwrap_or(configured)
    }

    pub async fn cancel_run(&self, run: String) -> AppResult<()> {
        if let Some(handle) = self.0.runs.write().await.remove(&run) {
            handle.cancel.cancel();
        }
        if !run.starts_with("ask:") {
            let data = serde_json::json!({ "status": RunStatus::Cancelled });
            let _ = self
                .apply(vec![NewAction {
                    model: models::SKILL_RUN.into(),
                    model_id: run,
                    action: SyncActionCode::C,
                    data: Some(data),
                }])
                .await;
        }
        Ok(())
    }

    /// Findings from several agents merge into one (plan §4, §8.4). Called
    /// once the MCP `add_finding` tool hands a result back.
    /// Returns the id of the stored finding (an existing one when merged).
    pub async fn add_finding(&self, pr: PrId, mut finding: Finding) -> AppResult<FindingId> {
        finding.pr = pr.clone();
        // The MCP tools only know the run they serve: a finding reported
        // from a skill run names that run, and belongs to its skill (the
        // panel lists a run's findings by skill, and merges duplicates
        // across agents by skill).
        let run_id = finding.skill.0.clone();
        let store = self.0.deps.store.clone();
        let run_id2 = run_id.clone();
        if let Some(skill) =
            tokio::task::spawn_blocking(move || store.get(models::SKILL_RUN, &run_id2))
                .await
                .map_err(AppError::join)??
                .and_then(|r| {
                    r.data
                        .get("skill")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                })
        {
            finding.skill = theodore_domain::SkillId(skill);
        }
        // `review.ignore_findings`: dropped before it's stored, and counted
        // on its run so the AI panel can say so.
        if self.finding_ignored(&pr, &finding.anchor.path).await {
            self.count_hidden_finding(&run_id).await?;
            return Ok(finding.id);
        }
        let pr_s = pr.0.clone();
        let store = self.0.deps.store.clone();
        let rows = tokio::task::spawn_blocking(move || store.query(models::FINDING, "pr", &pr_s))
            .await
            .map_err(AppError::join)??;
        let existing: Vec<Finding> = rows
            .iter()
            .filter_map(|r| serde_json::from_value(r.data.clone()).ok())
            .collect();

        let (action, mut merged) = match theodore_domain::findings::merge(&existing, finding) {
            theodore_domain::findings::MergeOutcome::New(f) => (SyncActionCode::I, f),
            theodore_domain::findings::MergeOutcome::MergedInto(f) => (SyncActionCode::U, f),
        };
        // A finding stored before skills were resolved names its run; fix
        // it up when a new report merges into it.
        let stored_skill = merged.skill.0.clone();
        let store = self.0.deps.store.clone();
        if let Some(skill) =
            tokio::task::spawn_blocking(move || store.get(models::SKILL_RUN, &stored_skill))
                .await
                .map_err(AppError::join)??
                .and_then(|r| {
                    r.data
                        .get("skill")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                })
        {
            merged.skill = theodore_domain::SkillId(skill);
        }
        let data = serde_json::to_value(&merged).map_err(|e| AppError::Invalid(e.to_string()))?;
        self.apply(vec![NewAction {
            model: models::FINDING.into(),
            model_id: merged.id.0.clone(),
            action,
            data: Some(data),
        }])
        .await?;
        Ok(merged.id)
    }

    /// One more finding of `run` hidden by `review.ignore_findings`. A
    /// report that names no stored run is dropped uncounted.
    async fn count_hidden_finding(&self, run: &str) -> AppResult<()> {
        let _guard = self.0.review_lock.lock().await;
        let store = self.0.deps.store.clone();
        let id = run.to_string();
        let Some(row) = tokio::task::spawn_blocking(move || store.get(models::SKILL_RUN, &id))
            .await
            .map_err(AppError::join)??
        else {
            return Ok(());
        };
        let n = row
            .data
            .get("hidden_findings")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        self.apply(vec![NewAction {
            model: models::SKILL_RUN.into(),
            model_id: run.to_string(),
            action: SyncActionCode::C,
            data: Some(serde_json::json!({ "hidden_findings": n + 1 })),
        }])
        .await?;
        Ok(())
    }

    /// Attach an agent-proposed fix to a finding of `pr` (MCP `propose_patch`).
    pub async fn attach_fix(&self, pr: &PrId, finding: &FindingId, fix: Fix) -> AppResult<()> {
        let store = self.0.deps.store.clone();
        let id = finding.0.clone();
        let row = tokio::task::spawn_blocking(move || store.get(models::FINDING, &id))
            .await
            .map_err(AppError::join)??
            .ok_or_else(|| AppError::NotFound(format!("finding {finding}")))?;
        let existing: Finding =
            serde_json::from_value(row.data).map_err(|e| AppError::Invalid(e.to_string()))?;
        if &existing.pr != pr {
            return Err(AppError::Invalid(format!(
                "finding {finding} belongs to another PR"
            )));
        }
        let data = serde_json::json!({ "fix": fix });
        self.apply(vec![NewAction {
            model: models::FINDING.into(),
            model_id: finding.0.clone(),
            action: SyncActionCode::C,
            data: Some(data),
        }])
        .await?;
        Ok(())
    }
}

async fn drive_skill_run(
    core: &Core,
    run_id: String,
    pr: PrId,
    skill: String,
    agents: Vec<AgentId>,
    cancel: CancellationToken,
) {
    // `agents` is never empty (checked by `run_skill` before spawning this
    // task) — used to attribute a setup failure that happens before any
    // agent actually starts.
    let first_agent = agents[0];
    let cwd = {
        let head = core.0.current_head.read().await.get(&pr).cloned();
        let Some(head) = head else {
            fail_run(core, &run_id, &pr, first_agent, "pr is not open").await;
            return;
        };
        let revisions = core.0.revisions.read().await;
        match revisions.get(&(pr.clone(), head)) {
            Some(e) => e.worktree_path.clone(),
            None => {
                fail_run(core, &run_id, &pr, first_agent, "revision not cached").await;
                return;
            }
        }
    };
    let stall_after_ms = core.stall_after_ms().await;
    // The user's own MCP servers are for spaces only (issue #16).
    let user_mcp = false;
    // The repo's `review.instructions` / `review.context`.
    let rules = core.review_rules_of(&pr).await;
    let started = std::time::Instant::now();
    let findings_before = count_findings(core, &pr, &skill).await;
    let agent_count = agents.len() as u32;

    let mut error: Option<String> = None;
    let mut last_agent = first_agent;
    for agent in agents {
        last_agent = agent;
        if cancel.is_cancelled() {
            break;
        }
        let (mcp, code) = core.pr_tools(&run_id, &pr, agent).await;
        let req = AgentRequest {
            agent,
            prompt: skill_prompt(&skill, agent, &cwd, &core.0.deps.claude_home),
            system: rules.with_system(code_prompt(code).map(str::to_string)),
            cwd: cwd.clone(),
            mode: AgentMode::ReadOnly,
            mcp: mcp.clone(),
            model: None,
            effort: None,
            stall_after_ms,
            env: Vec::new(),
            user_mcp,
            attachments: vec![],
            resume: None,
            stream_text: true,
        };
        let _guard = ToolsGuard { core, mcp };
        let mut live = LiveRun::new(run_id.as_str(), agent).with_thinking();
        match core.0.deps.agents.run(req).await {
            Ok(mut run) => loop {
                tokio::select! {
                    _ = cancel.cancelled() => { run.cancel.cancel(); break; }
                    event = run.events.recv() => {
                        let Some(event) = event else { break };
                        live.show(core, &event);
                        match event {
                            AgentEvent::Done { .. } => break,
                            AgentEvent::Error { message, .. } => {
                                error = Some(message);
                                break;
                            }
                            _ => {}
                        }
                    }
                }
            },
            Err(e) => error = Some(e.to_string()),
        }
    }

    let cancelled = cancel.is_cancelled();
    let status = if cancelled {
        RunStatus::Cancelled
    } else if error.is_some() {
        RunStatus::Failed
    } else {
        RunStatus::Done
    };
    // The chat message first: whoever waits for the run's final status
    // (the UI, tests) must find it already there.
    if cancelled || error.is_some() {
        let text = error.clone().unwrap_or_else(|| "cancelled".into());
        persist_failed_message(core, &pr, last_agent, &text).await;
    }
    let data = serde_json::json!({ "status": status, "error": error });
    let _ = core
        .apply(vec![NewAction {
            model: models::SKILL_RUN.into(),
            model_id: run_id.clone(),
            action: SyncActionCode::C,
            data: Some(data),
        }])
        .await;
    let ms = started.elapsed().as_millis() as u64;
    if let Some(e) = &error
        && theodore_ports::offline_message_in(e).is_some()
    {
        // The agent's servers were out of reach: the network, not a bug.
        tracing::info!(
            error.code = theodore_ports::AGENT_OFFLINE_CODE,
            run = %run_id,
            agent = last_agent.as_str(),
            duration_ms = ms,
            "skill run couldn't reach the agent"
        );
    } else if let Some(e) = &error {
        tracing::warn!(
            error.code = "skill.failed",
            run = %run_id,
            agent = last_agent.as_str(),
            duration_ms = ms,
            error = %e,
            "skill run failed"
        );
    }
    if !cancelled && error.is_none() {
        let findings = count_findings(core, &pr, &skill)
            .await
            .saturating_sub(findings_before);
        theodore_telemetry::product(theodore_telemetry::ProductEvent::SkillRun {
            skill: skill_label(&skill),
            agents: agent_count,
            findings,
            ms,
        });
    }
    core.0.runs.write().await.remove(&run_id);
}

/// A built-in skill's id; anything the user wrote is just `custom` (its
/// name could say what the code is about).
pub(crate) fn skill_label(skill: &str) -> String {
    if crate::usecases::skills::builtin_content(skill).is_some() {
        skill.to_string()
    } else {
        "custom".to_string()
    }
}

async fn count_findings(core: &Core, pr: &PrId, skill: &str) -> u32 {
    let store = core.0.deps.store.clone();
    let pr_s = pr.0.clone();
    let rows = tokio::task::spawn_blocking(move || store.query(models::FINDING, "pr", &pr_s))
        .await
        .ok()
        .and_then(|r| r.ok())
        .unwrap_or_default();
    rows.iter()
        .filter(|r| r.data.get("skill").and_then(|s| s.as_str()) == Some(skill))
        .count() as u32
}

/// Persists a `ChatMessage` with `failed` set and `agent` as author instead
/// of only toasting (plan "failed agent runs"), so a run that errored,
/// crashed, hit its runtime cap, or was cancelled leaves a durable, visible,
/// retryable record even after the app restarts. `pub(crate)`: also used by
/// `usecases::tasks` for fix runs.
pub(crate) async fn persist_failed_message(core: &Core, pr: &PrId, agent: AgentId, text: &str) {
    let message_id = uuid::Uuid::new_v4().to_string();
    let message = ChatMessage {
        id: message_id.clone(),
        pr: pr.clone(),
        author: Author::Agent(agent),
        text: String::new(),
        refs: vec![],
        at: core.0.deps.clock.now(),
        failed: Some(text.to_string()),
        attachments: vec![],
        queued: None,
    };
    if let Ok(data) = serde_json::to_value(&message) {
        let _ = core
            .apply(vec![NewAction {
                model: models::CHAT_MESSAGE.into(),
                model_id: message_id,
                action: SyncActionCode::I,
                data: Some(data),
            }])
            .await;
    }
}

/// The run-log line for a tool call. Theodore's own tools read as what
/// they do instead of `mcp__space-tools__run_check` (Claude),
/// `space-tools__run_check` (Codex) or the bare name; anything else is
/// shown as the agent named it.
/// `tool: Bash · npm test`: the call's name and what it acted on (a
/// command, a file, a pattern), for the UI's step list only — this line
/// goes to the window as an ephemeral event, never to the log file.
pub(crate) fn tool_log_line_with(name: &str, input: &serde_json::Value) -> String {
    let line = tool_log_line(name);
    match tool_target(input) {
        Some(t) => format!("{line} · {t}"),
        None => line,
    }
}

/// The part of a tool call worth showing: the first line of a command, a
/// file's last two path segments, or a search pattern; 80 chars at most.
fn tool_target(input: &serde_json::Value) -> Option<String> {
    let s = |k: &str| input.get(k).and_then(serde_json::Value::as_str);
    let raw = if let Some(c) = s("command") {
        c.lines().next().unwrap_or_default().trim().to_string()
    } else if let Some(p) = s("file_path")
        .or_else(|| s("path"))
        .or_else(|| s("notebook_path"))
        .or_else(|| s("file"))
    {
        let parts: Vec<&str> = p.trim_end_matches('/').rsplit('/').take(2).collect();
        parts.into_iter().rev().collect::<Vec<_>>().join("/")
    } else {
        s("pattern")
            .or_else(|| s("query"))
            .or_else(|| s("url"))
            // Theodore's `set_base` and `ship`.
            .or_else(|| s("branch"))
            .or_else(|| s("action"))?
            .trim()
            .to_string()
    };
    if raw.is_empty() {
        return None;
    }
    Some(if raw.chars().count() > 80 {
        format!("{}…", raw.chars().take(79).collect::<String>())
    } else {
        raw
    })
}

pub(crate) fn tool_log_line(name: &str) -> String {
    let own = [theodore_ports::REVIEW_TOOLS, theodore_ports::SPACE_TOOLS]
        .iter()
        .find_map(|server| {
            let name = name.strip_prefix("mcp__").unwrap_or(name);
            name.strip_prefix(server)?.strip_prefix("__")
        })
        .unwrap_or(name);
    let label = match own {
        "run_check" => "run the check",
        "start_app" => "start the app",
        "stop_app" => "stop the app",
        "app_status" => "app status",
        "read_logs" => "read the logs",
        "space_info" => "space info",
        "ship" => "ship the change",
        "get_space_diff" => "read the diff",
        "get_review_comments" => "read the review comments",
        "add_review_comment" => "leave a review comment",
        "set_base" => "change the base branch",
        "update_progress" => "update the progress",
        "get_pr_context" => "read the PR",
        "get_group" => "read a group",
        "get_base_file" => "read a file before the change",
        "add_finding" => "add a finding",
        "propose_patch" => "propose a fix",
        "find_symbol" => "find a symbol",
        "get_symbol" => "read a symbol",
        "references" => "find references",
        "definition" => "go to definition",
        "diagnostics" => "read diagnostics",
        "code_status" => "code intelligence status",
        "replace_symbol_body" => "replace a symbol",
        "insert_after_symbol" => "insert after a symbol",
        "rename_symbol" => "rename a symbol",
        _ => name,
    };
    format!("tool: {label}")
}

/// Revokes a run's MCP token when the run's driver returns. `pub(crate)`:
/// also used by `usecases::tasks` for fix runs.
pub(crate) struct ToolsGuard<'a> {
    pub(crate) core: &'a Core,
    pub(crate) mcp: Option<theodore_ports::McpEndpoint>,
}

impl Drop for ToolsGuard<'_> {
    fn drop(&mut self) {
        if let (Some(tools), Some(mcp)) = (&self.core.0.deps.tools, &self.mcp) {
            tools.unregister(mcp);
        }
    }
}

/// Claude discovers `SKILL.md` files under `~/.claude/skills` and the
/// worktree's `.claude/skills` on its own (plan §8.4), so it only needs the
/// skill referenced by name. Every other agent gets the full text inlined.
fn skill_prompt(skill: &str, agent: AgentId, worktree: &Path, claude_home: &Path) -> String {
    let instructions = "Start with the review-tools `get_pr_context` tool. Report every issue \
         you find with `add_finding` (file path, line on the new side, severity \
         info|warn|high, short title, body). Do not edit files. Finish with a \
         one-paragraph summary.";
    if agent == AgentId::Claude {
        return format!("Review this pull request with the `{skill}` skill. {instructions}");
    }
    match crate::usecases::skills::resolve_skill_content(skill, worktree, claude_home) {
        Some(content) => format!(
            "{content}\n\n---\n\nReview this pull request using the skill above. {instructions}"
        ),
        None => format!("Review this pull request with the `{skill}` skill. {instructions}"),
    }
}

async fn fail_run(core: &Core, run_id: &str, pr: &PrId, agent: AgentId, message: &str) {
    persist_failed_message(core, pr, agent, message).await;
    let data = serde_json::json!({ "status": RunStatus::Failed, "error": message });
    let _ = core
        .apply(vec![NewAction {
            model: models::SKILL_RUN.into(),
            model_id: run_id.to_string(),
            action: SyncActionCode::C,
            data: Some(data),
        }])
        .await;
    core.0.runs.write().await.remove(run_id);
}

/// Chat questions get the same footing as skills: the worktree holds only
/// the head, so without this the agent can't tell what the PR changed and
/// says so instead of answering.
const ASK_SYSTEM: &str = "You are answering a question about the pull request checked out \
     in the current directory. The directory has only the PR's head; what the PR changes \
     comes from the review-tools MCP server: `get_pr_context` (title, body, commits, \
     files, groups), `get_group` (a group's hunks as a unified diff) and `get_base_file` \
     (a file before the change). Look at the change through those tools before \
     answering. Do not edit files. Answer briefly and point at file:line where it helps.";

/// What `drive_ask` sends: the question with its files listed, and the
/// files themselves.
struct Question {
    prompt: String,
    attachments: Vec<AgentAttachment>,
}

async fn drive_ask(
    core: &Core,
    run_id: String,
    pr: PrId,
    agent: AgentId,
    question: Question,
    cwd: PathBuf,
    cancel: CancellationToken,
) {
    let (mcp, code) = core.pr_tools(&run_id, &pr, agent).await;
    let system = core
        .review_rules_of(&pr)
        .await
        .with_system(Some(match code_prompt(code) {
            Some(hint) => format!("{ASK_SYSTEM}\n{hint}"),
            None => ASK_SYSTEM.into(),
        }));
    let req = AgentRequest {
        agent,
        prompt: question.prompt,
        system,
        cwd,
        mode: AgentMode::ReadOnly,
        mcp: mcp.clone(),
        model: None,
        effort: None,
        stall_after_ms: core.stall_after_ms().await,
        env: Vec::new(),
        // Spaces only (issue #16): a chat answer stays on Theodore's tools.
        user_mcp: false,
        attachments: question.attachments,
        resume: None,
        stream_text: true,
    };
    let _guard = ToolsGuard { core, mcp };
    let mut live = LiveRun::new(run_id.as_str(), agent);
    let mut text = String::new();
    let mut error = None;
    match core.0.deps.agents.run(req).await {
        Ok(mut run) => loop {
            tokio::select! {
                _ = cancel.cancelled() => { run.cancel.cancel(); break; }
                event = run.events.recv() => {
                    let Some(event) = event else { break };
                    live.show(core, &event);
                    match event {
                        AgentEvent::Text { text: t } => text.push_str(&t),
                        AgentEvent::Done { final_text, .. } => {
                            text = final_text;
                            break;
                        }
                        AgentEvent::Error { message, .. } => {
                            error = Some(message);
                            break;
                        }
                        _ => {}
                    }
                }
            }
        },
        Err(e) => error = Some(e.to_string()),
    }
    core.0.runs.write().await.remove(&run_id);

    // A crash, the runtime cap, or a cancel all leave a durable, retryable
    // record instead of silently dropping the exchange (plan "failed agent
    // runs") — cancelling mid-answer must not look, after a restart, as if
    // nothing was ever asked.
    if cancel.is_cancelled() || error.is_some() {
        let text = error.unwrap_or_else(|| "cancelled".into());
        persist_failed_message(core, &pr, agent, &text).await;
        return;
    }

    let message_id = uuid::Uuid::new_v4().to_string();
    let now = core.0.deps.clock.now();
    let reply = ChatMessage {
        id: message_id.clone(),
        pr,
        author: Author::Agent(agent),
        text,
        refs: vec![],
        at: now,
        failed: None,
        attachments: vec![],
        queued: None,
    };
    if let Ok(data) = serde_json::to_value(&reply) {
        let _ = core
            .apply(vec![NewAction {
                model: models::CHAT_MESSAGE.into(),
                model_id: message_id,
                action: SyncActionCode::I,
                data: Some(data),
            }])
            .await;
    }
}

#[cfg(test)]
mod tool_label_tests {
    use super::{tool_log_line, tool_log_line_with};

    #[test]
    fn theodores_tools_read_as_what_they_do() {
        assert_eq!(
            tool_log_line("mcp__space-tools__run_check"),
            "tool: run the check"
        );
        assert_eq!(
            tool_log_line("space-tools__start_app"),
            "tool: start the app"
        );
        assert_eq!(
            tool_log_line("mcp__review-tools__read_logs"),
            "tool: read the logs"
        );
        assert_eq!(tool_log_line("app_status"), "tool: app status");
        assert_eq!(tool_log_line("Bash"), "tool: Bash");
        assert_eq!(
            tool_log_line_with(
                "Bash",
                &serde_json::json!({ "command": "npm test\nnpm run lint" })
            ),
            "tool: Bash · npm test"
        );
        assert_eq!(
            tool_log_line_with(
                "Edit",
                &serde_json::json!({ "file_path": "/repo/src/lib/price.ts" })
            ),
            "tool: Edit · lib/price.ts"
        );
        assert_eq!(
            tool_log_line_with("Grep", &serde_json::json!({ "pattern": "formatPrice" })),
            "tool: Grep · formatPrice"
        );
        assert_eq!(
            tool_log_line_with("Bash", &serde_json::json!({})),
            "tool: Bash"
        );
        let long = "x".repeat(200);
        assert_eq!(
            tool_log_line_with("Bash", &serde_json::json!({ "command": long }))
                .chars()
                .count(),
            "tool: Bash · ".chars().count() + 80
        );
        assert_eq!(
            tool_log_line_with(
                "mcp__space-tools__ship",
                &serde_json::json!({ "action": "open_pr" })
            ),
            "tool: ship the change · open_pr"
        );
        assert_eq!(
            tool_log_line_with(
                "space-tools__get_space_diff",
                &serde_json::json!({ "file": "src/auth/login.ts" })
            ),
            "tool: read the diff · auth/login.ts"
        );
        assert_eq!(
            tool_log_line_with(
                "mcp__space-tools__set_base",
                &serde_json::json!({ "branch": "release" })
            ),
            "tool: change the base branch · release"
        );
        for (name, label) in [
            ("get_review_comments", "read the review comments"),
            ("add_review_comment", "leave a review comment"),
            ("update_progress", "update the progress"),
        ] {
            assert_eq!(
                tool_log_line(&format!("mcp__space-tools__{name}")),
                format!("tool: {label}")
            );
        }
        assert_eq!(
            tool_log_line("mcp__other__run_check"),
            "tool: mcp__other__run_check",
            "only Theodore's own servers"
        );
    }
}
