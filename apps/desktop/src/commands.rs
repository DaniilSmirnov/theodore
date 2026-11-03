//! Tauri commands (`.context/commands.md`). Each is a thin wrapper around
//! `State<Arc<dyn CoreApi>>`; argument names match the contract exactly so
//! the generated TS bindings line up with `ui/src/core/transport.ts`.

use std::sync::Arc;

use tauri::{AppHandle, State};
use theodore_contract::{
    AddProjectRequest, AgentModel, AgentStatus, AppInfo, AskRequest, BlobResponse, Bootstrap,
    BootstrapScope, CommandError, CommitSuggestionsRequest, ConflictFile, DelegateRequest,
    DeviceLogin, DraftStateRequest, EditCommentRequest, FileRequest, FindSymbolRequest,
    GetBlobRequest, HostAuthStatus, InterdiffEntry, InterdiffRequest, LinearIssue,
    LinearIssuesRequest, LinearStatus, LocalBranch, LoginState, MergeInfo, MergeRequest,
    OpenBranchRequest, OpenPrRequest, OpenPrResponse, Project, ReactRequest, ReplyRequest,
    RerunCheckRequest, ResolveRequest, ReviewersRequest, RevisionRequest, RunSkillRequest,
    SaveSkillRequest, SetLinearTokenRequest, SetTokenRequest, SkillBody, SkillInfo,
    SubmitReviewRequest, SymbolHit, SyncFrame, TaskDecision, Transaction, TransactionResult,
    TriageRequest, UpdateInfo,
};
use theodore_domain::{AgentId, HostKind, HostRepo, Person, PrId, Revision};

use crate::core_api::CoreApi;
use crate::updater;

