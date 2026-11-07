//! REST v4 calls, grouped by resource. Pagination for list endpoints is left
//! to callers (via [`crate::pagination::paginate_all`]) since the page
//! parameter lives in the query string each caller already builds.

use theodore_host_http::HttpClient;
use theodore_ports::{Fetched, HostError, HostResult};

use crate::anchor::GitlabPosition;
use crate::model::{
    AwardEmojiBody, CreateDraftNote, CreateMrBody, GlApprovals, GlAwardEmoji, GlChanges, GlCommit,
    GlDiscussion, GlIssue, GlMergeRequest, GlMrRef, GlNote, GlPipeline, GlProject,
    GlProjectSettings, GlTodo, GlUser, MergeBody, NoteBody, UpdateMrBody,
};

pub(crate) async fn viewer(http: &HttpClient) -> HostResult<GlUser> {
    Ok(http.get::<GlUser>("user").await?.value)
}

pub(crate) async fn projects_page(
    http: &HttpClient,
    page: u32,
) -> HostResult<(Vec<GlProject>, reqwest::header::HeaderMap)> {
    let path = format!("projects?membership=true&min_access_level=20&page={page}&per_page=50");
    let resp = http.get::<Vec<GlProject>>(&path).await?;
    Ok((resp.value, resp.headers))
}

pub(crate) async fn list_merge_requests_page(
    http: &HttpClient,
    query: &str,
    page: u32,
) -> HostResult<(Vec<GlMergeRequest>, reqwest::header::HeaderMap)> {
    let path = format!("merge_requests?{query}&page={page}&per_page=50");
    let resp = http.get::<Vec<GlMergeRequest>>(&path).await?;
    Ok((resp.value, resp.headers))
}

pub(crate) async fn todos_page(
    http: &HttpClient,
    page: u32,
) -> HostResult<(Vec<GlTodo>, reqwest::header::HeaderMap)> {
    let path =
        format!("todos?state=pending&action=mentioned&type=MergeRequest&page={page}&per_page=50");
    let resp = http.get::<Vec<GlTodo>>(&path).await?;
    Ok((resp.value, resp.headers))
}

pub(crate) async fn get_merge_request(
    http: &HttpClient,
    project: &str,
    iid: u64,
    etag: Option<&str>,
) -> HostResult<Fetched<GlMergeRequest>> {
    let path = format!("projects/{project}/merge_requests/{iid}");
    match http.get_conditional::<GlMergeRequest>(&path, etag).await? {
        Fetched::NotModified => Ok(Fetched::NotModified),
        Fetched::Modified { etag, value } => Ok(Fetched::Modified {
            etag,
            value: value.value,
        }),
    }
}

pub(crate) async fn changes(http: &HttpClient, project: &str, iid: u64) -> HostResult<GlChanges> {
    let path = format!("projects/{project}/merge_requests/{iid}/changes");
    Ok(http.get::<GlChanges>(&path).await?.value)
}

pub(crate) async fn discussions_page(
    http: &HttpClient,
    project: &str,
    iid: u64,
    page: u32,
) -> HostResult<(Vec<GlDiscussion>, reqwest::header::HeaderMap)> {
    let path =
        format!("projects/{project}/merge_requests/{iid}/discussions?page={page}&per_page=50");
    let resp = http.get::<Vec<GlDiscussion>>(&path).await?;
    Ok((resp.value, resp.headers))
}

pub(crate) async fn pipelines(
    http: &HttpClient,
    project: &str,
    iid: u64,
) -> HostResult<Vec<GlPipeline>> {
    let path = format!("projects/{project}/merge_requests/{iid}/pipelines");
    Ok(http.get::<Vec<GlPipeline>>(&path).await?.value)
}

pub(crate) async fn approvals(
    http: &HttpClient,
    project: &str,
    iid: u64,
) -> HostResult<GlApprovals> {
    let path = format!("projects/{project}/merge_requests/{iid}/approvals");
    match http.get::<GlApprovals>(&path).await {
        // CE without approval rules, or the feature disabled: no useful
        // approvals data rather than a hard failure.
        Err(HostError::NotFound(_)) => Ok(GlApprovals {
            approved_by: vec![],
            approvals_required: None,
        }),
        other => Ok(other?.value),
    }
}

pub(crate) async fn commits_page(
    http: &HttpClient,
    project: &str,
    iid: u64,
    page: u32,
) -> HostResult<(Vec<GlCommit>, reqwest::header::HeaderMap)> {
    let path = format!("projects/{project}/merge_requests/{iid}/commits?page={page}&per_page=50");
    let resp = http.get::<Vec<GlCommit>>(&path).await?;
    Ok((resp.value, resp.headers))
}

