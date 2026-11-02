//! `CoreApi`: one async method per command in `.context/commands.md`. Tauri
//! commands (`commands.rs`) delegate to `State<Arc<dyn CoreApi>>`; the real
//! implementation (`theodore-app`) is wired in by the integration slice.

use async_trait::async_trait;
use theodore_contract::{
    AccountSignInRequest, AccountSignInStart, AccountSignInState, AccountStatus,
};
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
use theodore_contract::{
    OpenTerminalRequest, ResizeTerminalRequest, SpaceTerminalInfo, WriteTerminalRequest,
};
use theodore_contract::{SandboxFaultRequest, SandboxStatus};
use theodore_domain::{AgentId, HostKind, HostRepo, Person, PrId, Revision};

fn not_ready() -> CommandError {
    CommandError {
        code: "not_ready".into(),
        message: "the core is not wired up yet".into(),
    }
}

/// One method per command. Implemented by `theodore-app` in the integration
/// slice; `NotReadyCoreApi` below stands in until then.
#[async_trait]
pub trait CoreApi: Send + Sync {
    async fn bootstrap(&self, scope: BootstrapScope) -> Result<Bootstrap, CommandError>;
    async fn catch_up(&self, since: u64) -> Result<SyncFrame, CommandError>;
    async fn transact(&self, tx: Transaction) -> Result<TransactionResult, CommandError>;
    async fn open_pr(&self, req: OpenPrRequest) -> Result<OpenPrResponse, CommandError>;
    async fn get_revision(&self, req: RevisionRequest) -> Result<Revision, CommandError>;
    async fn get_file(&self, req: FileRequest) -> Result<Option<String>, CommandError>;
    async fn regroup(&self, pr: PrId) -> Result<(), CommandError>;
    async fn run_skill(&self, req: RunSkillRequest) -> Result<String, CommandError>;
    async fn ask(&self, req: AskRequest) -> Result<String, CommandError>;
    /// Saves a file sent with a chat message; its id goes in
    /// `SpaceSendRequest.attachments` / `AskRequest.attachments`.
    async fn save_attachment(
        &self,
        req: theodore_contract::SaveAttachmentRequest,
    ) -> Result<theodore_contract::Attachment, CommandError>;
    /// A saved attachment as a `data:<mime>;base64,…` URL.
    async fn attachment_data(&self, id: String) -> Result<String, CommandError>;
    async fn cancel_run(&self, run: String) -> Result<(), CommandError>;
    async fn submit_review(&self, req: SubmitReviewRequest) -> Result<(), CommandError>;
    async fn detect_agents(&self) -> Result<Vec<AgentStatus>, CommandError>;
    /// The models `agent` can run, for the model picker; `[]` when it
    /// can't list them, never an error.
    async fn agent_models(&self, agent: AgentId) -> Result<Vec<AgentModel>, CommandError>;
    async fn app_info(&self) -> Result<AppInfo, CommandError>;
    async fn check_update(&self) -> Result<Option<UpdateInfo>, CommandError>;
    async fn install_update(&self) -> Result<(), CommandError>;
    async fn refresh_inbox(&self) -> Result<(), CommandError>;
    async fn viewer(&self) -> Result<Person, CommandError>;
    async fn host_status(&self) -> Result<HostAuthStatus, CommandError>;
    async fn host_status_for(&self, host: HostKind) -> Result<HostAuthStatus, CommandError>;
    async fn set_host_token(&self, req: SetTokenRequest) -> Result<HostAuthStatus, CommandError>;
    async fn reply_thread(&self, req: ReplyRequest) -> Result<(), CommandError>;
    async fn resolve_thread(&self, req: ResolveRequest) -> Result<(), CommandError>;
    async fn list_skills(&self, pr: Option<PrId>) -> Result<Vec<SkillInfo>, CommandError>;
    async fn delegate_task(&self, req: DelegateRequest) -> Result<String, CommandError>;
    async fn decide_task(&self, req: TaskDecision) -> Result<(), CommandError>;
    async fn list_host_repos(&self) -> Result<Vec<HostRepo>, CommandError>;
    async fn add_project(&self, req: AddProjectRequest) -> Result<Project, CommandError>;
    async fn remove_project(&self, id: String) -> Result<(), CommandError>;
    async fn refresh_project(&self, id: String) -> Result<(), CommandError>;
    async fn list_branches(&self, project: String) -> Result<Vec<LocalBranch>, CommandError>;
    async fn open_branch(&self, req: OpenBranchRequest) -> Result<OpenPrResponse, CommandError>;
    async fn merge_info(&self, pr: PrId) -> Result<MergeInfo, CommandError>;
    async fn merge_pr(&self, req: MergeRequest) -> Result<(), CommandError>;
    async fn assignable_users(&self, pr: PrId) -> Result<Vec<Person>, CommandError>;
    async fn set_reviewers(&self, req: ReviewersRequest) -> Result<(), CommandError>;
    async fn set_draft(&self, req: DraftStateRequest) -> Result<(), CommandError>;
    async fn update_branch(&self, pr: PrId) -> Result<(), CommandError>;
    async fn react(&self, req: ReactRequest) -> Result<(), CommandError>;
    async fn edit_comment(&self, req: EditCommentRequest) -> Result<(), CommandError>;
    async fn commit_suggestions(&self, req: CommitSuggestionsRequest) -> Result<(), CommandError>;
    async fn get_skill(&self, id: String, pr: Option<PrId>) -> Result<SkillBody, CommandError>;
    async fn save_skill(&self, req: SaveSkillRequest) -> Result<SkillInfo, CommandError>;
    async fn interdiff(&self, req: InterdiffRequest) -> Result<Vec<InterdiffEntry>, CommandError>;
    /// Starts a GitHub OAuth device-flow login (plan "login").
    async fn login_start(&self) -> Result<DeviceLogin, CommandError>;
    /// Polls once for that login's outcome.
    async fn login_poll(&self) -> Result<LoginState, CommandError>;
    async fn rerun_check(&self, req: RerunCheckRequest) -> Result<(), CommandError>;
    async fn triage(&self, req: TriageRequest) -> Result<(), CommandError>;
    /// Validates the token via Linear's `viewer` query, then stores it
    /// (`token: None` disconnects).
    async fn set_linear_token(
        &self,
        req: SetLinearTokenRequest,
    ) -> Result<LinearStatus, CommandError>;
    async fn linear_status(&self) -> Result<LinearStatus, CommandError>;
    /// Issues linked to a PR by a key found in its title, head branch or body.
    async fn linear_issues(
        &self,
        req: LinearIssuesRequest,
    ) -> Result<Vec<LinearIssue>, CommandError>;
    /// Go-to-definition / find-references for a ⌘-clicked identifier
    /// (plan "symbols").
    async fn find_symbol(&self, req: FindSymbolRequest) -> Result<Vec<SymbolHit>, CommandError>;
    /// Raw bytes of a changed file at `base`/`head`, base64'd (plan "image diff").
    async fn get_blob(&self, req: GetBlobRequest) -> Result<BlobResponse, CommandError>;
    /// Locally-detected conflicting paths (plan "conflicts"); `Unsupported`
    /// on a `git` older than 2.38 — the UI hides the list on that code.
    async fn conflicts(&self, pr: PrId) -> Result<Vec<ConflictFile>, CommandError>;
    /// A local folder project's checkout: current branch, uncommitted paths.
    async fn project_working_state(
        &self,
        project: String,
    ) -> Result<theodore_contract::ProjectWorkingState, CommandError>;
    /// Closes a branch review: its local rows and worktree. Rejects a host PR.
    async fn close_branch_review(&self, pr: PrId) -> Result<(), CommandError>;

