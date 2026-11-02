//! REST v4 wire shapes. Only the fields the adapter reads; GitLab's
//! responses carry far more, `serde` ignores the rest.

use serde::{Deserialize, Serialize};

use crate::anchor::GitlabPosition;

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GlUser {
    pub id: u64,
    pub username: String,
    pub name: Option<String>,
    pub avatar_url: Option<String>,
}

/// The bits of a GitLab issue `get_issue` / `my_open_issues` read.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GlIssue {
    pub iid: u64,
    pub title: String,
    pub description: Option<String>,
    pub web_url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GlDiffRefs {
    pub base_sha: String,
    pub start_sha: String,
    pub head_sha: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GlHeadPipeline {
    pub status: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GlProject {
    pub path_with_namespace: String,
    #[serde(default)]
    pub description: Option<String>,
    /// `private` / `internal` / `public`.
    #[serde(default)]
    pub visibility: Option<String>,
    /// GitLab has no exact "last push" timestamp; this is the closest cheap
    /// proxy (bumped by pushes, MRs, issues, …).
    #[serde(default)]
    pub last_activity_at: Option<String>,
}

/// A merge request as returned by both the single-MR `GET` and the list
/// endpoints (`/merge_requests`, `/projects/:id/merge_requests`); GitLab
/// gives both the same shape.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GlMergeRequest {
    pub iid: u64,
    pub project_id: u64,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    pub state: String,
    #[serde(default)]
    pub draft: bool,
    pub web_url: String,
    pub source_branch: String,
    pub target_branch: String,
    pub sha: Option<String>,
    pub author: GlUser,
    #[serde(default)]
    pub assignees: Vec<GlUser>,
    #[serde(default)]
    pub reviewers: Vec<GlUser>,
    #[serde(default)]
    pub labels: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    pub diff_refs: Option<GlDiffRefs>,
    pub head_pipeline: Option<GlHeadPipeline>,
    /// String, and `"1000+"` once GitLab stops counting exactly.
    pub changes_count: Option<String>,
    /// Legacy three-value field (`can_be_merged` / `cannot_be_merged` /
    /// `unchecked`); kept alongside `detailed_merge_status` because it maps
    /// 1:1 onto `MergeStatus::mergeable`.
    pub merge_status: Option<String>,
    #[serde(default)]
    pub detailed_merge_status: Option<String>,
    #[serde(default)]
    pub merge_when_pipeline_succeeds: bool,
    pub merge_commit_sha: Option<String>,
    pub squash_commit_sha: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GlProjectSettings {
    /// `never` / `always` / `default_on` / `default_off`.
    pub squash_option: Option<String>,
    /// `merge` / `rebase_merge` / `ff`.
    pub merge_method: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GlAwardEmoji {
    pub id: u64,
    pub name: String,
    pub user: GlUser,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GlChange {
    #[serde(default)]
    pub diff: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GlChanges {
    #[serde(default)]
    pub changes: Vec<GlChange>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GlNote {
    pub id: u64,
    pub body: String,
    pub author: GlUser,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub system: bool,
    #[serde(default)]
    pub resolvable: bool,
    #[serde(default)]
    pub resolved: bool,
    pub position: Option<GitlabPosition>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GlDiscussion {
    pub id: String,
    pub notes: Vec<GlNote>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GlPipeline {
    pub id: u64,
    pub status: String,
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub web_url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GlApprovals {
    #[serde(default)]
    pub approved_by: Vec<GlApprovedBy>,
    pub approvals_required: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GlApprovedBy {
    pub user: GlUser,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GlCommit {
    pub id: String,
    pub title: String,
    pub author_name: String,
    pub author_email: Option<String>,
    pub committed_date: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GlTodo {
    pub target: GlMergeRequest,
}

#[derive(Debug, Serialize)]
pub(crate) struct CreateDraftNote<'a> {
    pub note: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<&'a GitlabPosition>,
}

#[derive(Debug, Serialize)]
pub(crate) struct NoteBody<'a> {
    pub body: &'a str,
}

#[derive(Debug, Serialize, Default)]
pub(crate) struct MergeBody<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_commit_message: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub squash_commit_message: Option<&'a str>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub squash: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub merge_when_pipeline_succeeds: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct UpdateMrBody<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reviewer_ids: Option<&'a [u64]>,
}

/// `POST /projects/:id/merge_requests`. GitLab has no draft flag on create:
/// a draft is a `Draft: ` title.
#[derive(Debug, Serialize)]
pub(crate) struct CreateMrBody<'a> {
    pub source_branch: &'a str,
    pub target_branch: &'a str,
    pub title: &'a str,
    pub description: &'a str,
}

/// Just the identity of an MR — what `create_pr`/`find_open_pr` hand back.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GlMrRef {
    pub iid: u64,
    pub project_id: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct AwardEmojiBody<'a> {
    pub name: &'a str,
}
