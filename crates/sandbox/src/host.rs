//! `SandboxHost`: the `HostAdapter` Theodore talks to in sandbox mode. It
//! answers like GitHub (same ids, search syntax, ETags, review/merge rules)
//! from the sandbox's own state and bare repos, behind the configured
//! faults. Never touches the network, a token, the keychain or `gh`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use theodore_domain::{
    Check, CommentId, HostKind, HostRepo, InboxReason, Person, PrDetail, PrRef, PrState, PrSummary,
    RepoRef, ReviewEvent, Sha, ThreadId, ViewedState,
};
use theodore_ports::{
    Capabilities, Changes, Fetched, HostAdapter, HostError, HostResult, InboxSection, MergeMethod,
    MergeStatus, NewPr, ReviewSubmission, SubmittedReview,
};

use crate::state::ReviewState;
use crate::world::{Locked, Refusal, World, WorldResult, marker_tag, now_ms};

pub struct SandboxHost {
    world: Arc<World>,
}

fn host_err(r: Refusal) -> HostError {
    match r {
        Refusal::NotFound(m) => HostError::NotFound(m),
        Refusal::Validation(m) => HostError::Validation(m),
        Refusal::Conflict(m) => HostError::Conflict(m),
        Refusal::Internal(m) => HostError::Protocol(format!("sandbox: {m}")),
    }
}

impl SandboxHost {
    pub fn new(world: Arc<World>) -> Self {
        Self { world }
    }

    /// Applies every active fault to one call: latency first, then the
    /// first failure that applies.
    async fn gate(&self, call: &'static str, pr: Option<&PrRef>) -> HostResult<()> {
        let (delay, verdict) = {
            let mut l = self.world.lock();
            let now = now_ms();
            let f = l.st.faults.clone();
            let mut delay = f.latency_by_call.get(call).copied().unwrap_or(0);
            if f.latency {
                delay += f.latency_ms;
            }
            if f.slow_merge_status && call == "merge_status" {
                delay += f.slow_merge_status_ms;
            }
            let verdict = if f.offline {
                Err(HostError::Network(
                    "sandbox: connection refused (offline fault)".into(),
                ))
            } else if f.auth {
                Err(HostError::Auth)
            } else if f.rate_limit {
                let reset_at = *l
                    .st
                    .rate_limit_reset_at
                    .get_or_insert(now + f.rate_limit_reset_ms as i64);
                if now >= reset_at {
                    l.st.rate_limit_reset_at = None;
                    l.st.faults.rate_limit = false;
                    l.save();
                    Ok(())
                } else {
                    l.save();
                    Err(HostError::RateLimited { reset_at })
                }
            } else if f.server_errors && self.world.random() < f.server_error_rate {
                Err(HostError::Network(
                    "sandbox: server error 502 Bad Gateway (server_errors fault)".into(),
                ))
            } else if f.not_found
                && let Some(pr) = pr
                && f.not_found_prs.contains(&pr.number)
            {
                Err(HostError::NotFound(format!(
                    "{}#{} (not_found fault)",
                    pr.repo.slug(),
                    pr.number
                )))
            } else {
                Ok(())
            };
            (delay, verdict)
        };
        if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
        verdict
    }

    /// Runs `f` on the locked world off the async runtime (it shells out to
    /// git), saving the state if `f` changed it.
    async fn with<T, F>(&self, f: F) -> HostResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Locked<'_>) -> WorldResult<T> + Send + 'static,
    {
        let world = self.world.clone();
        tokio::task::spawn_blocking(move || {
            let mut l = world.lock();
            let before = l.st.version;
            let out = f(&mut l);
            if l.st.version != before {
                l.save();
            }
            out
        })
        .await
        .map_err(|e| HostError::Protocol(format!("sandbox task: {e}")))?
        .map_err(host_err)
    }

