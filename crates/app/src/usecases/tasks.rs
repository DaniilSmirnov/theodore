//! `delegate_task`, `decide_task` (plan §4 "Fix target"): an agent edits
//! the PR worktree in `AgentMode::Edit`; its diff becomes a `Fix::patch`
//! and the task moves to `review`. Accepting commits + pushes on an own or
//! local PR, or drafts a suggestion on anyone else's; rejecting resets the
//! worktree and sends the task back to `todo`.

use std::path::{Path, PathBuf};

use theodore_contract::{DelegateRequest, Ephemeral, TaskDecision, ToastLevel, models};
use theodore_domain::{
    Assignee, Author, Draft, DraftId, Fix, FixStatus, HostKind, Patch, Task, TaskStatus,
};
use theodore_ports::{AgentEvent, AgentMode, AgentRequest, NewAction, SyncActionCode};
use tokio_util::sync::CancellationToken;

use crate::core::{Core, RunHandle};
use crate::error::{AppError, AppResult};
use crate::usecases::agents::ToolsGuard;
use crate::usecases::live::LiveRun;

impl Core {
    pub async fn delegate_task(&self, req: DelegateRequest) -> AppResult<String> {
        let task = self.load_task(&req.task).await?;
        let agent = match req.agent {
            Some(a) => a,
            None => {
                let repo_scope = self.repo_scope_of(&task.pr).await;
                let configured = self
                    .setting_agent_scoped(
                        "agents.fix",
                        repo_scope.as_deref(),
                        theodore_domain::AgentId::Claude,
                    )
                    .await;
                self.effective_agent(configured).await
            }
        };
        let worktree = self
            .worktree_of(&task.pr)
            .await
            .ok_or_else(|| AppError::Invalid("pr is not open".into()))?;

        // Held for the whole run (plan "worktree locks"): see
        // `usecases::agents::run_skill`.
        let lease = self
            .pr_ref(&task.pr)
            .await
            .map(|r| self.0.deps.worktree_lease.acquire(r));

        let reason_key = format!("task:{}:reject_reason", task.id);
        let store = self.0.deps.store.clone();
        let key = reason_key.clone();
        let reason = tokio::task::spawn_blocking(move || store.kv_get(&key))
            .await
            .ok()
            .and_then(|r| r.ok())
            .flatten()
            .and_then(|v| v.as_str().map(str::to_string));

        let data = serde_json::json!({
            "status": TaskStatus::Progress,
            "assignee": Assignee::Agent(agent),
            "fix": null,
        });
        self.apply(vec![NewAction {
            model: models::TASK.into(),
            model_id: task.id.0.clone(),
            action: SyncActionCode::C,
            data: Some(data),
        }])
        .await?;

        let run_id = format!("fix:{}", uuid::Uuid::new_v4());
        let cancel = CancellationToken::new();
        self.0.runs.write().await.insert(
            run_id.clone(),
            RunHandle {
                cancel: cancel.clone(),
            },
        );

        theodore_telemetry::product(theodore_telemetry::ProductEvent::TaskDelegated {
            agent: agent.as_str().into(),
        });
        let core = self.clone();
        let run_id2 = run_id.clone();
        tokio::spawn(tracing::Instrument::in_current_span(async move {
            let _lease = lease;
            drive_fix_run(&core, run_id2, task, agent, worktree, reason, cancel).await;
        }));
        Ok(run_id)
    }

    pub async fn decide_task(&self, req: TaskDecision) -> AppResult<()> {
        let task = self.load_task(&req.task).await?;
        theodore_telemetry::product(theodore_telemetry::ProductEvent::TaskDecided {
            accepted: req.accept,
        });
        if req.accept {
            self.accept_task(task).await
        } else {
            self.reject_task(task, req.reason).await
        }
    }

    async fn load_task(&self, id: &theodore_domain::TaskId) -> AppResult<Task> {
        let store = self.0.deps.store.clone();
        let id_s = id.0.clone();
        let row = tokio::task::spawn_blocking(move || store.get(models::TASK, &id_s))
            .await
            .map_err(AppError::join)??
            .ok_or_else(|| AppError::NotFound(format!("task {id}")))?;
        serde_json::from_value(row.data).map_err(|e| AppError::Invalid(e.to_string()))
    }

