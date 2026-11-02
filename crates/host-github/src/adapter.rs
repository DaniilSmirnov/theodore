use std::sync::Arc;

use async_trait::async_trait;
use theodore_domain::{
    Check, CheckRollup, CheckStatus, Comment, CommentId, Commit, FileViewed, HostKind, HostRepo,
    InboxReason, Millis, Person, PrDetail, PrRef, PrState, PrSummary, ReactionCount, RepoRef,
    ReviewDecision, ReviewEvent, Reviewer, ReviewerState, Sha, Thread, ThreadId, ViewedState,
};
use theodore_host_http::{HttpClient, TokenSource};
use theodore_ports::{
    Capabilities, Changes, Clock, CreatedIssue, Fetched, HostAdapter, HostError, HostResult,
    InboxSection, IssueTracker, MergeMethod, MergeState, MergeStatus, NewIssue, NewPr,
    ReviewComment, ReviewSubmission, SubmittedReview,
};

use crate::{anchor, graphql, logs, pagination, rest, time};

/// `HostAdapter` for github.com and GitHub Enterprise Server.
pub struct GithubAdapter {
    rest: HttpClient,
    graphql: HttpClient,
    token: Arc<dyn TokenSource>,
}

impl GithubAdapter {
    pub fn new(token: Arc<dyn TokenSource>, clock: Arc<dyn Clock>) -> Self {
        Self::with_base_urls(
            url::Url::parse("https://api.github.com/").expect("valid url"),
            url::Url::parse("https://api.github.com/graphql").expect("valid url"),
            token,
            clock,
        )
    }