    /// The channel the updater polls: `updates.channel` (`"stable"` or
    /// `"beta"`), always `"staging"` in a staging build
    /// (`updater::effective_channel`). Not a wire command; the shell calls
    /// it directly.
    async fn update_channel(&self) -> String {
        "stable".into()
    }

    /// The account gate (plan §6.12 "Sign-in required"): `Err` with code
    /// `account_required` while signed out where signing in is required.
    /// Not a wire command; `gate::guard` asks it before every command off
    /// `gate::ALLOWED_SIGNED_OUT`. Open by default.
    async fn account_gate(&self) -> Result<(), CommandError> {
        Ok(())
    }

    /// Gates `install_update`'s restart: no agent running, outbox empty.
    async fn is_idle(&self) -> bool {
        true
    }

    /// `false` in sandbox mode: no update checks (no network at all).
    async fn updates_enabled(&self) -> bool {
        true
    }

    /// Stops every space's running `run` command and waits briefly for
    /// each. Not a wire command; the shell calls it on a clean app exit
    /// (`RunEvent::Exit`) so quitting normally never orphans a space's
    /// server the way an unclean exit can (docs/spaces.md "Crash recovery").
    async fn stop_all_space_commands(&self) {}

    /// The two sharing choices and what each would send (docs/telemetry.md).
    async fn telemetry_status(&self) -> Result<theodore_contract::TelemetryStatus, CommandError> {
        Err(not_ready())
    }
    /// Records the user's sharing choices and applies them to the sinks.
    async fn set_telemetry(
        &self,
        _req: theodore_contract::TelemetryChoice,
    ) -> Result<theodore_contract::TelemetryStatus, CommandError> {
        Err(not_ready())
    }
    /// Builds a problem report (redacted logs + app info) on disk.
    async fn report_problem(&self) -> Result<theodore_contract::ProblemReport, CommandError> {
        Err(not_ready())
    }
    /// Where a problem report issue would go, as whom, or why it can't.
    async fn issue_target(&self) -> Result<theodore_contract::IssueTarget, CommandError> {
        Err(not_ready())
    }
    /// Exactly the issue `submit_issue` would post for `req`.
    async fn preview_issue(
        &self,
        _req: theodore_contract::IssueRequest,
    ) -> Result<theodore_contract::IssuePreview, CommandError> {
        Err(not_ready())
    }
    /// Posts the problem report as a public GitHub issue.
    async fn submit_issue(
        &self,
        _req: theodore_contract::IssueRequest,
    ) -> Result<theodore_contract::ReportedIssue, CommandError> {
        Err(not_ready())
    }

