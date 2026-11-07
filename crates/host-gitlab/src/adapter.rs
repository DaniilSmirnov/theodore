use std::sync::Arc;

use async_trait::async_trait;
use theodore_domain::{
    Check, CheckRollup, CheckStatus, Comment, CommentId, Commit, HostKind, HostRepo, InboxReason,
    Person, PrDetail, PrRef, PrState, PrSummary, RepoRef, ReviewDecision, ReviewEvent, Reviewer,
    ReviewerState, Sha, Thread, ThreadId,
};
use theodore_host_http::{HttpClient, TokenSource};
use theodore_ports::{
    Capabilities, Changes, Clock, Fetched, HostAdapter, HostError, HostResult, InboxSection,
    MergeMethod, MergeState, MergeStatus, NewPr, ReviewSubmission, SubmittedReview, TrackedIssue,
};

use crate::anchor::{self, DiffRefs};
use crate::model::{
    CreateMrBody, GlDiscussion, GlMergeRequest, GlMrRef, GlNote, GlUser, MergeBody, UpdateMrBody,
};
use crate::{pagination, rest, time};

/// `HostAdapter` for gitlab.com and self-managed GitLab.
pub struct GitlabAdapter {
    http: HttpClient,
    clock: Arc<dyn Clock>,
    viewer_username: tokio::sync::OnceCell<String>,
}

impl GitlabAdapter {
    pub fn new(token: Arc<dyn TokenSource>, clock: Arc<dyn Clock>) -> Self {
        Self::with_base_url(
            url::Url::parse("https://gitlab.com/api/v4/").expect("valid url"),
            token,
            clock,
        )
    }

    /// `base` must end with `/` (e.g. `https://gitlab.example.com/api/v4/`
    /// for self-managed, or a wiremock server's `uri() + "/"` in tests).
    pub fn with_base_url(
        base: url::Url,
        token: Arc<dyn TokenSource>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            http: HttpClient::new(base, token, clock.clone()).with_accept("application/json"),
            clock,
            viewer_username: tokio::sync::OnceCell::new(),
        }
    }

    async fn username(&self) -> HostResult<&str> {
        self.viewer_username
            .get_or_try_init(|| async { Ok(rest::viewer(&self.http).await?.username) })
            .await
            .map(String::as_str)
    }

    async fn fetch_diff_refs(&self, pr: &PrRef) -> HostResult<DiffRefs> {
        let project = project_id(pr);
        let mr = match rest::get_merge_request(&self.http, &project, pr.number, None).await? {
            Fetched::Modified { value, .. } => value,
            Fetched::NotModified => unreachable!("no etag sent"),
        };
        let refs = mr
            .diff_refs
            .ok_or_else(|| HostError::Protocol("merge request has no diff_refs".to_string()))?;
        Ok(DiffRefs {
            base_sha: refs.base_sha,
            start_sha: refs.start_sha,
            head_sha: refs.head_sha,
        })
    }

    async fn fetch_all_discussions(&self, pr: &PrRef) -> HostResult<Vec<GlDiscussion>> {
        let project = project_id(pr);
        pagination::paginate_all(|page| {
            rest::discussions_page(&self.http, &project, pr.number, page)
        })
        .await
    }

    async fn find_discussion_for_marker(
        &self,
        pr: &PrRef,
        marker: &str,
    ) -> HostResult<Option<(String, u64)>> {
        let tag = marker_tag(marker);
        let discussions = self.fetch_all_discussions(pr).await?;
        for d in &discussions {
            if let Some(note) = d.notes.iter().find(|n| n.body.contains(&tag)) {
                return Ok(Some((d.id.clone(), note.id)));
            }
        }
        Ok(None)
    }

    async fn build_pr_detail(&self, pr: &PrRef, mr: GlMergeRequest) -> HostResult<PrDetail> {
        let project = project_id(pr);
        let iid = pr.number;

        let changes = rest::changes(&self.http, &project, iid).await?;
        let discussions = self.fetch_all_discussions(pr).await?;
        let pipelines = rest::pipelines(&self.http, &project, iid).await?;
        let approvals = rest::approvals(&self.http, &project, iid).await?;
        let commits: Vec<_> =
            pagination::paginate_all(|page| rest::commits_page(&self.http, &project, iid, page))
                .await?;

        let head_sha = Sha::new(mr.sha.clone().unwrap_or_default());

        let (additions, deletions) = diff_stats(&changes.changes);
        let changed_files = changes.changes.len() as u32;

        let checks: Vec<Check> = pipelines.iter().map(map_pipeline).collect();
        let checks_rollup = mr
            .head_pipeline
            .as_ref()
            .map(|p| map_check_rollup(&p.status))
            .unwrap_or(CheckRollup::None);

        let reviewers = map_reviewers(&mr, &approvals);
        let assignees = mr.assignees.iter().map(user_to_person).collect();

        let threads: Vec<Thread> = discussions
            .iter()
            .filter(|d| d.notes.iter().any(|n| !n.system))
            .map(|d| map_thread(d, &head_sha))
            .collect();

        let summary = PrSummary {
            pr: PrRef {
                repo: pr.repo.clone(),
                number: pr.number,
                host_id: Some(mr.project_id.to_string()),
            },
            title: mr.title.clone(),
            author: user_to_person(&mr.author),
            state: pr_state(&mr.state, mr.draft),
            base_ref: mr.target_branch.clone(),
            head_ref: mr.source_branch.clone(),
            head_sha: head_sha.clone(),
            additions,
            deletions,
            changed_files,
            reasons: vec![],
            checks: checks_rollup,
            approvals: approvals.approved_by.len() as u32,
            required_approvals: approvals.approvals_required,
            updated_at: time::parse_millis(&mr.updated_at),
            url: mr.web_url.clone(),
            review_decision: review_decision_from_approvals(&approvals),
            conflicts: gl_conflicts(&mr),
            created_at: Some(time::parse_millis(&mr.created_at)),
            // Computed app-side from the stored PR rows (plan "Stack").
            stack: None,
        };

        let base_sha = mr
            .diff_refs
            .as_ref()
            .map(|r| Sha::new(r.base_sha.clone()))
            .unwrap_or_else(|| Sha::new(""));

        Ok(PrDetail {
            summary,
            body: mr.description.clone().unwrap_or_default(),
            base_sha,
            commits: commits.iter().map(map_commit).collect(),
            checks,
            reviewers,
            assignees,
            labels: mr.labels.clone(),
            threads,
            // GitLab has no server-side per-file Viewed (plan §6.4); kept
            // locally by the caller.
            viewed: vec![],
        })
    }
}