    async fn pr_call<T, F>(&self, call: &'static str, pr: &PrRef, f: F) -> HostResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Locked<'_>, usize) -> WorldResult<T> + Send + 'static,
    {
        self.gate(call, Some(pr)).await?;
        let pr = pr.clone();
        self.with(move |l| {
            let idx = l.find_ref(&pr)?;
            f(l, idx)
        })
        .await
    }
}

/// GitHub search qualifiers the app sends (`is:pr is:open repo:o/r`,
/// `author:@me`, …); anything else is matched against the title.
fn matches_query(l: &Locked<'_>, idx: usize, q: &str) -> bool {
    let me = l.world.viewer().to_string();
    let p = &l.st.prs[idx];
    let reasons = l.reasons(idx);
    let who = |v: &str| -> String {
        if v == "@me" {
            me.clone()
        } else {
            v.to_string()
        }
    };
    for tok in q.split_whitespace() {
        let ok = match tok.split_once(':') {
            Some(("is", "pr")) | Some(("sort", _)) | Some(("archived", _)) => true,
            Some(("is", "open")) => p.is_open(),
            Some(("is", "closed")) => !p.is_open(),
            Some(("is", "merged")) => p.state == PrState::Merged,
            Some(("is", "unmerged")) => p.state != PrState::Merged,
            Some(("is", "draft")) => p.state == PrState::Draft,
            Some(("repo", slug)) => p.slug().eq_ignore_ascii_case(slug),
            Some(("author", v)) => p.author == who(v),
            Some(("assignee", v)) => p.assignees.contains(&who(v)),
            Some(("review-requested", v)) => {
                let v = who(v);
                p.requested_users.contains(&v)
                    || (v == me && reasons.contains(&InboxReason::TeamReviewRequested))
            }
            Some(("mentions", v)) => {
                let tag = format!("@{}", who(v));
                p.body.contains(&tag)
                    || p.threads
                        .iter()
                        .any(|t| t.comments.iter().any(|c| c.body.contains(&tag)))
            }
            Some(("involves", v)) => {
                let v = who(v);
                p.author == v
                    || p.assignees.contains(&v)
                    || p.requested_users.contains(&v)
                    || p.reviews.iter().any(|r| r.user == v)
            }
            Some(("label", v)) => p.labels.iter().any(|x| x == v),
            _ => p.title.to_lowercase().contains(&tok.to_lowercase()),
        };
        if !ok {
            return false;
        }
    }
    true
}

fn section_matches(l: &Locked<'_>, idx: usize, section: &InboxSection) -> bool {
    if let InboxSection::Query(q) = section {
        return matches_query(l, idx, q);
    }
    if !l.st.prs[idx].is_open() {
        return false;
    }
    let reasons = l.reasons(idx);
    let any = |rs: &[InboxReason]| rs.iter().any(|r| reasons.contains(r));
    match section {
        InboxSection::ReviewRequested => any(&[
            InboxReason::ReviewRequested,
            InboxReason::TeamReviewRequested,
            InboxReason::UpdatedAfterReview,
        ]),
        InboxSection::Assigned => any(&[InboxReason::Assigned]),
        InboxSection::Authored => any(&[InboxReason::Authored]),
        InboxSection::Mentioned => any(&[InboxReason::Mentioned]),
        InboxSection::Query(_) => unreachable!(),
    }
}

