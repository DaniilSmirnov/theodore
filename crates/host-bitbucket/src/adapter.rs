use std::sync::Arc;

use async_trait::async_trait;
use theodore_domain::{
    Check, CheckRollup, CheckStatus, Comment, CommentId, Commit, HostKind, HostRepo, InboxReason,
    Person, PrDetail, PrRef, PrState, PrSummary, RepoRef, ReviewDecision, ReviewEvent, Reviewer,
    ReviewerState, Sha, Thread, ThreadId,
};
use theodore_host_http::TokenSource;
use theodore_ports::{
    Capabilities, Changes, Clock, Fetched, HostAdapter, HostError, HostResult, InboxSection,
    MergeMethod, NewPr, ReviewSubmission, SubmittedReview,
};

use crate::anchor;
use crate::auth::Credentials;
use crate::http::BbHttp;
use crate::model::{
    BbComment, BbCommit, BbContent, BbDiffStatEntry, BbPullRequest, BbStatus, BbUser,
    BranchEndpoint, BranchName, CreateCommentBody, CreatePrBody, MergeBody, ParentIdBody,
};
use crate::rest;
use crate::time;

/// `HostAdapter` for Bitbucket Cloud.
pub struct BitbucketAdapter {
    http: BbHttp,
    viewer_cache: tokio::sync::OnceCell<BbUser>,
}

impl BitbucketAdapter {
    pub fn new(token: Arc<dyn TokenSource>, clock: Arc<dyn Clock>) -> Self {
        Self::with_base_url(
            url::Url::parse("https://api.bitbucket.org/2.0/").expect("valid url"),
            token,
            clock,
        )
    }

    /// `base` must end with `/` (e.g. `https://api.bitbucket.org/2.0/`, or a
    /// wiremock server's `uri() + "/"` in tests).
    pub fn with_base_url(
        base: url::Url,
        token: Arc<dyn TokenSource>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let creds = Credentials::from_source(token.as_ref());
        Self {
            http: BbHttp::new(base, creds, clock),
            viewer_cache: tokio::sync::OnceCell::new(),
        }
    }

    async fn me(&self) -> HostResult<&BbUser> {
        self.viewer_cache
            .get_or_try_init(|| async { rest::viewer(&self.http).await })
            .await
    }

    async fn build_pr_detail(&self, pr_ref: &PrRef, pr: BbPullRequest) -> HostResult<PrDetail> {
        let workspace = &pr_ref.repo.owner;
        let repo_slug = &pr_ref.repo.name;
        let id = pr.id;

        let diffstat = rest::diffstat(&self.http, workspace, repo_slug, id).await?;
        let commits = rest::commits(&self.http, workspace, repo_slug, id).await?;
        let statuses = rest::statuses(&self.http, workspace, repo_slug, id).await?;
        let comments = rest::comments(&self.http, workspace, repo_slug, id).await?;

        let head_sha = pr
            .source
            .commit
            .as_ref()
            .map(|c| Sha::new(c.hash.clone()))
            .unwrap_or_else(|| Sha::new(""));
        let base_sha = pr
            .destination
            .commit
            .as_ref()
            .map(|c| Sha::new(c.hash.clone()))
            .unwrap_or_else(|| Sha::new(""));

        let (additions, deletions) = diff_stats(&diffstat);
        let changed_files = diffstat.len() as u32;

        let checks: Vec<Check> = statuses.iter().map(map_status).collect();
        let checks_rollup = rollup(&checks);
        let reviewers = map_reviewers(&pr);
        let review_decision = review_decision_from_participants(&pr);
        let approvals = reviewers
            .iter()
            .filter(|r| r.state == ReviewerState::Approved)
            .count() as u32;

        let summary = PrSummary {
            pr: pr_ref.clone(),
            title: pr.title.clone(),
            author: user_to_person(&pr.author),
            state: pr_state(&pr.state, pr.draft),
            base_ref: pr.destination.branch.name.clone(),
            head_ref: pr.source.branch.name.clone(),
            head_sha: head_sha.clone(),
            additions,
            deletions,
            changed_files,
            reasons: vec![],
            checks: checks_rollup,
            approvals,
            // Bitbucket exposes no "approvals required" count over this API
            // (merge checks are a branch-restriction setting, not read here).
            required_approvals: None,
            updated_at: time::parse_millis(&pr.updated_on),
            url: pr.links.html.href.clone(),
            review_decision,
            // Ditto for mergeability; `merge_status` is left `Unsupported`
            // (plan §6.4 — not asked for by this slice) rather than guessed.
            conflicts: false,
            created_at: pr.created_on.as_deref().map(time::parse_millis),
            // Computed by the app across stored PR rows.
            stack: None,
        };

        let threads = group_into_threads(comments, &head_sha);

        Ok(PrDetail {
            summary,
            body: pr.description.clone().unwrap_or_default(),
            base_sha,
            commits: commits.iter().map(map_commit).collect(),
            checks,
            reviewers,
            // Bitbucket PRs have no assignee distinct from author/reviewers.
            assignees: vec![],
            // Bitbucket Cloud pull requests have no labels.
            labels: vec![],
            threads,
            // No server-side per-file Viewed (plan §6.4 deviation, same as
            // GitLab).
            viewed: vec![],
        })
    }
}

