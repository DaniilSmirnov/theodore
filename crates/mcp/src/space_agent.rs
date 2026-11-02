//! The `space-tools` that act on the change itself (docs/spaces.md "Tools
//! the agent gets"): `ship`, `get_space_diff`, `get_review_comments`,
//! `add_review_comment`, `set_base`, `update_progress`. Wire types here,
//! the work in [`crate::backend::SpaceToolsBackend`].

use rmcp::ErrorData;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use theodore_domain::FileStatus;

use crate::backend::{
    NewReviewComment, ProgressStep, ProgressStepState, ShipToolAction, SpaceDiff,
};
use crate::server::{ReviewToolsServer, run_space, space_result};
use crate::tools::{SeverityParam, render_unified_diff};

/// The most diff text one `get_space_diff` returns; `file` or `stat`
/// narrow a bigger change.
pub const MAX_DIFF_CHARS: usize = 120_000;

// ── ship ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShipActionParam {
    /// Host project: squash the turns into one commit by the user, push the
    /// space's branch, open a ready PR.
    OpenPr,
    /// The same, as a draft PR.
    OpenDraftPr,
    /// The space has a PR: push the turns since as one more commit.
    Push,
    /// Local folder: merge into its checked-out branch.
    MergeLocally,
    /// Local folder: write the space's branch only.
    KeepBranch,
}

impl From<ShipActionParam> for ShipToolAction {
    fn from(a: ShipActionParam) -> Self {
        match a {
            ShipActionParam::OpenPr => Self::OpenPr,
            ShipActionParam::OpenDraftPr => Self::OpenDraftPr,
            ShipActionParam::Push => Self::Push,
            ShipActionParam::MergeLocally => Self::MergeLocally,
            ShipActionParam::KeepBranch => Self::KeepBranch,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ShipParams {
    pub action: ShipActionParam,
    /// Commit message / PR title. Defaults to the goal's first line (Push:
    /// the user's latest message).
    pub title: Option<String>,
}

// ── get_space_diff ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SpaceDiffParams {
    /// Only this repo-relative path.
    pub file: Option<String>,
    /// Per-file `+`/`-` counts only, no diff text.
    pub stat: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct DiffFileResult {
    pub path: String,
    pub old_path: Option<String>,
    pub status: &'static str,
    pub additions: u32,
    pub deletions: u32,
}

#[derive(Debug, Serialize)]
pub struct SpaceDiffResult {
    pub base: String,
    /// The merge-base with `base`: what the change is diffed against.
    pub base_sha: String,
    pub head: String,
    /// Edits since the last checkpoint are included.
    pub uncommitted: bool,
    pub files: Vec<DiffFileResult>,
    pub additions: u32,
    pub deletions: u32,
    /// Unified diff text; absent with `stat`.
    pub diff: Option<String>,
    /// `diff` was cut at `MAX_DIFF_CHARS`: ask for one `file` at a time.
    pub truncated: bool,
}

fn status_str(s: FileStatus) -> &'static str {
    match s {
        FileStatus::Added => "added",
        FileStatus::Modified => "modified",
        FileStatus::Deleted => "deleted",
        FileStatus::Renamed => "renamed",
        FileStatus::Copied => "copied",
    }
}

impl SpaceDiffResult {
    pub fn new(d: SpaceDiff, stat: bool) -> Self {
        let (diff, truncated) = if stat {
            (None, false)
        } else {
            let hunks: Vec<_> = d.files.iter().flat_map(|f| f.hunks.clone()).collect();
            let text = render_unified_diff(&hunks);
            match text.char_indices().nth(MAX_DIFF_CHARS) {
                Some((cut, _)) => (Some(text[..cut].to_string()), true),
                None => (Some(text), false),
            }
        };
        SpaceDiffResult {
            base: d.base,
            base_sha: d.base_sha.0,
            head: d.head.0,
            uncommitted: d.uncommitted,
            additions: d.files.iter().map(|f| f.additions).sum(),
            deletions: d.files.iter().map(|f| f.deletions).sum(),
            files: d
                .files
                .into_iter()
                .map(|f| DiffFileResult {
                    status: status_str(f.status),
                    path: f.path,
                    old_path: f.old_path,
                    additions: f.additions,
                    deletions: f.deletions,
                })
                .collect(),
            diff,
            truncated,
        }
    }
}

// ── get_review_comments ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReviewCommentsParams {
    /// Only comments on this repo-relative path.
    pub file: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ReviewCommentsResult {
    pub items: Vec<crate::backend::ReviewItem>,
}

// ── add_review_comment ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddReviewCommentParams {
    /// Repo-relative path.
    pub file: String,
    /// 1-based line in the file as it is now.
    pub line: u32,
    /// The comment; its first line becomes the title.
    pub body: String,
    /// Default `info`.
    pub severity: Option<SeverityParam>,
}

#[derive(Debug, Serialize)]
pub struct AddReviewCommentResult {
    pub finding_id: String,
}

// ── set_base ─────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetBaseParams {
    /// The branch the change should merge into, e.g. `main` or `release/2.1`.
    pub branch: String,
}

