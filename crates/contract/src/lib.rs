//! UI ⇄ core contract (plan §3). Rust is the source of truth; TS is generated
//! into `ui/src/gen/contract.ts` by `cargo run -p theodore-contract --bin export-ts`.

use serde::{Deserialize, Serialize};
use specta::Type;
use theodore_domain::{
    AgentId, Anchor, Author, CommentId, Draft, Finding, Group, Grouping, HostKind, HostRepo,
    HunkId, Millis, Person, PrDetail, PrId, PrSummary, RepoRef, ReviewEvent, Revision, Sha,
    SnoozeUntil, Task, TaskId, Thread, ThreadId, ViewedState,
};

mod diff_media;
pub use diff_media::*;
mod interdiff;
pub use interdiff::*;
mod linear;
pub use linear::*;
mod login;
pub use login::*;
mod pr_meta;
pub use pr_meta::*;
mod pr_state;
pub use pr_state::*;
mod symbols;
pub use symbols::*;
mod code_intel;
pub use code_intel::*;
mod sandbox;
pub use sandbox::*;
mod project_state;
pub use project_state::*;
mod space_progress;
pub use space_progress::*;
mod space_checkpoints;
pub use space_checkpoints::*;
mod spaces;
pub use spaces::*;
mod space_queue;
pub use space_queue::*;
mod config;
pub use config::*;
mod account;
pub use account::*;
mod attachments;
pub use attachments::*;
mod telemetry;
pub use telemetry::*;
mod terminal;
pub use terminal::*;

/// Model names used in transactions and sync frames.
pub mod models {
    pub const PR: &str = "pr";
    pub const PR_DETAIL: &str = "pr_detail";
    pub const GROUPING: &str = "grouping";
    pub const HUNK_READ: &str = "hunk_read";
    pub const FILE_VIEWED: &str = "file_viewed";
    pub const THREAD: &str = "thread";
    pub const DRAFT: &str = "draft";
    pub const FINDING: &str = "finding";
    pub const TASK: &str = "task";
    pub const SKILL_RUN: &str = "skill_run";
    pub const CHAT_MESSAGE: &str = "chat_message";
    pub const INBOX_STATE: &str = "inbox_state";
    pub const SETTING: &str = "setting";
    pub const PROJECT: &str = "project";
    /// Spaces (docs/spaces.md); their chat lives in `chat_message` rows
    /// keyed `pr = "space:<id>"`.
    pub const SPACE: &str = "space";
    pub const ALL: &[&str] = &[
        PR,
        PR_DETAIL,
        GROUPING,
        HUNK_READ,
        FILE_VIEWED,
        THREAD,
        DRAFT,
        FINDING,
        TASK,
        SKILL_RUN,
        CHAT_MESSAGE,
        INBOX_STATE,
        SETTING,
        PROJECT,
        SPACE,
    ];
}