    /// The PR's worktree, opening the PR first if this session hasn't (a
    /// task or suggestion acted on from Tasks after a restart, before the
    /// PR's own page was visited). `None` only if it can't be opened.
    pub(crate) async fn worktree_of(&self, pr: &theodore_domain::PrId) -> Option<PathBuf> {
        if self.ensure_open(pr).await.is_err() {
            return None;
        }
        let head = self.0.current_head.read().await.get(pr).cloned()?;
        let revisions = self.0.revisions.read().await;
        revisions
            .get(&(pr.clone(), head))
            .map(|e| e.worktree_path.clone())
    }

    async fn pr_detail_cached(
        &self,
        pr: &theodore_domain::PrId,
    ) -> Option<theodore_domain::PrDetail> {
        let store = self.0.deps.store.clone();
        let id = pr.0.clone();
        let row = tokio::task::spawn_blocking(move || store.get(models::PR_DETAIL, &id))
            .await
            .ok()?
            .ok()??;
        serde_json::from_value(row.data).ok()
    }

    async fn accept_task(&self, task: Task) -> AppResult<()> {
        let pr_ref = self
            .pr_ref(&task.pr)
            .await
            .ok_or_else(|| AppError::Invalid("pr not found".into()))?;

        let Some(fix) = task.fix.clone() else {
            return self.finish_task_accept(&task, None).await;
        };
        let Some(patch) = fix.patch.clone() else {
            return self.finish_task_accept(&task, Some(fix)).await;
        };

        let own_or_local = pr_ref.repo.host == HostKind::Local || {
            match (
                self.0.deps.host.viewer().await,
                self.pr_detail_cached(&task.pr).await,
            ) {
                (Ok(me), Some(detail)) => detail.summary.author.login == me.login,
                _ => false,
            }
        };

        let new_fix = if own_or_local {
            let worktree = self
                .worktree_of(&task.pr)
                .await
                .ok_or_else(|| AppError::Invalid("pr is not open".into()))?;
            let branch = self
                .pr_detail_cached(&task.pr)
                .await
                .map(|d| d.summary.head_ref)
                .unwrap_or_else(|| "HEAD".into());
            // The agent's edit may be gone from the worktree (an app restart
            // checks the PR out afresh); the stored patch is the fix.
            ensure_patch_applied(&worktree, &patch).await?;
            let message = format!("fix: {} ({})", task.title, patch.by.as_str());
            let sha = self
                .0
                .deps
                .git
                .commit_and_push(&worktree, &pr_ref.repo, &branch, &message)
                .await?;
            Fix {
                status: FixStatus::Applied,
                patch: Some(patch),
                commit: Some(sha),
            }
        } else if let Some(anchor) = task.anchor.clone() {
            let body = format!(
                "{}\n```suggestion\n{}\n```",
                task.title,
                patch.after.join("\n")
            );
            let draft = Draft {
                id: DraftId::new(uuid::Uuid::new_v4().to_string()),
                pr: task.pr.clone(),
                anchor,
                body,
                source: Author::Agent(patch.by),
                outdated: false,
            };
            let data =
                serde_json::to_value(&draft).map_err(|e| AppError::Invalid(e.to_string()))?;
            self.apply(vec![NewAction {
                model: models::DRAFT.into(),
                model_id: draft.id.0.clone(),
                action: SyncActionCode::I,
                data: Some(data),
            }])
            .await?;
            Fix {
                status: FixStatus::Suggested,
                patch: Some(patch),
                commit: None,
            }
        } else {
            // No anchor to hang a suggestion on; leave the patch recorded
            // but unapplied rather than silently dropping it.
            Fix {
                status: FixStatus::Ready,
                patch: Some(patch),
                commit: None,
            }
        };

        self.finish_task_accept(&task, Some(new_fix)).await
    }

    async fn finish_task_accept(&self, task: &Task, fix: Option<Fix>) -> AppResult<()> {
        let data = serde_json::json!({ "status": TaskStatus::Done, "fix": fix });
        let action = NewAction {
            model: models::TASK.into(),
            model_id: task.id.0.clone(),
            action: SyncActionCode::C,
            data: Some(data),
        };
        match &task.thread {
            // Mark the task Done and enqueue the thread resolve atomically
            // (plan "Fix target" accept): either both land, or neither
            // does — never a resolved thread on a task that's still open,
            // or a Done task whose thread was never told.
            Some(thread) => {
                let payload = crate::outbox::ResolvePayload {
                    pr: task.pr.clone(),
                    thread: thread.clone(),
                    resolved: true,
                };
                let payload_data =
                    serde_json::to_value(&payload).map_err(|e| AppError::Invalid(e.to_string()))?;
                self.apply_with_outbox(vec![action], "resolve_thread", payload_data)
                    .await?;
            }
            None => {
                self.apply(vec![action]).await?;
            }
        }
        Ok(())
    }