#[async_trait]
impl HostAdapter for BitbucketAdapter {
    fn kind(&self) -> HostKind {
        HostKind::Bitbucket
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            viewed: false,
            pending_review: false,
            suggestions: false,
            resolve_threads: true,
            merge_queue: false,
            auto_merge: false,
            reactions: false,
            // Bitbucket's issue tracker is a separate, often-disabled
            // per-repo feature; "From an issue" hides for it.
            issues: false,
        }
    }

    async fn viewer(&self) -> HostResult<Person> {
        Ok(user_to_person(self.me().await?))
    }

    async fn changed_since(&self, _cursor: Option<&str>) -> HostResult<Changes> {
        // Bitbucket's query language has no indexed "updated after" filter
        // cheap enough to use as a real delta probe (plan §6.6 deviation);
        // every poll re-lists the PRs the viewer is involved in instead.
        let handle = self.me().await?.handle().to_string();
        let prs =
            rest::pull_requests_for_user(&self.http, &handle, &q_param("state=\"OPEN\"")).await?;
        Ok(Changes {
            prs: prs.iter().filter_map(pr_ref_from_bb).collect(),
            cursor: self.http.now().to_string(),
            poll_after_ms: Some(60_000),
        })
    }

    async fn search(&self, section: &InboxSection) -> HostResult<Vec<PrSummary>> {
        match section {
            // Bitbucket pull requests have no assignee, and no cheap
            // "mentioned" index over this API (plan §6.4/§6.6 deviation).
            InboxSection::Assigned | InboxSection::Mentioned => Ok(vec![]),
            InboxSection::Query(raw) => {
                let handle = self.me().await?.handle().to_string();
                let q = q_param(&format!("state=\"OPEN\" AND ({raw})"));
                let prs = rest::pull_requests_for_user(&self.http, &handle, &q).await?;
                Ok(prs.iter().map(|p| pr_to_summary(p, None)).collect())
            }
            InboxSection::ReviewRequested => {
                let me = self.me().await?;
                let handle = me.handle().to_string();
                let uuid = me.uuid.clone();
                let prs =
                    rest::pull_requests_for_user(&self.http, &handle, &q_param("state=\"OPEN\""))
                        .await?;
                Ok(prs
                    .iter()
                    .filter(|p| is_reviewer(p, &uuid))
                    .map(|p| pr_to_summary(p, Some(InboxReason::ReviewRequested)))
                    .collect())
            }
            InboxSection::Authored => {
                let me = self.me().await?;
                let handle = me.handle().to_string();
                let uuid = me.uuid.clone();
                let prs =
                    rest::pull_requests_for_user(&self.http, &handle, &q_param("state=\"OPEN\""))
                        .await?;
                Ok(prs
                    .iter()
                    .filter(|p| p.author.uuid == uuid)
                    .map(|p| pr_to_summary(p, Some(InboxReason::Authored)))
                    .collect())
            }
        }
    }

    async fn search_repo(
        &self,
        repo: &RepoRef,
        section: &InboxSection,
    ) -> HostResult<Vec<PrSummary>> {
        match section {
            InboxSection::Assigned | InboxSection::Mentioned => Ok(vec![]),
            InboxSection::Query(raw) => {
                let q = q_param(&format!("state=\"OPEN\" AND ({raw})"));
                let prs = rest::repo_pull_requests(&self.http, &repo.owner, &repo.name, &q).await?;
                Ok(prs.iter().map(|p| pr_to_summary(p, None)).collect())
            }
            InboxSection::ReviewRequested => {
                let uuid = self.me().await?.uuid.clone();
                let prs = rest::repo_pull_requests(
                    &self.http,
                    &repo.owner,
                    &repo.name,
                    &q_param("state=\"OPEN\""),
                )
                .await?;
                Ok(prs
                    .iter()
                    .filter(|p| is_reviewer(p, &uuid))
                    .map(|p| pr_to_summary(p, Some(InboxReason::ReviewRequested)))
                    .collect())
            }
            InboxSection::Authored => {
                let uuid = self.me().await?.uuid.clone();
                let prs = rest::repo_pull_requests(
                    &self.http,
                    &repo.owner,
                    &repo.name,
                    &q_param("state=\"OPEN\""),
                )
                .await?;
                Ok(prs
                    .iter()
                    .filter(|p| p.author.uuid == uuid)
                    .map(|p| pr_to_summary(p, Some(InboxReason::Authored)))
                    .collect())
            }
        }
    }

    async fn list_repos(&self) -> HostResult<Vec<HostRepo>> {
        let repos = rest::repositories(&self.http).await?;
        Ok(repos
            .iter()
            .filter_map(|r| {
                let (owner, name) = r.full_name.split_once('/')?;
                Some(HostRepo {
                    repo: RepoRef {
                        host: HostKind::Bitbucket,
                        owner: owner.to_string(),
                        name: name.to_string(),
                    },
                    description: r.description.clone(),
                    private: r.is_private,
                    pushed_at: r.updated_on.as_deref().map(time::parse_millis),
                })
            })
            .collect())
    }

    async fn get_pr(&self, pr: &PrRef, _etag: Option<&str>) -> HostResult<Fetched<PrDetail>> {
        // No conditional-fetch story over this API (plan §6.1 deviation):
        // always a full fetch, `etag` always `None`.
        let bb_pr =
            rest::get_pull_request(&self.http, &pr.repo.owner, &pr.repo.name, pr.number).await?;
        let detail = self.build_pr_detail(pr, bb_pr).await?;
        Ok(Fetched::Modified {
            etag: None,
            value: detail,
        })
    }

    async fn find_open_pr(&self, repo: &RepoRef, head: &str) -> HostResult<Option<PrRef>> {
        let q = format!(
            "state=OPEN&{}",
            q_param(&format!(
                "source.branch.name=\"{}\" AND state=\"OPEN\"",
                bbql_escape(head)
            ))
        );
        let prs = rest::repo_pull_requests(&self.http, &repo.owner, &repo.name, &q).await?;
        let full_name = repo.slug();
        Ok(prs
            .iter()
            // Same-repo PRs only: a fork's PR from a same-named branch
            // isn't ours to reuse.
            .find(|p| {
                p.source.branch.name == head
                    && p.source
                        .repository
                        .as_ref()
                        .is_none_or(|r| r.full_name.eq_ignore_ascii_case(&full_name))
            })
            .map(|p| PrRef {
                repo: repo.clone(),
                number: p.id,
                host_id: None,
            }))
    }

    async fn create_pr(&self, repo: &RepoRef, pr: &NewPr) -> HostResult<PrRef> {
        let created = rest::create_pull_request(
            &self.http,
            &repo.owner,
            &repo.name,
            &CreatePrBody {
                title: &pr.title,
                description: &pr.body,
                source: BranchEndpoint {
                    branch: BranchName { name: &pr.head },
                },
                destination: BranchEndpoint {
                    branch: BranchName { name: &pr.base },
                },
                draft: pr.draft,
            },
        )
        .await?;
        // Same shape as `get_pr`/`search`: Bitbucket's PR id, no host_id.
        Ok(PrRef {
            repo: repo.clone(),
            number: created.id,
            host_id: None,
        })
    }

    async fn set_viewed(&self, _pr: &PrRef, _path: &str, _viewed: bool) -> HostResult<()> {
        Err(HostError::Unsupported("set_viewed"))
    }

    async fn submit_review(
        &self,
        pr: &PrRef,
        review: &ReviewSubmission,
    ) -> HostResult<SubmittedReview> {
        let workspace = &pr.repo.owner;
        let repo_slug = &pr.repo.name;

        let mut threads = Vec::new();
        for comment in &review.comments {
            let inline = anchor::to_inline(&comment.anchor);
            let body = format!("{}\n\n{}", comment.body, marker_tag(&comment.marker));
            let created = rest::create_comment(
                &self.http,
                workspace,
                repo_slug,
                pr.number,
                &CreateCommentBody {
                    content: BbContent { raw: body },
                    inline: Some(&inline),
                    parent: None,
                },
            )
            .await?;
            threads.push((
                comment.marker.clone(),
                ThreadId::new(created.id.to_string()),
            ));
        }
        if !review.body.is_empty() {
            rest::create_comment(
                &self.http,
                workspace,
                repo_slug,
                pr.number,
                &CreateCommentBody {
                    content: BbContent {
                        raw: review.body.clone(),
                    },
                    inline: None,
                    parent: None,
                },
            )
            .await?;
        }

        match review.event {
            ReviewEvent::Approve => {
                rest::approve(&self.http, workspace, repo_slug, pr.number).await?
            }
            ReviewEvent::RequestChanges => {
                rest::request_changes(&self.http, workspace, repo_slug, pr.number).await?
            }
            ReviewEvent::Comment => {}
        }

        Ok(SubmittedReview {
            // Bitbucket has no single "review" resource id; synthesize one.
            review_id: format!("comments:{}", review.commit.as_str()),
            threads,
        })
    }

    async fn find_marker(&self, pr: &PrRef, marker: &str) -> HostResult<Option<CommentId>> {
        let tag = marker_tag(marker);
        let comments = rest::comments(&self.http, &pr.repo.owner, &pr.repo.name, pr.number).await?;
        Ok(comments
            .iter()
            .find(|c| c.content.raw.contains(&tag))
            .map(|c| CommentId::new(c.id.to_string())))
    }

    async fn reply(&self, pr: &PrRef, thread: &ThreadId, body: &str) -> HostResult<CommentId> {
        let parent_id: u64 = thread
            .as_str()
            .parse()
            .map_err(|_| HostError::Protocol(format!("bad thread id {thread}")))?;
        let created = rest::create_comment(
            &self.http,
            &pr.repo.owner,
            &pr.repo.name,
            pr.number,
            &CreateCommentBody {
                content: BbContent {
                    raw: body.to_string(),
                },
                inline: None,
                parent: Some(ParentIdBody { id: parent_id }),
            },
        )
        .await?;
        Ok(CommentId::new(created.id.to_string()))
    }

    async fn resolve(&self, pr: &PrRef, thread: &ThreadId, resolved: bool) -> HostResult<()> {
        if resolved {
            rest::resolve_comment(
                &self.http,
                &pr.repo.owner,
                &pr.repo.name,
                pr.number,
                thread.as_str(),
            )
            .await
        } else {
            rest::reopen_comment(
                &self.http,
                &pr.repo.owner,
                &pr.repo.name,
                pr.number,
                thread.as_str(),
            )
            .await
        }
    }

    async fn edit_comment(&self, pr: &PrRef, comment: &CommentId, body: &str) -> HostResult<()> {
        rest::edit_comment(
            &self.http,
            &pr.repo.owner,
            &pr.repo.name,
            pr.number,
            comment.as_str(),
            body,
        )
        .await?;
        Ok(())
    }

    async fn delete_comment(&self, pr: &PrRef, comment: &CommentId) -> HostResult<()> {
        rest::delete_comment(
            &self.http,
            &pr.repo.owner,
            &pr.repo.name,
            pr.number,
            comment.as_str(),
        )
        .await
    }

    async fn merge(
        &self,
        pr: &PrRef,
        method: MergeMethod,
        title: Option<String>,
        body: Option<String>,
    ) -> HostResult<Sha> {
        let strategy = match method {
            MergeMethod::Merge => "merge_commit",
            MergeMethod::Squash => "squash",
            // Bitbucket has no true rebase-and-merge over this API;
            // `Rebase` maps to its `fast_forward` strategy, the closest
            // analogue (fails outright if the branches have diverged —
            // plan §6.4 deviation).
            MergeMethod::Rebase => "fast_forward",
        };
        let message = merge_message(title.as_deref(), body.as_deref());
        let merged = rest::merge(
            &self.http,
            &pr.repo.owner,
            &pr.repo.name,
            pr.number,
            &MergeBody {
                merge_strategy: strategy,
                message: message.as_deref(),
                close_source_branch: false,
            },
        )
        .await?;
        let sha = merged
            .merge_commit
            .map(|c| c.hash)
            .ok_or_else(|| HostError::Protocol("merge response had no commit sha".into()))?;
        Ok(Sha::new(sha))
    }
}

