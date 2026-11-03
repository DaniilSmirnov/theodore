//! `CoreApi` over the real core (`theodore-app`).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use theodore_app::{AppError, Core};
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
use theodore_domain::{AgentId, HostKind, HostRepo, Person, PrId, Revision};
use theodore_ports::{HostError, IssueTracker, NewIssue};
use theodore_telemetry::issue;

use crate::build_info;
use crate::core_api::CoreApi;

pub struct AppCore {
    core: Core,
    sandbox: Option<theodore_sandbox::Sandbox>,
    /// Remote telemetry sinks may exist (off in sandbox and fixture runs).
    remote_telemetry: bool,
    /// Posts "Report a problem" issues; `None` in sandbox and fixture runs.
    issues: Option<Arc<dyn IssueTracker>>,
    /// The last issue preview's app info and log lines, so the post sends
    /// exactly what the preview showed.
    issue_draft: Mutex<Option<IssueDraft>>,
}

#[derive(Clone)]
struct IssueDraft {
    id: String,
    info: Vec<(String, String)>,
    /// Collected only once logs were asked for.
    logs: Option<theodore_telemetry::report::LogTail>,
}

impl AppCore {
    pub fn new(core: Core) -> Self {
        Self {
            core,
            sandbox: None,
            remote_telemetry: false,
            issues: None,
            issue_draft: Mutex::new(None),
        }
    }

    /// Lets "Report a problem" open GitHub issues (real runs only).
    pub fn with_issues(mut self, issues: Option<Arc<dyn IssueTracker>>) -> Self {
        self.issues = issues;
        self
    }

    /// Lets this run point telemetry at remote sinks (env, settings, the
    /// build); still nothing is sent without the user's choice.
    pub fn with_remote_telemetry(mut self, allowed: bool) -> Self {
        self.remote_telemetry = allowed;
        self
    }

    /// Resolves the sinks from env, settings and the build, and applies
    /// the recorded choice and log level (docs/telemetry.md "Settings").
    /// A no-op where telemetry wasn't initialised (tests, the CLI).
    pub async fn apply_telemetry(&self) {
        let Some(t) = theodore_telemetry::global() else {
            return;
        };
        let s = self.core.telemetry_settings().await;
        t.set_level(s.log_level.as_deref());
        t.set_channel(&self.update_channel().await);
        let config = remote_config(self.remote_telemetry, &s);
        let choice = s.choice();
        let _ = tokio::task::spawn_blocking(move || t.apply(config, choice)).await;
    }

    /// Sandbox mode (docs/sandbox.md): `app_info` carries the badge info
    /// and the `sandbox_*` commands work.
    pub fn with_sandbox(mut self, sandbox: Option<theodore_sandbox::Sandbox>) -> Self {
        self.sandbox = sandbox;
        self
    }

    fn sandbox(&self) -> Result<theodore_sandbox::Sandbox, CommandError> {
        self.sandbox.clone().ok_or_else(|| CommandError {
            code: "invalid".into(),
            message: "not running in sandbox mode".into(),
        })
    }

    /// `owner/name` for report issues: the setting, the build, the default.
    async fn report_repo(&self) -> (String, String) {
        let setting = self
            .core
            .setting_string(theodore_telemetry::config::SETTING_REPORT_REPO, "")
            .await;
        issue::target_repo(Some(&setting))
    }

    /// The app-info table of a report issue: what the zip's
    /// `app-info.json` says about the build, minus agent CLIs.
    async fn issue_info(&self) -> Vec<(String, String)> {
        let row = |k: &str, v: String| (k.to_string(), v);
        vec![
            row("Version", build_info::VERSION.into()),
            row("Channel", self.update_channel().await),
            row("Commit", build_info::COMMIT.into()),
            row("Build", build_info::BUILD_ENV.into()),
            row("Built", issue::built_at(build_info::built_at())),
            row("OS", os_version().await),
            row("Arch", std::env::consts::ARCH.into()),
        ]
    }

    /// The draft `req` names when it still has what `req` needs; else a
    /// fresh one (logs read and redacted only when asked for).
    async fn issue_draft(
        &self,
        req: &theodore_contract::IssueRequest,
    ) -> Result<IssueDraft, CommandError> {
        let lock = || self.issue_draft.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(d) = lock().as_ref()
            && req.draft.as_deref() == Some(d.id.as_str())
            && (d.logs.is_some() || !req.with_logs)
        {
            return Ok(d.clone());
        }
        let logs = if req.with_logs {
            let t = theodore_telemetry::global().ok_or_else(|| CommandError {
                code: "internal".into(),
                message: "logging isn't set up in this run".into(),
            })?;
            t.flush(std::time::Duration::from_secs(2));
            let (dir, redactor) = (t.logs_dir().to_path_buf(), t.redactor().clone());
            let tail = tokio::task::spawn_blocking(move || {
                theodore_telemetry::report::log_tail(&dir, &redactor, issue::MAX_BODY)
            })
            .await
            .map_err(|e| internal(e.to_string()))?
            .map_err(|e| internal(e.to_string()))?;
            Some(tail)
        } else {
            None
        };
        let draft = IssueDraft {
            id: uuid::Uuid::new_v4().simple().to_string(),
            info: self.issue_info().await,
            logs,
        };
        *lock() = Some(draft.clone());
        Ok(draft)
    }

    /// The title and body `req` would post, from `draft`.
    fn issue_body(
        req: &theodore_contract::IssueRequest,
        draft: &IssueDraft,
    ) -> Result<(String, issue::IssueBody), CommandError> {
        let invalid = |e: issue::IssueError| CommandError {
            code: "invalid".into(),
            message: e.to_string(),
        };
        let title = issue::title(&req.title).map_err(invalid)?;
        let body = issue::build(issue::IssueInput {
            comment: &req.comment,
            info: &draft.info,
            logs: draft.logs.as_ref().filter(|_| req.with_logs),
        })
        .map_err(invalid)?;
        Ok((title, body))
    }
}