#[async_trait]
impl HostAdapter for SandboxHost {
    fn kind(&self) -> HostKind {
        HostKind::Github
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
            issues: false,
        }
    }

    async fn viewer(&self) -> HostResult<Person> {
        self.gate("viewer", None).await?;
        Ok(self.world.person(self.world.viewer()))
    }

    async fn viewer_for(&self, host: HostKind) -> HostResult<Person> {
        if host == HostKind::Github {
            self.viewer().await
        } else {
            Err(HostError::Unsupported("host"))
        }
    }

    async fn changed_since(&self, cursor: Option<&str>) -> HostResult<Changes> {
        self.gate("changed_since", None).await?;
        let cursor = cursor.and_then(|c| c.parse::<u64>().ok());
        self.with(move |l| {
            l.sync_all_heads(now_ms());
            let prs = match cursor {
                None => Vec::new(),
                Some(c) => (0..l.st.prs.len())
                    .filter(|&i| l.st.prs[i].version > c)
                    .map(|i| l.pr_ref(i))
                    .collect(),
            };
            Ok(Changes {
                prs,
                cursor: l.st.version.to_string(),
                poll_after_ms: Some(2_000),
            })
        })
        .await
    }

    async fn search(&self, section: &InboxSection) -> HostResult<Vec<PrSummary>> {
        self.gate("search", None).await?;
        let section = section.clone();
        self.with(move |l| {
            l.sync_all_heads(now_ms());
            let mut idx: Vec<usize> = (0..l.st.prs.len())
                .filter(|&i| l.world.hosted_repo(&l.st.prs[i].owner, &l.st.prs[i].repo))
                .filter(|&i| section_matches(l, i, &section))
                .collect();
            idx.sort_by_key(|&i| std::cmp::Reverse(l.st.prs[i].updated_at));
            Ok(idx.into_iter().map(|i| l.summary(i)).collect())
        })
        .await
    }

    async fn list_repos(&self) -> HostResult<Vec<HostRepo>> {
        self.gate("list_repos", None).await?;
        self.with(|l| Ok(l.repos())).await
    }

    async fn repo_info(&self, repo: &RepoRef) -> HostResult<HostRepo> {
        self.gate("repo_info", None).await?;
        let repo = repo.clone();
        self.with(move |l| {
            l.repos()
                .into_iter()
                .find(|r| r.repo.owner == repo.owner && r.repo.name == repo.name)
                .ok_or_else(|| Refusal::NotFound(repo.slug()))
        })
        .await
    }

    async fn get_pr(&self, pr: &PrRef, etag: Option<&str>) -> HostResult<Fetched<PrDetail>> {
        let etag = etag.map(str::to_string);
        self.pr_call("get_pr", pr, move |l, idx| {
            l.sync_head(idx, now_ms());
            let tag = l.etag(idx);
            if etag.as_deref() == Some(tag.as_str()) {
                return Ok(Fetched::NotModified);
            }
            Ok(Fetched::Modified {
                etag: Some(tag),
                value: l.detail(idx),
            })
        })
        .await
    }

    async fn find_open_pr(&self, repo: &RepoRef, head: &str) -> HostResult<Option<PrRef>> {
        self.gate("find_open_pr", None).await?;
        let (repo, head) = (repo.clone(), head.to_string());
        self.with(move |l| {
            Ok(l.find_open_by_head(&repo.owner, &repo.name, &head)
                .map(|i| l.pr_ref(i)))
        })
        .await
    }

    async fn create_pr(&self, repo: &RepoRef, pr: &NewPr) -> HostResult<PrRef> {
        self.gate("create_pr", None).await?;
        let (repo, pr) = (repo.clone(), pr.clone());
        self.with(move |l| {
            let idx = l.open_pr(
                &repo.owner,
                &repo.name,
                &pr.head,
                &pr.base,
                &pr.title,
                &pr.body,
                pr.draft,
                now_ms(),
            )?;
            Ok(l.pr_ref(idx))
        })
        .await
    }

    async fn set_viewed(&self, pr: &PrRef, path: &str, viewed: bool) -> HostResult<()> {
        let path = path.to_string();
        self.pr_call("set_viewed", pr, move |l, idx| {
            let state = if viewed {
                ViewedState::Viewed
            } else {
                ViewedState::Unviewed
            };
            l.st.prs[idx].viewed.insert(path, state);
            l.st.bump(idx);
            Ok(())
        })
        .await
    }

    async fn submit_review(
        &self,
        pr: &PrRef,
        review: &ReviewSubmission,
    ) -> HostResult<SubmittedReview> {
        let review = review.clone();
        self.pr_call("submit_review", pr, move |l, idx| {
            let me = l.world.viewer().to_string();
            let now = now_ms();
            let p = &l.st.prs[idx];
            if !p.is_open() {
                return Err(Refusal::Validation("Pull request is not open.".into()));
            }
            if p.author == me {
                match review.event {
                    ReviewEvent::Approve => {
                        return Err(Refusal::Validation(
                            "Can not approve your own pull request".into(),
                        ));
                    }
                    ReviewEvent::RequestChanges => {
                        return Err(Refusal::Validation(
                            "Can not request changes on your own pull request".into(),
                        ));
                    }
                    ReviewEvent::Comment => {}
                }
            }
            if review.event != ReviewEvent::Approve
                && review.body.trim().is_empty()
                && review.comments.is_empty()
            {
                return Err(Refusal::Validation("Review body is required.".into()));
            }
            let (base, _) = l.base_head(idx);
            let bare = l.world.bare(&p.owner, &p.repo);
            let files = bare
                .changed_paths(&base, review.commit.as_str(), true)
                .unwrap_or_default();
            for c in &review.comments {
                if !files.contains(&c.anchor.path) {
                    return Err(Refusal::Validation(format!(
                        "Path {} is not part of the pull request diff",
                        c.anchor.path
                    )));
                }
            }
            let mut threads = Vec::new();
            for c in &review.comments {
                // Idempotent on retry: the marker is already there.
                if let Some((thread, _)) = l.find_marker(idx, &c.marker) {
                    threads.push((c.marker.clone(), ThreadId::new(thread)));
                    continue;
                }
                let body = format!("{}\n\n{}", c.body, marker_tag(&c.marker));
                let (thread, _) = l.new_thread(
                    idx,
                    &c.anchor.path,
                    c.anchor.line,
                    c.anchor.side,
                    c.anchor.start_line,
                    body,
                    &me,
                    now,
                );
                threads.push((c.marker.clone(), ThreadId::new(thread)));
            }
            let state = match review.event {
                ReviewEvent::Approve => ReviewState::Approved,
                ReviewEvent::RequestChanges => ReviewState::ChangesRequested,
                ReviewEvent::Comment => ReviewState::Commented,
            };
            let review_id = l.add_review(idx, &me, state, review.body.clone(), now);
            Ok(SubmittedReview { review_id, threads })
        })
        .await
    }

    async fn find_marker(&self, pr: &PrRef, marker: &str) -> HostResult<Option<CommentId>> {
        let marker = marker.to_string();
        self.pr_call("find_marker", pr, move |l, idx| {
            Ok(l.find_marker(idx, &marker).map(|(_, c)| CommentId::new(c)))
        })
        .await
    }

    async fn reply(&self, pr: &PrRef, thread: &ThreadId, body: &str) -> HostResult<CommentId> {
        let thread = thread.0.clone();
        let body = body.to_string();
        self.pr_call("reply", pr, move |l, idx| {
            let me = l.world.viewer().to_string();
            l.reply(idx, Some(&thread), body, &me, now_ms())
                .map(CommentId::new)
        })
        .await
    }

    async fn resolve(&self, pr: &PrRef, thread: &ThreadId, resolved: bool) -> HostResult<()> {
        let thread = thread.0.clone();
        self.pr_call("resolve", pr, move |l, idx| {
            let t = l.st.prs[idx]
                .threads
                .iter_mut()
                .find(|t| t.id == thread)
                .ok_or_else(|| Refusal::NotFound(format!("thread {thread}")))?;
            t.resolved = resolved;
            l.st.bump(idx);
            Ok(())
        })
        .await
    }

    async fn edit_comment(&self, pr: &PrRef, comment: &CommentId, body: &str) -> HostResult<()> {
        let comment = comment.0.clone();
        let body = body.to_string();
        self.pr_call("edit_comment", pr, move |l, idx| {
            let (ti, ci) = l
                .find_comment(idx, &comment)
                .ok_or_else(|| Refusal::NotFound(format!("comment {comment}")))?;
            let me = l.world.viewer().to_string();
            let c = &mut l.st.prs[idx].threads[ti].comments[ci];
            if c.author != me {
                return Err(Refusal::Validation(
                    "You can only edit your own comments.".into(),
                ));
            }
            c.body = body;
            c.edited = true;
            l.st.touch(idx, now_ms());
            Ok(())
        })
        .await
    }

    async fn delete_comment(&self, pr: &PrRef, comment: &CommentId) -> HostResult<()> {
        let comment = comment.0.clone();
        self.pr_call("delete_comment", pr, move |l, idx| {
            let (ti, ci) = l
                .find_comment(idx, &comment)
                .ok_or_else(|| Refusal::NotFound(format!("comment {comment}")))?;
            let me = l.world.viewer().to_string();
            let threads = &mut l.st.prs[idx].threads;
            if threads[ti].comments[ci].author != me {
                return Err(Refusal::Validation(
                    "You can only delete your own comments.".into(),
                ));
            }
            threads[ti].comments.remove(ci);
            if threads[ti].comments.is_empty() {
                threads.remove(ti);
            }
            l.st.touch(idx, now_ms());
            Ok(())
        })
        .await
    }

    async fn add_reaction(&self, pr: &PrRef, comment: &CommentId, emoji: &str) -> HostResult<()> {
        let comment = comment.0.clone();
        let emoji = emoji.to_string();
        self.pr_call("add_reaction", pr, move |l, idx| {
            l.react(idx, &comment, &emoji, true, now_ms())
        })
        .await
    }

    async fn remove_reaction(
        &self,
        pr: &PrRef,
        comment: &CommentId,
        emoji: &str,
    ) -> HostResult<()> {
        let comment = comment.0.clone();
        let emoji = emoji.to_string();
        self.pr_call("remove_reaction", pr, move |l, idx| {
            l.react(idx, &comment, &emoji, false, now_ms())
        })
        .await
    }

    async fn assignable_users(&self, _repo: &RepoRef) -> HostResult<Vec<Person>> {
        self.gate("assignable_users", None).await?;
        Ok(self
            .world
            .scenario
            .users
            .iter()
            .filter(|u| !u.bot)
            .map(|u| self.world.person(&u.login))
            .collect())
    }

    async fn request_reviewers(
        &self,
        pr: &PrRef,
        users: &[String],
        teams: &[String],
    ) -> HostResult<()> {
        let (users, teams) = (users.to_vec(), teams.to_vec());
        self.pr_call("request_reviewers", pr, move |l, idx| {
            l.request_review(idx, &users, &teams, now_ms())
        })
        .await
    }

    async fn remove_reviewer(&self, pr: &PrRef, user: &str) -> HostResult<()> {
        let user = user.to_string();
        self.pr_call("remove_reviewer", pr, move |l, idx| {
            let p = &mut l.st.prs[idx];
            p.requested_users.retain(|u| *u != user);
            p.requested_teams.retain(|t| *t != user);
            l.st.touch(idx, now_ms());
            Ok(())
        })
        .await
    }

    async fn mark_ready(&self, pr: &PrRef) -> HostResult<()> {
        self.pr_call("mark_ready", pr, |l, idx| {
            match l.st.prs[idx].state {
                PrState::Draft => l.st.prs[idx].state = PrState::Open,
                PrState::Open => {}
                _ => return Err(Refusal::Validation("Pull request is not open.".into())),
            }
            l.st.touch(idx, now_ms());
            Ok(())
        })
        .await
    }

    async fn convert_to_draft(&self, pr: &PrRef) -> HostResult<()> {
        self.pr_call("convert_to_draft", pr, |l, idx| {
            match l.st.prs[idx].state {
                PrState::Open => l.st.prs[idx].state = PrState::Draft,
                PrState::Draft => {}
                _ => return Err(Refusal::Validation("Pull request is not open.".into())),
            }
            let p = &mut l.st.prs[idx];
            p.auto_merge = None;
            p.queued_at = None;
            l.st.touch(idx, now_ms());
            Ok(())
        })
        .await
    }

    async fn merge_status(&self, pr: &PrRef) -> HostResult<MergeStatus> {
        self.pr_call("merge_status", pr, |l, idx| {
            l.sync_head(idx, now_ms());
            Ok(l.merge_status(idx))
        })
        .await
    }

    async fn merge(
        &self,
        pr: &PrRef,
        method: MergeMethod,
        _title: Option<String>,
        _body: Option<String>,
    ) -> HostResult<Sha> {
        self.pr_call("merge", pr, move |l, idx| {
            let me = l.world.viewer().to_string();
            l.merge(idx, method, &me, false, now_ms()).map(Sha::new)
        })
        .await
    }

    async fn enable_auto_merge(&self, pr: &PrRef, method: MergeMethod) -> HostResult<()> {
        self.pr_call("enable_auto_merge", pr, move |l, idx| {
            if l.st.prs[idx].state != PrState::Open {
                return Err(Refusal::Validation(
                    "Auto-merge needs an open, non-draft pull request.".into(),
                ));
            }
            l.st.prs[idx].auto_merge = Some(method);
            l.st.touch(idx, now_ms());
            Ok(())
        })
        .await
    }

    async fn disable_auto_merge(&self, pr: &PrRef) -> HostResult<()> {
        self.pr_call("disable_auto_merge", pr, |l, idx| {
            l.st.prs[idx].auto_merge = None;
            l.st.touch(idx, now_ms());
            Ok(())
        })
        .await
    }

    async fn enqueue(&self, pr: &PrRef) -> HostResult<()> {
        self.pr_call("enqueue", pr, |l, idx| {
            if !l.st.faults.merge_queue {
                return Err(Refusal::Validation(
                    "Merge queue is not enabled for this repository.".into(),
                ));
            }
            if l.st.prs[idx].state != PrState::Open {
                return Err(Refusal::Validation(
                    "Only open, non-draft pull requests can join the merge queue.".into(),
                ));
            }
            let now = now_ms();
            l.st.prs[idx].queued_at.get_or_insert(now);
            l.st.touch(idx, now);
            Ok(())
        })
        .await
    }

    async fn update_branch(&self, pr: &PrRef) -> HostResult<()> {
        self.pr_call("update_branch", pr, |l, idx| l.update_branch(idx, now_ms()))
            .await
    }

    async fn rerun_check(&self, pr: &PrRef, check: &Check) -> HostResult<()> {
        let id = check.id.clone();
        self.pr_call("rerun_check", pr, move |l, idx| {
            let now = now_ms();
            let after = l.world.ci_delay(&l.world.ci().rerun_after.clone());
            let result = l.world.ci().rerun_result;
            let c = l.st.prs[idx]
                .checks
                .iter_mut()
                .find(|c| c.id == id)
                .ok_or_else(|| Refusal::NotFound(format!("check {id}")))?;
            c.status = theodore_domain::CheckStatus::Pending;
            c.duration_ms = None;
            c.transition = Some((now + after, result));
            l.st.bump(idx);
            Ok(())
        })
        .await
    }

    /// A made-up log for a failing check (docs/spaces.md "Fix CI") — enough
    /// for a demo or a QA pass to see an attachment and a sensible prompt,
    /// never a real CI's output.
    async fn check_log(&self, pr: &PrRef, check: &Check) -> HostResult<Option<String>> {
        let id = check.id.clone();
        self.pr_call("check_log", pr, move |l, idx| {
            let c = l.st.prs[idx]
                .checks
                .iter()
                .find(|c| c.id == id)
                .ok_or_else(|| Refusal::NotFound(format!("check {id}")))?;
            if c.status != theodore_domain::CheckStatus::Fail {
                return Ok(None);
            }
            Ok(Some(format!(
                "##[group]Run {name}\n\
                 [sandbox] simulated failure for check \"{name}\"\n\
                 ##[error]Process completed with exit code 1.\n\
                 ##[endgroup]",
                name = c.name,
            )))
        })
        .await
    }
}