    /// Settings → Sandbox (docs/sandbox.md); `None` outside sandbox mode.
    async fn sandbox_status(&self) -> Result<Option<SandboxStatus>, CommandError> {
        Ok(None)
    }
    /// Turns one sandbox fault on or off.
    async fn sandbox_set_fault(
        &self,
        _req: SandboxFaultRequest,
    ) -> Result<SandboxStatus, CommandError> {
        Err(not_sandbox())
    }
    /// Fires one scripted sandbox timeline event now.
    async fn sandbox_fire_event(&self, _id: String) -> Result<SandboxStatus, CommandError> {
        Err(not_sandbox())
    }

    // ── spaces (docs/spaces.md) ──────────────────────────────────────────

    /// Worktree + checkpoint ref at the base; sends `goal` as turn 1.
    async fn create_space(
        &self,
        _req: theodore_contract::CreateSpaceRequest,
    ) -> Result<theodore_contract::Space, CommandError> {
        Err(not_ready())
    }
    /// New space's "From an issue" picker: the project's own open issues
    /// assigned to the viewer (`Capabilities::issues`; empty when the host
    /// doesn't support it).
    async fn project_issues(
        &self,
        _req: theodore_contract::ProjectIssuesRequest,
    ) -> Result<Vec<theodore_contract::SpaceIssue>, CommandError> {
        Err(not_ready())
    }
    /// Spaces of one project, or all.
    async fn list_spaces(
        &self,
        _project: Option<String>,
    ) -> Result<Vec<theodore_contract::Space>, CommandError> {
        Err(not_ready())
    }
    /// Starts one agent turn; returns the stored message id.
    async fn space_send(
        &self,
        _req: theodore_contract::SpaceSendRequest,
    ) -> Result<String, CommandError> {
        Err(not_ready())
    }
    /// "Send now": into the running turn, or queued like `space_send`.
    async fn space_send_now(
        &self,
        _req: theodore_contract::SpaceSendRequest,
    ) -> Result<String, CommandError> {
        Err(not_ready())
    }
    /// Send / Cancel / Edit on a queued message.
    async fn space_queued(
        &self,
        _req: theodore_contract::SpaceQueuedRequest,
    ) -> Result<(), CommandError> {
        Err(not_ready())
    }
    async fn space_stop(&self, _space: String) -> Result<(), CommandError> {
        Err(not_ready())
    }
    /// Skills a space's chat runs with `/<id>`: yours and the worktree's.
    async fn space_skills(&self, _space: String) -> Result<Vec<SkillInfo>, CommandError> {
        Err(not_ready())
    }
    /// Opens (or refreshes) the space's branch review.
    async fn space_review(
        &self,
        _req: theodore_contract::SpaceReviewRequest,
    ) -> Result<OpenPrResponse, CommandError> {
        Err(not_ready())
    }
    async fn archive_space(&self, _space: String) -> Result<(), CommandError> {
        Err(not_ready())
    }
    /// Open PR / Push / Merge locally / Keep as branch.
    async fn ship_space(
        &self,
        _req: theodore_contract::ShipSpaceRequest,
    ) -> Result<theodore_contract::Space, CommandError> {
        Err(not_ready())
    }
    /// Merges the space's PR: default method, auto-merge when blocked.
    async fn merge_space(
        &self,
        _req: theodore_contract::MergeSpaceRequest,
    ) -> Result<theodore_contract::MergeSpaceResult, CommandError> {
        Err(not_ready())
    }
    /// Continue after merge: a new branch on the base's fresh tip.
    async fn continue_space(
        &self,
        _space: String,
    ) -> Result<theodore_contract::Space, CommandError> {
        Err(not_ready())
    }
    /// Undo last turn: back to the previous checkpoint (docs/spaces.md §7).
    async fn undo_space_turn(
        &self,
        _space: String,
    ) -> Result<theodore_contract::Space, CommandError> {
        Err(not_ready())
    }
    /// Recreates an archived space's worktree from its checkpoint ref.
    async fn unarchive_space(
        &self,
        _space: String,
    ) -> Result<theodore_contract::Space, CommandError> {
        Err(not_ready())
    }
    /// Pin/unpin a space: pinned ones sort first (docs/spaces.md §3).
    async fn set_space_pinned(
        &self,
        _req: theodore_contract::SetSpacePinnedRequest,
    ) -> Result<theodore_contract::Space, CommandError> {
        Err(not_ready())
    }
    /// "Send to agent": review threads / drafts / findings as the next turn.
    async fn space_send_feedback(
        &self,
        _req: theodore_contract::SpaceFeedbackRequest,
    ) -> Result<String, CommandError> {
        Err(not_ready())
    }
    /// Commits the space's base has that it doesn't (no network).
    async fn space_base_status(
        &self,
        _space: String,
    ) -> Result<theodore_contract::SpaceBaseStatus, CommandError> {
        Err(not_ready())
    }
    /// Update from base: merge the base's tip into the space (or complete
    /// the merge being resolved).
    async fn update_space_from_base(
        &self,
        _space: String,
    ) -> Result<theodore_contract::Space, CommandError> {
        Err(not_ready())
    }
    /// Abort the update from base being resolved.
    async fn abort_space_update(
        &self,
        _space: String,
    ) -> Result<theodore_contract::Space, CommandError> {
        Err(not_ready())
    }
    /// "Fix with agent": the space PR's failing checks' logs as the next
    /// turn (docs/spaces.md "Fix CI").
    async fn space_fix_ci(
        &self,
        _req: theodore_contract::SpaceFixCiRequest,
    ) -> Result<String, CommandError> {
        Err(not_ready())
    }
    // ── project config (docs/config.md) ──────────────────────────────────