    async fn reject_task(&self, task: Task, reason: Option<String>) -> AppResult<()> {
        if let Some(reason) = &reason {
            let key = format!("task:{}:reject_reason", task.id);
            let store = self.0.deps.store.clone();
            let value = serde_json::json!(reason);
            let _ = tokio::task::spawn_blocking(move || store.kv_set(&key, &value)).await;
        }
        let data = serde_json::json!({ "status": TaskStatus::Todo, "fix": null });
        self.apply(vec![NewAction {
            model: models::TASK.into(),
            model_id: task.id.0.clone(),
            action: SyncActionCode::C,
            data: Some(data),
        }])
        .await?;

        if let Some(pr_ref) = self.pr_ref(&task.pr).await
            && let Some(head) = self.0.current_head.read().await.get(&task.pr).cloned()
        {
            // The worktree is always Theodore's own, detached checkout:
            // re-running `worktree()` on the same head hard-resets it.
            let _ = self.0.deps.git.worktree(&pr_ref, &head).await;
        }
        Ok(())
    }
}

async fn drive_fix_run(
    core: &Core,
    run_id: String,
    task: Task,
    agent: theodore_domain::AgentId,
    worktree: PathBuf,
    reject_reason: Option<String>,
    cancel: CancellationToken,
) {
    let (mcp, code) = core.pr_tools(&run_id, &task.pr, agent).await;
    let mut prompt = format!(
        "Fix this review task in the pull request.\n\nTitle: {}\n\n{}\n\n\
         Make the necessary code changes directly in this worktree. Do not commit or push.",
        task.title, task.body
    );
    if let Some(reason) = reject_reason {
        prompt.push_str(&format!("\n\nA previous attempt was rejected: {reason}"));
    }
    let req = AgentRequest {
        agent,
        prompt,
        // The repo's `review.instructions` / `review.context`.
        system: core
            .review_rules_of(&task.pr)
            .await
            .with_system(crate::usecases::code_intel::code_prompt(code).map(str::to_string)),
        cwd: worktree.clone(),
        mode: AgentMode::Edit,
        mcp: mcp.clone(),
        model: None,
        effort: None,
        stall_after_ms: core.stall_after_ms().await,
        env: Vec::new(),
        // Spaces only (issue #16): a fix stays on Theodore's tools.
        user_mcp: false,
        attachments: vec![],
        resume: None,
        stream_text: true,
    };
    let _guard = ToolsGuard { core, mcp };

    // What the agent says and does, live under the run id `delegate_task`
    // returned: the task row shows it while the fix runs.
    let mut live = LiveRun::new(run_id.as_str(), agent).with_thinking();
    let mut error: Option<String> = None;
    match core.0.deps.agents.run(req).await {
        Ok(mut run) => loop {
            tokio::select! {
                _ = cancel.cancelled() => { run.cancel.cancel(); break; }
                event = run.events.recv() => {
                    let Some(event) = event else { break };
                    live.show(core, &event);
                    match event {
                        AgentEvent::Done { .. } => break,
                        AgentEvent::Error { message, .. } => { error = Some(message); break; }
                        _ => {}
                    }
                }
            }
        },
        Err(e) => error = Some(e.to_string()),
    }
    core.0.runs.write().await.remove(&run_id);

    if cancel.is_cancelled() {
        let data = serde_json::json!({ "status": TaskStatus::Todo, "fix": null });
        let _ = core
            .apply(vec![NewAction {
                model: models::TASK.into(),
                model_id: task.id.0.clone(),
                action: SyncActionCode::C,
                data: Some(data),
            }])
            .await;
        crate::usecases::agents::persist_failed_message(core, &task.pr, agent, "cancelled").await;
        return;
    }

    if let Some(err) = error {
        let data = serde_json::json!({ "status": TaskStatus::Todo, "fix": null });
        let _ = core
            .apply(vec![NewAction {
                model: models::TASK.into(),
                model_id: task.id.0.clone(),
                action: SyncActionCode::C,
                data: Some(data),
            }])
            .await;
        core.emit(Ephemeral::Toast {
            level: ToastLevel::Error,
            text: format!("fix failed: {err}"),
        });
        // Persisted in addition to the toast (plan "failed agent runs"): a
        // toast disappears, but the task's chat history must still show,
        // after a restart, that this fix run crashed and let it be retried.
        crate::usecases::agents::persist_failed_message(core, &task.pr, agent, &err).await;
        return;
    }

    let patch = capture_patch(core, &task, &worktree, agent).await;
    let fix = Fix {
        status: FixStatus::Ready,
        patch,
        commit: None,
    };
    let data = serde_json::json!({ "status": TaskStatus::Review, "fix": fix });
    let _ = core
        .apply(vec![NewAction {
            model: models::TASK.into(),
            model_id: task.id.0.clone(),
            action: SyncActionCode::C,
            data: Some(data),
        }])
        .await;
}