fn internal(message: String) -> CommandError {
    CommandError {
        code: "internal".into(),
        message,
    }
}

/// A failed issue post, in words the dialog shows as they are.
fn issue_err(e: HostError) -> CommandError {
    tracing::warn!(error.code = e.code(), error = %e, "problem report issue failed");
    let (code, message) = match e {
        HostError::Auth => (
            "github_auth",
            "GitHub sign-in expired or can't open issues there. Sign in again in Settings → GitHub."
                .to_string(),
        ),
        HostError::NotFound(_) => (
            "not_found",
            "GitHub can't find that repository for this account.".to_string(),
        ),
        HostError::RateLimited { .. } => (
            "rate_limited",
            "GitHub's rate limit was hit. Try again in a few minutes.".to_string(),
        ),
        HostError::Network(_) => (
            "network",
            "Couldn't reach GitHub. Check the connection and try again.".to_string(),
        ),
        HostError::Validation(m) => ("invalid", format!("GitHub refused the issue: {m}")),
        other => ("internal", other.to_string()),
    };
    CommandError {
        code: code.into(),
        message,
    }
}

/// Each sink from the first source that sets it: env, setting, build.
/// None at all where remote sinks aren't allowed (sandbox, fixtures).
fn remote_config(
    allowed: bool,
    s: &theodore_app::TelemetrySettings,
) -> theodore_telemetry::RemoteConfig {
    use theodore_telemetry::config as c;
    if !allowed {
        return theodore_telemetry::RemoteConfig::default();
    }
    let env = |k: &str| std::env::var(k).ok();
    let (endpoint, headers, host, key) = (
        env(c::ENV_OTLP_ENDPOINT),
        env(c::ENV_OTLP_HEADERS),
        env(c::ENV_POSTHOG_HOST),
        env(c::ENV_POSTHOG_KEY),
    );
    let from_env = theodore_telemetry::Sources {
        otlp_endpoint: endpoint.as_deref(),
        otlp_headers: headers.as_deref(),
        posthog_host: host.as_deref(),
        posthog_key: key.as_deref(),
    };
    theodore_telemetry::RemoteConfig::resolve(&[from_env, s.sources(), c::BUILT_IN])
}

/// `macOS 15.1` and the like, for the problem report.
async fn os_version() -> String {
    let os = std::env::consts::OS;
    if os == "macos"
        && let Ok(out) = tokio::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .await
    {
        let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !v.is_empty() {
            return format!("macOS {v}");
        }
    }
    os.to_string()
}

/// Sandbox calls shell out to git under a lock: keep them off the runtime.
async fn sandbox_call<T: Send + 'static>(
    sb: theodore_sandbox::Sandbox,
    f: impl FnOnce(&theodore_sandbox::Sandbox) -> theodore_sandbox::SandboxResult<T> + Send + 'static,
) -> Result<T, CommandError> {
    tokio::task::spawn_blocking(move || f(&sb))
        .await
        .map_err(|e| CommandError {
            code: "internal".into(),
            message: e.to_string(),
        })?
        .map_err(|e| CommandError {
            code: "invalid".into(),
            message: e.to_string(),
        })
}

fn err(e: AppError) -> CommandError {
    // In the command's span: the record carries its `op_id` and `cmd`.
    if e.is_expected() {
        tracing::info!(error.code = e.code(), error = %e, "command refused");
    } else {
        tracing::warn!(error.code = e.code(), error = %e, "command failed");
    }
    let code = match &e {
        AppError::Store(_) => "store",
        // An old local `git` lacking `merge-tree --write-tree` (`conflicts`):
        // the UI hides the conflicts list on this code instead of toasting it.
        AppError::Git(theodore_ports::GitError::Unsupported(_)) => "unsupported",
        // The git remote can't be reached: offline, not a git failure.
        AppError::Git(theodore_ports::GitError::Network(_)) | AppError::Offline => "offline",
        AppError::Git(_) => "git",
        AppError::Host(theodore_ports::HostError::Auth) => "auth",
        AppError::Host(theodore_ports::HostError::RateLimited { .. }) => "rate_limited",
        AppError::Host(_) => "host",
        AppError::Grouper(_) => "grouping",
        AppError::NotFound(_) => "not_found",
        AppError::Invalid(_) => "invalid",
        AppError::Join(_) => "internal",
        AppError::Account(_) => "account",
        // The UI shows the sign-in screen on this code (plan §6.12).
        AppError::AccountRequired => "account_required",
        AppError::OfflineNotCached => "offline_not_cached",
    };
    let message = match &e {
        AppError::Host(HostError::RateLimited { reset_at }) => {
            rate_limited_message(*reset_at, now_ms())
        }
        _ => e.to_string(),
    };
    CommandError {
        code: code.into(),
        message,
    }
}

