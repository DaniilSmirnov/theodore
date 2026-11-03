//! Spaces (docs/spaces.md): the stage before a pull request — an agent
//! writes the change in its own worktree, you review it, then it becomes a
//! PR. Hidden behind the UI's `FEATURES.buildMode` / `labs.build_mode`.

use serde::{Deserialize, Serialize};
use specta::Type;
use theodore_domain::{AgentId, Millis, PrId, Sha};

use crate::{MergeMethod, SpaceCheck, SpaceProcess, SpaceSetup};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SpaceStatus {
    /// No turn running.
    Idle,
    Running,
    /// The last turn errored, was interrupted, or the worktree is gone.
    Failed,
    Archived,
}

/// What the agent may do in a turn (docs/spaces.md §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SpaceMode {
    /// Read, grep, MCP; no writes.
    Ask,
    /// Edit files inside the space.
    Edit,
    /// Edit, plus run shell commands (tests, installs) without asking. No
    /// sandbox of Theodore's own: the agent CLI runs as configured.
    Run,
    /// Read and explore, no edits; the turn ends with a plan you approve
    /// (the next turn implements it in `Space::build_mode`) or revise.
    Plan,
}

/// How a space was shipped (docs/spaces.md §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ShipKind {
    /// Pushed to the host; `Space::pr` is (or is about to be) its PR.
    Pr,
    /// Merged into a local folder's checked-out branch.
    MergedLocally,
    /// Written as a branch in a local folder.
    Branch,
}

/// The space's last ship: the squashed commit and where it went.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SpaceShipped {
    pub how: ShipKind,
    /// Branch it was pushed to / written as (the folder's branch for
    /// `merged_locally`).
    pub branch: String,
    /// The squashed commit last pushed or merged
    /// (`refs/theodore/shipped/<id>`).
    pub sha: Sha,
    /// The checkpoint it was made from: turns after it are "not pushed".
    pub head: Sha,
    /// Pushes to the PR branch so far, the first one included.
    pub pushes: u32,
    pub at: Millis,
}

