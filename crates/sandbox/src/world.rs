//! `World`: scenario + persisted state + the bare repos, and every
//! synchronous operation on them. The async `SandboxHost`, the timeline
//! ticker and the control spool all go through here, under one lock, so
//! the simulated host is always consistent with git.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use theodore_domain::{
    Anchor, Check, CheckRollup, CheckStatus, Comment, CommentId, Commit, FileViewed, HostKind,
    HostRepo, InboxReason, Millis, Person, PrDetail, PrRef, PrState, PrSummary, ReactionCount,
    RepoRef, ReviewDecision, Reviewer, ReviewerState, Side, Thread, ThreadId, ViewedState,
};
use theodore_ports::{MergeMethod, MergeState, MergeStatus};

use crate::error::SandboxError;
use crate::gitops::{self, Bare, Change, Ident, MergeTree};
use crate::materialize::{self, remote_dir};
use crate::scenario::{CiConfig, Scenario, UserSpec, parse_duration};
use crate::state::{CheckRec, CommentRec, PrRec, ReviewRec, ReviewState, State, ThreadRec};

pub const URL_BASE: &str = "https://sandbox.theodore.invalid";

pub fn now_ms() -> Millis {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Host-shaped failure of a world operation; mapped 1:1 onto `HostError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    NotFound(String),
    Validation(String),
    Conflict(String),
    Internal(String),
}

impl From<SandboxError> for Refusal {
    fn from(e: SandboxError) -> Self {
        match e {
            SandboxError::Invalid(m) => Refusal::Validation(m),
            other => Refusal::Internal(other.to_string()),
        }
    }
}

pub type WorldResult<T> = Result<T, Refusal>;

#[derive(Default)]
struct Caches {
    numstat: HashMap<(String, String), (u32, u32, u32)>,
    conflicts: HashMap<(String, String), Vec<String>>,
    files: HashMap<(String, String), Vec<String>>,
    log: HashMap<(String, String), Vec<gitops::LogEntry>>,
}

pub struct World {
    pub scenario: Scenario,
    pub scenario_dir: PathBuf,
    /// `<data dir>/sandbox`.
    pub root: PathBuf,
    state: Mutex<State>,
    caches: Mutex<Caches>,
    pub(crate) started_at: Mutex<Millis>,
    rng: Mutex<u64>,
}

pub(crate) struct Locked<'a> {
    pub world: &'a World,
    pub st: MutexGuard<'a, State>,
    refs: RefCell<HashMap<(String, String), HashMap<String, String>>>,
}

impl World {
    pub fn new(scenario: Scenario, scenario_dir: PathBuf, root: PathBuf, state: State) -> Self {
        let seed = (now_ms() as u64) ^ 0x9E37_79B9_7F4A_7C15;
        Self {
            scenario,
            scenario_dir,
            root,
            state: Mutex::new(state),
            caches: Mutex::new(Caches::default()),
            started_at: Mutex::new(now_ms()),
            rng: Mutex::new(seed | 1),
        }
    }

    pub(crate) fn lock(&self) -> Locked<'_> {
        Locked {
            world: self,
            st: self.state.lock().unwrap_or_else(|p| p.into_inner()),
            refs: RefCell::new(HashMap::new()),
        }
    }

    /// A copy of the state, for status reporting and tests.
    pub fn snapshot(&self) -> State {
        self.lock().st.clone()
    }

    /// xorshift: good enough for "fail 30% of calls".
    pub fn random(&self) -> f64 {
        let mut s = self.rng.lock().unwrap_or_else(|p| p.into_inner());
        let mut x = *s;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *s = x;
        (x >> 11) as f64 / (1u64 << 53) as f64
    }

    pub fn bare(&self, owner: &str, name: &str) -> Bare {
        Bare::open(remote_dir(&self.root, owner, name))
    }

    pub fn viewer(&self) -> &str {
        &self.scenario.viewer
    }

    pub fn user(&self, login: &str) -> Option<&UserSpec> {
        self.scenario.users.iter().find(|u| u.login == login)
    }

    pub fn person(&self, login: &str) -> Person {
        match self.user(login) {
            Some(u) => Person {
                login: u.login.clone(),
                name: u.name.clone(),
                avatar_url: u.avatar.clone(),
            },
            None => Person {
                login: login.to_string(),
                name: None,
                avatar_url: None,
            },
        }
    }

    pub fn viewer_teams(&self) -> Vec<&str> {
        self.scenario
            .teams
            .iter()
            .filter(|t| t.members.iter().any(|m| m == self.viewer()))
            .map(|t| t.slug.as_str())
            .collect()
    }

    pub fn ident(&self, login: &str) -> Ident {
        materialize::ident(&self.scenario, login)
    }

    pub fn ci(&self) -> &CiConfig {
        &self.scenario.ci
    }

    pub fn ci_delay(&self, s: &str) -> Millis {
        parse_duration(s).unwrap_or(8_000)
    }

    /// A teammate to act as when an event doesn't name one.
    pub fn teammate(&self) -> String {
        self.scenario
            .users
            .iter()
            .find(|u| u.login != self.viewer() && !u.bot)
            .map(|u| u.login.clone())
            .unwrap_or_else(|| self.viewer().to_string())
    }

    pub fn hosted_repo(&self, owner: &str, name: &str) -> bool {
        self.scenario
            .repos
            .iter()
            .any(|r| r.hosted && r.owner == owner && r.name == name)
    }

    fn numstat(&self, bare: &Bare, base: &str, head: &str) -> (u32, u32, u32) {
        let key = (base.to_string(), head.to_string());
        if let Some(v) = self.caches.lock().unwrap().numstat.get(&key) {
            return *v;
        }
        let v = bare.numstat(base, head).unwrap_or((0, 0, 0));
        self.caches.lock().unwrap().numstat.insert(key, v);
        v
    }

    /// Conflicting paths of merging `head` into `base` (empty = clean).
    pub fn conflicts(&self, bare: &Bare, base: &str, head: &str) -> Vec<String> {
        let key = (base.to_string(), head.to_string());
        if let Some(v) = self.caches.lock().unwrap().conflicts.get(&key) {
            return v.clone();
        }
        let v = match bare.merge_tree(base, head, None) {
            Ok(MergeTree::Conflicts(p)) => p,
            _ => Vec::new(),
        };
        self.caches.lock().unwrap().conflicts.insert(key, v.clone());
        v
    }

    /// `base..head` commits (immutable per sha pair, so cached).
    pub fn log(&self, bare: &Bare, base: &str, head: &str) -> Vec<gitops::LogEntry> {
        let key = (base.to_string(), head.to_string());
        if let Some(v) = self.caches.lock().unwrap().log.get(&key) {
            return v.clone();
        }
        let v = bare.log(base, head).unwrap_or_default();
        self.caches.lock().unwrap().log.insert(key, v.clone());
        v
    }

    fn diff_files(&self, bare: &Bare, base: &str, head: &str) -> Vec<String> {
        let key = (base.to_string(), head.to_string());
        if let Some(v) = self.caches.lock().unwrap().files.get(&key) {
            return v.clone();
        }
        let v = bare.changed_paths(base, head, true).unwrap_or_default();
        self.caches.lock().unwrap().files.insert(key, v.clone());
        v
    }
}

/// Moves `line` (new side at `old` head) across the `-U0` diff of its file
/// between `old` and `new`; `None` when the line itself was changed.
fn shift_line(diff: &str, line: u32) -> Option<u32> {
    let mut delta: i64 = 0;
    for l in diff.lines() {
        let Some(rest) = l.strip_prefix("@@ -") else {
            continue;
        };
        let mut parts = rest.split(' ');
        let old = parts.next().unwrap_or_default();
        let new = parts.next().unwrap_or_default().trim_start_matches('+');
        let range = |s: &str| -> (i64, i64) {
            let mut it = s.split(',');
            let start = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
            let len = it.next().and_then(|x| x.parse().ok()).unwrap_or(1);
            (start, len)
        };
        let (os, ol) = range(old);
        let (_ns, nl) = range(new);
        let line = line as i64;
        // A pure insertion (`ol == 0`) sits after line `os`.
        let (first, last) = if ol == 0 {
            (os + 1, os)
        } else {
            (os, os + ol - 1)
        };
        if ol > 0 && line >= first && line <= last {
            return None;
        }
        if line > last || (ol == 0 && line > os) {
            delta += nl - ol;
        }
    }
    u32::try_from(line as i64 + delta).ok()
}

pub fn marker_tag(marker: &str) -> String {
    format!("<!-- theodore:{marker} -->")
}

const EMOJI: [&str; 8] = [
    "+1", "-1", "laugh", "hooray", "confused", "heart", "rocket", "eyes",
];

impl Locked<'_> {
    pub fn save(&self) {
        if let Err(e) = self.st.save(&self.world.root) {
            tracing::warn!("sandbox: saving state failed: {e}");
        }
    }

    pub fn find(&self, owner: &str, repo: &str, number: u64) -> WorldResult<usize> {
        self.st
            .prs
            .iter()
            .position(|p| p.owner == owner && p.repo == repo && p.number == number)
            .ok_or_else(|| Refusal::NotFound(format!("{owner}/{repo}#{number}")))
    }

    pub fn find_ref(&self, pr: &PrRef) -> WorldResult<usize> {
        self.find(&pr.repo.owner, &pr.repo.name, pr.number)
    }

    fn bare_of(&self, idx: usize) -> Bare {
        let p = &self.st.prs[idx];
        self.world.bare(&p.owner, &p.repo)
    }

    /// Branch tip of `idx`'s repo, from one `for-each-ref` per repo per
    /// lock (a ticker pass over every PR is two git calls, not forty).
    pub fn tip(&self, idx: usize, branch: &str) -> Option<String> {
        let p = &self.st.prs[idx];
        let key = (p.owner.clone(), p.repo.clone());
        let mut refs = self.refs.borrow_mut();
        let map = refs
            .entry(key)
            .or_insert_with(|| self.world.bare(&p.owner, &p.repo).refs());
        map.get(&format!("refs/heads/{branch}")).cloned()
    }

    /// After any ref update made under this lock.
    pub fn forget_refs(&self) {
        self.refs.borrow_mut().clear();
    }

    /// (base, head) shas: live for an open PR, frozen once merged/closed.
    pub fn base_head(&self, idx: usize) -> (String, String) {
        let p = &self.st.prs[idx];
        if let Some((b, h)) = &p.frozen {
            return (b.clone(), h.clone());
        }
        let base = self.tip(idx, &p.base_ref).unwrap_or_default();
        (base, p.head_sha.clone())
    }

    /// Notices a push to the head branch (from `accept_task`, a suggestion
    /// commit, `update_branch`, the timeline or the CLI): moves
    /// `refs/pull/<n>/head`, shifts or outdates threads, dismisses Viewed
    /// files, and restarts CI. Returns whether the head moved.
    pub fn sync_head(&mut self, idx: usize, now: Millis) -> bool {
        if !self.st.prs[idx].is_open() {
            return false;
        }
        let bare = self.bare_of(idx);
        let (head_ref, old, number) = {
            let p = &self.st.prs[idx];
            (p.head_ref.clone(), p.head_sha.clone(), p.number)
        };
        let Some(tip) = self.tip(idx, &head_ref) else {
            return false;
        };
        if tip == old {
            return false;
        }
        let _ = bare.update_ref(&format!("refs/pull/{number}/head"), &tip);
        self.forget_refs();
        let changed: BTreeSet<String> = bare
            .changed_paths(&old, &tip, false)
            .unwrap_or_default()
            .into_iter()
            .collect();
        let push_after = self.world.ci_delay(&self.world.ci().push_after.clone());
        let push_result = self.world.ci().push_result;
        let p = &mut self.st.prs[idx];
        p.head_sha = tip.clone();
        for t in p.threads.iter_mut().filter(|t| !t.outdated) {
            if !changed.contains(&t.path) || t.side == Side::Old {
                continue;
            }
            let diff = bare.diff_path(&old, &tip, &t.path).unwrap_or_default();
            match shift_line(&diff, t.line) {
                Some(l) => t.line = l,
                None => t.outdated = true,
            }
        }
        for (path, state) in p.viewed.iter_mut() {
            if changed.contains(path) && *state == ViewedState::Viewed {
                *state = ViewedState::Dismissed;
            }
        }
        for c in &mut p.checks {
            c.status = CheckStatus::Pending;
            c.duration_ms = None;
            c.transition = Some((now + push_after, push_result));
        }
        self.st.touch(idx, now);
        true
    }

    pub fn sync_all_heads(&mut self, now: Millis) -> bool {
        let mut any = false;
        for i in 0..self.st.prs.len() {
            any |= self.sync_head(i, now);
        }
        any
    }

    pub fn required_approvals(&self, idx: usize) -> Option<u32> {
        let p = &self.st.prs[idx];
        p.required_approvals.or_else(|| {
            self.st
                .faults
                .required_approvals
                .then_some(self.st.faults.required_approvals_count)
        })
    }

    fn rollup(checks: &[CheckRec]) -> CheckRollup {
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

    fn review_decision(&self, idx: usize) -> Option<ReviewDecision> {
        let p = &self.st.prs[idx];
        let required = self.required_approvals(idx);
        if p.changes_requested() {
            Some(ReviewDecision::ChangesRequested)
        } else if p.approvals() >= required.unwrap_or(1) && p.approvals() > 0 {
            Some(ReviewDecision::Approved)
        } else if required.is_some()
            || !p.requested_users.is_empty()
            || !p.requested_teams.is_empty()
        {
            Some(ReviewDecision::ReviewRequired)
        } else {
            None
        }
    }

    fn my_last_review_at(&self, idx: usize) -> Option<Millis> {
        let me = self.world.viewer();
        self.st.prs[idx]
            .reviews
            .iter()
            .filter(|r| r.user == me)
            .map(|r| r.at)
            .max()
    }

    fn last_commit_at(&self, idx: usize) -> Option<Millis> {
        let (base, head) = self.base_head(idx);
        self.world
            .log(&self.bare_of(idx), &base, &head)
            .last()
            .map(|c| c.at)
    }

    pub fn reasons(&self, idx: usize) -> Vec<InboxReason> {
        let me = self.world.viewer();
        let p = &self.st.prs[idx];
        let mut out = Vec::new();
        let requested = p.requested_users.iter().any(|u| u == me);
        if requested {
            out.push(InboxReason::ReviewRequested);
        }
        let teams = self.world.viewer_teams();
        if p.requested_teams
            .iter()
            .any(|t| teams.contains(&t.as_str()))
        {
            out.push(InboxReason::TeamReviewRequested);
        }
        if p.assignees.iter().any(|a| a == me) {
            out.push(InboxReason::Assigned);
        }
        if p.author == me {
            out.push(InboxReason::Authored);
        }
        let tag = format!("@{me}");
        let mentioned = p.body.contains(&tag)
            || p.threads.iter().any(|t| {
                t.comments
                    .iter()
                    .any(|c| c.author != me && c.body.contains(&tag))
            });
        if mentioned {
            out.push(InboxReason::Mentioned);
        }
        if !requested
            && let Some(reviewed) = self.my_last_review_at(idx)
            && self.last_commit_at(idx).is_some_and(|c| c > reviewed)
        {
            out.push(InboxReason::UpdatedAfterReview);
        }
        out
    }

    pub fn pr_ref(&self, idx: usize) -> PrRef {
        let p = &self.st.prs[idx];
        PrRef {
            repo: RepoRef {
                host: HostKind::Github,
                owner: p.owner.clone(),
                name: p.repo.clone(),
            },
            number: p.number,
            host_id: Some(p.node_id.clone()),
        }
    }

    pub fn summary(&self, idx: usize) -> PrSummary {
        let (base, head) = self.base_head(idx);
        let bare = self.bare_of(idx);
        let (additions, deletions, changed_files) = self.world.numstat(&bare, &base, &head);
        let p = &self.st.prs[idx];
        let conflicts = p.is_open() && !self.world.conflicts(&bare, &base, &head).is_empty();
        PrSummary {
            pr: self.pr_ref(idx),
            title: p.title.clone(),
            author: self.world.person(&p.author),
            state: p.state,
            base_ref: p.base_ref.clone(),
            head_ref: p.head_ref.clone(),
            head_sha: theodore_domain::Sha::new(head.clone()),
            additions,
            deletions,
            changed_files,
            reasons: self.reasons(idx),
            checks: Self::rollup(&p.checks),
            approvals: p.approvals(),
            required_approvals: self.required_approvals(idx),
            updated_at: p.updated_at,
            url: format!("{URL_BASE}/{}/{}/pull/{}", p.owner, p.repo, p.number),
            review_decision: self.review_decision(idx),
            conflicts,
            created_at: Some(p.created_at),
            stack: None,
        }
    }

    fn comment(&self, c: &CommentRec) -> Comment {
        let me = self.world.viewer();
        Comment {
            id: CommentId::new(c.id.clone()),
            author: self.world.person(&c.author),
            body: c.body.clone(),
            created_at: c.at,
            edited: c.edited,
            reactions: c
                .reactions
                .iter()
                .filter(|(_, users)| !users.is_empty())
                .map(|(emoji, users)| ReactionCount {
                    emoji: emoji.clone(),
                    count: users.len() as u32,
                    viewer_reacted: users.contains(me),
                })
                .collect(),
        }
    }

    pub fn detail(&self, idx: usize) -> PrDetail {
        let summary = self.summary(idx);
        let (base, head) = self.base_head(idx);
        let bare = self.bare_of(idx);
        let p = &self.st.prs[idx];
        let my_review = self.my_last_review_at(idx);
        let commits = self
            .world
            .log(&bare, &base, &head)
            .into_iter()
            .map(|c| {
                let author = match gitops::login_of_email(&c.author_email) {
                    Some(login) => self.world.person(login),
                    None => Person {
                        login: c.author_name.clone(),
                        name: Some(c.author_name.clone()),
                        avatar_url: None,
                    },
                };
                Commit {
                    sha: theodore_domain::Sha::new(c.sha),
                    title: c.subject,
                    author,
                    at: c.at,
                    after_my_review: my_review.is_some_and(|r| c.at > r),
                }
            })
            .collect();
        let checks = p
            .checks
            .iter()
            .map(|c| Check {
                id: c.id.clone(),
                name: c.name.clone(),
                status: c.status,
                url: c.url.clone(),
                duration_ms: c.duration_ms,
            })
            .collect();
        let mut reviewers: Vec<Reviewer> = p
            .requested_users
            .iter()
            .map(|u| Reviewer {
                person: self.world.person(u),
                state: ReviewerState::Requested,
                team: None,
            })
            .collect();
        reviewers.extend(p.requested_teams.iter().map(|t| Reviewer {
            person: Person {
                login: t.clone(),
                name: Some(t.clone()),
                avatar_url: None,
            },
            state: ReviewerState::Requested,
            team: Some(t.clone()),
        }));
        reviewers.extend(p.latest_reviews().into_iter().map(|r| Reviewer {
            person: self.world.person(&r.user),
            state: match r.state {
                ReviewState::Approved => ReviewerState::Approved,
                ReviewState::ChangesRequested => ReviewerState::ChangesRequested,
                ReviewState::Commented => ReviewerState::Commented,
            },
            team: None,
        }));
        let head_sha = theodore_domain::Sha::new(head.clone());
        let threads = p
            .threads
            .iter()
            .map(|t| Thread {
                id: ThreadId::new(t.id.clone()),
                // Like GitHub: an outdated thread has no current line.
                anchor: (!t.outdated).then(|| Anchor {
                    rev: head_sha.clone(),
                    path: t.path.clone(),
                    side: t.side,
                    line: t.line,
                    start_line: t.start_line,
                    start_side: t.start_line.map(|_| t.side),
                }),
                resolved: t.resolved,
                outdated: t.outdated,
                comments: t.comments.iter().map(|c| self.comment(c)).collect(),
            })
            .collect();
        let viewed = self
            .world
            .diff_files(&bare, &base, &head)
            .into_iter()
            .map(|path| FileViewed {
                state: p
                    .viewed
                    .get(&path)
                    .copied()
                    .unwrap_or(ViewedState::Unviewed),
                path,
            })
            .collect();
        PrDetail {
            summary,
            body: p.body.clone(),
            base_sha: theodore_domain::Sha::new(base),
            commits,
            checks,
            reviewers,
            assignees: p.assignees.iter().map(|a| self.world.person(a)).collect(),
            labels: p.labels.clone(),
            threads,
            viewed,
        }
    }

    pub fn etag(&self, idx: usize) -> String {
        let p = &self.st.prs[idx];
        format!("W/\"sbx-{}-{}\"", p.number, p.version)
    }

    pub fn repos(&self) -> Vec<HostRepo> {
        self.world
            .scenario
            .repos
            .iter()
            .filter(|r| r.hosted)
            .map(|r| HostRepo {
                repo: RepoRef {
                    host: HostKind::Github,
                    owner: r.owner.clone(),
                    name: r.name.clone(),
                },
                description: r.description.clone(),
                private: r.private,
                pushed_at: self
                    .st
                    .prs
                    .iter()
                    .filter(|p| p.owner == r.owner && p.repo == r.name)
                    .map(|p| p.updated_at)
                    .max()
                    .or(Some(self.st.epoch)),
            })
            .collect()
    }

    // ── merge ────────────────────────────────────────────────────────────

    pub fn merge_status(&self, idx: usize) -> MergeStatus {
        let p = &self.st.prs[idx];
        let (base, head) = self.base_head(idx);
        let bare = self.bare_of(idx);
        let conflicts = p.is_open() && !self.world.conflicts(&bare, &base, &head).is_empty();
        let state = if !p.is_open() {
            MergeState::Unknown
        } else if conflicts {
            MergeState::Dirty
        } else if p.state == PrState::Draft || self.blocked_reason(idx).is_some() {
            MergeState::Blocked
        } else if !bare.is_ancestor(&base, &head) && self.require_up_to_date(idx) {
            MergeState::Behind
        } else if p.checks.iter().any(|c| c.status == CheckStatus::Fail) {
            MergeState::Unstable
        } else {
            MergeState::Clean
        };
        MergeStatus {
            mergeable: p.is_open().then_some(!conflicts),
            state,
            allowed_methods: vec![MergeMethod::Merge, MergeMethod::Squash, MergeMethod::Rebase],
            auto_merge_enabled: p.auto_merge.is_some(),
            in_queue: p.queued_at.is_some(),
            required_approvals: self.required_approvals(idx),
            auto_merge_allowed: true,
            // The `merge_queue` fault turns the queue on for every repo.
            merge_queue: self.st.faults.merge_queue,
            default_method: Some(MergeMethod::Squash),
        }
    }

    fn require_up_to_date(&self, idx: usize) -> bool {
        let p = &self.st.prs[idx];
        self.world
            .scenario
            .repo(&p.slug())
            .is_some_and(|r| r.require_up_to_date)
    }

    /// Why branch protection blocks a merge right now, if it does.
    fn blocked_reason(&self, idx: usize) -> Option<String> {
        let p = &self.st.prs[idx];
        let required = self.required_approvals(idx)?;
        if p.changes_requested() {
            return Some("Changes have been requested on this pull request.".into());
        }
        (p.approvals() < required).then(|| {
            format!("At least {required} approving review(s) are required by reviewers with write access.")
        })
    }

    /// Merges for real: the base branch moves in the bare repo.
    pub fn merge(
        &mut self,
        idx: usize,
        method: MergeMethod,
        by: &str,
        via_queue: bool,
        now: Millis,
    ) -> WorldResult<String> {
        let (state, number, title) = {
            let p = &self.st.prs[idx];
            (p.state, p.number, p.title.clone())
        };
        match state {
            PrState::Draft => {
                return Err(Refusal::Validation(
                    "Pull request is in draft state and can't be merged.".into(),
                ));
            }
            PrState::Merged | PrState::Closed => {
                return Err(Refusal::Validation("Pull request is not open.".into()));
            }
            PrState::Open => {}
        }
        if self.st.faults.merge_queue && !via_queue {
            return Err(Refusal::Validation(
                "Changes must be made through the merge queue.".into(),
            ));
        }
        if let Some(reason) = self.blocked_reason(idx) {
            return Err(Refusal::Validation(reason));
        }
        let (base, head) = self.base_head(idx);
        let bare = self.bare_of(idx);
        if !self.world.conflicts(&bare, &base, &head).is_empty() {
            return Err(Refusal::Validation("Pull Request is not mergeable.".into()));
        }
        let author = self.world.ident(by);
        let base_ref = self.st.prs[idx].base_ref.clone();
        let sha = match method {
            MergeMethod::Merge => materialize::merge_commit(
                &bare, &base_ref, &base, &head, &title, number, &author, now,
            )?,
            MergeMethod::Squash => {
                let tree = match bare.merge_tree(&base, &head, None)? {
                    MergeTree::Clean(t) => t,
                    MergeTree::Conflicts(_) => {
                        return Err(Refusal::Validation("Pull Request is not mergeable.".into()));
                    }
                };
                let msg = format!("{title} (#{number})");
                let sha = bare.commit_tree(&tree, &[&base], &msg, &author, now)?;
                bare.update_ref(&format!("refs/heads/{base_ref}"), &sha)?;
                sha
            }
            MergeMethod::Rebase => {
                let mut cur = base.clone();
                for c in bare.log(&base, &head)? {
                    let parent = bare.parent_of(&c.sha);
                    let tree = match bare.merge_tree(&cur, &c.sha, parent.as_deref())? {
                        MergeTree::Clean(t) => t,
                        MergeTree::Conflicts(_) => {
                            return Err(Refusal::Validation(
                                "This branch can't be rebased.".into(),
                            ));
                        }
                    };
                    let ident = Ident {
                        name: c.author_name.clone(),
                        email: c.author_email.clone(),
                    };
                    cur = bare.commit_tree(&tree, &[&cur], &c.subject, &ident, now)?;
                }
                bare.update_ref(&format!("refs/heads/{base_ref}"), &cur)?;
                cur
            }
        };
        self.forget_refs();
        {
            let p = &mut self.st.prs[idx];
            p.state = PrState::Merged;
            p.frozen = Some((base, head));
            p.merged_by = Some(by.to_string());
            p.merge_commit = Some(sha.clone());
            p.auto_merge = None;
            p.queued_at = None;
        }
        self.st.touch(idx, now);
        self.touch_base(&base_ref, idx, now);
        Ok(sha)
    }

    /// The base branch moved: every other open PR on it has a new base sha
    /// (and maybe new conflicts), so a new ETag.
    fn touch_base(&mut self, base_ref: &str, except: usize, now: Millis) {
        let (owner, repo) = {
            let p = &self.st.prs[except];
            (p.owner.clone(), p.repo.clone())
        };
        let others: Vec<usize> = self
            .st
            .prs
            .iter()
            .enumerate()
            .filter(|(i, p)| {
                *i != except
                    && p.is_open()
                    && p.owner == owner
                    && p.repo == repo
                    && p.base_ref == base_ref
            })
            .map(|(i, _)| i)
            .collect();
        for i in others {
            self.st.touch(i, now);
        }
    }

    /// Merges the base branch into the head ("Update branch").
    pub fn update_branch(&mut self, idx: usize, now: Millis) -> WorldResult<()> {
        if !self.st.prs[idx].is_open() {
            return Err(Refusal::Validation("Pull request is not open.".into()));
        }
        let (base, head) = self.base_head(idx);
        let bare = self.bare_of(idx);
        if bare.is_ancestor(&base, &head) {
            return Err(Refusal::Validation(
                "There are no new commits on the base branch.".into(),
            ));
        }
        let tree = match bare.merge_tree(&head, &base, None)? {
            MergeTree::Clean(t) => t,
            MergeTree::Conflicts(paths) => {
                return Err(Refusal::Conflict(format!(
                    "merge conflict between base and head in {}",
                    paths.join(", ")
                )));
            }
        };
        let (base_ref, head_ref) = {
            let p = &self.st.prs[idx];
            (p.base_ref.clone(), p.head_ref.clone())
        };
        let msg = format!("Merge branch '{base_ref}' into {head_ref}");
        let me = self.world.viewer().to_string();
        let sha = bare.commit_tree(&tree, &[&head, &base], &msg, &self.world.ident(&me), now)?;
        bare.update_ref(&format!("refs/heads/{head_ref}"), &sha)?;
        self.forget_refs();
        self.sync_head(idx, now);
        Ok(())
    }

    // ── content writes ───────────────────────────────────────────────────

    /// The open (or draft) PR in `owner/repo` whose head branch is `head`.
    pub fn find_open_by_head(&self, owner: &str, repo: &str, head: &str) -> Option<usize> {
        self.st
            .prs
            .iter()
            .position(|p| p.owner == owner && p.repo == repo && p.head_ref == head && p.is_open())
    }

    /// Opens a PR from `head` (a branch already pushed to the bare repo)
    /// into `base`, authored by the viewer — GitHub's `POST /pulls` rules:
    /// both branches must exist, `head` must have commits `base` lacks, and
    /// a second open PR for the same `head` is a conflict. CI starts on it
    /// with the check names the repo's other PRs run.
    #[allow(clippy::too_many_arguments)]
    pub fn open_pr(
        &mut self,
        owner: &str,
        repo: &str,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
        draft: bool,
        now: Millis,
    ) -> WorldResult<usize> {
        if !self.world.hosted_repo(owner, repo) {
            return Err(Refusal::NotFound(format!("{owner}/{repo}")));
        }
        if title.trim().is_empty() {
            return Err(Refusal::Validation("Title can't be blank.".into()));
        }
        if let Some(i) = self.find_open_by_head(owner, repo, head) {
            return Err(Refusal::Conflict(format!(
                "A pull request already exists for {owner}:{head} ({}).",
                self.st.prs[i].key()
            )));
        }
        let bare = self.world.bare(owner, repo);
        let head_sha = bare
            .branch_tip(head)
            .ok_or_else(|| Refusal::Validation(format!("head: no branch {head}")))?;
        let base_sha = bare
            .branch_tip(base)
            .ok_or_else(|| Refusal::Validation(format!("base: no branch {base}")))?;
        if head == base || bare.is_ancestor(&head_sha, &base_sha) {
            return Err(Refusal::Validation(format!(
                "No commits between {base} and {head}."
            )));
        }

        let number = self
            .st
            .prs
            .iter()
            .filter(|p| p.owner == owner && p.repo == repo)
            .map(|p| p.number)
            .max()
            .unwrap_or(0)
            + 1;
        bare.update_ref(&format!("refs/pull/{number}/head"), &head_sha)?;
        self.forget_refs();

        // The repo's CI: the check names of its most recent PR that has any.
        let names: Vec<String> = self
            .st
            .prs
            .iter()
            .filter(|p| p.owner == owner && p.repo == repo && !p.checks.is_empty())
            .max_by_key(|p| p.created_at)
            .map(|p| p.checks.iter().map(|c| c.name.clone()).collect())
            .unwrap_or_default();
        let after = self.world.ci_delay(&self.world.ci().push_after.clone());
        let result = self.world.ci().push_result;
        let mut checks = Vec::with_capacity(names.len());
        for name in names {
            let id = self.st.next_id("");
            checks.push(CheckRec {
                url: Some(format!("{URL_BASE}/{owner}/{repo}/checks/{id}")),
                id,
                name,
                status: CheckStatus::Pending,
                duration_ms: None,
                transition: Some((now + after, result)),
            });
        }

        self.st.prs.push(PrRec {
            owner: owner.to_string(),
            repo: repo.to_string(),
            number,
            node_id: format!("SBX_PR_{repo}_{number}"),
            title: title.to_string(),
            body: body.to_string(),
            author: self.world.viewer().to_string(),
            head_ref: head.to_string(),
            base_ref: base.to_string(),
            state: if draft { PrState::Draft } else { PrState::Open },
            created_at: now,
            updated_at: now,
            labels: Vec::new(),
            assignees: Vec::new(),
            requested_users: Vec::new(),
            requested_teams: Vec::new(),
            reviews: Vec::new(),
            checks,
            threads: Vec::new(),
            viewed: Default::default(),
            head_sha,
            frozen: None,
            merged_by: None,
            merge_commit: None,
            auto_merge: None,
            queued_at: None,
            required_approvals: None,
            version: 0,
        });
        let idx = self.st.prs.len() - 1;
        self.st.touch(idx, now);
        Ok(idx)
    }

    /// A new commit on the PR's head branch; picked up as a push.
    pub fn push(
        &mut self,
        idx: usize,
        files: &[crate::scenario::FileSpec],
        message: Option<&str>,
        author: Option<&str>,
        now: Millis,
    ) -> WorldResult<String> {
        if !self.st.prs[idx].is_open() {
            return Err(Refusal::Validation("can't push to a closed PR".into()));
        }
        let bare = self.bare_of(idx);
        let head_ref = self.st.prs[idx].head_ref.clone();
        let tip = bare
            .branch_tip(&head_ref)
            .ok_or_else(|| Refusal::NotFound(format!("branch {head_ref}")))?;
        let changes = if files.is_empty() {
            self.generated_edit(idx, &tip)?
        } else {
            materialize::changes_for(&bare, Some(&tip), files, &self.world.scenario_dir)?
        };
        let author = author
            .map(str::to_string)
            .unwrap_or_else(|| self.st.prs[idx].author.clone());
        let n = self.st.next_id("");
        let message = message
            .map(str::to_string)
            .unwrap_or_else(|| format!("Address review feedback ({n})"));
        let sha = bare.commit(
            Some(&tip),
            &changes,
            &message,
            &self.world.ident(&author),
            now,
        )?;
        bare.update_ref(&format!("refs/heads/{head_ref}"), &sha)?;
        self.forget_refs();
        self.sync_head(idx, now);
        Ok(sha)
    }

    /// The default push: one more line at the end of the PR's first
    /// changed text file.
    fn generated_edit(&mut self, idx: usize, tip: &str) -> WorldResult<Vec<Change>> {
        let (base, _) = self.base_head(idx);
        let bare = self.bare_of(idx);
        let n = self.st.next_id("");
        for path in bare.changed_paths(&base, tip, true).unwrap_or_default() {
            let Some(bytes) = bare.read_file(tip, &path) else {
                continue;
            };
            let Ok(mut text) = String::from_utf8(bytes) else {
                continue;
            };
            if text.contains('\0') {
                continue;
            }
            if !text.ends_with('\n') && !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&comment_line(&path, &format!("sandbox push {n}")));
            text.push('\n');
            return Ok(vec![Change::Write {
                path,
                bytes: text.into_bytes(),
            }]);
        }
        Err(Refusal::Validation(
            "the PR has no text file to edit".into(),
        ))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_thread(
        &mut self,
        idx: usize,
        path: &str,
        line: u32,
        side: Side,
        start_line: Option<u32>,
        body: String,
        author: &str,
        now: Millis,
    ) -> (String, String) {
        let comment_id = self.st.next_id("");
        let thread_id = self.st.next_id("SBX_T_");
        self.st.prs[idx].threads.push(ThreadRec {
            id: thread_id.clone(),
            path: path.to_string(),
            line,
            side,
            start_line,
            resolved: false,
            outdated: false,
            comments: vec![CommentRec {
                id: comment_id.clone(),
                author: author.to_string(),
                body,
                at: now,
                edited: false,
                reactions: Default::default(),
            }],
        });
        self.st.touch(idx, now);
        (thread_id, comment_id)
    }

    pub fn reply(
        &mut self,
        idx: usize,
        thread: Option<&str>,
        body: String,
        author: &str,
        now: Millis,
    ) -> WorldResult<String> {
        let id = self.st.next_id("");
        let p = &mut self.st.prs[idx];
        let t = match thread {
            Some(tid) => p.threads.iter_mut().find(|t| t.id == tid),
            None => p.threads.last_mut(),
        }
        .ok_or_else(|| Refusal::NotFound(format!("thread {}", thread.unwrap_or("(latest)"))))?;
        t.comments.push(CommentRec {
            id: id.clone(),
            author: author.to_string(),
            body,
            at: now,
            edited: false,
            reactions: Default::default(),
        });
        self.st.touch(idx, now);
        Ok(id)
    }

    pub fn find_comment(&self, idx: usize, comment: &str) -> Option<(usize, usize)> {
        let p = &self.st.prs[idx];
        p.threads.iter().enumerate().find_map(|(ti, t)| {
            t.comments
                .iter()
                .position(|c| c.id == comment)
                .map(|ci| (ti, ci))
        })
    }

    pub fn find_marker(&self, idx: usize, marker: &str) -> Option<(String, String)> {
        let tag = marker_tag(marker);
        self.st.prs[idx].threads.iter().find_map(|t| {
            t.comments
                .iter()
                .find(|c| c.body.contains(&tag))
                .map(|c| (t.id.clone(), c.id.clone()))
        })
    }

    pub fn add_review(
        &mut self,
        idx: usize,
        user: &str,
        state: ReviewState,
        body: String,
        now: Millis,
    ) -> String {
        let id = self.st.next_id("R");
        let head = self.st.prs[idx].head_sha.clone();
        let p = &mut self.st.prs[idx];
        p.reviews.push(ReviewRec {
            id: id.clone(),
            user: user.to_string(),
            state,
            body,
            at: now,
            commit: Some(head),
        });
        p.requested_users.retain(|u| u != user);
        self.st.touch(idx, now);
        id
    }

    pub fn request_review(
        &mut self,
        idx: usize,
        users: &[String],
        teams: &[String],
        now: Millis,
    ) -> WorldResult<()> {
        let author = self.st.prs[idx].author.clone();
        for u in users {
            if self.world.user(u).is_none() {
                return Err(Refusal::Validation(format!(
                    "Reviews may only be requested from collaborators: {u}"
                )));
            }
            if *u == author {
                return Err(Refusal::Validation(
                    "Review cannot be requested from pull request author.".into(),
                ));
            }
        }
        for t in teams {
            if !self.world.scenario.teams.iter().any(|x| &x.slug == t) {
                return Err(Refusal::Validation(format!("unknown team {t}")));
            }
        }
        let p = &mut self.st.prs[idx];
        for u in users {
            if !p.requested_users.contains(u) {
                p.requested_users.push(u.clone());
            }
        }
        for t in teams {
            if !p.requested_teams.contains(t) {
                p.requested_teams.push(t.clone());
            }
        }
        self.st.touch(idx, now);
        Ok(())
    }

    pub fn set_check(&mut self, idx: usize, name: &str, status: CheckStatus) {
        let id = self.st.next_id("");
        let slug = self.st.prs[idx].slug();
        let p = &mut self.st.prs[idx];
        match p.checks.iter_mut().find(|c| c.name == name) {
            Some(c) => {
                c.status = status;
                c.transition = None;
                c.duration_ms = (status != CheckStatus::Pending).then_some(42_000);
            }
            None => p.checks.push(CheckRec {
                id: id.clone(),
                name: name.to_string(),
                status,
                url: Some(format!("{URL_BASE}/{slug}/checks/{id}")),
                duration_ms: (status != CheckStatus::Pending).then_some(42_000),
                transition: None,
            }),
        }
        self.st.bump(idx);
    }

    pub fn react(
        &mut self,
        idx: usize,
        comment: &str,
        emoji: &str,
        on: bool,
        now: Millis,
    ) -> WorldResult<()> {
        if !EMOJI.contains(&emoji) {
            return Err(Refusal::Validation(format!("unknown reaction {emoji}")));
        }
        let (ti, ci) = self
            .find_comment(idx, comment)
            .ok_or_else(|| Refusal::NotFound(format!("comment {comment}")))?;
        let me = self.world.viewer().to_string();
        let set = self.st.prs[idx].threads[ti].comments[ci]
            .reactions
            .entry(emoji.to_string())
            .or_default();
        if on {
            set.insert(me);
        } else {
            set.remove(&me);
        }
        self.st.touch(idx, now);
        Ok(())
    }

    /// Advances time-based state: CI transitions, auto-merge, the merge
    /// queue and an expiring rate limit. Returns whether anything changed.
    pub fn tick(&mut self, now: Millis) -> bool {
        let mut changed = self.sync_all_heads(now);
        for i in 0..self.st.prs.len() {
            let mut moved = false;
            for c in &mut self.st.prs[i].checks {
                if let Some((at, then)) = c.transition
                    && now >= at
                {
                    c.status = then;
                    c.transition = None;
                    c.duration_ms = Some(61_000);
                    moved = true;
                }
            }
            if moved {
                self.st.bump(i);
                changed = true;
            }
        }
        let queue_after = self.world.ci_delay(&self.world.ci().queue_after.clone());
        for i in 0..self.st.prs.len() {
            let p = &self.st.prs[i];
            if p.state != PrState::Open {
                continue;
            }
            let me = self.world.viewer().to_string();
            if let Some(since) = p.queued_at
                && now >= since + queue_after
            {
                if self.merge(i, MergeMethod::Merge, &me, true, now).is_err() {
                    self.st.prs[i].queued_at = None;
                    self.st.touch(i, now);
                }
                changed = true;
                continue;
            }
            if let Some(method) = p.auto_merge {
                let ready = !p
                    .checks
                    .iter()
                    .any(|c| matches!(c.status, CheckStatus::Pending | CheckStatus::Fail))
                    && self.blocked_reason(i).is_none();
                if ready && self.merge(i, method, &me, true, now).is_ok() {
                    changed = true;
                }
            }
        }
        if let Some(reset) = self.st.rate_limit_reset_at
            && now >= reset
        {
            self.st.rate_limit_reset_at = None;
            self.st.faults.rate_limit = false;
            self.st.version += 1;
            changed = true;
        }
        changed
    }
}

/// `text` as a one-line comment in `path`'s language.
pub fn comment_line(path: &str, text: &str) -> String {
    match Path::new(path).extension().and_then(|e| e.to_str()) {
        Some("py" | "sh" | "rb" | "yml" | "yaml" | "toml") => format!("# {text}"),
        Some("md" | "html") => format!("<!-- {text} -->"),
        Some("json") => format!("{{\"_\": \"{text}\"}}"),
        _ => format!("// {text}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_line_moves_past_insertions_and_outdates_edited_lines() {
        // Two lines inserted after line 2, line 7 replaced.
        let diff = "@@ -2,0 +3,2 @@\n+a\n+b\n@@ -7 +9 @@\n-x\n+y\n";
        assert_eq!(shift_line(diff, 1), Some(1));
        assert_eq!(shift_line(diff, 2), Some(2));
        assert_eq!(shift_line(diff, 3), Some(5));
        assert_eq!(shift_line(diff, 7), None);
        assert_eq!(shift_line(diff, 8), Some(10));
        // A deletion of lines 4-5.
        assert_eq!(shift_line("@@ -4,2 +3,0 @@\n-a\n-b\n", 6), Some(4));
        assert_eq!(shift_line("@@ -4,2 +3,0 @@\n-a\n-b\n", 5), None);
    }

    #[test]
    fn comment_lines_follow_the_extension() {
        assert_eq!(comment_line("a/b.ts", "x"), "// x");
        assert_eq!(comment_line("ci.yml", "x"), "# x");
        assert_eq!(comment_line("README.md", "x"), "<!-- x -->");
    }
}