fn q_param(q: &str) -> String {
    format!(
        "q={}",
        url::form_urlencoded::byte_serialize(q.as_bytes()).collect::<String>()
    )
}

/// BBQL string literal escaping: `\` and `"` are the only specials.
fn bbql_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn is_reviewer(pr: &BbPullRequest, uuid: &str) -> bool {
    pr.participants
        .iter()
        .any(|p| p.role == "REVIEWER" && p.user.uuid == uuid)
}

fn merge_message(title: Option<&str>, body: Option<&str>) -> Option<String> {
    match (title, body) {
        (Some(t), Some(b)) if !b.is_empty() => Some(format!("{t}\n\n{b}")),
        (Some(t), _) => Some(t.to_string()),
        (None, Some(b)) => Some(b.to_string()),
        (None, None) => None,
    }
}

fn marker_tag(marker: &str) -> String {
    format!("<!-- theodore:{marker} -->")
}

fn user_to_person(u: &BbUser) -> Person {
    Person {
        login: u.handle().to_string(),
        name: u.display_name.clone(),
        avatar_url: u.links.avatar.as_ref().map(|a| a.href.clone()),
    }
}

fn pr_state(state: &str, draft: bool) -> PrState {
    if draft {
        return PrState::Draft;
    }
    match state {
        "MERGED" => PrState::Merged,
        "DECLINED" | "SUPERSEDED" => PrState::Closed,
        _ => PrState::Open,
    }
}