/// Synced row `space`, id = `Space::id`. Core-owned: written only by the
/// space commands, never by a client transaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct Space {
    pub id: String,
    /// `Project::id` the space belongs to.
    pub project: String,
    pub name: String,
    /// What the space is for; sent as the first turn.
    pub goal: String,
    /// Branch name the work is shipped as: `<host login>/<slug>`, or
    /// `space/<slug>` with no login (a local folder, or offline).
    pub branch: String,
    /// Base branch name, and the commit the space forked from.
    pub base: String,
    pub base_sha: Sha,
    /// Last checkpoint (`refs/theodore/spaces/<id>`).
    pub head_sha: Sha,
    /// Absolute path of the space's worktree.
    pub worktree: String,
    pub status: SpaceStatus,
    pub agent: AgentId,
    #[serde(default)]
    pub model: Option<String>,
    pub mode: SpaceMode,
    /// The branch review (`open_branch` plumbing) the live diff and Review use.
    #[serde(default)]
    pub review_pr: Option<PrId>,
    /// The host PR, once opened: the link from drafting to review.
    #[serde(default)]
    pub pr: Option<PrId>,
    pub turns: u32,
    /// Run id of the turn in flight (`space:<id>:<message>`); its streaming
    /// text arrives as `Ephemeral::AgentDelta` under this id.
    #[serde(default)]
    pub run: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    /// The last Open PR / Push / Merge locally / Keep as branch.
    #[serde(default)]
    pub shipped: Option<SpaceShipped>,
    /// When the change was done: its PR merged, or it was merged locally /
    /// kept as a branch. `spaces.auto_archive_days` counts from here.
    #[serde(default)]
    pub done_at: Option<Millis>,
    /// Every turn's checkpoints, oldest first: what "Undo last turn" moves
    /// back to. Spaces from before it was recorded have none.
    #[serde(default)]
    pub turn_log: Vec<SpaceTurn>,
    /// TCP ports reserved for the space's life (`spaces.ports`): free when
    /// reserved, unique across live spaces, released on archive.
    #[serde(default)]
    pub ports: Vec<u16>,
    /// The "Setting up…" stage: copy, then `spaces.setup`, before turn 1.
    #[serde(default)]
    pub setup: Option<SpaceSetup>,
    /// The `spaces.run` command, while running or since it last ended.
    #[serde(default)]
    pub process: Option<SpaceProcess>,
    /// "Not now" to the repo's commands: they are skipped in this space.
    #[serde(default)]
    pub commands_denied: bool,
    /// The agent session the last turn ran in: the next turn with the same
    /// agent continues it instead of replaying the transcript.
    #[serde(default)]
    pub agent_session: Option<AgentSession>,
    /// Items a "Send to agent" already sent, and which turn carries them
    /// (docs/spaces.md §3 "Send N to agent"): kept so the same draft,
    /// finding or thread isn't sent twice, and so a finding it addressed
    /// can be dismissed in bulk once that turn ends.
    #[serde(default)]
    pub sent_feedback: Vec<SentFeedback>,
    pub created_at: Millis,
    pub updated_at: Millis,
    /// A short chat title the chat agent proposed from the goal, shown
    /// instead of its raw first line (`ui/src/ui/space/spaces.ts`'s
    /// `defaultTitle`); `None` until that run lands or when it failed.
    #[serde(default)]
    pub title: Option<String>,
    /// The earlier conversation, summarized once and cached: what a turn
    /// that can't resume its agent session prepends instead of replaying
    /// the whole transcript (docs/spaces.md §5 "One session per space").
    #[serde(default)]
    pub handoff_summary: Option<String>,
    /// The running turn's progress tracker (`update_progress`); cleared
    /// when the turn ends.
    #[serde(default)]
    pub progress: Option<crate::SpaceProgress>,
    /// An Update from base Theodore started and the agent is resolving:
    /// set while its merge conflicts (docs/spaces.md §6).
    #[serde(default)]
    pub updating: Option<SpaceUpdate>,
    /// Earlier rounds, oldest first: each PR of the space that merged
    /// before "Continue" moved it to a new branch (docs/spaces.md §7).
    #[serde(default)]
    pub rounds: Vec<SpaceRound>,
    /// Told to the agent at the start of the next turn, then cleared: the
    /// previous PR merged and the work goes on on a new branch.
    #[serde(default)]
    pub continue_note: Option<String>,
    /// Set when the space started on an existing branch or PR
    /// (`CreateSpaceRequest::start_from`): its tip when the space forked.
    /// Shipping fast-forwards onto `branch` from here instead of squashing
    /// onto `base_sha` — `branch` already exists on the host.
    #[serde(default)]
    pub fork_sha: Option<Sha>,
    /// Set when the space started from a host issue: its number in the
    /// project's repo. `ship_space`'s PR body says "Closes #<n>".
    #[serde(default)]
    pub issue: Option<u64>,
    /// Pinned spaces sort first in the sidebar and ⌘K (docs/spaces.md §3).
    /// Set only by `set_space_pinned`, never by a client transaction.
    #[serde(default)]
    pub pinned: bool,
    /// The mode that writes, Edit or Run, a turn last ran in: what an
    /// approved plan is implemented in. `None` = Edit.
    #[serde(default)]
    pub build_mode: Option<SpaceMode>,
    /// The agent's `ask_user` questions waiting for your answer; the turn
    /// is paused on them.
    #[serde(default)]
    pub question: Option<SpaceQuestion>,
    /// Mirroring into the project's local folder (docs/spaces.md "Mirror
    /// into a local folder"); `None` when it was never turned on, or after
    /// Mirror off. At most one space of a project mirrors at a time.
    #[serde(default)]
    pub mirror: Option<SpaceMirror>,
    /// The worktree's own `HEAD` when `head_sha` was taken (a checkpoint
    /// is a snapshot on top of it); `None`: `head_sha` is that `HEAD`
    /// (a new space, or a checkpoint from before snapshots).
    #[serde(default)]
    pub worktree_head: Option<Sha>,
}

/// A merge of the base into the space, left in progress on conflicts
/// (docs/spaces.md §6 "Update from base"). Theodore commits it once the
/// agent has resolved them; Abort goes back to `from`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SpaceUpdate {
    /// The base's tip being merged in; `base_sha` once committed.
    pub base_tip: Sha,
    /// The checkpoint the merge started from.
    pub from: Sha,
    /// Files with conflicts, as of the last look.
    #[serde(default)]
    pub conflicts: Vec<String>,
    pub started_at: Millis,
}

/// How far the space's base branch moved on since its `base_sha`, as of
/// the project's last fetch (`space_base_status`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SpaceBaseStatus {
    /// The base branch's tip in Theodore's copy of the repo.
    pub base_tip: Sha,
    /// Commits on the base since `base_sha`.
    pub behind: u32,
}

/// Questions the agent asked mid-turn (`ask_user` on `space-tools`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SpaceQuestion {
    pub id: String,
    /// The turn that asked (`Space::run`).
    pub run: String,
    /// 1–3, each with 2–4 options; "Other" is always offered too.
    pub questions: Vec<SpaceQuestionItem>,
    pub asked_at: Millis,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SpaceQuestionItem {
    pub question: String,
    pub options: Vec<String>,
    /// Several options may be picked.
    #[serde(default)]
    pub multi: bool,
}