    /// What a space runs, where each command comes from, trust.
    async fn space_config(
        &self,
        _space: String,
    ) -> Result<theodore_contract::SpaceConfigView, CommandError> {
        Err(not_ready())
    }
    /// "Run commands from this repo?": Allow / Not now.
    async fn space_trust(
        &self,
        _req: theodore_contract::SpaceTrustRequest,
    ) -> Result<theodore_contract::Space, CommandError> {
        Err(not_ready())
    }
    /// Retry a failed setup, or continue without it.
    async fn space_setup(
        &self,
        _req: theodore_contract::SpaceSetupRequest,
    ) -> Result<theodore_contract::Space, CommandError> {
        Err(not_ready())
    }
    /// "Set up with agent": no `spaces:` commands yet — ask the space's
    /// agent to look at the repo and write them.
    async fn space_setup_assist(
        &self,
        _space: String,
    ) -> Result<theodore_contract::Space, CommandError> {
        Err(not_ready())
    }
    /// "Fix with agent": setup failed — ask the agent to fix the cause.
    async fn space_fix_setup(
        &self,
        _space: String,
    ) -> Result<theodore_contract::Space, CommandError> {
        Err(not_ready())
    }
    /// Starts or stops the space's `run` command.
    async fn space_run(
        &self,
        _req: theodore_contract::SpaceRunRequest,
    ) -> Result<theodore_contract::Space, CommandError> {
        Err(not_ready())
    }
    /// The last lines of the space's commands.
    async fn space_logs(
        &self,
        _space: String,
    ) -> Result<Vec<theodore_contract::SpaceLogLine>, CommandError> {
        Err(not_ready())
    }
    /// Starts a terminal in the space's worktree (docs/spaces.md "Terminal").
    async fn open_space_terminal(
        &self,
        _req: OpenTerminalRequest,
    ) -> Result<SpaceTerminalInfo, CommandError> {
        Err(not_ready())
    }
    /// Bytes typed or pasted into it.
    async fn write_space_terminal(&self, _req: WriteTerminalRequest) -> Result<(), CommandError> {
        Err(not_ready())
    }
    /// A new size in cells.
    async fn resize_space_terminal(&self, _req: ResizeTerminalRequest) -> Result<(), CommandError> {
        Err(not_ready())
    }
    /// Kills its shell.
    async fn close_space_terminal(&self, _terminal: String) -> Result<(), CommandError> {
        Err(not_ready())
    }
    /// The space's open (or just-exited) terminals, with scrollback to seed
    /// `xterm.js`.
    async fn list_space_terminals(
        &self,
        _space: String,
    ) -> Result<Vec<SpaceTerminalInfo>, CommandError> {
        Err(not_ready())
    }
    /// A project's merged config, with each value's file.
    async fn project_config(
        &self,
        _project: String,
    ) -> Result<theodore_contract::EffectiveConfig, CommandError> {
        Err(not_ready())
    }
    /// `~/.theodore/config.yaml`: path and whether it exists.
    async fn global_config(&self) -> Result<theodore_contract::GlobalConfigInfo, CommandError> {
        Err(not_ready())
    }
    /// Settings → Languages: each language of a project (docs/code-intel.md §9).
    async fn code_intel_status(
        &self,
        _project: String,
    ) -> Result<theodore_contract::CodeIntelStatus, CommandError> {
        Err(not_ready())
    }
    /// Downloads a language's grammar and server.
    async fn code_intel_install(&self, _language: String) -> Result<(), CommandError> {
        Err(not_ready())
    }
    /// Settings → Languages: language servers in reviews / in spaces.
    async fn code_intel_set_lsp(
        &self,
        _req: theodore_contract::CodeIntelLspRequest,
    ) -> Result<(), CommandError> {
        Err(not_ready())
    }
    /// No to an install suggestion; remembered.
    async fn code_intel_decline(&self, _language: String) -> Result<(), CommandError> {
        Err(not_ready())
    }
    /// Same, created from a commented template when missing.
    async fn ensure_global_config(
        &self,
    ) -> Result<theodore_contract::GlobalConfigInfo, CommandError> {
        Err(not_ready())
    }
    /// Settings → Projects → Config: each layer's file and YAML generated
    /// from the project's settings.
    async fn config_draft(
        &self,
        _project: String,
    ) -> Result<theodore_contract::ConfigDraft, CommandError> {
        Err(not_ready())
    }
    /// The editor's text checked (problems with lines) and previewed.
    async fn check_config(
        &self,
        _req: theodore_contract::CheckConfigRequest,
    ) -> Result<theodore_contract::ConfigCheck, CommandError> {
        Err(not_ready())
    }
    /// Saves the editor's text to a layer (a host repo through a PR).
    async fn save_config(
        &self,
        _req: theodore_contract::SaveConfigRequest,
    ) -> Result<theodore_contract::SaveConfigResult, CommandError> {
        Err(not_ready())
    }
    /// Theodore account (plan "Theodore account"): signed in or out, and
    /// whether the feature is configured at all.
    async fn account_status(&self) -> Result<AccountStatus, CommandError>;
    /// Starts a browser (PKCE; loopback or app URL scheme redirect) or
    /// device-code sign-in.
    async fn account_sign_in_start(
        &self,
        req: AccountSignInRequest,
    ) -> Result<AccountSignInStart, CommandError>;
    /// The outcome of the sign-in in progress (one device-code poll).
    async fn account_sign_in_poll(&self) -> Result<AccountSignInState, CommandError>;
    /// Drops the sign-in in progress (frees the loopback port).
    async fn account_sign_in_cancel(&self) -> Result<(), CommandError>;
    async fn account_sign_out(&self) -> Result<AccountStatus, CommandError>;
    /// A `theodore://callback?…` sign-in redirect the OS opened the app
    /// with. Not a command: the shell's URL-scheme handler calls it. `true`
    /// when it belonged to the sign-in in progress (signed in or failed),
    /// `false` when ignored.
    async fn account_sign_in_callback(&self, _url: String) -> bool {
        false
    }
}