    /// `rest_base` must end with `/` (e.g. `https://ghe.example.com/api/v3/`
    /// for Enterprise, or a wiremock server's `uri() + "/"` in tests).
    /// `graphql_url` is the exact GraphQL endpoint.
    pub fn with_base_urls(
        rest_base: url::Url,
        graphql_url: url::Url,
        token: Arc<dyn TokenSource>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            rest: HttpClient::new(rest_base, token.clone(), clock.clone()),
            graphql: HttpClient::new(graphql_url, token.clone(), clock),
            token,
        }
    }

    async fn resolve_node_id(&self, pr: &PrRef) -> HostResult<String> {
        match &pr.host_id {
            Some(id) => Ok(id.clone()),
            None => {
                graphql::resolve_pr_node_id(&self.graphql, &pr.repo.owner, &pr.repo.name, pr.number)
                    .await
            }
        }
    }

    async fn fetch_all_threads(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> HostResult<Vec<graphql::GqlThread>> {
        pagination::paginate_all(|after| {
            graphql::fetch_threads_page(&self.graphql, owner, repo, number, after)
        })
        .await
    }

    async fn find_thread_ids_for_markers(
        &self,
        pr: &PrRef,
        comments: &[ReviewComment],
    ) -> HostResult<Vec<(String, ThreadId)>> {
        let threads = self
            .fetch_all_threads(&pr.repo.owner, &pr.repo.name, pr.number)
            .await?;
        let mut out = Vec::new();
        for comment in comments {
            let tag = marker_tag(&comment.marker);
            if let Some(t) = threads
                .iter()
                .find(|t| t.comments.iter().any(|c| c.body.contains(&tag)))
            {
                out.push((comment.marker.clone(), ThreadId::new(t.id.clone())));
            }
        }
        Ok(out)
    }

    async fn changed_since_via_search(&self) -> HostResult<Changes> {
        let nodes = graphql::search_prs(&self.graphql, "is:pr is:open involves:@me").await?;
        let prs = nodes.iter().filter_map(search_node_to_pr_ref).collect();
        Ok(Changes {
            prs,
            cursor: "search-fallback".to_string(),
            poll_after_ms: Some(60_000),
        })
    }

    async fn fetch_pr_detail(&self, pr: &PrRef) -> HostResult<PrDetail> {
        let (owner, repo, number) = (pr.repo.owner.as_str(), pr.repo.name.as_str(), pr.number);

        let graphql::PrCoreAndViewer { core, viewer_login } =
            graphql::fetch_pr_core(&self.graphql, owner, repo, number).await?;
        let files = pagination::paginate_all(|after| {
            graphql::fetch_files_page(&self.graphql, owner, repo, number, after)
        })
        .await?;
        let threads = self.fetch_all_threads(owner, repo, number).await?;

        let head_sha = Sha::new(core.head_ref_oid.clone());
        let base_sha = Sha::new(core.base_ref_oid.clone());

        let mut reviewers: Vec<Reviewer> = Vec::new();
        if let Some(rr) = &core.review_requests {
            for req in rr.iter() {
                if let Some(reviewer) = &req.requested_reviewer {
                    let (person, team) = if reviewer.typename == "Team" {
                        let name = reviewer.name.clone().unwrap_or_default();
                        (
                            Person {
                                login: name.clone(),
                                name: Some(name.clone()),
                                avatar_url: None,
                            },
                            Some(name),
                        )
                    } else {
                        (
                            Person {
                                login: reviewer.login.clone().unwrap_or_default(),
                                name: reviewer.name.clone(),
                                avatar_url: reviewer.avatar_url.clone(),
                            },
                            None,
                        )
                    };
                    reviewers.push(Reviewer {
                        person,
                        state: ReviewerState::Requested,
                        team,
                    });
                }
            }
        }
        if let Some(lr) = &core.latest_reviews {
            for r in lr.iter() {
                reviewers.push(Reviewer {
                    person: person_or_ghost(r.author.clone()),
                    state: map_reviewer_state(&r.state),
                    team: None,
                });
            }
        }

        let assignees = core
            .assignees
            .iter()
            .cloned()
            .map(graphql::GqlActor::into_person)
            .collect();
        let labels = core
            .labels
            .as_ref()
            .map(|l| l.iter().map(|x| x.name.clone()).collect())
            .unwrap_or_default();

        // The viewer's own latest review (plan "Commits tab"): a commit is
        // "new" when it landed after that review was submitted.
        let my_last_review_at = core.latest_reviews.as_ref().and_then(|lr| {
            lr.iter()
                .find(|r| {
                    r.author.as_ref().map(|a| a.login.as_str()) == Some(viewer_login.as_str())
                })
                .and_then(|r| r.submitted_at.as_deref())
                .map(time::parse_millis)
        });
        let commits = core
            .commits
            .iter()
            .map(|n| map_commit(&n.commit, my_last_review_at))
            .collect();
        let checks = core
            .status_check_rollup
            .as_ref()
            .map(|r| r.contexts.iter().map(map_check_context).collect())
            .unwrap_or_default();

        let domain_threads = threads.iter().map(|t| map_thread(t, &head_sha)).collect();
        let viewed = files
            .iter()
            .map(|f| FileViewed {
                path: f.path.clone(),
                state: map_viewed_state(&f.viewer_viewed_state),
            })
            .collect();

        let approvals = core
            .latest_reviews
            .as_ref()
            .map(|c| c.iter().filter(|r| r.state == "APPROVED").count() as u32)
            .unwrap_or(0);
        let checks_rollup = core
            .status_check_rollup
            .as_ref()
            .map(|r| map_check_rollup(&r.state))
            .unwrap_or(CheckRollup::None);

        let summary = PrSummary {
            pr: PrRef {
                repo: pr.repo.clone(),
                number: pr.number,
                host_id: Some(core.id.clone()),
            },
            title: core.title.clone(),
            author: person_or_ghost(core.author.clone()),
            state: pr_state(&core.state, core.is_draft),
            base_ref: core.base_ref_name.clone(),
            head_ref: core.head_ref_name.clone(),
            head_sha: head_sha.clone(),
            additions: core.additions,
            deletions: core.deletions,
            changed_files: core.changed_files,
            reasons: vec![],
            checks: checks_rollup,
            approvals,
            required_approvals: None,
            updated_at: time::parse_millis(&core.updated_at),
            url: core.url.clone(),
            review_decision: map_review_decision(core.review_decision.as_deref()),
            conflicts: core.mergeable.as_deref() == Some("CONFLICTING"),
            created_at: Some(time::parse_millis(&core.created_at)),
            // Computed app-side from the stored PR rows (plan "Stack"), not
            // host data — never filled by an adapter.
            stack: None,
        };

        Ok(PrDetail {
            summary,
            body: core.body.clone(),
            base_sha,
            commits,
            checks,
            reviewers,
            assignees,
            labels,
            threads: domain_threads,
            viewed,
        })
    }
}

#[async_trait]
impl HostAdapter for GithubAdapter {
    fn kind(&self) -> HostKind {
        HostKind::Github
    }