#[async_trait]
impl HostAdapter for GitlabAdapter {
    fn kind(&self) -> HostKind {
        HostKind::Gitlab
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            viewed: false,
            pending_review: true,
            suggestions: true,
            resolve_threads: true,
            merge_queue: false,
            auto_merge: true,
            reactions: true,
            issues: true,
        }
    }

    async fn viewer(&self) -> HostResult<Person> {
        let u = rest::viewer(&self.http).await?;
        Ok(user_to_person(&u))
    }

    async fn changed_since(&self, cursor: Option<&str>) -> HostResult<Changes> {
        let now = self.clock.now();
        let query = match cursor {
            Some(c) => format!("scope=all&state=opened&updated_after={c}"),
            None => "scope=all&state=opened".to_string(),
        };
        let mrs: Vec<GlMergeRequest> = pagination::paginate_all(|page| {
            rest::list_merge_requests_page(&self.http, &query, page)
        })
        .await?;
        let prs = mrs.iter().filter_map(mr_to_pr_ref).collect();
        Ok(Changes {
            prs,
            cursor: time::format_millis(now),
            poll_after_ms: Some(60_000),
        })
    }

    async fn search(&self, section: &InboxSection) -> HostResult<Vec<PrSummary>> {
        // GitLab has no "mentioned" filter on `/merge_requests`; it tracks
        // that as a to-do instead (plan: "mentions via todos"), so this
        // section bypasses the merge-request listing entirely.
        if matches!(section, InboxSection::Mentioned) {
            let todos: Vec<_> =
                pagination::paginate_all(|page| rest::todos_page(&self.http, page)).await?;
            return Ok(todos
                .iter()
                .map(|t| mr_to_summary(&t.target, Some(InboxReason::Mentioned)))
                .collect());
        }

        let (query, reason) = self.section_query(section).await?;
        let mrs: Vec<GlMergeRequest> = pagination::paginate_all(|page| {
            rest::list_merge_requests_page(&self.http, &query, page)
        })
        .await?;
        Ok(mrs.iter().map(|mr| mr_to_summary(mr, reason)).collect())
    }

    async fn list_repos(&self) -> HostResult<Vec<HostRepo>> {
        let projects =
            pagination::paginate_all(|page| rest::projects_page(&self.http, page)).await?;
        Ok(projects
            .iter()
            .filter_map(|p| {
                let (owner, name) = split_namespace(&p.path_with_namespace)?;
                Some(HostRepo {
                    repo: RepoRef {
                        host: HostKind::Gitlab,
                        owner,
                        name,
                    },
                    description: p.description.clone(),
                    private: p.visibility.as_deref() != Some("public"),
                    pushed_at: p.last_activity_at.as_deref().map(time::parse_millis),
                })
            })
            .collect())
    }

    async fn get_pr(&self, pr: &PrRef, etag: Option<&str>) -> HostResult<Fetched<PrDetail>> {
        let project = project_id(pr);
        match rest::get_merge_request(&self.http, &project, pr.number, etag).await? {
            Fetched::NotModified => Ok(Fetched::NotModified),
            Fetched::Modified { etag, value } => {
                let detail = self.build_pr_detail(pr, value).await?;
                Ok(Fetched::Modified {
                    etag,
                    value: detail,
                })
            }
        }
    }

    async fn find_open_pr(&self, repo: &RepoRef, head: &str) -> HostResult<Option<PrRef>> {
        let mrs =
            rest::open_merge_requests_for_source(&self.http, &project_path(repo), head).await?;
        Ok(mrs.into_iter().next().map(|mr| mr_ref_to_pr_ref(repo, &mr)))
    }

    async fn create_pr(&self, repo: &RepoRef, pr: &NewPr) -> HostResult<PrRef> {
        let title = if pr.draft {
            add_draft_prefix(&pr.title)
        } else {
            pr.title.clone()
        };
        let mr = rest::create_merge_request(
            &self.http,
            &project_path(repo),
            &CreateMrBody {
                source_branch: &pr.head,
                target_branch: &pr.base,
                title: &title,
                description: &pr.body,
            },
        )
        .await?;
        Ok(mr_ref_to_pr_ref(repo, &mr))
    }

    async fn open_issues(&self, repo: &RepoRef) -> HostResult<Vec<TrackedIssue>> {
        let project = project_path(repo);
        let all = rest::list_open_issues(&self.http, &project, None).await?;
        // Yours first; without a viewer the list is still the repo's.
        let login = self.username().await.ok().map(|l| l.to_string());
        let mine = match login {
            Some(login) => rest::list_open_issues(&self.http, &project, Some(&login)).await?,
            None => Vec::new(),
        };
        let tracked = |v: Vec<crate::model::GlIssue>| -> Vec<TrackedIssue> {
            v.into_iter().map(gl_issue_to_tracked).collect()
        };
        Ok(theodore_ports::mine_first(tracked(mine), tracked(all)))
    }

    async fn get_issue(&self, repo: &RepoRef, number: u64) -> HostResult<TrackedIssue> {
        let issue = rest::get_issue(&self.http, &project_path(repo), number).await?;
        Ok(gl_issue_to_tracked(issue))
    }

    async fn set_viewed(&self, _pr: &PrRef, _path: &str, _viewed: bool) -> HostResult<()> {
        Err(HostError::Unsupported("set_viewed"))
    }

    async fn submit_review(
        &self,
        pr: &PrRef,
        review: &ReviewSubmission,
    ) -> HostResult<SubmittedReview> {
        let project = project_id(pr);
        let refs = self.fetch_diff_refs(pr).await?;

        for comment in &review.comments {
            let position = anchor::to_position(&comment.anchor, &refs);
            let note = format!("{}\n\n{}", comment.body, marker_tag(&comment.marker));
            rest::create_draft_note(&self.http, &project, pr.number, &note, Some(&position))
                .await?;
        }
        if !review.body.is_empty() {
            rest::create_draft_note(&self.http, &project, pr.number, &review.body, None).await?;
        }
        rest::bulk_publish_draft_notes(&self.http, &project, pr.number).await?;

        if review.event == ReviewEvent::Approve {
            rest::approve(&self.http, &project, pr.number).await?;
        }
        // GitLab has no formal "request changes" review state (plan §6.4
        // deviation): the comments above are posted, approval is simply
        // withheld.

        let mut threads = Vec::new();
        for comment in &review.comments {
            if let Some((discussion_id, _note_id)) =
                self.find_discussion_for_marker(pr, &comment.marker).await?
            {
                threads.push((comment.marker.clone(), ThreadId::new(discussion_id)));
            }
        }

        Ok(SubmittedReview {
            // GitLab has no single "review" resource id; synthesize one.
            review_id: format!("draft-notes:{}", review.commit.as_str()),
            threads,
        })
    }

    async fn find_marker(&self, pr: &PrRef, marker: &str) -> HostResult<Option<CommentId>> {
        Ok(self
            .find_discussion_for_marker(pr, marker)
            .await?
            .map(|(_discussion_id, note_id)| CommentId::new(note_id.to_string())))
    }

    async fn reply(&self, pr: &PrRef, thread: &ThreadId, body: &str) -> HostResult<CommentId> {
        let project = project_id(pr);
        let note =
            rest::reply_to_discussion(&self.http, &project, pr.number, thread.as_str(), body)
                .await?;
        Ok(CommentId::new(note.id.to_string()))
    }

    async fn resolve(&self, pr: &PrRef, thread: &ThreadId, resolved: bool) -> HostResult<()> {
        let project = project_id(pr);
        rest::resolve_discussion(&self.http, &project, pr.number, thread.as_str(), resolved).await
    }

    async fn edit_comment(&self, pr: &PrRef, comment: &CommentId, body: &str) -> HostResult<()> {
        let project = project_id(pr);
        rest::edit_note(&self.http, &project, pr.number, comment.as_str(), body).await
    }

    async fn delete_comment(&self, pr: &PrRef, comment: &CommentId) -> HostResult<()> {
        let project = project_id(pr);
        rest::delete_note(&self.http, &project, pr.number, comment.as_str()).await
    }

    async fn add_reaction(&self, pr: &PrRef, comment: &CommentId, emoji: &str) -> HostResult<()> {
        let project = project_id(pr);
        rest::award_emoji_create(&self.http, &project, pr.number, comment.as_str(), emoji).await
    }

    async fn remove_reaction(
        &self,
        pr: &PrRef,
        comment: &CommentId,
        emoji: &str,
    ) -> HostResult<()> {
        let project = project_id(pr);
        let me = self.username().await?.to_string();
        let awards =
            rest::award_emoji_list(&self.http, &project, pr.number, comment.as_str()).await?;
        let mine = awards
            .iter()
            .find(|a| a.name == emoji && a.user.username == me);
        match mine {
            Some(a) => {
                rest::award_emoji_delete(&self.http, &project, pr.number, comment.as_str(), a.id)
                    .await
            }
            None => Ok(()),
        }
    }

    async fn assignable_users(&self, repo: &RepoRef) -> HostResult<Vec<Person>> {
        let project = project_path(repo);
        let members =
            pagination::paginate_all(|page| rest::members_page(&self.http, &project, page)).await?;
        Ok(members.iter().map(user_to_person).collect())
    }

    /// GitLab merge requests have no "team reviewer" concept over this API;
    /// `teams` is silently ignored.
    async fn request_reviewers(
        &self,
        pr: &PrRef,
        users: &[String],
        _teams: &[String],
    ) -> HostResult<()> {
        let project = project_id(pr);
        let mr = current_mr(&self.http, &project, pr.number).await?;
        let mut ids: Vec<u64> = mr.reviewers.iter().map(|u| u.id).collect();
        for username in users {
            let user = rest::find_user_by_username(&self.http, username)
                .await?
                .ok_or_else(|| HostError::NotFound(format!("user {username}")))?;
            if !ids.contains(&user.id) {
                ids.push(user.id);
            }
        }
        rest::update_merge_request(
            &self.http,
            &project,
            pr.number,
            &UpdateMrBody {
                title: None,
                reviewer_ids: Some(&ids),
            },
        )
        .await?;
        Ok(())
    }

    async fn remove_reviewer(&self, pr: &PrRef, user: &str) -> HostResult<()> {
        let project = project_id(pr);
        let mr = current_mr(&self.http, &project, pr.number).await?;
        let ids: Vec<u64> = mr
            .reviewers
            .iter()
            .filter(|u| u.username != user)
            .map(|u| u.id)
            .collect();
        rest::update_merge_request(
            &self.http,
            &project,
            pr.number,
            &UpdateMrBody {
                title: None,
                reviewer_ids: Some(&ids),
            },
        )
        .await?;
        Ok(())
    }

    async fn mark_ready(&self, pr: &PrRef) -> HostResult<()> {
        let project = project_id(pr);
        let mr = current_mr(&self.http, &project, pr.number).await?;
        let title = strip_draft_prefix(&mr.title);
        rest::update_merge_request(
            &self.http,
            &project,
            pr.number,
            &UpdateMrBody {
                title: Some(&title),
                reviewer_ids: None,
            },
        )
        .await?;
        Ok(())
    }

    async fn convert_to_draft(&self, pr: &PrRef) -> HostResult<()> {
        let project = project_id(pr);
        let mr = current_mr(&self.http, &project, pr.number).await?;
        let title = add_draft_prefix(&mr.title);
        rest::update_merge_request(
            &self.http,
            &project,
            pr.number,
            &UpdateMrBody {
                title: Some(&title),
                reviewer_ids: None,
            },
        )
        .await?;
        Ok(())
    }

    async fn update_branch(&self, pr: &PrRef) -> HostResult<()> {
        let project = project_id(pr);
        rest::rebase(&self.http, &project, pr.number).await
    }

    async fn rerun_check(&self, pr: &PrRef, check: &Check) -> HostResult<()> {
        let project = project_id(pr);
        rest::retry_pipeline(&self.http, &project, &check.id).await
    }

    async fn merge_status(&self, pr: &PrRef) -> HostResult<MergeStatus> {
        let project = project_id(pr);
        let mr = current_mr(&self.http, &project, pr.number).await?;
        let approvals = rest::approvals(&self.http, &project, pr.number).await?;
        let settings = rest::project_settings(&self.http, &project).await?;

        let mut allowed_methods = Vec::new();
        if settings.merge_method.as_deref() != Some("ff") {
            allowed_methods.push(MergeMethod::Merge);
        }
        if settings.squash_option.as_deref() != Some("never") {
            allowed_methods.push(MergeMethod::Squash);
        }

        Ok(MergeStatus {
            mergeable: map_mergeable(mr.merge_status.as_deref()),
            state: map_merge_state(mr.detailed_merge_status.as_deref()),
            allowed_methods,
            auto_merge_enabled: mr.merge_when_pipeline_succeeds,
            in_queue: false,
            required_approvals: approvals.approvals_required,
            // "Merge when pipeline succeeds" is always available.
            auto_merge_allowed: true,
            merge_queue: false,
            // GitLab merges by the project's method; squash is a flag.
            default_method: Some(
                if matches!(
                    settings.squash_option.as_deref(),
                    Some("default_on" | "always")
                ) {
                    MergeMethod::Squash
                } else {
                    MergeMethod::Merge
                },
            ),
        })
    }

    async fn merge(
        &self,
        pr: &PrRef,
        method: MergeMethod,
        title: Option<String>,
        body: Option<String>,
    ) -> HostResult<Sha> {
        let project = project_id(pr);
        let message = merge_message(title.as_deref(), body.as_deref());
        let merge_body = match method {
            MergeMethod::Merge => MergeBody {
                merge_commit_message: message.as_deref(),
                ..Default::default()
            },
            MergeMethod::Squash => MergeBody {
                squash: true,
                squash_commit_message: message.as_deref(),
                ..Default::default()
            },
            MergeMethod::Rebase => {
                return Err(HostError::Unsupported(
                    "gitlab has no per-request rebase-and-merge",
                ));
            }
        };
        let merged =
            rest::merge_request_merge(&self.http, &project, pr.number, &merge_body).await?;
        let sha = merged
            .squash_commit_sha
            .or(merged.merge_commit_sha)
            .ok_or_else(|| HostError::Protocol("merge response had no commit sha".into()))?;
        Ok(Sha::new(sha))
    }

    async fn enable_auto_merge(&self, pr: &PrRef, method: MergeMethod) -> HostResult<()> {
        let project = project_id(pr);
        let merge_body = match method {
            MergeMethod::Merge => MergeBody {
                merge_when_pipeline_succeeds: true,
                ..Default::default()
            },
            MergeMethod::Squash => MergeBody {
                squash: true,
                merge_when_pipeline_succeeds: true,
                ..Default::default()
            },
            MergeMethod::Rebase => {
                return Err(HostError::Unsupported(
                    "gitlab has no per-request rebase-and-merge",
                ));
            }
        };
        rest::merge_request_merge(&self.http, &project, pr.number, &merge_body).await?;
        Ok(())
    }

    async fn disable_auto_merge(&self, pr: &PrRef) -> HostResult<()> {
        let project = project_id(pr);
        rest::cancel_merge_when_pipeline_succeeds(&self.http, &project, pr.number).await
    }
}

