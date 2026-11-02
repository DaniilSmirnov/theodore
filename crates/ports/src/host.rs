//! Code host port (plan §6).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use theodore_domain::{
    Anchor, Check, CommentId, HostRepo, Millis, Person, PrDetail, PrRef, PrSummary, RepoRef,
    ReviewEvent, Sha, ThreadId,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Capabilities {
    /// Server-side Viewed per file.
    pub viewed: bool,
    /// Drafts can be submitted as one batched review.
    pub pending_review: bool,
    pub suggestions: bool,
    pub resolve_threads: bool,
    pub merge_queue: bool,
    pub auto_merge: bool,
    pub reactions: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("not authenticated or token rejected")]
    Auth,
    /// The token is good but may not do this one thing (GraphQL `FORBIDDEN`
    /// on a mutation: no permission on this repo, a protected branch, …).
    /// Unlike `Auth` it says nothing about the connection.
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("rate limited until {reset_at}")]
    RateLimited { reset_at: Millis },
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("rejected by host: {0}")]
    Validation(String),
    #[error("network: {0}")]
    Network(String),
    #[error("not supported by this host: {0}")]
    Unsupported(&'static str),
    #[error("unexpected response: {0}")]
    Protocol(String),
}

pub type HostResult<T> = Result<T, HostError>;

impl HostError {
    /// The stable error code logged as `error.code` (docs/telemetry.md).
    pub fn code(&self) -> &'static str {
        match self {
            HostError::Auth => "host.auth",
            HostError::Forbidden(_) => "host.forbidden",
            HostError::RateLimited { .. } => "host.rate_limited",
            HostError::NotFound(_) => "host.not_found",
            HostError::Conflict(_) => "host.conflict",
            HostError::Validation(_) => "host.validation",
            HostError::Network(_) => "host.network",
            HostError::Unsupported(_) => "host.unsupported",
            HostError::Protocol(_) => "host.protocol",
        }
    }
}

/// Conditional fetch result (ETag).
#[derive(Debug, Clone)]
pub enum Fetched<T> {
    NotModified,
    Modified { etag: Option<String>, value: T },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InboxSection {
    ReviewRequested,
    Assigned,
    Authored,
    Mentioned,
    /// Raw host search query, e.g. `is:pr is:open repo:o/r`.
    Query(String),
}

/// Cheap "did anything change" probe (GitHub notifications, GitLab
/// `updated_after`, …). `cursor` is opaque to callers.
#[derive(Debug, Clone)]
pub struct Changes {
    pub prs: Vec<PrRef>,
    pub cursor: String,
    /// Host-requested minimum delay before the next probe.
    pub poll_after_ms: Option<i64>,
}

/// One comment of a batched review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewComment {
    pub anchor: Anchor,
    pub body: String,
    /// Idempotency marker, embedded invisibly in the body by the adapter
    /// (`<!-- theodore:<marker> -->`).
    pub marker: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewSubmission {
    pub event: ReviewEvent,
    pub body: String,
    pub comments: Vec<ReviewComment>,
    /// Head sha the review is made against.
    pub commit: theodore_domain::Sha,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmittedReview {
    pub review_id: String,
    /// marker → created thread id
    pub threads: Vec<(String, ThreadId)>,
}

// ── merge ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

/// Host-neutral read of GitHub's `mergeStateStatus` / GitLab's
/// `detailed_merge_status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeState {
    Clean,
    Blocked,
    Behind,
    Dirty,
    Unstable,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeStatus {
    /// `None` when the host hasn't computed it yet.
    pub mergeable: Option<bool>,
    pub state: MergeState,
    pub allowed_methods: Vec<MergeMethod>,
    pub auto_merge_enabled: bool,
    pub in_queue: bool,
    pub required_approvals: Option<u32>,
    /// The repo lets a PR merge itself once requirements pass.
    pub auto_merge_allowed: bool,
    /// The base branch merges through a merge queue.
    pub merge_queue: bool,
    /// The method the host would pick (GitHub `viewerDefaultMergeMethod`);
    /// `None` when it doesn't say.
    #[serde(default)]
    pub default_method: Option<MergeMethod>,
}

// ── create ───────────────────────────────────────────────────────────────

/// A pull request to open from a branch already pushed to the same
/// repository (docs/spaces.md "Open PR"). No forks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewPr {
    /// Source branch name (no `refs/heads/` prefix).
    pub head: String,
    /// Target branch name.
    pub base: String,
    pub title: String,
    pub body: String,
    /// Open as a draft (GitHub/Bitbucket draft flag, GitLab `Draft:` title).
    pub draft: bool,
}

#[async_trait]
pub trait HostAdapter: Send + Sync {
    fn kind(&self) -> theodore_domain::HostKind;
    fn capabilities(&self) -> Capabilities;