    async fn viewer(&self) -> HostResult<Person> {
        Ok(graphql::viewer(&self.graphql).await?.into_person())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            viewed: true,
            pending_review: true,
            suggestions: true,
            resolve_threads: true,
            merge_queue: true,
            auto_merge: true,
            reactions: true,
        }
    }

    async fn changed_since(&self, cursor: Option<&str>) -> HostResult<Changes> {
        match rest::poll_notifications(&self.rest, cursor).await {
            Ok(poll) => {
                let prs = poll
                    .notifications
                    .iter()
                    .filter_map(notification_to_pr_ref)
                    .collect();
                Ok(Changes {
                    prs,
                    cursor: poll.cursor,
                    poll_after_ms: poll.poll_after_ms,
                })
            }
            // The notifications API needs a classic token; fine-grained PATs
            // and GitHub App tokens get rejected (plan §6.6).
            Err(HostError::Auth) | Err(HostError::NotFound(_)) => {
                self.changed_since_via_search().await
            }
            Err(e) => Err(e),
        }
    }

    async fn search(&self, section: &InboxSection) -> HostResult<Vec<PrSummary>> {
        let (query, reason) = section_query(section);
        let nodes = graphql::search_prs(&self.graphql, &query).await?;
        Ok(nodes
            .iter()
            .filter_map(|n| search_node_to_summary(n, reason))
            .collect())
    }

    async fn list_repos(&self) -> HostResult<Vec<HostRepo>> {
        let repos = graphql::list_repos(&self.graphql).await?;
        Ok(repos
            .into_iter()
            .map(|r| HostRepo {
                repo: RepoRef {
                    host: HostKind::Github,
                    owner: r.owner.login,
                    name: r.name,
                },
                description: r.description,
                private: r.is_private,
                pushed_at: r.pushed_at.as_deref().map(time::parse_millis),
            })
            .collect())
    }

    async fn get_pr(&self, pr: &PrRef, etag: Option<&str>) -> HostResult<Fetched<PrDetail>> {
        let path = format!(
            "repos/{}/{}/pulls/{}",
            pr.repo.owner, pr.repo.name, pr.number
        );
        match self
            .rest
            .get_conditional::<serde_json::Value>(&path, etag)
            .await?
        {
            Fetched::NotModified => Ok(Fetched::NotModified),
            Fetched::Modified { etag, .. } => {
                let detail = self.fetch_pr_detail(pr).await?;
                Ok(Fetched::Modified {
                    etag,
                    value: detail,
                })
            }
        }
    }

    async fn find_open_pr(&self, repo: &RepoRef, head: &str) -> HostResult<Option<PrRef>> {
        let pulls =
            rest::list_open_pulls_for_head(&self.rest, &repo.owner, &repo.name, head).await?;
        Ok(pulls.into_iter().next().map(|p| pull_to_pr_ref(repo, p)))
    }

    async fn create_pr(&self, repo: &RepoRef, pr: &NewPr) -> HostResult<PrRef> {
        let pull = rest::create_pull(&self.rest, &repo.owner, &repo.name, pr).await?;
        Ok(pull_to_pr_ref(repo, pull))
    }

    async fn set_viewed(&self, pr: &PrRef, path: &str, viewed: bool) -> HostResult<()> {
        let node_id = self.resolve_node_id(pr).await?;
        graphql::set_viewed(&self.graphql, &node_id, path, viewed).await
    }

    async fn submit_review(
        &self,
        pr: &PrRef,
        review: &ReviewSubmission,
    ) -> HostResult<SubmittedReview> {
        let node_id = self.resolve_node_id(pr).await?;
        let event = match review.event {
            ReviewEvent::Comment => "COMMENT",
            ReviewEvent::Approve => "APPROVE",
            ReviewEvent::RequestChanges => "REQUEST_CHANGES",
        };
        let threads: Vec<graphql::DraftThread> = review
            .comments
            .iter()
            .map(|c| {
                let pos = anchor::to_github(&c.anchor);
                graphql::DraftThread {
                    path: pos.path,
                    line: pos.line,
                    side: pos.side,
                    start_line: pos.start_line,
                    start_side: pos.start_side,
                    body: format!("{}\n\n{}", c.body, marker_tag(&c.marker)),
                }
            })
            .collect();

        let review_id = graphql::add_pull_request_review(
            &self.graphql,
            &node_id,
            event,
            &review.body,
            review.commit.as_str(),
            threads,
        )
        .await?;

        let threads = self
            .find_thread_ids_for_markers(pr, &review.comments)
            .await?;
        Ok(SubmittedReview { review_id, threads })
    }

    async fn find_marker(&self, pr: &PrRef, marker: &str) -> HostResult<Option<CommentId>> {
        let threads = self
            .fetch_all_threads(&pr.repo.owner, &pr.repo.name, pr.number)
            .await?;
        let tag = marker_tag(marker);
        for t in &threads {
            if let Some(c) = t.comments.iter().find(|c| c.body.contains(&tag)) {
                return Ok(Some(CommentId::new(c.full_database_id.clone())));
            }
        }
        Ok(None)
    }

    async fn reply(&self, _pr: &PrRef, thread: &ThreadId, body: &str) -> HostResult<CommentId> {
        let id = graphql::add_review_thread_reply(&self.graphql, thread.as_str(), body).await?;
        Ok(CommentId::new(id))
    }

    async fn resolve(&self, _pr: &PrRef, thread: &ThreadId, resolved: bool) -> HostResult<()> {
        graphql::resolve_thread(&self.graphql, thread.as_str(), resolved).await
    }

    async fn edit_comment(&self, pr: &PrRef, comment: &CommentId, body: &str) -> HostResult<()> {
        rest::edit_comment(
            &self.rest,
            &pr.repo.owner,
            &pr.repo.name,
            comment.as_str(),
            body,
        )
        .await
    }

    async fn delete_comment(&self, pr: &PrRef, comment: &CommentId) -> HostResult<()> {
        rest::delete_comment(&self.rest, &pr.repo.owner, &pr.repo.name, comment.as_str()).await
    }

    async fn add_reaction(&self, pr: &PrRef, comment: &CommentId, emoji: &str) -> HostResult<()> {
        rest::add_reaction(
            &self.rest,
            &pr.repo.owner,
            &pr.repo.name,
            comment.as_str(),
            emoji,
        )
        .await
    }

    async fn remove_reaction(
        &self,
        pr: &PrRef,
        comment: &CommentId,
        emoji: &str,
    ) -> HostResult<()> {
        // REST reaction deletion needs the reaction's own id, not the
        // comment's: list, find the viewer's matching one, delete that.
        let viewer = self.viewer().await?;
        let reactions =
            rest::list_reactions(&self.rest, &pr.repo.owner, &pr.repo.name, comment.as_str())
                .await?;
        let mine = reactions.iter().find(|r| {
            r.content == emoji
                && r.user
                    .as_ref()
                    .map(|u| u.login == viewer.login)
                    .unwrap_or(false)
        });
        match mine {
            Some(r) => {
                rest::delete_reaction(
                    &self.rest,
                    &pr.repo.owner,
                    &pr.repo.name,
                    comment.as_str(),
                    r.id,
                )
                .await
            }
            None => Ok(()),
        }
    }

    async fn assignable_users(&self, repo: &RepoRef) -> HostResult<Vec<Person>> {
        let actors = graphql::assignable_users(&self.graphql, &repo.owner, &repo.name).await?;
        Ok(actors
            .into_iter()
            .map(graphql::GqlActor::into_person)
            .collect())
    }

    async fn request_reviewers(
        &self,
        pr: &PrRef,
        users: &[String],
        teams: &[String],
    ) -> HostResult<()> {
        rest::request_reviewers(
            &self.rest,
            &pr.repo.owner,
            &pr.repo.name,
            pr.number,
            users,
            teams,
        )
        .await
    }

    async fn remove_reviewer(&self, pr: &PrRef, user: &str) -> HostResult<()> {
        rest::remove_requested_reviewer(&self.rest, &pr.repo.owner, &pr.repo.name, pr.number, user)
            .await
    }

    async fn mark_ready(&self, pr: &PrRef) -> HostResult<()> {
        let node_id = self.resolve_node_id(pr).await?;
        graphql::mark_ready_for_review(&self.graphql, &node_id).await
    }

    async fn convert_to_draft(&self, pr: &PrRef) -> HostResult<()> {
        let node_id = self.resolve_node_id(pr).await?;
        graphql::convert_to_draft(&self.graphql, &node_id).await
    }

    async fn update_branch(&self, pr: &PrRef) -> HostResult<()> {
        let node_id = self.resolve_node_id(pr).await?;
        graphql::update_pull_request_branch(&self.graphql, &node_id).await
    }

    async fn merge_status(&self, pr: &PrRef) -> HostResult<MergeStatus> {
        let settings =
            graphql::pr_merge_status(&self.graphql, &pr.repo.owner, &pr.repo.name, pr.number)
                .await?;
        let status = settings
            .pull_request
            .ok_or_else(|| HostError::NotFound(format!("{}#{}", pr.repo.slug(), pr.number)))?;

        let mut allowed_methods = Vec::new();
        if settings.merge_commit_allowed {
            allowed_methods.push(MergeMethod::Merge);
        }
        if settings.squash_merge_allowed {
            allowed_methods.push(MergeMethod::Squash);
        }
        if settings.rebase_merge_allowed {
            allowed_methods.push(MergeMethod::Rebase);
        }

        Ok(MergeStatus {
            mergeable: map_mergeable(&status.mergeable),
            state: map_merge_state(&status.merge_state_status),
            allowed_methods,
            auto_merge_enabled: status.auto_merge_request.is_some(),
            in_queue: status.is_in_merge_queue,
            required_approvals: None,
            auto_merge_allowed: settings.auto_merge_allowed,
            merge_queue: status.is_merge_queue_enabled,
        })
    }

    async fn merge(
        &self,
        pr: &PrRef,
        method: MergeMethod,
        title: Option<String>,
        body: Option<String>,
    ) -> HostResult<Sha> {
        let node_id = self.resolve_node_id(pr).await?;
        let oid = graphql::merge_pull_request(
            &self.graphql,
            &node_id,
            merge_method_name(method),
            title.as_deref(),
            body.as_deref(),
        )
        .await?;
        Ok(Sha::new(oid))
    }

    async fn enable_auto_merge(&self, pr: &PrRef, method: MergeMethod) -> HostResult<()> {
        let node_id = self.resolve_node_id(pr).await?;
        graphql::enable_auto_merge(&self.graphql, &node_id, merge_method_name(method)).await
    }

    async fn disable_auto_merge(&self, pr: &PrRef) -> HostResult<()> {
        let node_id = self.resolve_node_id(pr).await?;
        graphql::disable_auto_merge(&self.graphql, &node_id).await
    }

    async fn enqueue(&self, pr: &PrRef) -> HostResult<()> {
        let node_id = self.resolve_node_id(pr).await?;
        graphql::enqueue_pull_request(&self.graphql, &node_id).await
    }

    async fn rerun_check(&self, pr: &PrRef, check: &Check) -> HostResult<()> {
        let (owner, repo) = (pr.repo.owner.as_str(), pr.repo.name.as_str());
        // An Actions check run's `detailsUrl` looks like
        // `https://github.com/{owner}/{repo}/actions/runs/{run_id}/job/{job_id}`;
        // rerunning its *workflow run*'s failed jobs covers the whole job,
        // not just the one check-run GitHub split it into.
        if let Some(run_id) = check.url.as_deref().and_then(actions_run_id) {
            return rest::rerun_workflow_failed_jobs(&self.rest, owner, repo, run_id).await;
        }
        if check.id.chars().all(|c| c.is_ascii_digit()) {
            return rest::rerequest_check_run(&self.rest, owner, repo, &check.id).await;
        }
        Err(HostError::Unsupported(
            "this check has no re-runnable REST resource",
        ))
    }

    async fn check_log(&self, pr: &PrRef, check: &Check) -> HostResult<Option<String>> {
        let (owner, repo) = (pr.repo.owner.as_str(), pr.repo.name.as_str());
        // Only an Actions job's log is ours to fetch; anything else (a
        // third-party check-run app, a plain commit status) links out to
        // wherever it runs — `check.url` is all a caller can show for it.
        let Some(job_id) = check.url.as_deref().and_then(actions_job_id) else {
            return Ok(None);
        };
        let log = rest::job_log(&self.rest, owner, repo, job_id).await?;
        Ok(Some(logs::tail(&log, logs::DEFAULT_TAIL_LINES)))
    }

    async fn repo_info(&self, repo: &RepoRef) -> HostResult<HostRepo> {
        let info = graphql::repo_info(&self.graphql, &repo.owner, &repo.name).await?;
        Ok(HostRepo {
            repo: repo.clone(),
            description: info.description,
            private: info.is_private,
            pushed_at: info.pushed_at.as_deref().map(time::parse_millis),
        })
    }
}