impl GitlabAdapter {
    /// `InboxSection` -> (`/merge_requests` query string, reason tag).
    /// `Query` is GitLab's one escape hatch: GitLab has no search DSL like
    /// GitHub's, so the string is appended to the query verbatim (e.g.
    /// `"labels=bug&milestone=v2"`), not parsed.
    async fn section_query(
        &self,
        section: &InboxSection,
    ) -> HostResult<(String, Option<InboxReason>)> {
        Ok(match section {
            InboxSection::ReviewRequested => {
                let me = self.username().await?;
                (
                    format!("scope=all&state=opened&reviewer_username={me}"),
                    Some(InboxReason::ReviewRequested),
                )
            }
            InboxSection::Assigned => (
                "scope=assigned_to_me&state=opened".to_string(),
                Some(InboxReason::Assigned),
            ),
            InboxSection::Authored => (
                "scope=created_by_me&state=opened".to_string(),
                Some(InboxReason::Authored),
            ),
            // Handled directly in `search` (no `/merge_requests` query maps
            // to "mentioned"); kept out of this match's return type.
            InboxSection::Mentioned => unreachable!("Mentioned is handled in search()"),
            InboxSection::Query(q) => (format!("state=opened&{q}"), None),
        })
    }
}

fn project_id(pr: &PrRef) -> String {
    match &pr.host_id {
        Some(id) => id.clone(),
        // GitLab accepts a URL-encoded `namespace/path` wherever a numeric
        // project id is expected; `/` is the only character that needs
        // escaping here.
        None => format!("{}/{}", pr.repo.owner, pr.repo.name).replace('/', "%2F"),
    }
}