/// "rate limited until 1793393359000" means nothing to a person: say how
/// long to wait instead.
fn rate_limited_message(reset_at: i64, now: i64) -> String {
    let minutes = (reset_at - now + 59_999).div_euclid(60_000);
    match minutes {
        ..=1 => "Rate limit reached. Try again in a minute.".to_string(),
        m => format!("Rate limit reached. Try again in {m} minutes."),
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

#[async_trait]
impl CoreApi for AppCore {
    async fn bootstrap(&self, scope: BootstrapScope) -> Result<Bootstrap, CommandError> {
        self.core.bootstrap(scope).await.map_err(err)
    }
    async fn catch_up(&self, since: u64) -> Result<SyncFrame, CommandError> {
        self.core.catch_up(since).await.map_err(err)
    }
    async fn transact(&self, tx: Transaction) -> Result<TransactionResult, CommandError> {
        Ok(self.core.transact(tx).await)
    }
    async fn open_pr(&self, req: OpenPrRequest) -> Result<OpenPrResponse, CommandError> {
        self.core.open_pr(req).await.map_err(err)
    }
    async fn get_revision(&self, req: RevisionRequest) -> Result<Revision, CommandError> {
        self.core.get_revision(req).await.map_err(err)
    }
    async fn get_file(&self, req: FileRequest) -> Result<Option<String>, CommandError> {
        self.core.get_file(req).await.map_err(err)
    }
    async fn regroup(&self, pr: PrId) -> Result<(), CommandError> {
        self.core.regroup(pr).await.map_err(err)
    }
    async fn run_skill(&self, req: RunSkillRequest) -> Result<String, CommandError> {
        self.core.run_skill(req).await.map_err(err)
    }
    async fn ask(&self, req: AskRequest) -> Result<String, CommandError> {
        self.core.ask(req).await.map_err(err)
    }
    async fn save_attachment(
        &self,
        req: theodore_contract::SaveAttachmentRequest,
    ) -> Result<theodore_contract::Attachment, CommandError> {
        self.core.save_attachment(req).await.map_err(err)
    }
    async fn attachment_data(&self, id: String) -> Result<String, CommandError> {
        self.core.attachment_data(id).await.map_err(err)
    }
    async fn cancel_run(&self, run: String) -> Result<(), CommandError> {
        self.core.cancel_run(run).await.map_err(err)
    }
    async fn submit_review(&self, req: SubmitReviewRequest) -> Result<(), CommandError> {
        self.core.submit_review(req).await.map_err(err)
    }
    async fn detect_agents(&self) -> Result<Vec<AgentStatus>, CommandError> {
        Ok(self
            .core
            .deps()
            .agents
            .detect()
            .await
            .into_iter()
            .map(|a| AgentStatus {
                id: a.id,
                installed: a.installed,
                version: a.version,
                path: a.path.map(|p| p.display().to_string()),
            })
            .collect())
    }
    async fn agent_models(&self, agent: AgentId) -> Result<Vec<AgentModel>, CommandError> {
        Ok(self
            .core
            .deps()
            .agents
            .models(agent)
            .await
            .into_iter()
            .map(|m| AgentModel {
                id: m.id,
                label: m.label,
                description: m.description,
                efforts: m.efforts,
                default_effort: m.default_effort,
                older: m.older,
            })
            .collect())
    }
    async fn app_info(&self) -> Result<AppInfo, CommandError> {
        Ok(AppInfo {
            version: build_info::VERSION.into(),
            commit: build_info::COMMIT.into(),
            build_env: build_info::BUILD_ENV.into(),
            built_at: build_info::built_at(),
            channel: self.update_channel().await,
            update: None,
            sandbox: self.sandbox.as_ref().map(|s| s.info()),
        })
    }
    async fn check_update(&self) -> Result<Option<UpdateInfo>, CommandError> {
        // The shell's updater owns checks; the command never reaches here.
        Ok(None)
    }
    async fn install_update(&self) -> Result<(), CommandError> {
        Ok(())
    }
    async fn refresh_inbox(&self) -> Result<(), CommandError> {
        self.core.refresh_inbox().await.map_err(err)
    }
    async fn viewer(&self) -> Result<Person, CommandError> {
        self.core.viewer().await.map_err(err)
    }
    async fn host_status(&self) -> Result<HostAuthStatus, CommandError> {
        self.core.host_status().await.map_err(err)
    }
    async fn host_status_for(&self, host: HostKind) -> Result<HostAuthStatus, CommandError> {
        self.core.host_status_for(host).await.map_err(err)
    }
    async fn set_host_token(&self, req: SetTokenRequest) -> Result<HostAuthStatus, CommandError> {
        self.core.set_host_token(req).await.map_err(err)
    }
    async fn reply_thread(&self, req: ReplyRequest) -> Result<(), CommandError> {
        self.core.reply_thread(req).await.map_err(err)
    }
    async fn resolve_thread(&self, req: ResolveRequest) -> Result<(), CommandError> {
        self.core.resolve_thread(req).await.map_err(err)
    }
    async fn list_skills(&self, pr: Option<PrId>) -> Result<Vec<SkillInfo>, CommandError> {
        self.core.list_skills(pr).await.map_err(err)
    }
    async fn delegate_task(&self, req: DelegateRequest) -> Result<String, CommandError> {
        self.core.delegate_task(req).await.map_err(err)
    }
    async fn decide_task(&self, req: TaskDecision) -> Result<(), CommandError> {
        self.core.decide_task(req).await.map_err(err)
    }
    async fn list_host_repos(&self) -> Result<Vec<HostRepo>, CommandError> {
        self.core.list_host_repos().await.map_err(err)
    }
    async fn add_project(&self, req: AddProjectRequest) -> Result<Project, CommandError> {
        self.core.add_project(req).await.map_err(err)
    }
    async fn remove_project(&self, id: String) -> Result<(), CommandError> {
        self.core.remove_project(&id).await.map_err(err)
    }
    async fn refresh_project(&self, id: String) -> Result<(), CommandError> {
        self.core.refresh_project(&id).await.map_err(err)
    }
    async fn list_branches(&self, project: String) -> Result<Vec<LocalBranch>, CommandError> {
        self.core.list_branches(&project).await.map_err(err)
    }
    async fn open_branch(&self, req: OpenBranchRequest) -> Result<OpenPrResponse, CommandError> {
        self.core.open_branch(req).await.map_err(err)
    }
    async fn merge_info(&self, pr: PrId) -> Result<MergeInfo, CommandError> {
        self.core.merge_info(pr).await.map_err(err)
    }
    async fn merge_pr(&self, req: MergeRequest) -> Result<(), CommandError> {
        self.core.merge_pr(req).await.map_err(err)
    }
    async fn assignable_users(&self, pr: PrId) -> Result<Vec<Person>, CommandError> {
        self.core.assignable_users(pr).await.map_err(err)
    }
    async fn set_reviewers(&self, req: ReviewersRequest) -> Result<(), CommandError> {
        self.core.set_reviewers(req).await.map_err(err)
    }
    async fn set_draft(&self, req: DraftStateRequest) -> Result<(), CommandError> {
        self.core.set_draft(req).await.map_err(err)
    }
    async fn update_branch(&self, pr: PrId) -> Result<(), CommandError> {
        self.core.update_branch(pr).await.map_err(err)
    }
    async fn react(&self, req: ReactRequest) -> Result<(), CommandError> {
        self.core.react(req).await.map_err(err)
    }
    async fn edit_comment(&self, req: EditCommentRequest) -> Result<(), CommandError> {
        self.core.edit_comment(req).await.map_err(err)
    }
    async fn commit_suggestions(&self, req: CommitSuggestionsRequest) -> Result<(), CommandError> {
        self.core.commit_suggestions(req).await.map_err(err)
    }
    async fn get_skill(&self, id: String, pr: Option<PrId>) -> Result<SkillBody, CommandError> {
        self.core.get_skill(&id, pr).await.map_err(err)
    }
    async fn save_skill(&self, req: SaveSkillRequest) -> Result<SkillInfo, CommandError> {
        self.core.save_skill(req).await.map_err(err)
    }
    async fn interdiff(&self, req: InterdiffRequest) -> Result<Vec<InterdiffEntry>, CommandError> {
        self.core.interdiff(req).await.map_err(err)
    }
    async fn login_start(&self) -> Result<DeviceLogin, CommandError> {
        self.core.login_start().await.map_err(err)
    }
    async fn login_poll(&self) -> Result<LoginState, CommandError> {
        self.core.login_poll().await.map_err(err)
    }
    async fn rerun_check(&self, req: RerunCheckRequest) -> Result<(), CommandError> {
        self.core.rerun_check(req).await.map_err(err)
    }
    async fn triage(&self, req: TriageRequest) -> Result<(), CommandError> {
        self.core.triage(req).await.map_err(err)
    }
    async fn set_linear_token(
        &self,
        req: SetLinearTokenRequest,
    ) -> Result<LinearStatus, CommandError> {
        self.core.set_linear_token(req).await.map_err(err)
    }
    async fn linear_status(&self) -> Result<LinearStatus, CommandError> {
        self.core.linear_status().await.map_err(err)
    }
    async fn linear_issues(
        &self,
        req: LinearIssuesRequest,
    ) -> Result<Vec<LinearIssue>, CommandError> {
        self.core.linear_issues(req).await.map_err(err)
    }
    async fn find_symbol(&self, req: FindSymbolRequest) -> Result<Vec<SymbolHit>, CommandError> {
        self.core.find_symbol(req).await.map_err(err)
    }
    async fn get_blob(&self, req: GetBlobRequest) -> Result<BlobResponse, CommandError> {
        self.core.get_blob(req).await.map_err(err)
    }
    async fn conflicts(&self, pr: PrId) -> Result<Vec<ConflictFile>, CommandError> {
        self.core.conflicts(pr).await.map_err(err)
    }
    async fn project_working_state(
        &self,
        project: String,
    ) -> Result<theodore_contract::ProjectWorkingState, CommandError> {
        self.core.project_working_state(&project).await.map_err(err)
    }
    async fn close_branch_review(&self, pr: PrId) -> Result<(), CommandError> {
        self.core.close_branch_review(&pr).await.map_err(err)
    }
    async fn update_channel(&self) -> String {
        let setting = self.core.setting_string("updates.channel", "stable").await;
        crate::updater::effective_channel(build_info::BUILD_ENV, &setting).into()
    }
    async fn is_idle(&self) -> bool {
        self.core.is_idle().await
    }
    async fn account_gate(&self) -> Result<(), CommandError> {
        self.core.require_account().await.map_err(err)
    }
    async fn updates_enabled(&self) -> bool {
        self.sandbox.is_none()
    }
    async fn stop_all_space_commands(&self) {
        self.core.stop_all_running_spaces().await;
    }
    async fn telemetry_status(&self) -> Result<theodore_contract::TelemetryStatus, CommandError> {
        let (usage_available, errors_available, _) = theodore_telemetry::global()
            .map(|t| t.status())
            .unwrap_or_default();
        // The stored choice is the truth; the sinks follow it.
        let s = self.core.telemetry_settings().await;
        Ok(theodore_contract::TelemetryStatus {
            usage_available,
            errors_available,
            usage: s.usage,
            errors: s.errors,
            events: theodore_telemetry::ProductEvent::catalog(),
            never_sent: theodore_telemetry::product::never_sent(),
        })
    }
    async fn set_telemetry(
        &self,
        req: theodore_contract::TelemetryChoice,
    ) -> Result<theodore_contract::TelemetryStatus, CommandError> {
        self.core
            .set_telemetry_choice(req.usage, req.errors)
            .await
            .map_err(err)?;
        self.apply_telemetry().await;
        self.telemetry_status().await
    }
    async fn report_problem(&self) -> Result<theodore_contract::ProblemReport, CommandError> {
        let internal = |m: String| CommandError {
            code: "internal".into(),
            message: m,
        };
        let t = theodore_telemetry::global()
            .ok_or_else(|| internal("logging isn't set up in this run".into()))?;
        t.flush(std::time::Duration::from_secs(2));
        let meta = t.meta();
        let os = os_version().await;
        let agents = self.core.deps().agents.detect().await;
        let agent_line = agents
            .iter()
            .map(|a| match (&a.version, a.installed) {
                (Some(v), true) => format!("{} {v}", a.id.as_str()),
                (None, true) => format!("{} (version unknown)", a.id.as_str()),
                _ => format!("{} not found", a.id.as_str()),
            })
            .collect::<Vec<_>>()
            .join(", ");
        let mut info = serde_json::Map::new();
        info.insert("version".into(), meta.version.clone().into());
        info.insert("channel".into(), meta.channel.clone().into());
        info.insert("commit".into(), meta.commit.clone().into());
        info.insert("build_env".into(), meta.build_env.clone().into());
        info.insert("built_at".into(), build_info::built_at().into());
        info.insert("os".into(), os.clone().into());
        info.insert("arch".into(), std::env::consts::ARCH.into());
        info.insert("sandbox".into(), self.sandbox.is_some().into());
        info.insert(
            "agents".into(),
            serde_json::json!(
                agents
                    .iter()
                    .map(|a| serde_json::json!({
                        "id": a.id.as_str(),
                        "installed": a.installed,
                        "version": a.version,
                    }))
                    .collect::<Vec<_>>()
            ),
        );
        let summary = vec![
            format!(
                "Theodore {} ({} channel)",
                build_info::label(),
                meta.channel
            ),
            format!("Commit {}", meta.commit),
            format!("{os} ({})", std::env::consts::ARCH),
            format!("Agents: {agent_line}"),
        ];
        let logs_dir = t.logs_dir().to_path_buf();
        let redactor = t.redactor().clone();
        let report = tokio::task::spawn_blocking(move || {
            let root = theodore_telemetry::report::reports_dir();
            std::fs::create_dir_all(&root)?;
            theodore_telemetry::report::build(
                theodore_telemetry::report::ReportInput {
                    logs_dir: &logs_dir,
                    app_info: info,
                    summary,
                    max_log_bytes: theodore_telemetry::report::MAX_LOG_BYTES,
                    redactor: &redactor,
                },
                &root,
            )
        })
        .await
        .map_err(|e| internal(e.to_string()))?
        .map_err(|e| internal(e.to_string()))?;
        tracing::info!(files = report.files.len(), "problem report built");
        theodore_telemetry::product(theodore_telemetry::ProductEvent::ProblemReported {
            mode: "logs_only".into(),
        });
        Ok(report)
    }
    async fn issue_target(&self) -> Result<theodore_contract::IssueTarget, CommandError> {
        let (owner, name) = self.report_repo().await;
        let repo = format!("{owner}/{name}");
        let (login, unavailable) = match &self.issues {
            None if self.sandbox.is_some() => (None, Some("Sandbox mode never posts to GitHub.")),
            None => (None, Some("Fixture mode never posts to GitHub.")),
            Some(t) if !t.has_token() => (None, Some("Sign in to GitHub to open an issue.")),
            Some(t) => match t.login().await {
                Ok(login) => (Some(login), None),
                Err(HostError::Auth) => (
                    None,
                    Some("GitHub sign-in expired. Sign in again in Settings → GitHub."),
                ),
                Err(_) => (None, Some("Couldn't reach GitHub right now.")),
            },
        };
        Ok(theodore_contract::IssueTarget {
            url: format!("https://github.com/{repo}"),
            repo,
            login,
            unavailable: unavailable.map(str::to_string),
        })
    }
    async fn preview_issue(
        &self,
        req: theodore_contract::IssueRequest,
    ) -> Result<theodore_contract::IssuePreview, CommandError> {
        let draft = self.issue_draft(&req).await?;
        let (title, body) = Self::issue_body(&req, &draft)?;
        Ok(theodore_contract::IssuePreview {
            draft: draft.id,
            title,
            body: body.body,
            logs: body.logs,
            log_lines: body.log_lines as u32,
            cut_lines: body.cut_lines as u32,
        })
    }
    async fn submit_issue(
        &self,
        req: theodore_contract::IssueRequest,
    ) -> Result<theodore_contract::ReportedIssue, CommandError> {
        // Never reaches GitHub without a real tracker: sandbox and fixture
        // runs have none.
        let tracker = self.issues.clone().ok_or_else(|| CommandError {
            code: "unavailable".into(),
            message: "This run never posts to GitHub.".into(),
        })?;
        let draft = self.issue_draft(&req).await?;
        let (title, body) = Self::issue_body(&req, &draft)?;
        let (owner, name) = self.report_repo().await;
        let created = tracker
            .create_issue(
                &owner,
                &name,
                &NewIssue {
                    title,
                    body: body.body,
                    labels: vec![issue::LABEL.to_string()],
                },
            )
            .await
            .map_err(issue_err)?;
        *self.issue_draft.lock().unwrap_or_else(|p| p.into_inner()) = None;
        tracing::info!(
            number = created.number,
            with_logs = req.with_logs,
            "problem report issue opened"
        );
        let mode = if req.with_logs {
            "issue_with_logs"
        } else {
            "issue"
        };
        theodore_telemetry::product(theodore_telemetry::ProductEvent::ProblemReported {
            mode: mode.into(),
        });
        Ok(theodore_contract::ReportedIssue {
            number: created.number,
            url: created.url,
        })
    }
    async fn sandbox_status(
        &self,
    ) -> Result<Option<theodore_contract::SandboxStatus>, CommandError> {
        match self.sandbox.clone() {
            Some(sb) => sandbox_call(sb, |s| Ok(s.status())).await.map(Some),
            None => Ok(None),
        }
    }
    async fn sandbox_set_fault(
        &self,
        req: theodore_contract::SandboxFaultRequest,
    ) -> Result<theodore_contract::SandboxStatus, CommandError> {
        sandbox_call(self.sandbox()?, move |s| {
            s.set_fault(&req.name, req.on, None)
        })
        .await
    }
    async fn sandbox_fire_event(
        &self,
        id: String,
    ) -> Result<theodore_contract::SandboxStatus, CommandError> {
        sandbox_call(self.sandbox()?, move |s| {
            s.fire(&id)?;
            Ok(s.status())
        })
        .await
    }

    async fn space_config(
        &self,
        space: String,
    ) -> Result<theodore_contract::SpaceConfigView, CommandError> {
        self.core.space_config(&space).await.map_err(err)
    }
    async fn space_trust(
        &self,
        req: theodore_contract::SpaceTrustRequest,
    ) -> Result<theodore_contract::Space, CommandError> {
        self.core.space_trust(req).await.map_err(err)
    }
    async fn space_setup(
        &self,
        req: theodore_contract::SpaceSetupRequest,
    ) -> Result<theodore_contract::Space, CommandError> {
        self.core.space_setup(req).await.map_err(err)
    }
    async fn space_setup_assist(
        &self,
        space: String,
    ) -> Result<theodore_contract::Space, CommandError> {
        self.core.space_setup_assist(&space).await.map_err(err)
    }
    async fn space_fix_setup(
        &self,
        space: String,
    ) -> Result<theodore_contract::Space, CommandError> {
        self.core.space_fix_setup(&space).await.map_err(err)
    }
    async fn space_run(
        &self,
        req: theodore_contract::SpaceRunRequest,
    ) -> Result<theodore_contract::Space, CommandError> {
        self.core.space_run(req).await.map_err(err)
    }
    async fn space_logs(
        &self,
        space: String,
    ) -> Result<Vec<theodore_contract::SpaceLogLine>, CommandError> {
        self.core.space_logs(&space).await.map_err(err)
    }
    async fn open_space_terminal(
        &self,
        req: theodore_contract::OpenTerminalRequest,
    ) -> Result<theodore_contract::SpaceTerminalInfo, CommandError> {
        self.core
            .open_space_terminal(&req.space, req.cols, req.rows)
            .await
            .map_err(err)
    }
    async fn write_space_terminal(
        &self,
        req: theodore_contract::WriteTerminalRequest,
    ) -> Result<(), CommandError> {
        self.core
            .write_space_terminal(&req.terminal, &req.data)
            .await
            .map_err(err)
    }
    async fn resize_space_terminal(
        &self,
        req: theodore_contract::ResizeTerminalRequest,
    ) -> Result<(), CommandError> {
        self.core
            .resize_space_terminal(&req.terminal, req.cols, req.rows)
            .await
            .map_err(err)
    }
    async fn close_space_terminal(&self, terminal: String) -> Result<(), CommandError> {
        self.core.close_space_terminal(&terminal).await.map_err(err)
    }
    async fn list_space_terminals(
        &self,
        space: String,
    ) -> Result<Vec<theodore_contract::SpaceTerminalInfo>, CommandError> {
        self.core.list_space_terminals(&space).await.map_err(err)
    }
    async fn project_config(
        &self,
        project: String,
    ) -> Result<theodore_contract::EffectiveConfig, CommandError> {
        self.core.project_config(&project).await.map_err(err)
    }
    async fn global_config(&self) -> Result<theodore_contract::GlobalConfigInfo, CommandError> {
        self.core.global_config().await.map_err(err)
    }
    async fn code_intel_status(
        &self,
        project: String,
    ) -> Result<theodore_contract::CodeIntelStatus, CommandError> {
        self.core.code_intel_status(&project).await.map_err(err)
    }
    async fn code_intel_install(&self, language: String) -> Result<(), CommandError> {
        self.core.code_intel_install(&language).await.map_err(err)
    }
    async fn code_intel_set_lsp(
        &self,
        req: theodore_contract::CodeIntelLspRequest,
    ) -> Result<(), CommandError> {
        self.core.code_intel_set_lsp(req).await.map_err(err)
    }
    async fn code_intel_decline(&self, language: String) -> Result<(), CommandError> {
        self.core.code_intel_decline(&language).await.map_err(err)
    }
    async fn code_intel_remove(&self, language: String) -> Result<(), CommandError> {
        self.core.code_intel_remove(&language).await.map_err(err)
    }
    async fn code_intel_hover(
        &self,
        req: theodore_contract::CodeHoverRequest,
    ) -> Result<Option<theodore_contract::CodeHover>, CommandError> {
        self.core.code_intel_hover(req).await.map_err(err)
    }
    async fn ensure_global_config(
        &self,
    ) -> Result<theodore_contract::GlobalConfigInfo, CommandError> {
        self.core.ensure_global_config().await.map_err(err)
    }
    async fn config_draft(
        &self,
        project: String,
    ) -> Result<theodore_contract::ConfigDraft, CommandError> {
        self.core.config_draft(&project).await.map_err(err)
    }
    async fn check_config(
        &self,
        req: theodore_contract::CheckConfigRequest,
    ) -> Result<theodore_contract::ConfigCheck, CommandError> {
        self.core.check_config(req).await.map_err(err)
    }
    async fn save_config(
        &self,
        req: theodore_contract::SaveConfigRequest,
    ) -> Result<theodore_contract::SaveConfigResult, CommandError> {
        self.core.save_config(req).await.map_err(err)
    }
    async fn create_space(
        &self,
        req: theodore_contract::CreateSpaceRequest,
    ) -> Result<theodore_contract::Space, CommandError> {
        self.core.create_space(req).await.map_err(err)
    }
    async fn project_issues(
        &self,
        req: theodore_contract::ProjectIssuesRequest,
    ) -> Result<Vec<theodore_contract::SpaceIssue>, CommandError> {
        self.core.project_issues(req).await.map_err(err)
    }
    async fn list_spaces(
        &self,
        project: Option<String>,
    ) -> Result<Vec<theodore_contract::Space>, CommandError> {
        self.core.list_spaces(project).await.map_err(err)
    }
    async fn space_send(
        &self,
        req: theodore_contract::SpaceSendRequest,
    ) -> Result<String, CommandError> {
        self.core.space_send(req).await.map_err(err)
    }
    async fn space_send_now(
        &self,
        req: theodore_contract::SpaceSendRequest,
    ) -> Result<String, CommandError> {
        self.core.space_send_now(req).await.map_err(err)
    }
    async fn space_queued(
        &self,
        req: theodore_contract::SpaceQueuedRequest,
    ) -> Result<(), CommandError> {
        self.core.space_queued(req).await.map_err(err)
    }
    async fn space_stop(&self, space: String) -> Result<(), CommandError> {
        self.core.space_stop(&space).await.map_err(err)
    }
    async fn space_skills(&self, space: String) -> Result<Vec<SkillInfo>, CommandError> {
        self.core.space_skills(&space).await.map_err(err)
    }
    async fn space_review(
        &self,
        req: theodore_contract::SpaceReviewRequest,
    ) -> Result<OpenPrResponse, CommandError> {
        self.core.space_review(req).await.map_err(err)
    }
    async fn archive_space(&self, space: String) -> Result<(), CommandError> {
        self.core.archive_space(&space).await.map_err(err)
    }
    async fn ship_space(
        &self,
        req: theodore_contract::ShipSpaceRequest,
    ) -> Result<theodore_contract::Space, CommandError> {
        self.core.ship_space(req).await.map_err(err)
    }
    async fn merge_space(
        &self,
        req: theodore_contract::MergeSpaceRequest,
    ) -> Result<theodore_contract::MergeSpaceResult, CommandError> {
        self.core.merge_space(req).await.map_err(err)
    }
    async fn continue_space(
        &self,
        space: String,
    ) -> Result<theodore_contract::Space, CommandError> {
        self.core.continue_space(&space).await.map_err(err)
    }
    async fn undo_space_turn(
        &self,
        space: String,
    ) -> Result<theodore_contract::Space, CommandError> {
        self.core.undo_space_turn(&space).await.map_err(err)
    }
    async fn rewind_space(
        &self,
        req: theodore_contract::RewindSpaceRequest,
    ) -> Result<theodore_contract::Space, CommandError> {
        self.core.rewind_space(req).await.map_err(err)
    }
    async fn revert_space_file(
        &self,
        req: theodore_contract::RevertSpaceFileRequest,
    ) -> Result<theodore_contract::Space, CommandError> {
        self.core.revert_space_file(req).await.map_err(err)
    }
    async fn unarchive_space(
        &self,
        space: String,
    ) -> Result<theodore_contract::Space, CommandError> {
        self.core.unarchive_space(&space).await.map_err(err)
    }
    async fn set_space_pinned(
        &self,
        req: theodore_contract::SetSpacePinnedRequest,
    ) -> Result<theodore_contract::Space, CommandError> {
        self.core
            .set_space_pinned(&req.space, req.pinned)
            .await
            .map_err(err)
    }
    async fn space_send_feedback(
        &self,
        req: theodore_contract::SpaceFeedbackRequest,
    ) -> Result<String, CommandError> {
        self.core.space_send_feedback(req).await.map_err(err)
    }
    async fn space_base_status(
        &self,
        space: String,
    ) -> Result<theodore_contract::SpaceBaseStatus, CommandError> {
        self.core.space_base_status(&space).await.map_err(err)
    }
    async fn update_space_from_base(
        &self,
        space: String,
    ) -> Result<theodore_contract::Space, CommandError> {
        self.core.update_space_from_base(&space).await.map_err(err)
    }
    async fn abort_space_update(
        &self,
        space: String,
    ) -> Result<theodore_contract::Space, CommandError> {
        self.core.abort_space_update(&space).await.map_err(err)
    }
    async fn space_fix_ci(
        &self,
        req: theodore_contract::SpaceFixCiRequest,
    ) -> Result<String, CommandError> {
        self.core.space_fix_ci(req).await.map_err(err)
    }
    async fn approve_space_plan(&self, space: String) -> Result<String, CommandError> {
        self.core.approve_space_plan(&space).await.map_err(err)
    }
    async fn answer_space_question(
        &self,
        req: theodore_contract::SpaceAnswerRequest,
    ) -> Result<(), CommandError> {
        self.core.answer_space_question(req).await.map_err(err)
    }
    async fn mirror_space(&self, space: String) -> Result<theodore_contract::Space, CommandError> {
        self.core.mirror_space(&space).await.map_err(err)
    }
    async fn sync_mirror(&self, space: String) -> Result<theodore_contract::Space, CommandError> {
        self.core.sync_mirror(&space).await.map_err(err)
    }
    async fn unmirror_space(
        &self,
        req: theodore_contract::UnmirrorSpaceRequest,
    ) -> Result<theodore_contract::Space, CommandError> {
        self.core.unmirror_space(req).await.map_err(err)
    }
    async fn account_status(&self) -> Result<AccountStatus, CommandError> {
        self.core.account_status().await.map_err(err)
    }
    async fn account_sign_in_start(
        &self,
        req: AccountSignInRequest,
    ) -> Result<AccountSignInStart, CommandError> {
        self.core.account_sign_in_start(req).await.map_err(err)
    }
    async fn account_sign_in_poll(&self) -> Result<AccountSignInState, CommandError> {
        self.core.account_sign_in_poll().await.map_err(err)
    }
    async fn account_sign_in_cancel(&self) -> Result<(), CommandError> {
        self.core.account_sign_in_cancel();
        Ok(())
    }
    async fn account_sign_out(&self) -> Result<AccountStatus, CommandError> {
        self.core.account_sign_out().await.map_err(err)
    }
    async fn account_sign_in_callback(&self, url: String) -> bool {
        !matches!(
            self.core.account_sign_in_callback(&url).await,
            theodore_ports::CallbackPage::Ignored
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_sign_in_has_its_own_code() {
        let e = err(AppError::AccountRequired);
        assert_eq!(e.code, "account_required");
        assert_eq!(e.message, "Sign in to your Theodore account to continue");
    }

    #[test]
    fn a_rate_limit_says_how_long_to_wait() {
        let e = err(AppError::Host(HostError::RateLimited {
            reset_at: now_ms() + 59 * 60_000 + 30_000,
        }));
        assert_eq!(e.code, "rate_limited");
        assert_eq!(e.message, "Rate limit reached. Try again in 60 minutes.");

        let now = 1_793_389_849_000;
        assert_eq!(
            rate_limited_message(now + 3_510_000, now),
            "Rate limit reached. Try again in 59 minutes."
        );
        assert_eq!(
            rate_limited_message(now + 5_000, now),
            "Rate limit reached. Try again in a minute."
        );
        assert_eq!(
            rate_limited_message(now - 5_000, now),
            "Rate limit reached. Try again in a minute."
        );
    }

    #[test]
    fn sandbox_and_fixture_runs_get_no_remote_sinks_whatever_is_set() {
        let s = theodore_app::TelemetrySettings {
            usage: Some(true),
            errors: Some(true),
            otlp_endpoint: Some("https://otlp.example".into()),
            posthog_key: Some("phc_x".into()),
            ..Default::default()
        };
        assert_eq!(
            remote_config(false, &s),
            theodore_telemetry::RemoteConfig::default()
        );
        let c = remote_config(true, &s);
        assert!(c.otlp.is_some() && c.posthog.is_some());
    }

    // ── report a problem as an issue ─────────────────────────────────────

    /// Records posts instead of reaching GitHub.
    #[derive(Default)]
    struct FakeTracker {
        signed_out: bool,
        expired: bool,
        posted: Mutex<Vec<(String, String, NewIssue)>>,
    }

    #[async_trait]
    impl IssueTracker for FakeTracker {
        fn has_token(&self) -> bool {
            !self.signed_out
        }
        async fn login(&self) -> theodore_ports::HostResult<String> {
            if self.expired {
                return Err(HostError::Auth);
            }
            Ok("octocat".into())
        }
        async fn create_issue(
            &self,
            owner: &str,
            name: &str,
            issue: &NewIssue,
        ) -> theodore_ports::HostResult<theodore_ports::CreatedIssue> {
            if self.expired {
                return Err(HostError::Auth);
            }
            self.posted
                .lock()
                .unwrap()
                .push((owner.into(), name.into(), issue.clone()));
            Ok(theodore_ports::CreatedIssue {
                number: 7,
                url: format!("https://github.com/{owner}/{name}/issues/7"),
            })
        }
    }

    fn app(issues: Option<Arc<FakeTracker>>) -> (AppCore, theodore_app::Receivers) {
        let f = theodore_app::testkit::fixtures();
        let (core, rx) = Core::start(f.deps());
        let issues = issues.map(|t| t as Arc<dyn IssueTracker>);
        (AppCore::new(core).with_issues(issues), rx)
    }

    fn request(draft: Option<String>) -> theodore_contract::IssueRequest {
        theodore_contract::IssueRequest {
            title: "  Diff pane is blank ".into(),
            comment: "It shows nothing after a push.".into(),
            with_logs: false,
            draft,
        }
    }

    #[tokio::test]
    async fn without_a_tracker_nothing_is_ever_posted() {
        let (app, _rx) = app(None);
        let target = app.issue_target().await.unwrap();
        assert_eq!(target.login, None);
        assert_eq!(
            target.unavailable.as_deref(),
            Some("Fixture mode never posts to GitHub.")
        );
        let e = app.submit_issue(request(None)).await.unwrap_err();
        assert_eq!(e.code, "unavailable");
    }

    #[tokio::test]
    async fn the_target_names_the_repo_and_the_login() {
        let (app, _rx) = app(Some(Arc::new(FakeTracker::default())));
        let t = app.issue_target().await.unwrap();
        let (owner, name) = issue::target_repo(None);
        assert_eq!(t.repo, format!("{owner}/{name}"));
        assert_eq!(t.url, format!("https://github.com/{owner}/{name}"));
        assert_eq!((t.login.as_deref(), t.unavailable), (Some("octocat"), None));
    }

    #[tokio::test]
    async fn signed_out_or_expired_github_says_why() {
        let signed_out = Arc::new(FakeTracker {
            signed_out: true,
            ..Default::default()
        });
        let (a, _rx) = app(Some(signed_out));
        let t = a.issue_target().await.unwrap();
        assert_eq!(
            t.unavailable.as_deref(),
            Some("Sign in to GitHub to open an issue.")
        );

        let expired = Arc::new(FakeTracker {
            expired: true,
            ..Default::default()
        });
        let (a, _rx) = app(Some(expired));
        let t = a.issue_target().await.unwrap();
        assert!(t.unavailable.unwrap().contains("sign-in expired"));
        let e = a.submit_issue(request(None)).await.unwrap_err();
        assert_eq!(e.code, "github_auth");
        assert!(e.message.starts_with("GitHub sign-in expired"), "{e:?}");
    }

    #[tokio::test]
    async fn the_posted_issue_is_exactly_the_preview() {
        let tracker = Arc::new(FakeTracker::default());
        let (app, _rx) = app(Some(tracker.clone()));
        let preview = app.preview_issue(request(None)).await.unwrap();
        assert_eq!(preview.title, "Diff pane is blank");
        assert!(
            preview
                .body
                .starts_with("It shows nothing after a push.\n\n| App info | |")
        );
        assert!(
            preview
                .body
                .contains(&format!("| Version | {} |", build_info::VERSION))
        );
        assert_eq!((preview.logs, preview.log_lines), (None, 0));

        let issue = app
            .submit_issue(request(Some(preview.draft.clone())))
            .await
            .unwrap();
        assert_eq!(issue.number, 7);
        let posted = tracker.posted.lock().unwrap();
        let (owner, name, sent) = &posted[0];
        assert_eq!((owner.clone(), name.clone()), issue::target_repo(None));
        assert_eq!(sent.title, preview.title);
        assert_eq!(sent.body, preview.body);
        assert_eq!(sent.labels, ["from-app"]);
    }

    #[tokio::test]
    async fn title_and_comment_are_required() {
        let tracker = Arc::new(FakeTracker::default());
        let (app, _rx) = app(Some(tracker.clone()));
        let mut no_title = request(None);
        no_title.title = " ".into();
        let e = app.submit_issue(no_title).await.unwrap_err();
        assert_eq!(
            (e.code.as_str(), e.message.as_str()),
            ("invalid", "a title is required")
        );
        let mut no_comment = request(None);
        no_comment.comment = String::new();
        let e = app.preview_issue(no_comment).await.unwrap_err();
        assert_eq!(e.message, "a comment is required");
        assert!(tracker.posted.lock().unwrap().is_empty());
    }
}
