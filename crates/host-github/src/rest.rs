//! REST calls: notification polling and the writes that don't have a
//! GraphQL equivalent worth using (comment edit/delete, issues, …).

use serde::Deserialize;
use theodore_host_http::HttpClient;
use theodore_ports::{HostError, HostResult, NewIssue, NewPr};

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GhNotification {
    pub subject: GhSubject,
    pub repository: GhRepository,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GhSubject {
    #[serde(rename = "type")]
    pub kind: String,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GhRepository {
    pub name: String,
    pub owner: GhOwner,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GhOwner {
    pub login: String,
}

impl GhNotification {
    /// The PR number, parsed from the subject's API URL
    /// (`.../repos/o/r/pulls/42`). `None` for non-PR notifications.
    pub(crate) fn pr_number(&self) -> Option<u64> {
        if self.subject.kind != "PullRequest" {
            return None;
        }
        self.subject
            .url
            .as_deref()?
            .rsplit('/')
            .next()?
            .parse()
            .ok()
    }
}

pub(crate) struct NotificationsPoll {
    pub notifications: Vec<GhNotification>,
    /// Opaque cursor for the next call's `If-Modified-Since` (the response's
    /// `Last-Modified`, or the request's own cursor when nothing changed).
    pub cursor: String,
    pub poll_after_ms: Option<i64>,
}

pub(crate) async fn poll_notifications(
    http: &HttpClient,
    since: Option<&str>,
) -> HostResult<NotificationsPoll> {
    let headers: Vec<(&str, String)> = since
        .map(|s| vec![("if-modified-since", s.to_string())])
        .unwrap_or_default();
    let raw = http.get_raw("notifications", &headers).await?;
    let poll_after_ms = header(&raw.headers, "x-poll-interval")
        .and_then(|v| v.parse::<i64>().ok())
        .map(|s| s * 1000);

    if raw.not_modified {
        return Ok(NotificationsPoll {
            notifications: vec![],
            cursor: since.unwrap_or_default().to_string(),
            poll_after_ms,
        });
    }

    let notifications: Vec<GhNotification> = serde_json::from_str(&raw.body)
        .map_err(|e| theodore_ports::HostError::Protocol(format!("notifications decode: {e}")))?;
    let cursor = header(&raw.headers, "last-modified")
        .map(str::to_string)
        .unwrap_or_else(|| since.unwrap_or_default().to_string());
    Ok(NotificationsPoll {
        notifications,
        cursor,
        poll_after_ms,
    })
}

fn header<'a>(headers: &'a reqwest::header::HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}

pub(crate) async fn edit_comment(
    http: &HttpClient,
    owner: &str,
    repo: &str,
    comment_id: &str,
    body: &str,
) -> HostResult<()> {
    #[derive(serde::Serialize)]
    struct Body<'a> {
        body: &'a str,
    }
    let path = format!("repos/{owner}/{repo}/pulls/comments/{comment_id}");
    let _: theodore_host_http::HttpResponse<serde_json::Value> =
        http.patch_json(&path, &Body { body }).await?;
    Ok(())
}

pub(crate) async fn delete_comment(
    http: &HttpClient,
    owner: &str,
    repo: &str,
    comment_id: &str,
) -> HostResult<()> {
    let path = format!("repos/{owner}/{repo}/pulls/comments/{comment_id}");
    http.delete(&path).await
}

#[derive(serde::Serialize)]
struct RequestedReviewersBody<'a> {
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    reviewers: &'a [String],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    team_reviewers: &'a [String],
}

/// `POST .../requested_reviewers`: takes logins/team slugs directly, no node
/// id lookup needed (unlike GraphQL's `requestReviews`).
pub(crate) async fn request_reviewers(
    http: &HttpClient,
    owner: &str,
    repo: &str,
    number: u64,
    users: &[String],
    teams: &[String],
) -> HostResult<()> {
    let path = format!("repos/{owner}/{repo}/pulls/{number}/requested_reviewers");
    let _: theodore_host_http::HttpResponse<serde_json::Value> = http
        .post_json(
            &path,
            &RequestedReviewersBody {
                reviewers: users,
                team_reviewers: teams,
            },
        )
        .await?;
    Ok(())
}