/// "Report a problem" as an issue (docs/telemetry.md). The labels are a
/// nicety: GitHub refuses them (422, or 403 for a token that may open but
/// not label issues), so one refusal is retried once without them.
#[async_trait]
impl IssueTracker for GithubAdapter {
    fn has_token(&self) -> bool {
        self.token.token().is_some()
    }

    async fn login(&self) -> HostResult<String> {
        Ok(graphql::viewer(&self.graphql).await?.login)
    }

    async fn create_issue(
        &self,
        owner: &str,
        name: &str,
        issue: &NewIssue,
    ) -> HostResult<CreatedIssue> {
        let first = rest::create_issue(&self.rest, owner, name, issue).await;
        let created = match first {
            Err(HostError::Validation(_) | HostError::Auth) if !issue.labels.is_empty() => {
                tracing::info!("issue labels refused; retrying without them");
                let bare = NewIssue {
                    labels: Vec::new(),
                    ..issue.clone()
                };
                rest::create_issue(&self.rest, owner, name, &bare).await?
            }
            other => other?,
        };
        Ok(CreatedIssue {
            number: created.number,
            url: created.html_url,
        })
    }
}

/// Pulls `{run_id}` out of an Actions check-run `detailsUrl`
/// (`.../actions/runs/{run_id}/...`).
fn actions_run_id(url: &str) -> Option<&str> {
    let (_, rest) = url.split_once("/actions/runs/")?;
    rest.split('/').next().filter(|s| !s.is_empty())
}