fn map_check_status(state: &str) -> CheckStatus {
    match state {
        "SUCCESSFUL" => CheckStatus::Pass,
        "FAILED" => CheckStatus::Fail,
        "STOPPED" => CheckStatus::Skipped,
        _ => CheckStatus::Pending,
    }
}

fn map_status(s: &BbStatus) -> Check {
    Check {
        id: s.key.clone(),
        name: s.name.clone().unwrap_or_else(|| s.key.clone()),
        status: map_check_status(&s.state),
        url: s.url.clone(),
        duration_ms: None,
    }
}

fn rollup(checks: &[Check]) -> CheckRollup {
    if checks.is_empty() {
        CheckRollup::None
    } else if checks.iter().any(|c| c.status == CheckStatus::Fail) {
        CheckRollup::Fail
    } else if checks.iter().any(|c| c.status == CheckStatus::Pending) {
        CheckRollup::Pending
    } else {
        CheckRollup::Pass
    }
}

fn map_commit(c: &BbCommit) -> Commit {
    Commit {
        sha: Sha::new(c.hash.clone()),
        title: c.message.lines().next().unwrap_or_default().to_string(),
        author: c
            .author
            .user
            .as_ref()
            .map(user_to_person)
            .unwrap_or_else(|| Person {
                login: c.author.raw.clone(),
                name: Some(c.author.raw.clone()),
                avatar_url: None,
            }),
        at: time::parse_millis(&c.date),
        // Not computed for Bitbucket yet (plan "Commits tab" is GitHub-first).
        after_my_review: false,
    }
}

