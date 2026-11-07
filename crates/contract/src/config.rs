//! Project config (docs/config.md): one YAML file in three layers, and a
//! space's commands (`setup`, `run`, `check`, `archive`) with their trust
//! prompt, logs and results.

use serde::{Deserialize, Serialize};
use specta::Type;
use theodore_domain::Millis;

/// Which file a config value came from. Later layers win, key by key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ConfigLayer {
    /// `~/.theodore/config.yaml`.
    Global,
    /// `<repo>/.theodore/config.yaml` (or the legacy `.review/config.yaml`):
    /// shared with the team, so its commands need your approval.
    Repo,
    /// `<repo>/.theodore/config.local.yaml`: personal, gitignored.
    Local,
}

/// One of the files behind a project's config.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ConfigFile {
    pub layer: ConfigLayer,
    pub path: String,
    pub exists: bool,
    /// The deprecated `.review/config.yaml`, read because
    /// `.theodore/config.yaml` is absent.
    pub legacy: bool,
    /// Why the file was ignored (unreadable or invalid YAML).
    pub error: Option<String>,
}

/// One effective value and where it came from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ConfigEntry {
    /// Dotted path: `spaces.run`, `spaces.env.API_URL`, `review.agents.chat`.
    pub key: String,
    /// Strings as they are, anything else as JSON.
    pub value: String,
    pub layer: ConfigLayer,
}

/// The repo layer's commands (and env, which can change what a command
/// runs) that are in effect: what "Run commands from this repo?" shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct RepoCommands {
    /// sha256 of `items`; approval is stored per project against it.
    pub hash: String,
    /// You allowed exactly this set for this project.
    pub trusted: bool,
    pub items: Vec<ConfigEntry>,
}

/// A project's merged config, read-only (Settings → Labs).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct EffectiveConfig {
    pub files: Vec<ConfigFile>,
    pub entries: Vec<ConfigEntry>,
    /// Keys no Theodore version knows (typos, or a newer file).
    pub unknown: Vec<String>,
    /// `None` when the repo layer provides no command.
    pub repo_commands: Option<RepoCommands>,
}

/// `~/.theodore/config.yaml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct GlobalConfigInfo {
    pub path: String,
    pub exists: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SpaceCommandKind {
    Setup,
    Run,
    Check,
    Archive,
}

/// A space command as configured.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ConfiguredCommand {
    pub kind: SpaceCommandKind,
    pub script: String,
    pub layer: ConfigLayer,
    /// `false`: from the repo and not approved, so it is skipped.
    pub allowed: bool,
}

/// What a space will run (`space_config`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SpaceConfigView {
    pub commands: Vec<ConfiguredCommand>,
    /// `$THEODORE_PORT`, `$THEODORE_PORT_1`, …
    pub ports: Vec<u16>,
    pub repo_commands: Option<RepoCommands>,
    /// "Not now" was chosen for this space: repo commands are skipped.
    pub denied: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SetupStatus {
    /// Waiting for "Run commands from this repo?".
    Waiting,
    Running,
    Done,
    Failed,
    /// Nothing ran: no setup, not allowed, or "continue without setup".
    Skipped,
}

/// The "Setting up…" stage after create, before the first turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SpaceSetup {
    pub status: SetupStatus,
    /// The goal is sent as the first turn once setup ends.
    pub goal_pending: bool,
    #[serde(default)]
    pub error: Option<String>,
    /// Files not copied, commands skipped, …
    #[serde(default)]
    pub notes: Vec<String>,
    #[serde(default)]
    pub finished_at: Option<Millis>,
    /// A Retry took the command from the space's own worktree: the repo
    /// layer of `setup`, and of the trust prompt while it waits, is the
    /// worktree's file (docs/config.md §2 "Setup").
    #[serde(default)]
    pub from_worktree: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ProcessStatus {
    Running,
    /// Ended by itself.
    Exited,
    /// Stopped from the UI, by archive, or because Theodore closed.
    Stopped,
    /// Could not start.
    Failed,
}