/// Pulls `{job_id}` out of an Actions check-run `detailsUrl`
/// (`.../actions/runs/{run_id}/job/{job_id}`), the id `check_log` needs for
/// `GET .../actions/jobs/{job_id}/logs` — the run id alone isn't enough,
/// the logs endpoint is per-job.
fn actions_job_id(url: &str) -> Option<&str> {
    let (_, rest) = url.split_once("/job/")?;
    let id = rest.split(['/', '#', '?']).next()?;
    (!id.is_empty() && id.chars().all(|c| c.is_ascii_digit())).then_some(id)
}

// ── mapping helpers ─────────────────────────────────────────────────────

fn marker_tag(marker: &str) -> String {
    format!("<!-- theodore:{marker} -->")
}

fn person_or_ghost(a: Option<graphql::GqlActor>) -> Person {
    a.map(graphql::GqlActor::into_person).unwrap_or(Person {
        login: "ghost".to_string(),
        name: None,
        avatar_url: None,
    })
}

fn pr_state(state: &str, is_draft: bool) -> PrState {
    if is_draft {
        return PrState::Draft;
    }
    match state {
        "MERGED" => PrState::Merged,
        "CLOSED" => PrState::Closed,
        _ => PrState::Open,
    }
}

fn map_check_rollup(state: &str) -> CheckRollup {
    match state {
        "SUCCESS" => CheckRollup::Pass,
        "FAILURE" | "ERROR" => CheckRollup::Fail,
        "PENDING" | "EXPECTED" => CheckRollup::Pending,
        _ => CheckRollup::None,
    }
}