    /// The authenticated user (validates the token).
    async fn viewer(&self) -> HostResult<Person> {
        Err(HostError::Unsupported("viewer"))
    }
    /// Like `viewer`, but explicit about which host it validates against
    /// (plan §6.8 `set_host_token`): a caller holding several hosts behind
    /// one `HostAdapter` (GitHub primary, GitLab, …) routes this to the
    /// adapter for `host` rather than whichever one `kind()` happens to be.
    /// Default: only this adapter's own kind is supported — correct for a
    /// single-host adapter; a multi-host composition should override it to
    /// route per `host` the way it already does for `search_repo`/`get_pr`.
    async fn viewer_for(&self, host: theodore_domain::HostKind) -> HostResult<Person> {
        if host == self.kind() {
            self.viewer().await
        } else {
            Err(HostError::Unsupported("host"))
        }
    }

    // inbox
    async fn changed_since(&self, cursor: Option<&str>) -> HostResult<Changes>;
    async fn search(&self, section: &InboxSection) -> HostResult<Vec<PrSummary>>;
    /// Like `search`, but routed to the host that owns `repo`. A composed
    /// adapter (several hosts behind one `HostAdapter`) can't tell GitHub
    /// and GitLab apart from a bare query string, so project refreshes
    /// (plan "projects") call this instead of `search`; a single-host
    /// adapter just ignores `repo` and defers to `search`.
    async fn search_repo(
        &self,
        repo: &RepoRef,
        section: &InboxSection,
    ) -> HostResult<Vec<PrSummary>> {
        let _ = repo;
        self.search(section).await
    }
    /// Repos for the Add project picker, with the metadata it shows
    /// (description, visibility, last push) — not just the bare ref.
    async fn list_repos(&self) -> HostResult<Vec<HostRepo>>;

    // pulls
    async fn get_pr(&self, pr: &PrRef, etag: Option<&str>) -> HostResult<Fetched<PrDetail>>;

    // create
    /// The open PR whose source branch is `head` in `repo` (same-repo PRs
    /// only), if any. With `create_pr` this makes opening a PR idempotent
    /// by head branch: a retried outbox item finds the PR the first
    /// attempt opened instead of opening a second one.
    async fn find_open_pr(&self, _repo: &RepoRef, _head: &str) -> HostResult<Option<PrRef>> {
        Err(HostError::Unsupported("find_open_pr"))
    }
    /// Opens a PR from `pr.head` into `pr.base`. Returns the same `PrRef`
    /// the adapter's `search`/`get_pr` use for that PR. A host that refuses
    /// because a PR for `head` already exists answers `HostError::Conflict`
    /// (callers then look it up with `find_open_pr`).
    async fn create_pr(&self, _repo: &RepoRef, _pr: &NewPr) -> HostResult<PrRef> {
        Err(HostError::Unsupported("create_pr"))
    }

    // reviews
    async fn set_viewed(&self, pr: &PrRef, path: &str, viewed: bool) -> HostResult<()>;
    async fn submit_review(
        &self,
        pr: &PrRef,
        review: &ReviewSubmission,
    ) -> HostResult<SubmittedReview>;
    /// Find a comment previously posted with `marker` (outbox retry after timeout).
    async fn find_marker(&self, pr: &PrRef, marker: &str) -> HostResult<Option<CommentId>>;

    // threads
    async fn reply(&self, pr: &PrRef, thread: &ThreadId, body: &str) -> HostResult<CommentId>;
    /// Like `reply`, but embeds an idempotency marker in the posted body
    /// (the same `<!-- theodore:<marker> -->` tag `submit_review` uses) so a
    /// retried outbox item can tell, via `find_marker`, whether the reply
    /// already landed before retrying. Default: embeds the standard tag and
    /// defers to `reply`; override only if a host needs a different shape.
    async fn reply_with_marker(
        &self,
        pr: &PrRef,
        thread: &ThreadId,
        body: &str,
        marker: &str,
    ) -> HostResult<CommentId> {
        let tagged = format!("{body}\n\n<!-- theodore:{marker} -->");
        self.reply(pr, thread, &tagged).await
    }
    async fn resolve(&self, pr: &PrRef, thread: &ThreadId, resolved: bool) -> HostResult<()>;
    async fn edit_comment(&self, pr: &PrRef, comment: &CommentId, body: &str) -> HostResult<()>;
    async fn delete_comment(&self, pr: &PrRef, comment: &CommentId) -> HostResult<()>;