pub(crate) async fn create_draft_note(
    http: &HttpClient,
    project: &str,
    iid: u64,
    note: &str,
    position: Option<&GitlabPosition>,
) -> HostResult<()> {
    let path = format!("projects/{project}/merge_requests/{iid}/draft_notes");
    let _: theodore_host_http::HttpResponse<serde_json::Value> = http
        .post_json(&path, &CreateDraftNote { note, position })
        .await?;
    Ok(())
}

pub(crate) async fn bulk_publish_draft_notes(
    http: &HttpClient,
    project: &str,
    iid: u64,
) -> HostResult<()> {
    let path = format!("projects/{project}/merge_requests/{iid}/draft_notes/bulk_publish");
    // GitLab answers this one with 204 and an empty body.
    http.post_no_content(&path, &serde_json::json!({})).await
}

pub(crate) async fn approve(http: &HttpClient, project: &str, iid: u64) -> HostResult<()> {
    let path = format!("projects/{project}/merge_requests/{iid}/approve");
    http.post_no_content(&path, &serde_json::json!({})).await
}

pub(crate) async fn reply_to_discussion(
    http: &HttpClient,
    project: &str,
    iid: u64,
    discussion_id: &str,
    body: &str,
) -> HostResult<GlNote> {
    let path = format!("projects/{project}/merge_requests/{iid}/discussions/{discussion_id}/notes");
    Ok(http.post_json(&path, &NoteBody { body }).await?.value)
}

pub(crate) async fn resolve_discussion(
    http: &HttpClient,
    project: &str,
    iid: u64,
    discussion_id: &str,
    resolved: bool,
) -> HostResult<()> {
    let path = format!(
        "projects/{project}/merge_requests/{iid}/discussions/{discussion_id}?resolved={resolved}"
    );
    let _: theodore_host_http::HttpResponse<serde_json::Value> =
        http.put_json(&path, &serde_json::json!({})).await?;
    Ok(())
}

pub(crate) async fn edit_note(
    http: &HttpClient,
    project: &str,
    iid: u64,
    note_id: &str,
    body: &str,
) -> HostResult<()> {
    let path = format!("projects/{project}/merge_requests/{iid}/notes/{note_id}");
    let _: theodore_host_http::HttpResponse<serde_json::Value> =
        http.put_json(&path, &NoteBody { body }).await?;
    Ok(())
}

pub(crate) async fn delete_note(
    http: &HttpClient,
    project: &str,
    iid: u64,
    note_id: &str,
) -> HostResult<()> {
    let path = format!("projects/{project}/merge_requests/{iid}/notes/{note_id}");
    http.delete(&path).await
}

// ── merge ───────────────────────────────────────────────────────────────

pub(crate) async fn merge_request_merge(
    http: &HttpClient,
    project: &str,
    iid: u64,
    body: &MergeBody<'_>,
) -> HostResult<GlMergeRequest> {
    let path = format!("projects/{project}/merge_requests/{iid}/merge");
    Ok(http.put_json(&path, body).await?.value)
}

pub(crate) async fn cancel_merge_when_pipeline_succeeds(
    http: &HttpClient,
    project: &str,
    iid: u64,
) -> HostResult<()> {
    let path =
        format!("projects/{project}/merge_requests/{iid}/cancel_merge_when_pipeline_succeeds");
    http.post_no_content(&path, &serde_json::json!({})).await
}

/// `POST /projects/:id/pipelines/:pipeline_id/retry`: re-runs every failed
/// (or canceled) job of a pipeline. A `Check` is one pipeline here (plan
/// "re-run a check"), so this is GitLab's whole re-run story — there's no
/// finer-grained per-job check the way GitHub's check runs are.
pub(crate) async fn retry_pipeline(
    http: &HttpClient,
    project: &str,
    pipeline_id: &str,
) -> HostResult<()> {
    let path = format!("projects/{project}/pipelines/{pipeline_id}/retry");
    http.post_no_content(&path, &serde_json::json!({})).await
}

pub(crate) async fn rebase(http: &HttpClient, project: &str, iid: u64) -> HostResult<()> {
    let path = format!("projects/{project}/merge_requests/{iid}/rebase");
    let _: theodore_host_http::HttpResponse<serde_json::Value> =
        http.put_json(&path, &serde_json::json!({})).await?;
    Ok(())
}