/// Same as `project_id`, for calls keyed by `RepoRef` alone (no `PrRef`,
/// hence no cached numeric id to prefer).
fn project_path(repo: &RepoRef) -> String {
    format!("{}/{}", repo.owner, repo.name).replace('/', "%2F")
}

async fn current_mr(http: &HttpClient, project: &str, iid: u64) -> HostResult<GlMergeRequest> {
    match rest::get_merge_request(http, project, iid, None).await? {
        Fetched::Modified { value, .. } => Ok(value),
        Fetched::NotModified => unreachable!("no etag sent"),
    }
}

/// `Draft: `/`[Draft] ` style prefixes GitLab's web UI itself uses to flag a
/// draft MR (plan: GitLab has no separate draft boolean settable over this
/// API — see `mr.draft`, which is derived from the title GitLab-side).
fn strip_draft_prefix(title: &str) -> String {
    let lower = title.to_ascii_lowercase();
    for prefix in ["draft: ", "draft:", "[draft] ", "[draft]"] {
        if lower.starts_with(prefix) {
            return title[prefix.len()..].trim_start().to_string();
        }
    }
    title.to_string()
}

fn add_draft_prefix(title: &str) -> String {
    if title.to_ascii_lowercase().starts_with("draft:") {
        title.to_string()
    } else {
        format!("Draft: {title}")
    }
}