#[tauri::command]
#[specta::specta]
pub async fn bootstrap(
    op: crate::op::Op,
    scope: BootstrapScope,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<Bootstrap, CommandError> {
    op.run(async move { core.bootstrap(scope).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn catch_up(
    op: crate::op::Op,
    since: u64,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<SyncFrame, CommandError> {
    op.run(async move { core.catch_up(since).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn transact(
    op: crate::op::Op,
    tx: Transaction,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<TransactionResult, CommandError> {
    op.run(async move { core.transact(tx).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn open_pr(
    op: crate::op::Op,
    req: OpenPrRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<OpenPrResponse, CommandError> {
    op.run(async move { core.open_pr(req).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn get_revision(
    op: crate::op::Op,
    req: RevisionRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<Revision, CommandError> {
    op.run(async move { core.get_revision(req).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn get_file(
    op: crate::op::Op,
    req: FileRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<Option<String>, CommandError> {
    op.run(async move { core.get_file(req).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn regroup(
    op: crate::op::Op,
    pr: PrId,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<(), CommandError> {
    op.run(async move { core.regroup(pr).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn run_skill(
    op: crate::op::Op,
    req: RunSkillRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<String, CommandError> {
    op.run(async move { core.run_skill(req).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn ask(
    op: crate::op::Op,
    req: AskRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<String, CommandError> {
    op.run(async move { core.ask(req).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn save_attachment(
    op: crate::op::Op,
    req: theodore_contract::SaveAttachmentRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::Attachment, CommandError> {
    op.run(async move { core.save_attachment(req).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn attachment_data(
    op: crate::op::Op,
    id: String,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<String, CommandError> {
    op.run(async move { core.attachment_data(id).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn cancel_run(
    op: crate::op::Op,
    run: String,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<(), CommandError> {
    op.run(async move { core.cancel_run(run).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn submit_review(
    op: crate::op::Op,
    req: SubmitReviewRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<(), CommandError> {
    op.run(async move { core.submit_review(req).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn detect_agents(
    op: crate::op::Op,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<Vec<AgentStatus>, CommandError> {
    op.run(async move { core.detect_agents().await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn agent_models(
    op: crate::op::Op,
    agent: AgentId,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<Vec<AgentModel>, CommandError> {
    op.run(async move { core.agent_models(agent).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn refresh_inbox(
    op: crate::op::Op,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<(), CommandError> {
    op.run(async move { core.refresh_inbox().await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn viewer(
    op: crate::op::Op,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<Person, CommandError> {
    op.run(async move { core.viewer().await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn host_status(
    op: crate::op::Op,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<HostAuthStatus, CommandError> {
    op.run(async move { core.host_status().await }).await
}

/// Connection state of a non-primary host (GitLab, Bitbucket).
#[tauri::command]
#[specta::specta]
pub async fn host_status_for(
    op: crate::op::Op,
    host: HostKind,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<HostAuthStatus, CommandError> {
    op.run(async move { core.host_status_for(host).await })
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn set_host_token(
    op: crate::op::Op,
    req: SetTokenRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<HostAuthStatus, CommandError> {
    op.run(async move { core.set_host_token(req).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn reply_thread(
    op: crate::op::Op,
    req: ReplyRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<(), CommandError> {
    op.run(async move { core.reply_thread(req).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn resolve_thread(
    op: crate::op::Op,
    req: ResolveRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<(), CommandError> {
    op.run(async move { core.resolve_thread(req).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn list_skills(
    op: crate::op::Op,
    pr: Option<PrId>,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<Vec<SkillInfo>, CommandError> {
    op.run(async move { core.list_skills(pr).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn delegate_task(
    op: crate::op::Op,
    req: DelegateRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<String, CommandError> {
    op.run(async move { core.delegate_task(req).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn decide_task(
    op: crate::op::Op,
    req: TaskDecision,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<(), CommandError> {
    op.run(async move { core.decide_task(req).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn list_host_repos(
    op: crate::op::Op,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<Vec<HostRepo>, CommandError> {
    op.run(async move { core.list_host_repos().await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn add_project(
    op: crate::op::Op,
    req: AddProjectRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<Project, CommandError> {
    op.run(async move { core.add_project(req).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn remove_project(
    op: crate::op::Op,
    id: String,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<(), CommandError> {
    op.run(async move { core.remove_project(id).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn refresh_project(
    op: crate::op::Op,
    id: String,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<(), CommandError> {
    op.run(async move { core.refresh_project(id).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn list_branches(
    op: crate::op::Op,
    project: String,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<Vec<LocalBranch>, CommandError> {
    op.run(async move { core.list_branches(project).await })
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn open_branch(
    op: crate::op::Op,
    req: OpenBranchRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<OpenPrResponse, CommandError> {
    op.run(async move { core.open_branch(req).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn merge_info(
    op: crate::op::Op,
    pr: PrId,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<MergeInfo, CommandError> {
    op.run(async move { core.merge_info(pr).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn merge_pr(
    op: crate::op::Op,
    req: MergeRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<(), CommandError> {
    op.run(async move { core.merge_pr(req).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn assignable_users(
    op: crate::op::Op,
    pr: PrId,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<Vec<Person>, CommandError> {
    op.run(async move { core.assignable_users(pr).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn set_reviewers(
    op: crate::op::Op,
    req: ReviewersRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<(), CommandError> {
    op.run(async move { core.set_reviewers(req).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn set_draft(
    op: crate::op::Op,
    req: DraftStateRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<(), CommandError> {
    op.run(async move { core.set_draft(req).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn update_branch(
    op: crate::op::Op,
    pr: PrId,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<(), CommandError> {
    op.run(async move { core.update_branch(pr).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn react(
    op: crate::op::Op,
    req: ReactRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<(), CommandError> {
    op.run(async move { core.react(req).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn edit_comment(
    op: crate::op::Op,
    req: EditCommentRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<(), CommandError> {
    op.run(async move { core.edit_comment(req).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn commit_suggestions(
    op: crate::op::Op,
    req: CommitSuggestionsRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<(), CommandError> {
    op.run(async move { core.commit_suggestions(req).await })
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn get_skill(
    op: crate::op::Op,
    id: String,
    pr: Option<PrId>,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<SkillBody, CommandError> {
    op.run(async move { core.get_skill(id, pr).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn save_skill(
    op: crate::op::Op,
    req: SaveSkillRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<SkillInfo, CommandError> {
    op.run(async move { core.save_skill(req).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn interdiff(
    op: crate::op::Op,
    req: InterdiffRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<Vec<InterdiffEntry>, CommandError> {
    op.run(async move { core.interdiff(req).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn rerun_check(
    op: crate::op::Op,
    req: RerunCheckRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<(), CommandError> {
    op.run(async move { core.rerun_check(req).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn login_start(
    op: crate::op::Op,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<DeviceLogin, CommandError> {
    op.run(async move { core.login_start().await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn login_poll(
    op: crate::op::Op,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<LoginState, CommandError> {
    op.run(async move { core.login_poll().await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn triage(
    op: crate::op::Op,
    req: TriageRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<(), CommandError> {
    op.run(async move { core.triage(req).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn set_linear_token(
    op: crate::op::Op,
    req: SetLinearTokenRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<LinearStatus, CommandError> {
    op.run(async move { core.set_linear_token(req).await })
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn linear_status(
    op: crate::op::Op,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<LinearStatus, CommandError> {
    op.run(async move { core.linear_status().await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn linear_issues(
    op: crate::op::Op,
    req: LinearIssuesRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<Vec<LinearIssue>, CommandError> {
    op.run(async move { core.linear_issues(req).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn find_symbol(
    op: crate::op::Op,
    req: FindSymbolRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<Vec<SymbolHit>, CommandError> {
    op.run(async move { core.find_symbol(req).await }).await
}

/// Native folder picker for the Add project dialog. Runs on a background
/// thread: the dialog plugin's blocking call would otherwise stall the
/// async runtime until the user picks a folder or cancels.
#[tauri::command]
#[specta::specta]
pub async fn pick_folder(
    op: crate::op::Op,
    app: AppHandle,
) -> Result<Option<String>, CommandError> {
    op.run(async move {
        use tauri_plugin_dialog::DialogExt;
        tokio::task::spawn_blocking(move || app.dialog().file().blocking_pick_folder())
            .await
            .map(|picked| picked.map(|p| p.to_string()))
            .map_err(|e| CommandError {
                code: "internal".into(),
                message: e.to_string(),
            })
    })
    .await
}

/// The core doesn't know about updates the shell already downloaded in the
/// background, so this fills `AppInfo.update` from `UpdateState` itself
/// rather than trusting whatever `core.app_info()` returns there.
#[tauri::command]
#[specta::specta]
pub async fn app_info(
    op: crate::op::Op,
    core: State<'_, Arc<dyn CoreApi>>,
    update_state: State<'_, updater::UpdateState>,
) -> Result<AppInfo, CommandError> {
    op.run(async move {
        let mut info = core.app_info().await?;
        if let Some(update) = update_state.get() {
            info.update = Some(update);
        }
        Ok(info)
    })
    .await
}

/// Runs an immediate update check (download + install on disk; no restart).
/// Used for the launch check, the 6 h poll and Settings → About "Check now".
#[tauri::command]
#[specta::specta]
pub async fn check_update(
    op: crate::op::Op,
    app: AppHandle,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<Option<UpdateInfo>, CommandError> {
    op.run(async move {
        let core: Arc<dyn CoreApi> = core.inner().clone();
        updater::check(&app, &core)
            .await
            .map_err(|message| CommandError {
                code: "update".into(),
                message,
            })
    })
    .await
}

/// Restarts into the already-downloaded update, but only once the core is
/// idle (no agent running, outbox empty) — never forced.
#[tauri::command]
#[specta::specta]
pub async fn install_update(
    op: crate::op::Op,
    app: AppHandle,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<(), CommandError> {
    op.run(async move {
        if core.is_idle().await {
            app.request_restart();
        }
        Ok(())
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn get_blob(
    op: crate::op::Op,
    req: GetBlobRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<BlobResponse, CommandError> {
    op.run(async move { core.get_blob(req).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn conflicts(
    op: crate::op::Op,
    pr: PrId,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<Vec<ConflictFile>, CommandError> {
    op.run(async move { core.conflicts(pr).await }).await
}

/// Settings → Sandbox (docs/sandbox.md): `null` outside sandbox mode.
#[tauri::command]
#[specta::specta]
pub async fn sandbox_status(
    op: crate::op::Op,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<Option<theodore_contract::SandboxStatus>, CommandError> {
    op.run(async move { core.sandbox_status().await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn sandbox_set_fault(
    op: crate::op::Op,
    req: theodore_contract::SandboxFaultRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::SandboxStatus, CommandError> {
    op.run(async move { core.sandbox_set_fault(req).await })
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn sandbox_fire_event(
    op: crate::op::Op,
    id: String,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::SandboxStatus, CommandError> {
    op.run(async move { core.sandbox_fire_event(id).await })
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn project_working_state(
    op: crate::op::Op,
    project: String,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::ProjectWorkingState, CommandError> {
    op.run(async move { core.project_working_state(project).await })
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn close_branch_review(
    op: crate::op::Op,
    pr: PrId,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<(), CommandError> {
    op.run(async move { core.close_branch_review(pr).await })
        .await
}

// ── spaces (docs/spaces.md) ─────────────────────────────────────────────────

#[tauri::command]
#[specta::specta]
pub async fn create_space(
    op: crate::op::Op,
    req: theodore_contract::CreateSpaceRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::Space, CommandError> {
    op.run(async move { core.create_space(req).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn project_issues(
    op: crate::op::Op,
    req: theodore_contract::ProjectIssuesRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<Vec<theodore_contract::SpaceIssue>, CommandError> {
    op.run(async move { core.project_issues(req).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn list_spaces(
    op: crate::op::Op,
    project: Option<String>,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<Vec<theodore_contract::Space>, CommandError> {
    op.run(async move { core.list_spaces(project).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn space_send(
    op: crate::op::Op,
    req: theodore_contract::SpaceSendRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<String, CommandError> {
    op.run(async move { core.space_send(req).await }).await
}

/// "Send now": into the running turn when it takes messages, else queued.
#[tauri::command]
#[specta::specta]
pub async fn space_send_now(
    op: crate::op::Op,
    req: theodore_contract::SpaceSendRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<String, CommandError> {
    op.run(async move { core.space_send_now(req).await }).await
}

/// Send / Cancel / Edit on a queued message.
#[tauri::command]
#[specta::specta]
pub async fn space_queued(
    op: crate::op::Op,
    req: theodore_contract::SpaceQueuedRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<(), CommandError> {
    op.run(async move { core.space_queued(req).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn space_stop(
    op: crate::op::Op,
    space: String,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<(), CommandError> {
    op.run(async move { core.space_stop(space).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn space_skills(
    op: crate::op::Op,
    space: String,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<Vec<SkillInfo>, CommandError> {
    op.run(async move { core.space_skills(space).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn space_review(
    op: crate::op::Op,
    req: theodore_contract::SpaceReviewRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<OpenPrResponse, CommandError> {
    op.run(async move { core.space_review(req).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn archive_space(
    op: crate::op::Op,
    space: String,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<(), CommandError> {
    op.run(async move { core.archive_space(space).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn ship_space(
    op: crate::op::Op,
    req: theodore_contract::ShipSpaceRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::Space, CommandError> {
    op.run(async move { core.ship_space(req).await }).await
}

/// Merge the space's PR from the space page (docs/spaces.md §3).
#[tauri::command]
#[specta::specta]
pub async fn merge_space(
    op: crate::op::Op,
    req: theodore_contract::MergeSpaceRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::MergeSpaceResult, CommandError> {
    op.run(async move { core.merge_space(req).await }).await
}

/// Continue after merge: a new branch on the base's fresh tip (§7).
#[tauri::command]
#[specta::specta]
pub async fn continue_space(
    op: crate::op::Op,
    space: String,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::Space, CommandError> {
    op.run(async move { core.continue_space(space).await })
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn undo_space_turn(
    op: crate::op::Op,
    space: String,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::Space, CommandError> {
    op.run(async move { core.undo_space_turn(space).await })
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn rewind_space(
    op: crate::op::Op,
    req: theodore_contract::RewindSpaceRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::Space, CommandError> {
    op.run(async move { core.rewind_space(req).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn revert_space_file(
    op: crate::op::Op,
    req: theodore_contract::RevertSpaceFileRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::Space, CommandError> {
    op.run(async move { core.revert_space_file(req).await })
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn unarchive_space(
    op: crate::op::Op,
    space: String,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::Space, CommandError> {
    op.run(async move { core.unarchive_space(space).await })
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn set_space_pinned(
    op: crate::op::Op,
    req: theodore_contract::SetSpacePinnedRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::Space, CommandError> {
    op.run(async move { core.set_space_pinned(req).await })
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn mirror_space(
    op: crate::op::Op,
    space: String,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::Space, CommandError> {
    op.run(async move { core.mirror_space(space).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn sync_mirror(
    op: crate::op::Op,
    space: String,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::Space, CommandError> {
    op.run(async move { core.sync_mirror(space).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn unmirror_space(
    op: crate::op::Op,
    req: theodore_contract::UnmirrorSpaceRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::Space, CommandError> {
    op.run(async move { core.unmirror_space(req).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn space_send_feedback(
    op: crate::op::Op,
    req: theodore_contract::SpaceFeedbackRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<String, CommandError> {
    op.run(async move { core.space_send_feedback(req).await })
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn space_base_status(
    op: crate::op::Op,
    space: String,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::SpaceBaseStatus, CommandError> {
    op.run(async move { core.space_base_status(space).await })
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn update_space_from_base(
    op: crate::op::Op,
    space: String,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::Space, CommandError> {
    op.run(async move { core.update_space_from_base(space).await })
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn abort_space_update(
    op: crate::op::Op,
    space: String,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::Space, CommandError> {
    op.run(async move { core.abort_space_update(space).await })
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn space_fix_ci(
    op: crate::op::Op,
    req: theodore_contract::SpaceFixCiRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<String, CommandError> {
    op.run(async move { core.space_fix_ci(req).await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn approve_space_plan(
    op: crate::op::Op,
    space: String,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<String, CommandError> {
    op.run(async move { core.approve_space_plan(space).await })
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn answer_space_question(
    op: crate::op::Op,
    req: theodore_contract::SpaceAnswerRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<(), CommandError> {
    op.run(async move { core.answer_space_question(req).await })
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn account_status(
    op: crate::op::Op,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::AccountStatus, CommandError> {
    op.run(async move { core.account_status().await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn account_sign_in_start(
    op: crate::op::Op,
    req: theodore_contract::AccountSignInRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::AccountSignInStart, CommandError> {
    op.run(async move { core.account_sign_in_start(req).await })
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn account_sign_in_poll(
    op: crate::op::Op,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::AccountSignInState, CommandError> {
    op.run(async move { core.account_sign_in_poll().await })
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn account_sign_in_cancel(
    op: crate::op::Op,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<(), CommandError> {
    op.run(async move { core.account_sign_in_cancel().await })
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn account_sign_out(
    op: crate::op::Op,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::AccountStatus, CommandError> {
    op.run(async move { core.account_sign_out().await }).await
}

/// Brings the main window forward: the browser sign-in finishes in another
/// app, and the user should land back in Theodore without hunting for it.
#[tauri::command]
#[specta::specta]
pub async fn focus_window(op: crate::op::Op, app: AppHandle) -> Result<(), CommandError> {
    op.run(async move {
        focus_main(&app).map_err(|e| CommandError {
            code: "internal".into(),
            message: e.to_string(),
        })
    })
    .await
}

/// Shows a space's "turn finished" notification while the window is in the
/// background; a click brings the window forward and emits `open-space`
/// with the space id. `true` when the click will be reported, `false` when
/// it can't be (the UI then opens the space on the next window focus).
#[tauri::command]
#[specta::specta]
pub async fn notify_space(
    op: crate::op::Op,
    space: String,
    title: String,
    body: Option<String>,
    app: AppHandle,
    notifier: State<'_, Arc<dyn crate::notify::Notifier>>,
) -> Result<bool, CommandError> {
    op.run(async move {
        use crate::notify::{MainWindow, Notice};
        Ok(crate::notify::notify_space(
            notifier.inner().as_ref(),
            MainWindow(app),
            space,
            Notice { title, body },
        ))
    })
    .await
}

/// Un-minimizes and focuses the main window (also after a sign-in redirect
/// reached the app through its URL scheme).
pub fn focus_main<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> tauri::Result<()> {
    use tauri::Manager;
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        w.set_focus()?;
    }
    Ok(())
}

// ── project config (docs/config.md) ─────────────────────────────────────────

/// What a space runs, where each command comes from, trust.
#[tauri::command]
#[specta::specta]
pub async fn space_config(
    op: crate::op::Op,
    space: String,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::SpaceConfigView, CommandError> {
    op.run(async move { core.space_config(space).await }).await
}

/// "Run commands from this repo?": Allow / Not now.
#[tauri::command]
#[specta::specta]
pub async fn space_trust(
    op: crate::op::Op,
    req: theodore_contract::SpaceTrustRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::Space, CommandError> {
    op.run(async move { core.space_trust(req).await }).await
}

/// Retry a failed setup, or continue without it.
#[tauri::command]
#[specta::specta]
pub async fn space_setup(
    op: crate::op::Op,
    req: theodore_contract::SpaceSetupRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::Space, CommandError> {
    op.run(async move { core.space_setup(req).await }).await
}

/// "Set up with agent": no `spaces:` commands yet — ask the space's agent
/// to look at the repo and write them (docs/config.md §2, §8).
#[tauri::command]
#[specta::specta]
pub async fn space_setup_assist(
    op: crate::op::Op,
    space: String,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::Space, CommandError> {
    op.run(async move { core.space_setup_assist(space).await })
        .await
}

/// "Fix with agent": setup failed — ask the agent to fix the cause.
#[tauri::command]
#[specta::specta]
pub async fn space_fix_setup(
    op: crate::op::Op,
    space: String,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::Space, CommandError> {
    op.run(async move { core.space_fix_setup(space).await })
        .await
}

/// Starts or stops the space's `run` command.
#[tauri::command]
#[specta::specta]
pub async fn space_run(
    op: crate::op::Op,
    req: theodore_contract::SpaceRunRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::Space, CommandError> {
    op.run(async move { core.space_run(req).await }).await
}

/// The last lines of the space's commands.
#[tauri::command]
#[specta::specta]
pub async fn space_logs(
    op: crate::op::Op,
    space: String,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<Vec<theodore_contract::SpaceLogLine>, CommandError> {
    op.run(async move { core.space_logs(space).await }).await
}

/// Starts a terminal in the space's worktree (docs/spaces.md "Terminal").
#[tauri::command]
#[specta::specta]
pub async fn open_space_terminal(
    op: crate::op::Op,
    req: theodore_contract::OpenTerminalRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::SpaceTerminalInfo, CommandError> {
    op.run(async move { core.open_space_terminal(req).await })
        .await
}

/// Bytes typed or pasted into it.
#[tauri::command]
#[specta::specta]
pub async fn write_space_terminal(
    op: crate::op::Op,
    req: theodore_contract::WriteTerminalRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<(), CommandError> {
    op.run(async move { core.write_space_terminal(req).await })
        .await
}

/// A new size in cells.
#[tauri::command]
#[specta::specta]
pub async fn resize_space_terminal(
    op: crate::op::Op,
    req: theodore_contract::ResizeTerminalRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<(), CommandError> {
    op.run(async move { core.resize_space_terminal(req).await })
        .await
}

/// Kills its shell.
#[tauri::command]
#[specta::specta]
pub async fn close_space_terminal(
    op: crate::op::Op,
    terminal: String,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<(), CommandError> {
    op.run(async move { core.close_space_terminal(terminal).await })
        .await
}

/// The space's open (or just-exited) terminals, with scrollback to seed
/// `xterm.js`.
#[tauri::command]
#[specta::specta]
pub async fn list_space_terminals(
    op: crate::op::Op,
    space: String,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<Vec<theodore_contract::SpaceTerminalInfo>, CommandError> {
    op.run(async move { core.list_space_terminals(space).await })
        .await
}

/// A project's merged config, with each value's file.
#[tauri::command]
#[specta::specta]
pub async fn project_config(
    op: crate::op::Op,
    project: String,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::EffectiveConfig, CommandError> {
    op.run(async move { core.project_config(project).await })
        .await
}

/// Project settings → Languages: each language of a project, its grammar, server
/// and index progress (docs/code-intel.md §9).
#[tauri::command]
#[specta::specta]
pub async fn code_intel_status(
    op: crate::op::Op,
    project: String,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::CodeIntelStatus, CommandError> {
    op.run(async move { core.code_intel_status(project).await })
        .await
}

/// Downloads a language's grammar and server (an install suggestion's
/// Yes, or project settings → Languages → Install).
#[tauri::command]
#[specta::specta]
pub async fn code_intel_install(
    op: crate::op::Op,
    language: String,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<(), CommandError> {
    op.run(async move { core.code_intel_install(language).await })
        .await
}

/// Project settings → Languages: one project's language servers in reviews (a PR's code may
/// run) or in spaces, on or off: written to its local config file.
#[tauri::command]
#[specta::specta]
pub async fn code_intel_set_lsp(
    op: crate::op::Op,
    req: theodore_contract::CodeIntelLspRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<(), CommandError> {
    op.run(async move { core.code_intel_set_lsp(req).await })
        .await
}

/// An install suggestion's No: not suggested again.
#[tauri::command]
#[specta::specta]
pub async fn code_intel_decline(
    op: crate::op::Op,
    language: String,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<(), CommandError> {
    op.run(async move { core.code_intel_decline(language).await })
        .await
}

/// Project settings → Languages → Remove: deletes what was downloaded for a
/// language (its server and grammar). Refused while a review or a space
/// with the language is open; afterwards it counts as declined.
#[tauri::command]
#[specta::specta]
pub async fn code_intel_remove(
    op: crate::op::Op,
    language: String,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<(), CommandError> {
    op.run(async move { core.code_intel_remove(language).await })
        .await
}

/// Hover in a PR's diff: the language server's text, else the signature
/// of a definition with the name under the pointer. `null`: nothing.
#[tauri::command]
#[specta::specta]
pub async fn code_intel_hover(
    op: crate::op::Op,
    req: theodore_contract::CodeHoverRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<Option<theodore_contract::CodeHover>, CommandError> {
    op.run(async move { core.code_intel_hover(req).await })
        .await
}

/// Settings → Projects → Config: each layer's file and YAML generated from
/// the project's settings.
#[tauri::command]
#[specta::specta]
pub async fn config_draft(
    op: crate::op::Op,
    project: String,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::ConfigDraft, CommandError> {
    op.run(async move { core.config_draft(project).await })
        .await
}

/// The config editor's text checked (problems with lines) and the
/// project's effective config with it in place.
#[tauri::command]
#[specta::specta]
pub async fn check_config(
    op: crate::op::Op,
    req: theodore_contract::CheckConfigRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::ConfigCheck, CommandError> {
    op.run(async move { core.check_config(req).await }).await
}

/// Saves the config editor's text to the repo (a host repo through a PR),
/// local or global layer.
#[tauri::command]
#[specta::specta]
pub async fn save_config(
    op: crate::op::Op,
    req: theodore_contract::SaveConfigRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::SaveConfigResult, CommandError> {
    op.run(async move { core.save_config(req).await }).await
}

/// `~/.theodore/config.yaml`: path and whether it exists.
#[tauri::command]
#[specta::specta]
pub async fn global_config(
    op: crate::op::Op,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::GlobalConfigInfo, CommandError> {
    op.run(async move { core.global_config().await }).await
}

/// Creates `~/.theodore/config.yaml` from a commented template when
/// missing, then opens it with the default editor.
#[tauri::command]
#[specta::specta]
pub async fn open_global_config(
    op: crate::op::Op,
    app: AppHandle,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::GlobalConfigInfo, CommandError> {
    op.run(async move {
        use tauri_plugin_opener::OpenerExt;
        let info = core.ensure_global_config().await?;
        app.opener()
            .open_path(info.path.clone(), None::<&str>)
            .map_err(|e| CommandError {
                code: "internal".into(),
                message: e.to_string(),
            })?;
        Ok(info)
    })
    .await
}

/// A UI error or event for the core's log (docs/telemetry.md). Never
/// fails: a dropped log line is not worth an error toast.
#[tauri::command]
#[specta::specta]
pub async fn log_client(
    op: crate::op::Op,
    entry: theodore_contract::ClientLog,
) -> Result<(), CommandError> {
    op.run(async move {
        theodore_telemetry::client_log(&entry);
        Ok(())
    })
    .await
}

/// A product event from the UI (docs/telemetry.md). Unknown values are
/// dropped by the core; never fails.
#[tauri::command]
#[specta::specta]
pub async fn track_ui(
    op: crate::op::Op,
    event: theodore_contract::UiEvent,
) -> Result<(), CommandError> {
    op.run(async move {
        theodore_telemetry::track_ui(&event);
        Ok(())
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn telemetry_status(
    op: crate::op::Op,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::TelemetryStatus, CommandError> {
    op.run(async move { core.telemetry_status().await }).await
}

#[tauri::command]
#[specta::specta]
pub async fn set_telemetry(
    op: crate::op::Op,
    req: theodore_contract::TelemetryChoice,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::TelemetryStatus, CommandError> {
    op.run(async move { core.set_telemetry(req).await }).await
}

/// Settings → About & updates, Help → Report a Problem…, the error screen.
#[tauri::command]
#[specta::specta]
pub async fn report_problem(
    op: crate::op::Op,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::ProblemReport, CommandError> {
    op.run(async move { core.report_problem().await }).await
}

/// "Report a problem" as a GitHub issue: the repo, the login, or why not.
#[tauri::command]
#[specta::specta]
pub async fn issue_target(
    op: crate::op::Op,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::IssueTarget, CommandError> {
    op.run(async move { core.issue_target().await }).await
}

/// The exact issue body `submit_issue` would post.
#[tauri::command]
#[specta::specta]
pub async fn preview_issue(
    op: crate::op::Op,
    req: theodore_contract::IssueRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::IssuePreview, CommandError> {
    op.run(async move { core.preview_issue(req).await }).await
}

/// Posts the report as a public issue with the user's GitHub token.
#[tauri::command]
#[specta::specta]
pub async fn submit_issue(
    op: crate::op::Op,
    req: theodore_contract::IssueRequest,
    core: State<'_, Arc<dyn CoreApi>>,
) -> Result<theodore_contract::ReportedIssue, CommandError> {
    op.run(async move { core.submit_issue(req).await }).await
}

/// Shows a problem report in Finder. Only paths inside the reports folder.
#[tauri::command]
#[specta::specta]
pub async fn reveal_report(
    op: crate::op::Op,
    path: String,
    app: AppHandle,
) -> Result<(), CommandError> {
    op.run(async move {
        use tauri_plugin_opener::OpenerExt;
        let path = std::path::PathBuf::from(path);
        if !theodore_telemetry::report::is_report_path(&path) {
            return Err(CommandError {
                code: "invalid".into(),
                message: "not a problem report".into(),
            });
        }
        app.opener()
            .reveal_item_in_dir(&path)
            .map_err(|e| CommandError {
                code: "internal".into(),
                message: e.to_string(),
            })
    })
    .await
}