fn map_reviewer_state(state: &str) -> ReviewerState {
    match state {
        "APPROVED" => ReviewerState::Approved,
        "CHANGES_REQUESTED" => ReviewerState::ChangesRequested,
        _ => ReviewerState::Commented,
    }
}

fn merge_method_name(method: MergeMethod) -> &'static str {
    match method {
        MergeMethod::Merge => "MERGE",
        MergeMethod::Squash => "SQUASH",
        MergeMethod::Rebase => "REBASE",
    }
}

/// GitHub's `PullRequestReviewDecision`: `APPROVED`, `CHANGES_REQUESTED`,
/// `REVIEW_REQUIRED`, or `null` before any review has been requested.
fn map_review_decision(decision: Option<&str>) -> Option<ReviewDecision> {
    match decision {
        Some("APPROVED") => Some(ReviewDecision::Approved),
        Some("CHANGES_REQUESTED") => Some(ReviewDecision::ChangesRequested),
        Some("REVIEW_REQUIRED") => Some(ReviewDecision::ReviewRequired),
        _ => None,
    }
}

/// GitHub's `MergeableState`: `MERGEABLE`, `CONFLICTING`, `UNKNOWN`.
fn map_mergeable(state: &str) -> Option<bool> {
    match state {
        "MERGEABLE" => Some(true),
        "CONFLICTING" => Some(false),
        _ => None,
    }
}

