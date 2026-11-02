//! What a space's agent asks Theodore to do with the change itself
//! (docs/spaces.md "Tools the agent gets"): ship it, read it and its
//! review, comment on it, move its base, show its progress. Theodore does
//! each with its own guarantees (a squash commit by the user, never a
//! force push, the outbox); the agent never runs `git push` itself.
//!
//! ```text
//! agent ─▶ ship ──────────────▶ ship_space (Open PR · Push · Merge locally · Keep)
//!       ─▶ get_space_diff ────▶ base...worktree (uncommitted edits included)
//!       ─▶ get_review_comments ▶ drafts · findings · threads (review + PR)
//!       ─▶ add_review_comment ─▶ a finding on the space's review
//!       ─▶ set_base ──────────▶ base + merge-base, the live diff follows
//!       ─▶ update_progress ───▶ the tracker above the composer
//! ```

use serde::{Deserialize, Serialize};
use theodore_domain::{FileDiff, Severity, Sha};

/// `ship`: what to do, as the space page's Ship menu offers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShipToolAction {
    OpenPr,
    OpenDraftPr,
    Push,
    MergeLocally,
    KeepBranch,
}

/// `ship`: what happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShipOutcome {
    pub action: ShipToolAction,
    /// The branch pushed or written; a folder's checked-out branch for a
    /// local merge.
    pub branch: String,
    /// The squashed commit.
    pub sha: String,
    /// The host PR, once the host answered.
    pub pr_number: Option<u64>,
    pub pr_url: Option<String>,
    /// One line to tell the user.
    pub summary: String,
}

/// `get_space_diff`: the change against the merge-base with its base,
/// including what the agent wrote since the last checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceDiff {
    pub base: String,
    pub base_sha: Sha,
    /// The commit diffed: the last checkpoint, or a snapshot of the
    /// worktree when it has uncommitted edits.
    pub head: Sha,
    pub uncommitted: bool,
    pub files: Vec<FileDiff>,
}

/// Which review an item is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewSource {
    /// The space's own branch review (Review on the space page).
    Space,
    /// Its pull request on the host.
    Pr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewItemKind {
    Draft,
    Finding,
    Thread,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewItemState {
    /// A draft comment not submitted yet.
    Draft,
    Open,
    Resolved,
    /// The code moved under it since.
    Outdated,
}

/// One comment of a review item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewItemComment {
    /// `you` (the user), an agent (`claude`, …) or a reviewer's login.
    pub author: String,
    pub body: String,
}

/// `get_review_comments`: one draft, finding or thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewItem {
    pub kind: ReviewItemKind,
    pub id: String,
    pub on: ReviewSource,
    /// `src/a.ts:41`, or `pull request` for a thread on no line.
    pub location: String,
    pub path: Option<String>,
    pub line: Option<u32>,
    pub state: ReviewItemState,
    /// Findings only.
    pub severity: Option<Severity>,
    pub title: Option<String>,
    pub comments: Vec<ReviewItemComment>,
}

/// `add_review_comment`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewReviewComment {
    pub file: String,
    pub line: u32,
    pub body: String,
    pub severity: Severity,
}

/// `set_base`: where the space stands now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BaseChange {
    pub base: String,
    /// The merge-base of the space with the new base: what it diffs against.
    pub base_sha: String,
    pub previous: String,
    /// Set when the space already has a PR: its target stays as it was.
    pub note: Option<String>,
}

/// `update_progress`: one step of the tracker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressStepState {
    Pending,
    Active,
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressStep {
    pub label: String,
    pub state: ProgressStepState,
}