fn merge_message(title: Option<&str>, body: Option<&str>) -> Option<String> {
    match (title, body) {
        (Some(t), Some(b)) if !b.is_empty() => Some(format!("{t}\n\n{b}")),
        (Some(t), _) => Some(t.to_string()),
        (None, Some(b)) => Some(b.to_string()),
        (None, None) => None,
    }
}

/// GitLab's legacy three-value `merge_status` maps 1:1 onto
/// `MergeStatus::mergeable`.
fn map_mergeable(status: Option<&str>) -> Option<bool> {
    match status {
        Some("can_be_merged") => Some(true),
        Some("cannot_be_merged") => Some(false),
        _ => None,
    }
}

/// Best-effort fold of GitLab's `detailed_merge_status` (many more values
/// than GitHub's `mergeStateStatus`) onto the host-neutral `MergeState`.
fn map_merge_state(status: Option<&str>) -> MergeState {
    match status.unwrap_or("unchecked") {
        "mergeable" => MergeState::Clean,
        "conflict" => MergeState::Dirty,
        "need_rebase" => MergeState::Behind,
        "ci_still_running" | "ci_must_pass" | "commits_status" | "broken_status" => {
            MergeState::Unstable
        }
        "blocked_status"
        | "discussions_not_resolved"
        | "not_approved"
        | "requested_changes"
        | "jira_association_missing"
        | "external_status_checks"
        | "locked_status"
        | "merge_request_blocked"
        | "security_policy_violations"
        | "approvals_syncing" => MergeState::Blocked,
        _ => MergeState::Unknown,
    }
}