/// The space's long-running `run` command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SpaceProcess {
    pub status: ProcessStatus,
    pub started_at: Millis,
    #[serde(default)]
    pub ended_at: Option<Millis>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub error: Option<String>,
    /// Process-group id of a `running` command (its own pid, since it's
    /// spawned with `process_group(0)`); cleared once it exits or is
    /// stopped. Set while `status` is `running`, so a restart can reap a
    /// group an unclean exit left running (docs/spaces.md "Crash recovery").
    #[serde(default)]
    pub pgid: Option<u32>,
    /// The `spaces.run` script started at `pgid`, kept so a restart can
    /// check the process still there is the one Theodore started — not a
    /// pid since reused by something else — before touching it.
    #[serde(default)]
    pub script: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SpaceCheckStatus {
    Running,
    Passed,
    Failed,
}

/// `check` after a turn that changed files.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SpaceCheck {
    pub status: SpaceCheckStatus,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub timed_out: bool,
    /// The last lines of output.
    #[serde(default)]
    pub output: Vec<String>,
    pub started_at: Millis,
    #[serde(default)]
    pub finished_at: Option<Millis>,
}

/// One line of a space command's output (`space_logs`, `Ephemeral::SpaceLog`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SpaceLogLine {
    pub kind: SpaceCommandKind,
    pub line: String,
    pub stderr: bool,
    pub at: Millis,
}

/// "Run commands from this repo?" for a space's project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SpaceTrustRequest {
    pub space: String,
    /// Allow (stored for the project) or Not now (this space only).
    pub allow: bool,
    /// `RepoCommands::hash` the user saw; refused if the commands changed.
    pub hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SetupAction {
    Retry,
    /// Continue without setup: the first turn starts.
    Skip,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SpaceSetupRequest {
    pub space: String,
    pub action: SetupAction,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SpaceRunRequest {
    pub space: String,
    /// Start (`true`) or stop the `run` command.
    pub start: bool,
}

// ── creating and saving a project's config (Settings → Projects) ─────────

/// One layer's file as the editor sees it: where saving to that layer
/// writes, and its current text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ConfigDraftFile {
    pub layer: ConfigLayer,
    /// Where it is read from (a host project's repo file: `<slug>@<sha>:…`
    /// on its default branch).
    pub path: String,
    /// Where saving to this layer writes: the same file, or for a host
    /// project's repo layer the branch its PR is pushed to.
    pub save_to: String,
    /// `None`: the file doesn't exist.
    pub text: Option<String>,
}

/// What the config editor starts from (`config_draft`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ConfigDraft {
    pub project: String,
    /// The project's current settings as YAML (per-task agents, values
    /// from the repo and local files), the rest of `review:` and `spaces:`
    /// as a commented template.
    pub generated: String,
    /// Global, repo, local.
    pub files: Vec<ConfigDraftFile>,
    /// The file to open first: the repo's, else the local one; `None` when
    /// neither exists (start from `generated`).
    pub initial: Option<ConfigLayer>,
    /// Local folder project: whether saving to the repo can also commit
    /// the file (the folder has no other uncommitted change).
    pub can_commit: bool,
    /// Why it can't, when it can't.
    pub commit_note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ConfigProblemLevel {
    /// The file would be ignored, or a section dropped: saving is refused.
    Error,
    /// Applied without the key (an unknown key).
    Warning,
}

/// A problem in the editor's text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ConfigProblem {
    pub level: ConfigProblemLevel,
    /// 1-based, when known.
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub message: String,
}

/// `check_config`: the editor's text, as it would be saved to `layer`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct CheckConfigRequest {
    pub project: String,
    pub layer: ConfigLayer,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ConfigCheck {
    pub problems: Vec<ConfigProblem>,
    /// The project's effective config with the text in place of `layer`.
    pub preview: EffectiveConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SaveConfigRequest {
    pub project: String,
    pub layer: ConfigLayer,
    pub text: String,
    /// Local folder project, repo layer: also commit the file.
    #[serde(default)]
    pub commit: bool,
}

/// What `save_config` did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SaveConfigResult {
    pub layer: ConfigLayer,
    /// The file written, or the branch pushed.
    pub path: String,
    /// One line for the toast.
    pub message: String,
    /// Host project: the branch pushed; its PR arrives as
    /// `Ephemeral::ConfigPr`.
    pub branch: Option<String>,
    /// The commit made (pushed, or in the folder).
    pub commit: Option<String>,
    /// The repo's commands now in effect were marked allowed.
    pub trusted: bool,
}