/// `DELETE .../requested_reviewers` (same resource, a body is required even
/// though this is a DELETE).
pub(crate) async fn remove_requested_reviewer(
    http: &HttpClient,
    owner: &str,
    repo: &str,
    number: u64,
    user: &str,
) -> HostResult<()> {
    let path = format!("repos/{owner}/{repo}/pulls/{number}/requested_reviewers");
    let users = [user.to_string()];
    http.delete_with_body(
        &path,
        &RequestedReviewersBody {
            reviewers: &users,
            team_reviewers: &[],
        },
    )
    .await
}

#[derive(serde::Serialize)]
struct ReactionBody<'a> {
    content: &'a str,
}

#[derive(Debug, Deserialize)]
pub(crate) struct GhReaction {
    pub id: u64,
    pub content: String,
    pub user: Option<GhReactionUser>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct GhReactionUser {
    pub login: String,
}

pub(crate) async fn add_reaction(
    http: &HttpClient,
    owner: &str,
    repo: &str,
    comment_id: &str,
    content: &str,
) -> HostResult<()> {
    let path = format!("repos/{owner}/{repo}/pulls/comments/{comment_id}/reactions");
    let _: theodore_host_http::HttpResponse<serde_json::Value> =
        http.post_json(&path, &ReactionBody { content }).await?;
    Ok(())
}

pub(crate) async fn list_reactions(
    http: &HttpClient,
    owner: &str,
    repo: &str,
    comment_id: &str,
) -> HostResult<Vec<GhReaction>> {
    let path = format!("repos/{owner}/{repo}/pulls/comments/{comment_id}/reactions");
    Ok(http.get::<Vec<GhReaction>>(&path).await?.value)
}

pub(crate) async fn delete_reaction(
    http: &HttpClient,
    owner: &str,
    repo: &str,
    comment_id: &str,
    reaction_id: u64,
) -> HostResult<()> {
    let path = format!("repos/{owner}/{repo}/pulls/comments/{comment_id}/reactions/{reaction_id}");
    http.delete(&path).await
}

/// `POST .../check-runs/{id}/rerequest`: re-queues one check run. Works for
/// any check-run app, including a single Actions job — but for a *failed*
/// Actions job, `rerun_failed_jobs` is preferred (it reruns the whole job,
/// not just the one check-run GitHub happens to have split it into).
pub(crate) async fn rerequest_check_run(
    http: &HttpClient,
    owner: &str,
    repo: &str,
    check_run_id: &str,
) -> HostResult<()> {
    let path = format!("repos/{owner}/{repo}/check-runs/{check_run_id}/rerequest");
    let _: theodore_host_http::HttpResponse<serde_json::Value> =
        http.post_json(&path, &serde_json::json!({})).await?;
    Ok(())
}

/// `POST .../actions/runs/{run_id}/rerun-failed-jobs`: reruns every failed
/// job of an Actions workflow run.
pub(crate) async fn rerun_workflow_failed_jobs(
    http: &HttpClient,
    owner: &str,
    repo: &str,
    run_id: &str,
) -> HostResult<()> {
    let path = format!("repos/{owner}/{repo}/actions/runs/{run_id}/rerun-failed-jobs");
    let _: theodore_host_http::HttpResponse<serde_json::Value> =
        http.post_json(&path, &serde_json::json!({})).await?;
    Ok(())
}

/// `GET .../actions/jobs/{job_id}/logs`: a 302 to a time-limited plain-text
/// log file, which `reqwest`'s default redirect policy follows — the body
/// here is already the log text, not JSON.
pub(crate) async fn job_log(
    http: &HttpClient,
    owner: &str,
    repo: &str,
    job_id: &str,
) -> HostResult<String> {
    let path = format!("repos/{owner}/{repo}/actions/jobs/{job_id}/logs");
    Ok(http.get_raw(&path, &[]).await?.body)
}

/// The bits of a REST pull-request object `create_pr`/`find_open_pr` read.
#[derive(Debug, Deserialize)]
pub(crate) struct GhPull {
    pub number: u64,
    /// GraphQL node id — the same `PrRef::host_id` `get_pr` fills in.
    pub node_id: Option<String>,
}

/// `GET .../pulls?state=open&head={owner}:{head}`: GitHub filters by
/// `user:branch`, so a same-repo PR's head is qualified by the repo owner.
pub(crate) async fn list_open_pulls_for_head(
    http: &HttpClient,
    owner: &str,
    repo: &str,
    head: &str,
) -> HostResult<Vec<GhPull>> {
    let qualified = format!("{owner}:{head}");
    let path = format!(
        "repos/{owner}/{repo}/pulls?state=open&head={}&per_page=10",
        urlencode(&qualified)
    );
    Ok(http.get::<Vec<GhPull>>(&path).await?.value)
}

#[derive(serde::Serialize)]
struct CreatePullBody<'a> {
    title: &'a str,
    body: &'a str,
    head: &'a str,
    base: &'a str,
    draft: bool,
}