/// GitHub's `MergeStateStatus`. `DRAFT`/`HAS_HOOKS` have no counterpart in
/// the host-neutral `MergeState` and fold into `Unknown`.
fn map_merge_state(state: &str) -> MergeState {
    match state {
        "CLEAN" => MergeState::Clean,
        "BLOCKED" => MergeState::Blocked,
        "BEHIND" => MergeState::Behind,
        "DIRTY" => MergeState::Dirty,
        "UNSTABLE" => MergeState::Unstable,
        _ => MergeState::Unknown,
    }
}

/// GitHub's `ReactionContent` enum -> the REST content name used everywhere
/// else in this codebase (`ReactRequest::emoji`, the REST reaction
/// endpoints): `+1`, `-1`, `laugh`, `hooray`, `confused`, `heart`, `rocket`,
/// `eyes`.
fn reaction_emoji_name(content: &str) -> &'static str {
    match content {
        "THUMBS_UP" => "+1",
        "THUMBS_DOWN" => "-1",
        "LAUGH" => "laugh",
        "HOORAY" => "hooray",
        "CONFUSED" => "confused",
        "HEART" => "heart",
        "ROCKET" => "rocket",
        "EYES" => "eyes",
        _ => "eyes",
    }
}

fn map_reactions(groups: &[graphql::GqlReactionGroup]) -> Vec<ReactionCount> {
    groups
        .iter()
        .filter(|g| g.reactors.total_count > 0)
        .map(|g| ReactionCount {
            emoji: reaction_emoji_name(&g.content).to_string(),
            count: g.reactors.total_count,
            viewer_reacted: g.viewer_has_reacted,
        })
        .collect()
}

fn map_viewed_state(state: &str) -> ViewedState {
    match state {
        "VIEWED" => ViewedState::Viewed,
        "DISMISSED" => ViewedState::Dismissed,
        _ => ViewedState::Unviewed,
    }
}

fn map_check_context(c: &graphql::GqlCheckContext) -> Check {
    if c.typename == "CheckRun" {
        let status = match (c.status.as_deref(), c.conclusion.as_deref()) {
            (_, Some("SUCCESS")) => CheckStatus::Pass,
            (_, Some("NEUTRAL" | "CANCELLED" | "STALE" | "SKIPPED")) => CheckStatus::Skipped,
            (_, Some(_)) => CheckStatus::Fail,
            (_, None) => CheckStatus::Pending, // QUEUED / IN_PROGRESS, or COMPLETED with no conclusion yet
        };
        let duration_ms = match (&c.started_at, &c.completed_at) {
            (Some(s), Some(e)) => Some(time::parse_millis(e) - time::parse_millis(s)),
            _ => None,
        };
        Check {
            // The REST numeric id when GitHub has one (`rerun_check` needs
            // it); the opaque GraphQL node id is otherwise unusable for a
            // REST call, so there's no reason to prefer it here.
            id: c
                .database_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| c.id.clone()),
            name: c.name.clone().unwrap_or_default(),
            status,
            url: c.details_url.clone(),
            duration_ms,
        }
    } else {
        let status = match c.state.as_deref() {
            Some("SUCCESS") => CheckStatus::Pass,
            Some("FAILURE") | Some("ERROR") => CheckStatus::Fail,
            _ => CheckStatus::Pending,
        };
        Check {
            id: c.id.clone(),
            name: c.context.clone().unwrap_or_default(),
            status,
            url: c.target_url.clone(),
            duration_ms: None,
        }
    }
}

fn map_commit(c: &graphql::GqlCommit, my_last_review_at: Option<Millis>) -> Commit {
    let title = c.message.lines().next().unwrap_or_default().to_string();
    let author = match &c.author {
        Some(a) => match &a.user {
            Some(u) => Person {
                login: u.login.clone(),
                name: a.name.clone(),
                avatar_url: u.avatar_url.clone(),
            },
            None => Person {
                login: a.name.clone().unwrap_or_default(),
                name: a.name.clone(),
                avatar_url: None,
            },
        },
        None => Person {
            login: String::new(),
            name: None,
            avatar_url: None,
        },
    };
    let at = time::parse_millis(&c.committed_date);
    Commit {
        sha: Sha::new(c.oid.clone()),
        title,
        author,
        at,
        after_my_review: my_last_review_at.is_some_and(|reviewed_at| at > reviewed_at),
    }
}