fn marker_tag(marker: &str) -> String {
    format!("<!-- theodore:{marker} -->")
}

fn user_to_person(u: &GlUser) -> Person {
    Person {
        login: u.username.clone(),
        name: u.name.clone(),
        avatar_url: u.avatar_url.clone(),
    }
}

fn pr_state(state: &str, draft: bool) -> PrState {
    if draft {
        return PrState::Draft;
    }
    match state {
        "merged" => PrState::Merged,
        "closed" | "locked" => PrState::Closed,
        _ => PrState::Open,
    }
}

fn map_check_rollup(status: &str) -> CheckRollup {
    match status {
        "success" => CheckRollup::Pass,
        "failed" | "canceled" => CheckRollup::Fail,
        "running" | "pending" | "created" | "waiting_for_resource" | "preparing" | "scheduled" => {
            CheckRollup::Pending
        }
        _ => CheckRollup::None,
    }
}

fn map_check_status(status: &str) -> CheckStatus {
    match status {
        "success" => CheckStatus::Pass,
        "failed" => CheckStatus::Fail,
        "canceled" | "skipped" => CheckStatus::Skipped,
        _ => CheckStatus::Pending,
    }
}

fn map_pipeline(p: &crate::model::GlPipeline) -> Check {
    Check {
        id: p.id.to_string(),
        name: p.git_ref.clone(),
        status: map_check_status(&p.status),
        url: Some(p.web_url.clone()),
        duration_ms: None,
    }
}