/// Your answers to a [`SpaceQuestion`], one per question, in order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SpaceAnswerRequest {
    pub space: String,
    /// `SpaceQuestion::id`.
    pub question: String,
    pub answers: Vec<SpaceAnswerItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SpaceAnswerItem {
    /// The options picked, as written.
    #[serde(default)]
    pub selected: Vec<String>,
    /// Free text under "Other".
    #[serde(default)]
    pub other: Option<String>,
}

/// One merged round of a continued space.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SpaceRound {
    /// The PR that merged, and its branch.
    pub pr: PrId,
    pub branch: String,
    /// The base the round forked from, and its last pushed checkpoint.
    pub base_sha: Sha,
    pub head_sha: Sha,
    /// When "Continue" started the next round.
    pub at: Millis,
}

/// One agent CLI session a space's turns continue (docs/spaces.md §5
/// "One session per space"): Claude Code's session id, Codex's thread id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct AgentSession {
    pub agent: AgentId,
    pub id: String,
    /// The worktree it ran in. Claude Code keeps sessions per directory, so
    /// one from another path can't be resumed.
    pub cwd: String,
}

/// One agent turn of a space: the messages it answered and the checkpoint
/// it moved the space from and to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SpaceTurn {
    /// The `chat_message` id of your message that started it.
    pub message: String,
    /// The `chat_message` id of the agent's reply.
    pub reply: String,
    /// Checkpoint before the turn, and after it (equal when nothing changed).
    pub before: Sha,
    pub after: Sha,
    /// Undone: the space went back to `before`; the transcript keeps the
    /// turn, the next turns' prompts leave it out.
    #[serde(default)]
    pub undone: bool,
    /// `spaces.check` after the turn, when it changed files.
    #[serde(default)]
    pub check: Option<SpaceCheck>,
    /// Messages of yours sent into it while it ran ("Send now"):
    /// `chat_message` ids, undone with it.
    #[serde(default)]
    pub steered: Vec<String>,
    /// The turn completed an Update from base: the `base_sha` before it,
    /// which Undo puts back.
    #[serde(default)]
    pub base_before: Option<Sha>,
    /// A Plan-mode turn's plan: the reply shows as a plan card with
    /// Approve / Revise.
    #[serde(default)]
    pub plan: Option<String>,
    /// The worktree's `HEAD` at `before` and at `after` (a checkpoint is a
    /// snapshot on top of it); `None`: that checkpoint is `HEAD` itself.
    #[serde(default)]
    pub before_head: Option<Sha>,
    #[serde(default)]
    pub after_head: Option<Sha>,
    /// Not an agent turn but your Revert of these files in the diff: its
    /// `reply` is its own `message`, and the next turn's prompt says so.
    #[serde(default)]
    pub reverted: Vec<String>,
}

/// Where a new space's work starts (docs/spaces.md §3 "New space"), instead
/// of a fresh branch off the project's default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum SpaceStart {
    /// A host issue — a pasted URL, or `#<number>` / a bare number in the
    /// project's repo. `goal` becomes the issue's title + body + a link;
    /// the number is kept so `ship_space` can say "Closes #<number>".
    Issue(String),
    /// An existing remote branch: its tip becomes the worktree, the
    /// project's default branch stays `base`. Shipping pushes to this same
    /// branch (fast-forward, never forced) instead of `<login>/<slug>`.
    Branch(String),
    /// An existing PR's branch (same as `Branch`), plus `Space::pr` so the
    /// space opens "In review" right away.
    Pr(u64),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct CreateSpaceRequest {
    pub project: String,
    /// What should change. Sent as the first turn unless empty. Ignored
    /// (replaced by the issue's title + body) with `start_from: Issue`.
    pub goal: String,
    /// Defaults to a slug of the goal.
    #[serde(default)]
    pub name: Option<String>,
    /// Defaults to `<host login>/<name>` (`space/<name>` without a login);
    /// ignored with `start_from: Branch` / `Pr`, which keep that branch.
    #[serde(default)]
    pub branch: Option<String>,
    /// Defaults to the project's default branch.
    #[serde(default)]
    pub base: Option<String>,
    /// Defaults to the `agents.chat` setting.
    #[serde(default)]
    pub agent: Option<AgentId>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub start_from: Option<SpaceStart>,
}

/// New space's "From an issue" picker: the project's own open issues
/// assigned to the viewer (`HostAdapter::my_open_issues`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ProjectIssuesRequest {
    pub project: String,
}