    // reactions
    async fn add_reaction(
        &self,
        _pr: &PrRef,
        _comment: &CommentId,
        _emoji: &str,
    ) -> HostResult<()> {
        Err(HostError::Unsupported("add_reaction"))
    }
    async fn remove_reaction(
        &self,
        _pr: &PrRef,
        _comment: &CommentId,
        _emoji: &str,
    ) -> HostResult<()> {
        Err(HostError::Unsupported("remove_reaction"))
    }

    // people
    /// Candidates for the reviewer picker (repo collaborators / members).
    async fn assignable_users(&self, _repo: &RepoRef) -> HostResult<Vec<Person>> {
        Err(HostError::Unsupported("assignable_users"))
    }
    async fn request_reviewers(
        &self,
        _pr: &PrRef,
        _users: &[String],
        _teams: &[String],
    ) -> HostResult<()> {
        Err(HostError::Unsupported("request_reviewers"))
    }
    async fn remove_reviewer(&self, _pr: &PrRef, _user: &str) -> HostResult<()> {
        Err(HostError::Unsupported("remove_reviewer"))
    }

    // draft state
    async fn mark_ready(&self, _pr: &PrRef) -> HostResult<()> {
        Err(HostError::Unsupported("mark_ready"))
    }
    async fn convert_to_draft(&self, _pr: &PrRef) -> HostResult<()> {
        Err(HostError::Unsupported("convert_to_draft"))
    }

    // merge
    /// Current mergeability, allowed methods and queue/auto-merge state.
    async fn merge_status(&self, _pr: &PrRef) -> HostResult<MergeStatus> {
        Err(HostError::Unsupported("merge_status"))
    }
    /// Merges `pr`. Returns the resulting merge commit sha.
    async fn merge(
        &self,
        _pr: &PrRef,
        _method: MergeMethod,
        _title: Option<String>,
        _body: Option<String>,
    ) -> HostResult<Sha> {
        Err(HostError::Unsupported("merge"))
    }
    async fn enable_auto_merge(&self, _pr: &PrRef, _method: MergeMethod) -> HostResult<()> {
        Err(HostError::Unsupported("enable_auto_merge"))
    }
    async fn disable_auto_merge(&self, _pr: &PrRef) -> HostResult<()> {
        Err(HostError::Unsupported("disable_auto_merge"))
    }
    /// Joins the host's merge queue (GitHub merge queue / GitLab merge train).
    async fn enqueue(&self, _pr: &PrRef) -> HostResult<()> {
        Err(HostError::Unsupported("enqueue"))
    }
    /// Merges the base branch into the PR's head. `HostError::Conflict` when
    /// that can't be done automatically.
    async fn update_branch(&self, _pr: &PrRef) -> HostResult<()> {
        Err(HostError::Unsupported("update_branch"))
    }

    /// Re-runs one check (plan "re-run a check"). Takes the whole `Check` —
    /// not just its id — so an adapter can pick the right underlying API
    /// from its id and url (e.g. GitHub's check-run rerequest vs. its
    /// Actions rerun-failed-jobs) without a second round trip to look it up.
    async fn rerun_check(&self, _pr: &PrRef, _check: &Check) -> HostResult<()> {
        Err(HostError::Unsupported("rerun_check"))
    }

    /// The tail of a failed check's log (docs/spaces.md "Fix CI"), trimmed
    /// to the part worth putting in an agent's prompt (ANSI and per-line
    /// timestamps stripped, last few hundred lines). Same idea as
    /// `rerun_check`: the whole `Check` goes in so the adapter can resolve
    /// the right API from its id/url itself. `Ok(None)` means this check has
    /// no log to fetch — an external status context, or any check the host
    /// can't read logs for — callers fall back to its name and `url`.
    async fn check_log(&self, _pr: &PrRef, _check: &Check) -> HostResult<Option<String>> {
        Err(HostError::Unsupported("check_log"))
    }

    /// Cheap single-repo metadata fetch (plan "projects"): description,
    /// visibility, last push. The per-repo counterpart to `list_repos`, used
    /// to keep a project's header current without re-listing every repo the
    /// token can see.
    async fn repo_info(&self, _repo: &RepoRef) -> HostResult<HostRepo> {
        Err(HostError::Unsupported("repo_info"))
    }
}