// ── update_progress ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StepStateParam {
    Pending,
    Active,
    Done,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct StepParam {
    /// A few words: "Read the login flow".
    pub label: String,
    pub state: StepStateParam,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateProgressParams {
    /// The whole plan, in order; replaces the last one. Empty clears it.
    pub steps: Vec<StepParam>,
    /// One short line under the steps.
    pub note: Option<String>,
}

impl From<StepParam> for ProgressStep {
    fn from(s: StepParam) -> Self {
        ProgressStep {
            label: s.label,
            state: match s.state {
                StepStateParam::Pending => ProgressStepState::Pending,
                StepStateParam::Active => ProgressStepState::Active,
                StepStateParam::Done => ProgressStepState::Done,
            },
        }
    }
}

#[derive(Debug, Serialize)]
pub struct UpdateProgressResult {
    pub ok: bool,
}

#[tool_router(router = space_agent_router, vis = "pub(crate)")]
impl ReviewToolsServer {
    #[tool(
        description = "Ship the space the way its Ship menu does: open_pr / open_draft_pr / push (a host project) or merge_locally / keep_branch (a local folder). Theodore squashes the turns into one commit authored by the user and never force-pushes. Call it only when the user asks to push, open a PR or merge; never run git push, gh pr create or switch branches yourself. Returns the branch, the commit, the PR number and URL; a refusal says why."
    )]
    async fn ship(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(params): Parameters<ShipParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let (run, space) = run_space(&parts)?;
        space_result(
            self.spaces
                .ship(&space, &run.run_id, params.action.into(), params.title)
                .await,
        )
    }

    #[tool(
        description = "The space's change against the merge-base with its base branch, including edits not checkpointed yet. `file` narrows it to one path, `stat` returns per-file +/- counts only."
    )]
    async fn get_space_diff(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(params): Parameters<SpaceDiffParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let (_, space) = run_space(&parts)?;
        let stat = params.stat.unwrap_or(false);
        space_result(
            self.spaces
                .space_diff(&space, params.file.as_deref())
                .await
                .map(|d| SpaceDiffResult::new(d, stat)),
        )
    }

    #[tool(
        description = "Review comments on this space: drafts, findings and threads on its own review and on its PR, each with path:line, who wrote it (you = the user, an agent, or a reviewer's login) and its state (draft, open, resolved, outdated)."
    )]
    async fn get_review_comments(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(params): Parameters<ReviewCommentsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let (_, space) = run_space(&parts)?;
        space_result(
            self.spaces
                .review_comments(&space, params.file.as_deref())
                .await
                .map(|items| ReviewCommentsResult { items }),
        )
    }

    #[tool(
        description = "Leave a comment on a line of the space's change. It shows in the space's Review as a finding the user can send back to you. Returns its id."
    )]
    async fn add_review_comment(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(params): Parameters<AddReviewCommentParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let (run, space) = run_space(&parts)?;
        let comment = NewReviewComment {
            file: params.file,
            line: params.line,
            body: params.body,
            severity: params.severity.unwrap_or(SeverityParam::Info).into(),
        };
        space_result(
            self.spaces
                .add_review_comment(&space, run.agent, comment)
                .await
                .map(|id| AddReviewCommentResult {
                    finding_id: id.to_string(),
                }),
        )
    }

    #[tool(
        description = "Change the branch this space merges into. Theodore fetches it and diffs the space against their merge-base from now on; your files stay as they are. It does not retarget a PR that is already open. Call it only when the user asks."
    )]
    async fn set_base(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(params): Parameters<SetBaseParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let (run, space) = run_space(&parts)?;
        space_result(
            self.spaces
                .set_base(&space, &run.run_id, &params.branch)
                .await,
        )
    }

    #[tool(
        description = "Show your plan as a small progress tracker above the user's message box: every step with pending, active or done, plus an optional note. Each call replaces the last; it clears when the turn ends. Use it for work with several steps."
    )]
    async fn update_progress(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(params): Parameters<UpdateProgressParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let (run, space) = run_space(&parts)?;
        let steps = params.steps.into_iter().map(Into::into).collect();
        space_result(
            self.spaces
                .update_progress(&space, &run.run_id, steps, params.note)
                .await
                .map(|()| UpdateProgressResult { ok: true }),
        )
    }
}