fn map_commit(c: &crate::model::GlCommit) -> Commit {
    Commit {
        sha: Sha::new(c.id.clone()),
        title: c.title.clone(),
        author: Person {
            login: c
                .author_email
                .clone()
                .unwrap_or_else(|| c.author_name.clone()),
            name: Some(c.author_name.clone()),
            avatar_url: None,
        },
        at: time::parse_millis(&c.committed_date),
        // Not computed for GitLab yet (plan "Commits tab" is GitHub-first;
        // GitLab's approvals carry no per-commit "reviewed up to" marker).
        after_my_review: false,
    }
}

fn map_reviewers(mr: &GlMergeRequest, approvals: &crate::model::GlApprovals) -> Vec<Reviewer> {
    mr.reviewers
        .iter()
        .map(|r| {
            let state = if approvals.approved_by.iter().any(|a| a.user.id == r.id) {
                ReviewerState::Approved
            } else {
                ReviewerState::Requested
            };
            Reviewer {
                person: user_to_person(r),
                state,
                team: None,
            }
        })
        .collect()
}

fn diff_stats(changes: &[crate::model::GlChange]) -> (u32, u32) {
    let mut additions = 0u32;
    let mut deletions = 0u32;
    for c in changes {
        for line in c.diff.lines() {
            if line.starts_with("+++") || line.starts_with("---") {
                continue;
            }
            if line.starts_with('+') {
                additions += 1;
            } else if line.starts_with('-') {
                deletions += 1;
            }
        }
    }
    (additions, deletions)
}

fn map_thread(d: &GlDiscussion, head_sha: &Sha) -> Thread {
    let position = d.notes.iter().find_map(|n| n.position.as_ref());
    let anchor = position.and_then(|p| anchor::from_position(head_sha.clone(), p));
    let outdated = position
        .map(|p| p.head_sha != head_sha.as_str())
        .unwrap_or(false);
    let resolvable_resolved = d
        .notes
        .iter()
        .find(|n| n.resolvable)
        .map(|n| n.resolved)
        .unwrap_or(false);

    let comments = d
        .notes
        .iter()
        .filter(|n| !n.system)
        .map(map_comment)
        .collect();

    Thread {
        id: ThreadId::new(d.id.clone()),
        anchor,
        resolved: resolvable_resolved,
        outdated,
        comments,
    }
}

fn map_comment(n: &GlNote) -> Comment {
    Comment {
        id: CommentId::new(n.id.to_string()),
        author: user_to_person(&n.author),
        body: n.body.clone(),
        created_at: time::parse_millis(&n.created_at),
        edited: n.updated_at != n.created_at,
        // GitLab's award emoji are per-note, with no bulk field on the
        // discussions response (plan "reactions" is GitHub-first): listing
        // them here would cost one extra REST call per note. Left empty
        // until that's worth it; `add_reaction`/`remove_reaction` still work.
        reactions: Vec::new(),
    }
}