fn map_reviewers(pr: &BbPullRequest) -> Vec<Reviewer> {
    pr.reviewers
        .iter()
        .map(|r| {
            let participant = pr.participants.iter().find(|p| p.user.uuid == r.uuid);
            let state = match participant.and_then(|p| p.state.as_deref()) {
                Some("approved") => ReviewerState::Approved,
                Some("changes_requested") => ReviewerState::ChangesRequested,
                _ => ReviewerState::Requested,
            };
            Reviewer {
                person: user_to_person(r),
                state,
                team: None,
            }
        })
        .collect()
}

/// Bitbucket has no `reviewDecision` equivalent; approximated from the
/// reviewer participants' individual states, same spirit as GitLab's
/// approvals-based approximation.
fn review_decision_from_participants(pr: &BbPullRequest) -> Option<ReviewDecision> {
    let reviewers: Vec<_> = pr
        .participants
        .iter()
        .filter(|p| p.role == "REVIEWER")
        .collect();
    if reviewers.is_empty() {
        return None;
    }
    if reviewers
        .iter()
        .any(|p| p.state.as_deref() == Some("changes_requested"))
    {
        Some(ReviewDecision::ChangesRequested)
    } else if reviewers.iter().all(|p| p.approved) {
        Some(ReviewDecision::Approved)
    } else {
        Some(ReviewDecision::ReviewRequired)
    }
}

fn diff_stats(entries: &[BbDiffStatEntry]) -> (u32, u32) {
    let additions = entries.iter().map(|e| e.lines_added).sum();
    let deletions = entries.iter().map(|e| e.lines_removed).sum();
    (additions, deletions)
}