/// Makes `worktree` hold `patch.after` for `patch.path`: already there →
/// nothing to do; still `patch.before` → write it; anything else → the
/// file moved on since the fix was made, and committing would clobber that.
pub(crate) async fn ensure_patch_applied(worktree: &Path, patch: &Patch) -> AppResult<()> {
    let rel = Path::new(&patch.path);
    if rel.is_absolute()
        || rel
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(AppError::Invalid(format!("unsafe fix path {}", patch.path)));
    }
    let file = worktree.join(rel);
    let current = tokio::fs::read(&file).await.unwrap_or_default();
    let text = String::from_utf8_lossy(&current);
    let lines: Vec<&str> = text.lines().collect();
    if lines == patch.after.iter().map(String::as_str).collect::<Vec<_>>() {
        return Ok(());
    }
    if lines != patch.before.iter().map(String::as_str).collect::<Vec<_>>() {
        return Err(AppError::Invalid(format!(
            "{} changed since the fix was made; re-run the task",
            patch.path
        )));
    }
    let mut out = patch.after.join("\n");
    if text.is_empty() || text.ends_with('\n') {
        out.push('\n');
    }
    if let Some(dir) = file.parent() {
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|e| AppError::Invalid(e.to_string()))?;
    }
    tokio::fs::write(&file, out)
        .await
        .map_err(|e| AppError::Invalid(e.to_string()))
}

/// Reads the task's anchor file before (from the cached revision's base
/// repo at the current head) and after (straight off disk in the
/// worktree, where the agent just edited it) and turns the difference
/// into a `Patch`. `None` if there's no anchor or nothing changed.
async fn capture_patch(
    core: &Core,
    task: &Task,
    worktree: &Path,
    agent: theodore_domain::AgentId,
) -> Option<Patch> {
    let anchor = task.anchor.as_ref()?;
    let (repo_path, head) = {
        let head = core.0.current_head.read().await.get(&task.pr).cloned()?;
        let revisions = core.0.revisions.read().await;
        let e = revisions.get(&(task.pr.clone(), head.clone()))?;
        (e.repo_path.clone(), head)
    };
    let before = core
        .0
        .deps
        .git
        .show(&repo_path, &head, &anchor.path)
        .await
        .ok()
        .flatten()?;
    let after = tokio::fs::read(worktree.join(&anchor.path)).await.ok()?;
    if before == after {
        return None;
    }
    Some(Patch {
        path: anchor.path.clone(),
        before: String::from_utf8_lossy(&before)
            .lines()
            .map(str::to_string)
            .collect(),
        after: String::from_utf8_lossy(&after)
            .lines()
            .map(str::to_string)
            .collect(),
        note: format!("agent fix: {}", task.title),
        by: agent,
    })
}

#[cfg(test)]
mod patch_tests {
    use super::*;

    fn patch(before: &[&str], after: &[&str]) -> Patch {
        Patch {
            path: "src/a.ts".into(),
            before: before.iter().map(|s| s.to_string()).collect(),
            after: after.iter().map(|s| s.to_string()).collect(),
            note: String::new(),
            by: theodore_domain::AgentId::Claude,
        }
    }

    #[tokio::test]
    async fn reapplies_a_lost_edit_and_leaves_an_applied_one_alone() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let f = dir.path().join("src/a.ts");
        std::fs::write(&f, "a\nb\n").unwrap();
        let p = patch(&["a", "b"], &["a", "B"]);
        ensure_patch_applied(dir.path(), &p).await.unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "a\nB\n");
        // Already applied: untouched.
        ensure_patch_applied(dir.path(), &p).await.unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "a\nB\n");
    }

    #[tokio::test]
    async fn refuses_a_file_that_moved_on_and_unsafe_paths() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.ts"), "x\n").unwrap();
        assert!(
            ensure_patch_applied(dir.path(), &patch(&["a"], &["b"]))
                .await
                .is_err()
        );
        let mut bad = patch(&[], &["b"]);
        bad.path = "../escape".into();
        assert!(ensure_patch_applied(dir.path(), &bad).await.is_err());
    }
}