fn map_thread(t: &graphql::GqlThread, head_sha: &Sha) -> Thread {
    let anchor = t.line.and_then(|line| {
        anchor::from_github(
            head_sha.clone(),
            &t.path,
            &t.diff_side,
            line,
            t.start_line,
            t.start_diff_side.as_deref(),
        )
    });
    let comments = t
        .comments
        .iter()
        .map(|c| Comment {
            id: CommentId::new(c.full_database_id.clone()),
            author: person_or_ghost(c.author.clone()),
            body: c.body.clone(),
            created_at: time::parse_millis(&c.created_at),
            edited: c.last_edited_at.is_some(),
            reactions: map_reactions(&c.reaction_groups),
        })
        .collect();
    Thread {
        id: ThreadId::new(t.id.clone()),
        anchor,
        resolved: t.is_resolved,
        outdated: t.is_outdated,
        comments,
    }
}

fn notification_to_pr_ref(n: &rest::GhNotification) -> Option<PrRef> {
    let number = n.pr_number()?;
    Some(PrRef {
        repo: RepoRef {
            host: HostKind::Github,
            owner: n.repository.owner.login.clone(),
            name: n.repository.name.clone(),
        },
        number,
        host_id: None,
    })
}

/// Same shape `get_pr` returns: the repo as asked, the PR number, and the
/// GraphQL node id as `host_id` (saves `resolve_node_id` a round trip).
fn pull_to_pr_ref(repo: &RepoRef, pull: rest::GhPull) -> PrRef {
    PrRef {
        repo: repo.clone(),
        number: pull.number,
        host_id: pull.node_id,
    }
}

fn search_node_to_pr_ref(n: &graphql::GqlSearchPr) -> Option<PrRef> {
    let repo = n.repository.as_ref()?;
    Some(PrRef {
        repo: RepoRef {
            host: HostKind::Github,
            owner: repo.owner.login.clone(),
            name: repo.name.clone(),
        },
        number: n.number?,
        host_id: None,
    })
}

fn section_query(section: &InboxSection) -> (String, Option<InboxReason>) {
    match section {
        InboxSection::ReviewRequested => (
            "is:pr is:open review-requested:@me".to_string(),
            Some(InboxReason::ReviewRequested),
        ),
        InboxSection::Assigned => (
            "is:pr is:open assignee:@me".to_string(),
            Some(InboxReason::Assigned),
        ),
        InboxSection::Authored => (
            "is:pr is:open author:@me".to_string(),
            Some(InboxReason::Authored),
        ),
        InboxSection::Mentioned => (
            "is:pr is:open mentions:@me".to_string(),
            Some(InboxReason::Mentioned),
        ),
        InboxSection::Query(q) => (q.clone(), None),
    }
}

fn search_node_to_summary(
    n: &graphql::GqlSearchPr,
    reason: Option<InboxReason>,
) -> Option<PrSummary> {
    let repo_dto = n.repository.as_ref()?;
    let number = n.number?;
    let repo = RepoRef {
        host: HostKind::Github,
        owner: repo_dto.owner.login.clone(),
        name: repo_dto.name.clone(),
    };
    let approvals = n
        .latest_reviews
        .as_ref()
        .map(|c| c.iter().filter(|r| r.state == "APPROVED").count() as u32)
        .unwrap_or(0);
    let checks = n
        .status_check_rollup
        .as_ref()
        .map(|r| map_check_rollup(&r.state))
        .unwrap_or(CheckRollup::None);
    Some(PrSummary {
        pr: PrRef {
            repo,
            number,
            host_id: None,
        },
        title: n.title.clone().unwrap_or_default(),
        author: person_or_ghost(n.author.clone()),
        state: pr_state(
            n.state.as_deref().unwrap_or("OPEN"),
            n.is_draft.unwrap_or(false),
        ),
        base_ref: n.base_ref_name.clone().unwrap_or_default(),
        head_ref: n.head_ref_name.clone().unwrap_or_default(),
        head_sha: Sha::new(n.head_ref_oid.clone().unwrap_or_default()),
        additions: n.additions.unwrap_or(0),
        deletions: n.deletions.unwrap_or(0),
        changed_files: n.changed_files.unwrap_or(0),
        reasons: reason.into_iter().collect(),
        checks,
        approvals,
        required_approvals: None,
        updated_at: n.updated_at.as_deref().map(time::parse_millis).unwrap_or(0),
        url: n.url.clone().unwrap_or_default(),
        review_decision: map_review_decision(n.review_decision.as_deref()),
        conflicts: n.mergeable.as_deref() == Some("CONFLICTING"),
        created_at: n.created_at.as_deref().map(time::parse_millis),
        stack: None,
    })
}