/// `full_name` ("workspace/repo_slug") -> (owner, name). Unlike GitLab's
/// nested namespaces, Bitbucket slugs are always a single segment, so the
/// one `/` is unambiguous.
fn pr_repo_ref(pr: &BbPullRequest) -> Option<RepoRef> {
    let full_name = &pr.destination.repository.as_ref()?.full_name;
    let (owner, name) = full_name.split_once('/')?;
    Some(RepoRef {
        host: HostKind::Bitbucket,
        owner: owner.to_string(),
        name: name.to_string(),
    })
}

fn pr_ref_from_bb(pr: &BbPullRequest) -> Option<PrRef> {
    Some(PrRef {
        repo: pr_repo_ref(pr)?,
        number: pr.id,
        host_id: None,
    })
}

fn pr_to_summary(pr: &BbPullRequest, reason: Option<InboxReason>) -> PrSummary {
    let repo = pr_repo_ref(pr).unwrap_or(RepoRef {
        host: HostKind::Bitbucket,
        owner: String::new(),
        name: String::new(),
    });
    PrSummary {
        pr: PrRef {
            repo,
            number: pr.id,
            host_id: None,
        },
        title: pr.title.clone(),
        author: user_to_person(&pr.author),
        state: pr_state(&pr.state, pr.draft),
        base_ref: pr.destination.branch.name.clone(),
        head_ref: pr.source.branch.name.clone(),
        head_sha: pr
            .source
            .commit
            .as_ref()
            .map(|c| Sha::new(c.hash.clone()))
            .unwrap_or_else(|| Sha::new("")),
        additions: 0,
        deletions: 0,
        changed_files: 0,
        reasons: reason.into_iter().collect(),
        // Not fetched for a list row (plan "fill where it's cheap" — a
        // per-PR `/statuses` call for every inbox row isn't).
        checks: CheckRollup::None,
        approvals: 0,
        required_approvals: None,
        updated_at: time::parse_millis(&pr.updated_on),
        url: pr.links.html.href.clone(),
        review_decision: None,
        conflicts: false,
        created_at: pr.created_on.as_deref().map(time::parse_millis),
        stack: None,
    }
}

/// Groups a PR's flat comment list into threads by walking each comment's
/// `parent` chain up to its root. A root that was itself deleted (rare: a
/// thread survives on its replies) falls back to the oldest surviving reply
/// for the thread's anchor/resolution metadata.
fn group_into_threads(comments: Vec<BbComment>, head_sha: &Sha) -> Vec<Thread> {
    use std::collections::{BTreeMap, HashMap, HashSet};

    let live: Vec<BbComment> = comments.into_iter().filter(|c| !c.deleted).collect();
    let parent_of: HashMap<u64, u64> = live
        .iter()
        .filter_map(|c| c.parent.as_ref().map(|p| (c.id, p.id)))
        .collect();
    let root_of = |mut id: u64| -> u64 {
        let mut seen = HashSet::new();
        while let Some(&p) = parent_of.get(&id) {
            if !seen.insert(id) {
                break;
            }
            id = p;
        }
        id
    };

    let mut by_root: BTreeMap<u64, Vec<&BbComment>> = BTreeMap::new();
    for c in &live {
        by_root.entry(root_of(c.id)).or_default().push(c);
    }

    by_root
        .into_iter()
        .map(|(root_id, mut cs)| {
            cs.sort_by_key(|c| c.id);
            let root = cs
                .iter()
                .find(|c| c.id == root_id)
                .copied()
                .or_else(|| cs.first().copied());
            let anchor = root
                .and_then(|r| r.inline.as_ref())
                .and_then(|i| anchor::from_inline(head_sha.clone(), i));
            let resolved = root.is_some_and(|r| r.resolution.is_some());
            Thread {
                id: ThreadId::new(root_id.to_string()),
                anchor,
                resolved,
                // Bitbucket doesn't flag a comment's position as outdated
                // after a later push (plan §6.3 deviation).
                outdated: false,
                comments: cs.iter().map(|c| map_comment(c)).collect(),
            }
        })
        .collect()
}

fn map_comment(c: &BbComment) -> Comment {
    Comment {
        id: CommentId::new(c.id.to_string()),
        author: user_to_person(&c.user),
        body: c.content.raw.clone(),
        created_at: time::parse_millis(&c.created_on),
        edited: c.updated_on.as_deref().is_some_and(|u| u != c.created_on),
        // Bitbucket comments have no reactions.
        reactions: vec![],
    }
}