/// One issue, wire shape of `theodore_ports::TrackedIssue`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SpaceIssue {
    pub number: u64,
    pub title: String,
    pub body: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SpaceSendRequest {
    pub space: String,
    pub text: String,
    /// Switches the space's agent from this turn on.
    #[serde(default)]
    pub agent: Option<AgentId>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub mode: Option<SpaceMode>,
    /// `save_attachment` ids sent with the message.
    #[serde(default)]
    pub attachments: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SpaceReviewRequest {
    pub space: String,
    /// Also group the diff by meaning (Review); `false` for the live diff.
    pub group: bool,
}

/// What "ship" does (docs/spaces.md §1, §12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ShipAction {
    /// Host project: squash, push the branch, open a ready PR.
    OpenPr,
    /// Same, as a draft.
    OpenDraftPr,
    /// The space has a PR: push the turns since as one new commit.
    Push,
    /// Local folder: merge into its checked-out branch.
    MergeLocally,
    /// Local folder: write the branch only.
    KeepBranch,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ShipSpaceRequest {
    pub space: String,
    pub action: ShipAction,
    /// Commit message / PR title; defaults to the goal's first line (Push:
    /// the latest message since the last push).
    #[serde(default)]
    pub title: Option<String>,
}

/// A review comment sent to a space's agent (docs/spaces.md §3): it must
/// belong to the space's PR or to its own branch review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum SpaceFeedback {
    /// A review thread (`Thread::id`), every comment quoted.
    Thread(String),
    /// A draft comment (`Draft::id`).
    Draft(String),
    /// A finding (`Finding::id`).
    Finding(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SpaceFeedbackRequest {
    pub space: String,
    pub items: Vec<SpaceFeedback>,
}

/// One item a "Send to agent" sent, and the turn it went as — `Space::turns`
/// once that turn finishes (`space_send_feedback`, docs/spaces.md §3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SentFeedback {
    pub item: SpaceFeedback,
    pub turn: u32,
}

/// "Fix with agent" on a space PR's failing checks (docs/spaces.md "Fix
/// CI"): fetches each failed job's log and sends them to the space's agent
/// as one turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SpaceFixCiRequest {
    pub space: String,
}

/// Merge a space's PR from the space page (docs/spaces.md §3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct MergeSpaceRequest {
    pub space: String,
    /// One of the repo's allowed methods; defaults to the host's default.
    #[serde(default)]
    pub method: Option<MergeMethod>,
}

/// What `merge_space` asked the host for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct MergeSpaceResult {
    pub method: MergeMethod,
    /// Auto-merge: the PR merges itself once its checks and reviews pass.
    pub auto: bool,
    /// Added to the base branch's merge queue instead.
    pub queue: bool,
}

/// "Pin"/"Unpin" on a space (docs/spaces.md §3): sidebar context menu and
/// ⌘K.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SetSpacePinnedRequest {
    pub space: String,
    pub pinned: bool,
}

/// Mirroring into a local folder project ("Try in my folder" — some
/// projects can't run from a separate worktree: fixed ports, one local
/// database, a docker compose stack, an IDE run config bound to the
/// folder's own path).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MirrorStatus {
    /// Writing into the folder after every turn and on "Sync now".
    On,
    /// Was on when Theodore last closed; nothing is written until you
    /// resume it (never auto-writing into your folder on startup).
    Paused,
}

/// `Space::mirror`: the folder's own state before mirroring began (what
/// Mirror off restores), and the running set of paths mirroring has ever
/// touched there (what Mirror off only needs to look at or rewrite).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SpaceMirror {
    pub status: MirrorStatus,
    /// The folder's `HEAD` when mirroring turned on.
    pub remembered_head: Sha,
    /// The folder's checked-out branch at that point, for display only.
    #[serde(default)]
    pub remembered_branch: Option<String>,
    /// The last space checkpoint written into the folder; `None` before
    /// the first sync lands.
    #[serde(default)]
    pub mirrored_head: Option<Sha>,
    /// Every path a sync has ever written or removed in the folder, oldest
    /// first, deduplicated: what Mirror off restores or checks for edits,
    /// even a path the space added and then removed again (no net
    /// difference between the remembered `HEAD` and the latest checkpoint,
    /// but still touched).
    #[serde(default)]
    pub touched: Vec<String>,
    pub updated_at: Millis,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct UnmirrorSpaceRequest {
    pub space: String,
    /// Overwrite edits made in the folder while mirroring was on, instead
    /// of refusing and naming them.
    #[serde(default)]
    pub discard: bool,
}