/// `POST .../pulls`. GitHub answers 422 with
/// `errors[].message = "A pull request already exists for o:branch."` when
/// `head` already has an open PR; that becomes `HostError::Conflict`. Any
/// other 422 stays `Validation`.
pub(crate) async fn create_pull(
    http: &HttpClient,
    owner: &str,
    repo: &str,
    pr: &NewPr,
) -> HostResult<GhPull> {
    let path = format!("repos/{owner}/{repo}/pulls");
    let body = CreatePullBody {
        title: &pr.title,
        body: &pr.body,
        head: &pr.head,
        base: &pr.base,
        draft: pr.draft,
    };
    match http.post_json::<_, GhPull>(&path, &body).await {
        Ok(resp) => Ok(resp.value),
        Err(HostError::Validation(msg)) if msg.contains("A pull request already exists") => {
            Err(HostError::Conflict(msg))
        }
        Err(e) => Err(e),
    }
}

#[derive(serde::Serialize)]
struct CreateIssueBody<'a> {
    title: &'a str,
    body: &'a str,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    labels: &'a [String],
}

#[derive(Debug, Deserialize)]
pub(crate) struct GhIssue {
    pub number: u64,
    pub html_url: String,
}

/// `POST /repos/{owner}/{repo}/issues`.
pub(crate) async fn create_issue(
    http: &HttpClient,
    owner: &str,
    repo: &str,
    issue: &NewIssue,
) -> HostResult<GhIssue> {
    let path = format!("repos/{owner}/{repo}/issues");
    let body = CreateIssueBody {
        title: &issue.title,
        body: &issue.body,
        labels: &issue.labels,
    };
    // A refused label (422, or 403 for a token that may open but not label
    // issues) is retried without labels by the caller: not a warning.
    let expected: &[reqwest::StatusCode] = if issue.labels.is_empty() {
        &[]
    } else {
        &[
            reqwest::StatusCode::FORBIDDEN,
            reqwest::StatusCode::UNPROCESSABLE_ENTITY,
        ]
    };
    Ok(http
        .post_json_expecting::<_, GhIssue>(&path, &body, expected)
        .await?
        .value)
}

/// The bits of a REST issue object `get_issue` / `list_assigned_issues`
/// read; `pull_request` is `Some` when the issues endpoint handed back a
/// pull request (GitHub models PRs as issues too), filtered out by callers.
#[derive(Debug, Deserialize)]
pub(crate) struct GhIssueFull {
    pub number: u64,
    pub title: String,
    pub body: Option<String>,
    pub html_url: String,
    #[serde(default)]
    pub pull_request: Option<serde_json::Value>,
}

/// `GET /repos/{owner}/{repo}/issues/{number}`.
pub(crate) async fn get_issue(
    http: &HttpClient,
    owner: &str,
    repo: &str,
    number: u64,
) -> HostResult<GhIssueFull> {
    let path = format!("repos/{owner}/{repo}/issues/{number}");
    let issue = http.get::<GhIssueFull>(&path).await?.value;
    if issue.pull_request.is_some() {
        return Err(HostError::NotFound(format!("issue #{number}")));
    }
    Ok(issue)
}

/// `GET /repos/{owner}/{repo}/issues?state=open&assignee={login}`: open
/// issues assigned to `login`, pull requests filtered out.
pub(crate) async fn list_assigned_issues(
    http: &HttpClient,
    owner: &str,
    repo: &str,
    login: &str,
) -> HostResult<Vec<GhIssueFull>> {
    let path = format!(
        "repos/{owner}/{repo}/issues?state=open&assignee={}&per_page=50",
        urlencode(login)
    );
    let issues = http.get::<Vec<GhIssueFull>>(&path).await?.value;
    Ok(issues
        .into_iter()
        .filter(|i| i.pull_request.is_none())
        .collect())
}

fn urlencode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}