fn not_sandbox() -> CommandError {
    CommandError {
        code: "invalid".into(),
        message: "not running in sandbox mode".into(),
    }
}

/// Placeholder `CoreApi` mounted before `theodore-app` is wired in. Every
/// command rejects with `{code: "not_ready"}`.
pub struct NotReadyCoreApi;

#[async_trait]
impl CoreApi for NotReadyCoreApi {
    async fn bootstrap(&self, _scope: BootstrapScope) -> Result<Bootstrap, CommandError> {
        Err(not_ready())
    }
    async fn catch_up(&self, _since: u64) -> Result<SyncFrame, CommandError> {
        Err(not_ready())
    }
    async fn transact(&self, _tx: Transaction) -> Result<TransactionResult, CommandError> {
        Err(not_ready())
    }
    async fn open_pr(&self, _req: OpenPrRequest) -> Result<OpenPrResponse, CommandError> {
        Err(not_ready())
    }
    async fn get_revision(&self, _req: RevisionRequest) -> Result<Revision, CommandError> {
        Err(not_ready())
    }
    async fn get_file(&self, _req: FileRequest) -> Result<Option<String>, CommandError> {
        Err(not_ready())
    }
    async fn regroup(&self, _pr: PrId) -> Result<(), CommandError> {
        Err(not_ready())
    }
    async fn run_skill(&self, _req: RunSkillRequest) -> Result<String, CommandError> {
        Err(not_ready())
    }
    async fn ask(&self, _req: AskRequest) -> Result<String, CommandError> {
        Err(not_ready())
    }
    async fn save_attachment(
        &self,
        _req: theodore_contract::SaveAttachmentRequest,
    ) -> Result<theodore_contract::Attachment, CommandError> {
        Err(not_ready())
    }
    async fn attachment_data(&self, _id: String) -> Result<String, CommandError> {
        Err(not_ready())
    }
    async fn cancel_run(&self, _run: String) -> Result<(), CommandError> {
        Err(not_ready())
    }
    async fn submit_review(&self, _req: SubmitReviewRequest) -> Result<(), CommandError> {
        Err(not_ready())
    }
    async fn detect_agents(&self) -> Result<Vec<AgentStatus>, CommandError> {
        Err(not_ready())
    }
    async fn agent_models(&self, _agent: AgentId) -> Result<Vec<AgentModel>, CommandError> {
        Ok(Vec::new())
    }
    async fn app_info(&self) -> Result<AppInfo, CommandError> {
        Err(not_ready())
    }
    async fn check_update(&self) -> Result<Option<UpdateInfo>, CommandError> {
        Err(not_ready())
    }
    async fn install_update(&self) -> Result<(), CommandError> {
        Err(not_ready())
    }
    async fn refresh_inbox(&self) -> Result<(), CommandError> {
        Err(not_ready())
    }
    async fn viewer(&self) -> Result<Person, CommandError> {
        Err(not_ready())
    }
    async fn host_status(&self) -> Result<HostAuthStatus, CommandError> {
        Err(not_ready())
    }
    async fn host_status_for(&self, _host: HostKind) -> Result<HostAuthStatus, CommandError> {
        Err(not_ready())
    }
    async fn set_host_token(&self, _req: SetTokenRequest) -> Result<HostAuthStatus, CommandError> {
        Err(not_ready())
    }
    async fn reply_thread(&self, _req: ReplyRequest) -> Result<(), CommandError> {
        Err(not_ready())
    }
    async fn resolve_thread(&self, _req: ResolveRequest) -> Result<(), CommandError> {
        Err(not_ready())
    }
    async fn list_skills(&self, _pr: Option<PrId>) -> Result<Vec<SkillInfo>, CommandError> {
        Err(not_ready())
    }
    async fn delegate_task(&self, _req: DelegateRequest) -> Result<String, CommandError> {
        Err(not_ready())
    }
    async fn decide_task(&self, _req: TaskDecision) -> Result<(), CommandError> {
        Err(not_ready())
    }
    async fn list_host_repos(&self) -> Result<Vec<HostRepo>, CommandError> {
        Err(not_ready())
    }
    async fn add_project(&self, _req: AddProjectRequest) -> Result<Project, CommandError> {
        Err(not_ready())
    }
    async fn remove_project(&self, _id: String) -> Result<(), CommandError> {
        Err(not_ready())
    }
    async fn refresh_project(&self, _id: String) -> Result<(), CommandError> {
        Err(not_ready())
    }
    async fn list_branches(&self, _project: String) -> Result<Vec<LocalBranch>, CommandError> {
        Err(not_ready())
    }
    async fn open_branch(&self, _req: OpenBranchRequest) -> Result<OpenPrResponse, CommandError> {
        Err(not_ready())
    }
    async fn merge_info(&self, _pr: PrId) -> Result<MergeInfo, CommandError> {
        Err(not_ready())
    }
    async fn merge_pr(&self, _req: MergeRequest) -> Result<(), CommandError> {
        Err(not_ready())
    }
    async fn assignable_users(&self, _pr: PrId) -> Result<Vec<Person>, CommandError> {
        Err(not_ready())
    }
    async fn set_reviewers(&self, _req: ReviewersRequest) -> Result<(), CommandError> {
        Err(not_ready())
    }
    async fn set_draft(&self, _req: DraftStateRequest) -> Result<(), CommandError> {
        Err(not_ready())
    }
    async fn update_branch(&self, _pr: PrId) -> Result<(), CommandError> {
        Err(not_ready())
    }
    async fn react(&self, _req: ReactRequest) -> Result<(), CommandError> {
        Err(not_ready())
    }
    async fn edit_comment(&self, _req: EditCommentRequest) -> Result<(), CommandError> {
        Err(not_ready())
    }
    async fn commit_suggestions(&self, _req: CommitSuggestionsRequest) -> Result<(), CommandError> {
        Err(not_ready())
    }
    async fn get_skill(&self, _id: String, _pr: Option<PrId>) -> Result<SkillBody, CommandError> {
        Err(not_ready())
    }
    async fn save_skill(&self, _req: SaveSkillRequest) -> Result<SkillInfo, CommandError> {
        Err(not_ready())
    }
    async fn interdiff(&self, _req: InterdiffRequest) -> Result<Vec<InterdiffEntry>, CommandError> {
        Err(not_ready())
    }
    async fn login_start(&self) -> Result<DeviceLogin, CommandError> {
        Err(not_ready())
    }
    async fn login_poll(&self) -> Result<LoginState, CommandError> {
        Err(not_ready())
    }
    async fn rerun_check(&self, _req: RerunCheckRequest) -> Result<(), CommandError> {
        Err(not_ready())
    }
    async fn triage(&self, _req: TriageRequest) -> Result<(), CommandError> {
        Err(not_ready())
    }
    async fn set_linear_token(
        &self,
        _req: SetLinearTokenRequest,
    ) -> Result<LinearStatus, CommandError> {
        Err(not_ready())
    }
    async fn linear_status(&self) -> Result<LinearStatus, CommandError> {
        Err(not_ready())
    }
    async fn linear_issues(
        &self,
        _req: LinearIssuesRequest,
    ) -> Result<Vec<LinearIssue>, CommandError> {
        Err(not_ready())
    }
    async fn find_symbol(&self, _req: FindSymbolRequest) -> Result<Vec<SymbolHit>, CommandError> {
        Err(not_ready())
    }
    async fn get_blob(&self, _req: GetBlobRequest) -> Result<BlobResponse, CommandError> {
        Err(not_ready())
    }
    async fn conflicts(&self, _pr: PrId) -> Result<Vec<ConflictFile>, CommandError> {
        Err(not_ready())
    }
    async fn project_working_state(
        &self,
        _project: String,
    ) -> Result<theodore_contract::ProjectWorkingState, CommandError> {
        Err(not_ready())
    }
    async fn close_branch_review(&self, _pr: PrId) -> Result<(), CommandError> {
        Err(not_ready())
    }
    async fn account_status(&self) -> Result<AccountStatus, CommandError> {
        Err(not_ready())
    }
    async fn account_sign_in_start(
        &self,
        _req: AccountSignInRequest,
    ) -> Result<AccountSignInStart, CommandError> {
        Err(not_ready())
    }
    async fn account_sign_in_poll(&self) -> Result<AccountSignInState, CommandError> {
        Err(not_ready())
    }
    async fn account_sign_in_cancel(&self) -> Result<(), CommandError> {
        Err(not_ready())
    }
    async fn account_sign_out(&self) -> Result<AccountStatus, CommandError> {
        Err(not_ready())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn not_ready_rejects_with_the_expected_code() {
        let core = NotReadyCoreApi;
        let err = core.app_info().await.unwrap_err();
        assert_eq!(err.code, "not_ready");

        let err = core.bootstrap(BootstrapScope::Global).await.unwrap_err();
        assert_eq!(err.code, "not_ready");
    }

    #[tokio::test]
    async fn default_methods_are_safe_before_the_core_is_wired_up() {
        let core = NotReadyCoreApi;
        assert_eq!(core.update_channel().await, "stable");
        assert!(core.is_idle().await);
    }
}