// ── sync (core → UI) ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
pub enum SyncCode {
    I,
    U,
    C,
    A,
    D,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct WireAction {
    pub id: u64,
    pub model: String,
    pub model_id: String,
    pub action: SyncCode,
    #[specta(type = Option<specta_typescript::Unknown>)]
    pub data: Option<serde_json::Value>,
}

/// Event `sync`: covers `(from_sync_id, last_sync_id]`. A UI whose own
/// `lastSyncId < from_sync_id` missed a frame and calls `catch_up`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SyncFrame {
    pub from_sync_id: u64,
    pub last_sync_id: u64,
    pub actions: Vec<WireAction>,
    /// Set on a `catch_up` response whose `since` fell behind the sync
    /// log's compaction horizon: `actions` is empty and cannot be trusted
    /// to close the gap — the caller must bootstrap from scratch instead
    /// of applying this frame incrementally. Always `false` otherwise.
    #[serde(default)]
    pub resync_required: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct WireRow {
    pub model: String,
    pub id: String,
    #[specta(type = specta_typescript::Unknown)]
    pub data: serde_json::Value,
    pub archived: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct Bootstrap {
    pub last_sync_id: u64,
    pub rows: Vec<WireRow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(tag = "scope", content = "pr", rename_all = "snake_case")]
pub enum BootstrapScope {
    /// Inbox rows, settings, tasks — everything not tied to an open PR.
    Global,
    /// Models of one open PR (lazy load).
    Pr(PrId),
}

// ── transactions (UI → core) ────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum MutationOp {
    Create {
        model: String,
        id: String,
        #[specta(type = specta_typescript::Unknown)]
        data: serde_json::Value,
    },
    /// `data` holds changed fields only.
    Update {
        model: String,
        id: String,
        #[specta(type = specta_typescript::Unknown)]
        data: serde_json::Value,
    },
    Archive {
        model: String,
        id: String,
    },
    Delete {
        model: String,
        id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct Transaction {
    /// Client-generated, for de-duplication on retry.
    pub id: String,
    pub ops: Vec<MutationOp>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TransactionResult {
    Ok {
        last_sync_id: u64,
    },
    /// Rejected by domain validation; the UI rolls back.
    Rejected {
        reason: String,
    },
}

// ── commands (request/response) ─────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct OpenPrRequest {
    /// Host URL (`https://github.com/o/r/pull/12`) or local id.
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct OpenPrResponse {
    pub pr: PrId,
    pub head: Sha,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct RevisionRequest {
    pub pr: PrId,
    pub head: Sha,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct FileRequest {
    pub pr: PrId,
    pub rev: Sha,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct RunSkillRequest {
    pub pr: PrId,
    pub skill: String,
    /// Empty = use the `agents.skills` setting.
    pub agents: Vec<AgentId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct AskRequest {
    pub pr: PrId,
    pub question: String,
    pub about: Option<Anchor>,
    pub group: Option<String>,
    pub agent: Option<AgentId>,
    /// `save_attachment` ids sent with the question.
    #[serde(default)]
    pub attachments: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SubmitReviewRequest {
    pub pr: PrId,
    pub event: ReviewEvent,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct AgentStatus {
    pub id: AgentId,
    pub installed: bool,
    pub version: Option<String>,
    pub path: Option<String>,
}

/// One model an agent CLI can run, for the model picker (`agent_models`).
/// `id` is what `agents.<id>.model` stores; `efforts` what
/// `agents.<id>.effort` may hold for it (empty = no effort choice).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct AgentModel {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub efforts: Vec<String>,
    /// The effort the CLI uses when none is set, if it names one.
    pub default_effort: Option<String>,
    /// An earlier version the CLI still runs: under "More models".
    pub older: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct AppInfo {
    pub version: String,
    /// The full commit sha the build came from; `-dirty` for a dev build
    /// with uncommitted changes, `unknown` without git.
    pub commit: String,
    /// `production` | `staging` | `nightly` | `dev`.
    pub build_env: String,
    /// When the build was made (unix millis).
    pub built_at: Millis,
    /// The channel the updater polls: `updates.channel`, or `staging` in
    /// a staging build.
    pub channel: String,
    pub update: Option<UpdateInfo>,
    /// Set only when running against the sandbox host (docs/sandbox.md).
    #[serde(default)]
    pub sandbox: Option<SandboxInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct UpdateInfo {
    pub version: String,
    pub notes: String,
    pub downloaded: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct CommandError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum TokenSource {
    Keychain,
    Gh,
    Env,
    None,
    /// The simulated sandbox host (docs/sandbox.md): no real token at all.
    Sandbox,
}

/// Host connection state for onboarding and Settings → Account.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct HostAuthStatus {
    pub host: HostKind,
    pub connected: bool,
    pub login: Option<String>,
    pub source: TokenSource,
    /// Whether `login_start`'s device flow has a client id configured
    /// (plan "login"): with none, the UI hides that button and keeps the
    /// token/`gh` paths.
    #[serde(default)]
    pub device_login_available: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SetTokenRequest {
    pub host: HostKind,
    /// `None` removes the stored token.
    pub token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ReplyRequest {
    pub pr: PrId,
    pub thread: ThreadId,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ResolveRequest {
    pub pr: PrId,
    pub thread: ThreadId,
    pub resolved: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SkillInfo {
    pub id: String,
    pub title: String,
    pub description: String,
    /// `built-in`, `~/.claude/skills` or `<owner>/<repo> · .claude/skills`
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct DelegateRequest {
    pub task: TaskId,
    /// Empty = use the `agents.fix` setting.
    pub agent: Option<AgentId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct TaskDecision {
    pub task: TaskId,
    pub accept: bool,
    /// Why a fix was rejected (sent back to the agent on the next attempt).
    pub reason: Option<String>,
}

// ── projects (connected repositories) ───────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ProjectStatus {
    Cloning,
    Ready,
    Error,
}

/// A connected repository: a host repo (bare clone under `~/.theodore/repos`)
/// or a local folder (mirrored into the same cache, the user's repo is never
/// written to). Synced row `project`, id = `Project::id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct Project {
    /// `github:owner/name`, `gitlab:group/name`, or `local:<name>`.
    pub id: String,
    pub repo: RepoRef,
    /// Absolute path of a local folder project.
    pub path: Option<String>,
    pub status: ProjectStatus,
    pub error: Option<String>,
    pub default_branch: Option<String>,
    pub added_at: Millis,
    /// Host-side repo description, filled by `refresh_project` (empty for a
    /// local project, or before the first successful refresh).
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub private: bool,
    /// Host's last push to any branch, not just the default one.
    #[serde(default)]
    pub pushed_at: Option<Millis>,
    /// When `refresh_project` last completed, host or local — drives the
    /// header's "fetched Nm ago".
    #[serde(default)]
    pub last_fetched_at: Option<Millis>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct AddProjectRequest {
    /// A host repo (from `list_host_repos`), or …
    pub repo: Option<RepoRef>,
    /// … an absolute path to a local git repository.
    pub path: Option<String>,
}

/// A branch of a project, for reviewing work without a host PR.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct LocalBranch {
    pub name: String,
    pub head: Sha,
    /// Last commit subject.
    pub subject: String,
    /// Last commit author's name.
    #[serde(default)]
    pub author: String,
    /// Commits ahead of / behind the project's default branch.
    pub ahead: u32,
    pub behind: u32,
    pub updated_at: Millis,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct OpenBranchRequest {
    pub project: String,
    pub branch: String,
    /// Defaults to the project's default branch.
    pub base: Option<String>,
}

// ── host writes (merge, reviewers, comments) ────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MergeState {
    Clean,
    Blocked,
    Behind,
    Dirty,
    Unstable,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct MergeInfo {
    pub mergeable: Option<bool>,
    pub state: MergeState,
    pub allowed_methods: Vec<MergeMethod>,
    pub auto_merge_enabled: bool,
    pub in_queue: bool,
    pub required_approvals: Option<u32>,
    #[serde(default)]
    pub auto_merge_allowed: bool,
    #[serde(default)]
    pub merge_queue: bool,
    /// The method the host would pick (GitHub: your last one, else the
    /// repo's default); `None` when it doesn't say.
    #[serde(default)]
    pub default_method: Option<MergeMethod>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct MergeRequest {
    pub pr: PrId,
    pub method: MergeMethod,
    pub title: Option<String>,
    pub body: Option<String>,
    /// Merge when requirements pass instead of now.
    pub auto: bool,
    /// Add to the merge queue instead of merging.
    pub queue: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ReviewersRequest {
    pub pr: PrId,
    pub add: Vec<String>,
    pub add_teams: Vec<String>,
    pub remove: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct DraftStateRequest {
    pub pr: PrId,
    pub draft: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ReactRequest {
    pub pr: PrId,
    pub comment: CommentId,
    /// GitHub content name: +1, -1, laugh, hooray, confused, heart, rocket, eyes.
    pub emoji: String,
    pub on: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct EditCommentRequest {
    pub pr: PrId,
    pub comment: CommentId,
    /// `None` deletes the comment.
    pub body: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct CommitSuggestionsRequest {
    pub pr: PrId,
    /// Review comments whose ```suggestion blocks to apply.
    pub comments: Vec<CommentId>,
    pub message: String,
}

// ── skills ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SkillBody {
    pub info: SkillInfo,
    /// Full SKILL.md text (front matter included).
    pub body: String,
    pub editable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SaveSkillRequest {
    pub id: String,
    /// `user` (~/.claude/skills) or `repo` (the PR repo's .claude/skills —
    /// committed on a branch only when the user asks).
    pub scope: String,
    pub pr: Option<PrId>,
    pub body: String,
}

// ── ephemeral events (core → UI, not persisted) ─────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Ephemeral {
    AgentDelta {
        run: String,
        agent: AgentId,
        text: String,
    },
    RunLog {
        run: String,
        agent: AgentId,
        line: String,
    },
    /// No output for `agents.stall_after_ms` while `run` is still alive.
    /// The run keeps going (this isn't terminal) — the UI offers Retry
    /// (cancel + resend) or Cancel instead of waiting silently.
    RunStalled {
        run: String,
        agent: AgentId,
    },
    Toast {
        level: ToastLevel,
        text: String,
    },
    /// Grouping of a PR started or finished (no sync row until it's done).
    Grouping {
        pr: PrId,
        running: bool,
        error: Option<String>,
    },
    HostStatus {
        online: bool,
        rate_limited_until: Option<Millis>,
        auth_ok: bool,
    },
    /// The Theodore account signed in, signed out, or its session ended.
    AccountStatus {
        status: AccountStatus,
    },
    /// One line of a space command's output (docs/config.md); the last
    /// lines are kept per space and returned by `space_logs`.
    SpaceLog {
        space: String,
        line: SpaceLogLine,
    },
    /// The PR a config save to a host project's repo pushed for was opened
    /// (or found open) by the outbox.
    ConfigPr {
        project: String,
        number: u64,
        /// Empty when the host didn't say.
        url: String,
    },
    /// `code_intel.install: ask` and a language's server is missing
    /// (docs/code-intel.md §5.1): offer `code_intel_install(language)`,
    /// or `code_intel_decline(language)`. Once per language per session.
    CodeIntelSuggest {
        /// The project whose worktree needs it, when known.
        project: Option<String>,
        language: String,
        /// The server that would be downloaded (`clangd`).
        server: String,
    },
    /// An install (`code_intel_install`, or `code_intel.install: always`)
    /// downloading `language`'s grammar or server: `downloading` at most
    /// four times a second per item, then `installed` or `failed`.
    CodeIntelProgress {
        /// The project whose worktree asked for it; `None` from Settings.
        project: Option<String>,
        language: String,
        what: CodeInstallItem,
        /// The grammar package (`tree-sitter-rust`) or server
        /// (`rust-analyzer`).
        name: String,
        state: CodeInstallProgress,
    },
    /// Raw PTY output of a space's terminal (docs/spaces.md "Terminal"),
    /// base64 (control sequences and all) so a chunk split mid UTF-8 still
    /// round-trips — `xterm.js`'s own parser tolerates that. `seq` is
    /// monotonic per terminal, for merging with `SpaceTerminalInfo::seq`'s
    /// scrollback seed, the same way `space_log` merges with `space_logs`.
    TerminalOutput {
        space: String,
        terminal: String,
        seq: u64,
        data: String,
    },
    /// The terminal's shell exited (on its own, or `close_space_terminal`).
    TerminalExit {
        space: String,
        terminal: String,
        exit_code: Option<i32>,
        error: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum ToastLevel {
    Info,
    Warn,
    Error,
}

// ── synced model payloads (data of WireRow / WireAction) ────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct InboxState {
    pub pr: PrId,
    pub done: bool,
    pub snooze: Option<SnoozeUntil>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct HunkRead {
    pub pr: PrId,
    pub hunk: HunkId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct FileViewedRow {
    pub pr: PrId,
    pub path: String,
    pub state: ViewedState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Done,
    Cancelled,
    Failed,
    Stalled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SkillRun {
    pub id: String,
    pub pr: PrId,
    pub skill: String,
    pub agents: Vec<AgentId>,
    pub status: RunStatus,
    pub error: Option<String>,
    pub started_at: Millis,
    /// Findings this run reported on paths the project config's
    /// `review.ignore_findings` matches: dropped, only counted.
    #[serde(default)]
    pub hidden_findings: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ChatMessage {
    pub id: String,
    pub pr: PrId,
    pub author: Author,
    pub text: String,
    pub refs: Vec<Anchor>,
    pub at: Millis,
    /// Set instead of a normal reply when the run behind this message
    /// errored, crashed, hit its runtime cap, or was cancelled — the error
    /// text (or a short cancelled/stalled note). The UI renders this as a
    /// "stopped" card with Retry instead of as a normal agent reply.
    #[serde(default)]
    pub failed: Option<String>,
    /// Files sent with your message; empty on replies and older rows.
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    /// A space message waiting for its turn (docs/spaces.md §8).
    #[serde(default)]
    pub queued: Option<QueuedSend>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct Setting {
    /// e.g. `agents.grouping`, `updates.channel`
    pub key: String,
    /// `user` or `repo:<owner>/<name>`
    pub scope: String,
    #[specta(type = specta_typescript::Unknown)]
    pub value: serde_json::Value,
}

/// Registers every type exported to TS.
pub fn types() -> specta::Types {
    let t = specta::Types::default()
        .register::<SyncFrame>()
        .register::<Bootstrap>()
        .register::<BootstrapScope>()
        .register::<Transaction>()
        .register::<TransactionResult>()
        .register::<OpenPrRequest>()
        .register::<OpenPrResponse>()
        .register::<RevisionRequest>()
        .register::<FileRequest>()
        .register::<RunSkillRequest>()
        .register::<AskRequest>()
        .register::<SubmitReviewRequest>()
        .register::<AgentStatus>()
        .register::<AgentModel>()
        .register::<AppInfo>()
        .register::<CommandError>()
        .register::<Ephemeral>()
        .register::<InboxState>()
        .register::<HunkRead>()
        .register::<FileViewedRow>()
        .register::<SkillRun>()
        .register::<ChatMessage>()
        .register::<Attachment>()
        .register::<SaveAttachmentRequest>()
        .register::<Setting>()
        .register::<PrSummary>()
        .register::<PrDetail>()
        .register::<Revision>()
        .register::<Grouping>()
        .register::<Group>()
        .register::<Thread>()
        .register::<Draft>()
        .register::<Finding>()
        .register::<Task>()
        .register::<Person>()
        .register::<HostAuthStatus>()
        .register::<SetTokenRequest>()
        .register::<ReplyRequest>()
        .register::<ResolveRequest>()
        .register::<SkillInfo>()
        .register::<DelegateRequest>()
        .register::<TaskDecision>()
        .register::<Project>()
        .register::<AddProjectRequest>()
        .register::<LocalBranch>()
        .register::<OpenBranchRequest>()
        .register::<RepoRef>()
        .register::<MergeInfo>()
        .register::<MergeRequest>()
        .register::<ReviewersRequest>()
        .register::<DraftStateRequest>()
        .register::<ReactRequest>()
        .register::<EditCommentRequest>()
        .register::<CommitSuggestionsRequest>()
        .register::<SkillBody>()
        .register::<SaveSkillRequest>()
        .register::<InterdiffRequest>()
        .register::<InterdiffEntry>()
        .register::<DeviceLogin>()
        .register::<LoginState>()
        .register::<HostRepo>()
        .register::<RerunCheckRequest>()
        .register::<TriageRequest>()
        .register::<TriageAction>()
        .register::<SetLinearTokenRequest>()
        .register::<LinearStatus>()
        .register::<LinearIssuesRequest>()
        .register::<LinearIssue>()
        .register::<FindSymbolRequest>()
        .register::<SymbolHit>()
        .register::<SymbolSource>()
        .register::<CodeIntelStatus>()
        .register::<CodeLsp>()
        .register::<CodeLspToggle>()
        .register::<CodeIntelLspRequest>()
        .register::<CodeLspScope>()
        .register::<CodeInstallPolicy>()
        .register::<CodeLanguage>()
        .register::<CodeServer>()
        .register::<CodeInstall>()
        .register::<CodeServerState>()
        .register::<CodeInstallItem>()
        .register::<CodeInstallProgress>()
        .register::<CodeHoverRequest>()
        .register::<CodeHover>()
        .register::<StackInfo>()
        .register::<ReactionCount>()
        .register::<DiffSide>()
        .register::<GetBlobRequest>()
        .register::<BlobResponse>()
        .register::<ConflictKind>()
        .register::<ConflictFile>()
        .register::<SandboxInfo>()
        .register::<SandboxStatus>()
        .register::<SandboxFaultRequest>()
        .register::<ProjectWorkingState>()
        .register::<Space>()
        .register::<SpaceStatus>()
        .register::<SpaceMode>()
        .register::<CreateSpaceRequest>()
        .register::<SpaceStart>()
        .register::<ProjectIssuesRequest>()
        .register::<SpaceIssue>()
        .register::<SpaceSendRequest>()
        .register::<SpaceReviewRequest>()
        .register::<ShipKind>()
        .register::<SpaceShipped>()
        .register::<ShipAction>()
        .register::<ShipSpaceRequest>()
        .register::<MergeSpaceRequest>()
        .register::<MergeSpaceResult>()
        .register::<SpaceRound>()
        .register::<SpaceTurn>()
        .register::<RewindSpaceRequest>()
        .register::<SpaceRevertFrom>()
        .register::<RevertSpaceFileRequest>()
        .register::<AgentSession>()
        .register::<SpaceFeedback>()
        .register::<SpaceFeedbackRequest>()
        .register::<QueuedSend>()
        .register::<QueuedAction>()
        .register::<SpaceQueuedRequest>()
        .register::<SpaceUpdate>()
        .register::<SpaceBaseStatus>()
        .register::<SpaceFixCiRequest>()
        .register::<SetSpacePinnedRequest>()
        .register::<SpaceQuestion>()
        .register::<SpaceQuestionItem>()
        .register::<SpaceAnswerRequest>()
        .register::<SpaceAnswerItem>()
        .register::<MirrorStatus>()
        .register::<SpaceMirror>()
        .register::<UnmirrorSpaceRequest>()
        .register::<ConfigLayer>()
        .register::<ConfigFile>()
        .register::<ConfigEntry>()
        .register::<RepoCommands>()
        .register::<EffectiveConfig>()
        .register::<GlobalConfigInfo>()
        .register::<ConfigDraftFile>()
        .register::<ConfigDraft>()
        .register::<ConfigProblemLevel>()
        .register::<ConfigProblem>()
        .register::<CheckConfigRequest>()
        .register::<ConfigCheck>()
        .register::<SaveConfigRequest>()
        .register::<SaveConfigResult>()
        .register::<SpaceCommandKind>()
        .register::<ConfiguredCommand>()
        .register::<SpaceConfigView>()
        .register::<SetupStatus>()
        .register::<SpaceSetup>()
        .register::<ProcessStatus>()
        .register::<SpaceProcess>()
        .register::<SpaceCheckStatus>()
        .register::<SpaceCheck>()
        .register::<SpaceLogLine>()
        .register::<SpaceTrustRequest>()
        .register::<SetupAction>()
        .register::<SpaceSetupRequest>()
        .register::<SpaceRunRequest>()
        .register::<AccountUser>()
        .register::<AccountStatus>()
        .register::<AccountSignInMethod>()
        .register::<AccountSignInRequest>()
        .register::<AccountSignInStart>()
        .register::<AccountSignInState>()
        .register::<ClientLog>()
        .register::<ClientLogLevel>()
        .register::<UiEvent>()
        .register::<TelemetryEventInfo>()
        .register::<TelemetryStatus>()
        .register::<TelemetryChoice>()
        .register::<ReportFile>()
        .register::<ProblemReport>()
        .register::<ReportMode>()
        .register::<IssueTarget>()
        .register::<IssueRequest>()
        .register::<IssuePreview>()
        .register::<ReportedIssue>()
        .register::<TerminalStatus>()
        .register::<SpaceTerminalInfo>()
        .register::<OpenTerminalRequest>()
        .register::<WriteTerminalRequest>()
        .register::<ResizeTerminalRequest>();
    // JS numbers: ids, counts and unix-ms timestamps stay far below 2^53.
    use specta::datatype::Primitive as P;
    specta_util::Remapper::new()
        .rule(P::i64.into(), P::i32.into())
        .rule(P::u64.into(), P::u32.into())
        .rule(P::usize.into(), P::u32.into())
        .remap_types(t)
}