pub(crate) async fn update_merge_request(
    http: &HttpClient,
    project: &str,
    iid: u64,
    body: &UpdateMrBody<'_>,
) -> HostResult<GlMergeRequest> {
    let path = format!("projects/{project}/merge_requests/{iid}");
    Ok(http.put_json(&path, body).await?.value)
}

pub(crate) async fn project_settings(
    http: &HttpClient,
    project: &str,
) -> HostResult<GlProjectSettings> {
    let path = format!("projects/{project}");
    Ok(http.get::<GlProjectSettings>(&path).await?.value)
}

// ── create ──────────────────────────────────────────────────────────────

pub(crate) async fn open_merge_requests_for_source(
    http: &HttpClient,
    project: &str,
    source_branch: &str,
) -> HostResult<Vec<GlMrRef>> {
    let path = format!(
        "projects/{project}/merge_requests?state=opened&source_branch={}&per_page=10",
        urlencode(source_branch)
    );
    Ok(http.get::<Vec<GlMrRef>>(&path).await?.value)
}

/// GitLab answers 409 ("Another open merge request already exists for this
/// source branch") when `source_branch` already has an open MR; the shared
/// client already maps that to `HostError::Conflict`.
pub(crate) async fn create_merge_request(
    http: &HttpClient,
    project: &str,
    body: &CreateMrBody<'_>,
) -> HostResult<GlMrRef> {
    let path = format!("projects/{project}/merge_requests");
    Ok(http.post_json(&path, body).await?.value)
}

// ── reviewers ───────────────────────────────────────────────────────────

/// GitLab's user search is global, not scoped to a project; an exact
/// `username` filter returns at most one match.
pub(crate) async fn find_user_by_username(
    http: &HttpClient,
    username: &str,
) -> HostResult<Option<GlUser>> {
    let path = format!("users?username={}", urlencode(username));
    let users = http.get::<Vec<GlUser>>(&path).await?.value;
    Ok(users.into_iter().next())
}

pub(crate) async fn members_page(
    http: &HttpClient,
    project: &str,
    page: u32,
) -> HostResult<(Vec<GlUser>, reqwest::header::HeaderMap)> {
    let path = format!("projects/{project}/members/all?page={page}&per_page=50");
    let resp = http.get::<Vec<GlUser>>(&path).await?;
    Ok((resp.value, resp.headers))
}

// ── award emoji (reactions) ────────────────────────────────────────────

pub(crate) async fn award_emoji_create(
    http: &HttpClient,
    project: &str,
    iid: u64,
    note_id: &str,
    name: &str,
) -> HostResult<()> {
    let path = format!("projects/{project}/merge_requests/{iid}/notes/{note_id}/award_emoji");
    let _: theodore_host_http::HttpResponse<serde_json::Value> =
        http.post_json(&path, &AwardEmojiBody { name }).await?;
    Ok(())
}

pub(crate) async fn award_emoji_list(
    http: &HttpClient,
    project: &str,
    iid: u64,
    note_id: &str,
) -> HostResult<Vec<GlAwardEmoji>> {
    let path = format!("projects/{project}/merge_requests/{iid}/notes/{note_id}/award_emoji");
    Ok(http.get::<Vec<GlAwardEmoji>>(&path).await?.value)
}

pub(crate) async fn award_emoji_delete(
    http: &HttpClient,
    project: &str,
    iid: u64,
    note_id: &str,
    award_id: u64,
) -> HostResult<()> {
    let path =
        format!("projects/{project}/merge_requests/{iid}/notes/{note_id}/award_emoji/{award_id}");
    http.delete(&path).await
}

// ── issues (docs/spaces.md "New space from an issue") ──────────────────

pub(crate) async fn get_issue(http: &HttpClient, project: &str, iid: u64) -> HostResult<GlIssue> {
    let path = format!("projects/{project}/issues/{iid}");
    Ok(http.get::<GlIssue>(&path).await?.value)
}

/// `GET /projects/:id/issues?state=opened[&assignee_username=<login>]`.
pub(crate) async fn list_open_issues(
    http: &HttpClient,
    project: &str,
    assignee: Option<&str>,
) -> HostResult<Vec<GlIssue>> {
    let path = match assignee {
        Some(login) => format!(
            "projects/{project}/issues?state=opened&assignee_username={}&per_page=50",
            urlencode(login)
        ),
        None => format!("projects/{project}/issues?state=opened&per_page=50"),
    };
    Ok(http.get::<Vec<GlIssue>>(&path).await?.value)
}

fn urlencode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}