/// `path_with_namespace` ("group/subgroup/project") -> (owner, name).
fn split_namespace(path_with_namespace: &str) -> Option<(String, String)> {
    let (owner, name) = path_with_namespace.rsplit_once('/')?;
    Some((owner.to_string(), name.to_string()))
}

/// The `PrRef` `get_pr`/`search` use: iid as the number, the numeric
/// project id as `host_id`.
fn mr_ref_to_pr_ref(repo: &RepoRef, mr: &GlMrRef) -> PrRef {
    PrRef {
        repo: repo.clone(),
        number: mr.iid,
        host_id: Some(mr.project_id.to_string()),
    }
}

fn gl_issue_to_tracked(issue: crate::model::GlIssue) -> TrackedIssue {
    TrackedIssue {
        number: issue.iid,
        title: issue.title,
        body: issue.description.unwrap_or_default(),
        url: issue.web_url,
        closed: issue.state.as_deref() == Some("closed"),
    }
}

fn mr_to_pr_ref(mr: &GlMergeRequest) -> Option<PrRef> {
    let (owner, name) = parse_namespace_from_web_url(&mr.web_url)?;
    Some(PrRef {
        repo: RepoRef {
            host: HostKind::Gitlab,
            owner,
            name,
        },
        number: mr.iid,
        host_id: Some(mr.project_id.to_string()),
    })
}

fn mr_to_summary(mr: &GlMergeRequest, reason: Option<InboxReason>) -> PrSummary {
    let pr = mr_to_pr_ref(mr).unwrap_or(PrRef {
        repo: RepoRef {
            host: HostKind::Gitlab,
            owner: String::new(),
            name: String::new(),
        },
        number: mr.iid,
        host_id: Some(mr.project_id.to_string()),
    });
    let changed_files = mr
        .changes_count
        .as_deref()
        .and_then(|s| s.trim_end_matches('+').parse().ok())
        .unwrap_or(0);
    PrSummary {
        pr,
        title: mr.title.clone(),
        author: user_to_person(&mr.author),
        state: pr_state(&mr.state, mr.draft),
        base_ref: mr.target_branch.clone(),
        head_ref: mr.source_branch.clone(),
        head_sha: Sha::new(mr.sha.clone().unwrap_or_default()),
        additions: 0,
        deletions: 0,
        changed_files,
        reasons: reason.into_iter().collect(),
        checks: mr
            .head_pipeline
            .as_ref()
            .map(|p| map_check_rollup(&p.status))
            .unwrap_or(CheckRollup::None),
        approvals: 0,
        required_approvals: None,
        updated_at: time::parse_millis(&mr.updated_at),
        url: mr.web_url.clone(),
        // No approvals fetched for a list row (plan "fill where it's
        // cheap"): review_decision needs a second call, conflicts doesn't
        // — the list/to-do payload already carries `merge_status`.
        review_decision: None,
        conflicts: gl_conflicts(mr),
        created_at: Some(time::parse_millis(&mr.created_at)),
        stack: None,
    }
}

/// GitLab's merge-status fields are returned on both the single-MR and the
/// list/to-do shapes, so this is free wherever a `GlMergeRequest` already
/// is.
fn gl_conflicts(mr: &GlMergeRequest) -> bool {
    mr.detailed_merge_status.as_deref() == Some("conflict")
        || mr.merge_status.as_deref() == Some("cannot_be_merged")
}

/// GitLab has no `reviewDecision` equivalent; approximated from approvals:
/// satisfied required approvals -> `Approved`, still missing some ->
/// `ReviewRequired`. GitLab has no "changes requested" review state.
fn review_decision_from_approvals(approvals: &crate::model::GlApprovals) -> Option<ReviewDecision> {
    let required = approvals.approvals_required?;
    if approvals.approved_by.len() as u32 >= required {
        Some(ReviewDecision::Approved)
    } else {
        Some(ReviewDecision::ReviewRequired)
    }
}

/// Pulls `(owner, name)` out of an MR's `web_url`
/// (`.../<owner>[/<subgroup>...]/<name>/-/merge_requests/<iid>`) — the list
/// and to-do endpoints don't otherwise carry the project's namespace path.
fn parse_namespace_from_web_url(web_url: &str) -> Option<(String, String)> {
    let url = url::Url::parse(web_url).ok()?;
    let path = url.path().trim_start_matches('/');
    let (full_path, _rest) = path.split_once("/-/merge_requests")?;
    split_namespace(full_path)
}
