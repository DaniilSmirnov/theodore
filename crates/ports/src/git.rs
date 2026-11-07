//! Git port (plan §7). Implemented over the git CLI.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use theodore_domain::{
    CheckRollup, Commit, Millis, Person, PrDetail, PrRef, PrState, PrSummary, RepoRef, Revision,
    Sha,
};

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("git not found")]
    NotInstalled,
    #[error("git {args}: exit {code}: {stderr}")]
    Command {
        args: String,
        code: i32,
        stderr: String,
    },
    #[error("auth failed for {0}")]
    Auth(String),
    /// The remote couldn't be reached at all (no network, DNS, a timeout):
    /// an expected state on a laptop, not a failure. Holds git's stderr
    /// for the log; the message is meant for the user as is.
    #[error("You're offline — can't reach the git remote")]
    Network(String),
    #[error("parse: {0}")]
    Parse(String),
    /// A commit asked for isn't in the repository (a head force-pushed
    /// away, a stale last-seen sha): an expected state the caller falls
    /// back from, not a git failure.
    #[error("commit {0} not found")]
    MissingCommit(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("not supported: {0}")]
    Unsupported(&'static str),
    /// A safety check said no before anything was written: a dirty
    /// working tree, a merge that would conflict, a branch that moved.
    /// The message is meant for the user as is.
    #[error("{0}")]
    Refused(String),
    /// A space's directory is not its own git worktree any more (its `.git`
    /// is gone, or points elsewhere), so git would resolve another
    /// repository — a parent one, say. Nothing was run. The message is
    /// meant for the user as is.
    #[error("{0}")]
    NotAWorktree(String),
    /// A git operation is unfinished in a space's worktree (docs/spaces.md
    /// §6): nothing was committed or reset.
    #[error("{0} in the space's worktree; finish or abort it first")]
    Busy(WorktreeBusy),
}

pub type GitResult<T> = Result<T, GitError>;

/// What git is in the middle of in a worktree (`GitPort::worktree_busy`).
/// Checkpoint and Undo wait while it lasts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeBusy {
    /// `rebase-merge` / `rebase-apply` (also `git am`).
    Rebase,
    Merge,
    CherryPick,
    Revert,
    /// Unmerged index entries with none of the above (a conflicting
    /// `stash pop`, `checkout -m`).
    Conflicts,
}

impl std::fmt::Display for WorktreeBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            WorktreeBusy::Rebase => "rebase in progress",
            WorktreeBusy::Merge => "merge in progress",
            WorktreeBusy::CherryPick => "cherry-pick in progress",
            WorktreeBusy::Revert => "revert in progress",
            WorktreeBusy::Conflicts => "unresolved conflicts",
        })
    }
}

impl GitError {
    /// The stable error code logged as `error.code` (docs/telemetry.md).
    pub fn code(&self) -> &'static str {
        match self {
            GitError::NotInstalled => "git.not_installed",
            GitError::Command { .. } => "git.command_failed",
            GitError::Auth(_) => "git.auth",
            GitError::Network(_) => "git.network",
            GitError::Parse(_) => "git.parse",
            GitError::MissingCommit(_) => "git.commit_not_found",
            GitError::Io(_) => "git.io",
            GitError::Unsupported(_) => "git.unsupported",
            GitError::Refused(_) => "git.refused",
            GitError::NotAWorktree(_) => "git.not_a_worktree",
            GitError::Busy(_) => "git.busy",
        }
    }
}

#[derive(Debug, Clone)]
pub struct DiffOptions {
    pub context: u32,
    pub find_renames: bool,
    /// Files over this many changed lines get `FileFlag::Oversize` and no hunks.
    pub max_file_lines: u32,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            context: 3,
            find_renames: true,
            max_file_lines: 5_000,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Worktree {
    pub path: PathBuf,
    pub head: Sha,
}

/// One space checkpoint (docs/spaces.md §6): the worktree as a commit, and
/// the worktree's own `HEAD` when it was taken. `sha == head` when nothing
/// was uncommitted; a checkpoint from before snapshots is a commit that was
/// `HEAD`, so `head` is `sha` there too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceCheckpoint {
    pub sha: Sha,
    pub head: Sha,
}

/// One file's line counts between two commits ([`GitPort::numstat`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStat {
    pub path: String,
    pub additions: u32,
    pub deletions: u32,
}

/// One local branch of a project (plan "projects"): host projects list
/// remote branches of the bare clone, local folder projects list the
/// folder's own branches, both read off the same `refs/heads/*` namespace.
#[derive(Debug, Clone)]
pub struct BranchInfo {
    pub name: String,
    pub head: Sha,
    /// Last commit subject.
    pub subject: String,
    /// Last commit author's name.
    pub author: String,
    /// Commits ahead of / behind the project's default branch.
    pub ahead: u32,
    pub behind: u32,
    pub updated_at: Millis,
}

/// One commit's metadata, used to describe a local branch review that has
/// no host PR behind it.
#[derive(Debug, Clone)]
pub struct CommitLog {
    pub sha: Sha,
    pub title: String,
    pub author_name: String,
    pub author_email: String,
    pub at: Millis,
}

/// One `git grep` match (plan "symbols": go-to-definition / find-references
/// without a full LSP).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepHit {
    pub path: String,
    pub line: u32,
    pub text: String,
}

/// Provides an HTTPS token for a remote at call time (never written to disk).
pub trait CredentialSource: Send + Sync {
    fn token_for(&self, repo: &RepoRef) -> Option<String>;
}

#[async_trait]
pub trait GitPort: Send + Sync {
    /// Bare clone under `~/.theodore/repos/…`, or fetch if present. Returns its path.
    async fn ensure_repo(&self, repo: &RepoRef) -> GitResult<PathBuf>;
    /// Fetch `pull/<n>/head` (or the host's equivalent) and return its sha.
    async fn fetch_pr(&self, pr: &PrRef) -> GitResult<Sha>;
    /// Worktree for the PR at `head`, created or moved. Locked while `lock` is held.
    async fn worktree(&self, pr: &PrRef, head: &Sha) -> GitResult<Worktree>;
    async fn remove_worktree(&self, pr: &PrRef) -> GitResult<()>;
    /// Drop worktrees of PRs not in `keep`.
    async fn gc_worktrees(&self, keep: &[PrRef]) -> GitResult<u32>;
    async fn merge_base(&self, repo: &Path, a: &Sha, b: &Sha) -> GitResult<Sha>;
    /// `base...head` parsed into files and hunks (with stable hunk ids).
    async fn diff(
        &self,
        repo: &Path,
        base: &Sha,
        head: &Sha,
        opts: &DiffOptions,
    ) -> GitResult<Revision>;
    /// File contents at `rev`, `None` if absent.
    async fn show(&self, repo: &Path, rev: &Sha, path: &str) -> GitResult<Option<Vec<u8>>>;
    /// The files that differ between two commits (not their merge base), with
    /// lines added and removed; a binary file counts as 0 and 0.
    async fn numstat(&self, repo: &Path, from: &Sha, to: &Sha) -> GitResult<Vec<FileStat>> {
        let _ = (repo, from, to);
        Err(GitError::Unsupported("numstat"))
    }

    /// The bare clone of `repo` if one is already on disk, `None` if not.
    /// Unlike `ensure_repo`, never touches the network: opening a PR
    /// offline reads from what was downloaded before.
    async fn cached_repo(&self, repo: &RepoRef) -> GitResult<Option<PathBuf>> {
        let _ = repo;
        Ok(None)
    }

    /// Whether commit `sha` is present in `repo`, without fetching.
    /// `false` by default: a port that can't tell never claims a commit.
    async fn has_commit(&self, repo: &Path, sha: &Sha) -> GitResult<bool> {
        let _ = (repo, sha);
        Ok(false)
    }

    /// Stages every change in `worktree`, commits it as `author`, and
    /// pushes to `branch` on `repo`'s remote (plan §4 "Fix target": own or
    /// local PR). Returns the new head sha. `Unsupported` by default; only a
    /// `GitPort` backed by a real remote (the git CLI) can push.
    async fn commit_and_push(
        &self,
        worktree: &Path,
        repo: &RepoRef,
        branch: &str,
        message: &str,
        author: &Identity,
    ) -> GitResult<Sha> {
        let _ = (worktree, repo, branch, message, author);
        Err(GitError::Unsupported("commit_and_push"))
    }

    // ── projects (plan "projects") ───────────────────────────────────────

    /// Bare, hardlink-free mirror of a local folder under the cache,
    /// refreshed by fetching straight from `source`. Never opens `source`'s
    /// own `.git` for writing. `Unsupported` by default; only a `GitPort`
    /// backed by the real git CLI can mirror a folder.
    async fn ensure_local_mirror(&self, source: &Path, repo: &RepoRef) -> GitResult<PathBuf> {
        let _ = (source, repo);
        Err(GitError::Unsupported("ensure_local_mirror"))
    }

    /// Whether git tracks `path` (relative to `folder`), so whoever can
    /// commit to the repo decides what it holds.
    async fn is_tracked(&self, folder: &Path, path: &str) -> GitResult<bool> {
        let _ = (folder, path);
        Err(GitError::Unsupported("is_tracked"))
    }

    /// Ignores `pattern` in `folder`'s own `info/exclude`: this checkout
    /// only, nothing to commit, no file in the working tree changes.
    async fn exclude_locally(&self, folder: &Path, pattern: &str) -> GitResult<()> {
        let _ = (folder, pattern);
        Err(GitError::Unsupported("exclude_locally"))
    }

    /// Whether `path` is (inside) a git working copy or bare repository.
    async fn is_repo(&self, path: &Path) -> GitResult<bool> {
        let _ = path;
        Err(GitError::Unsupported("is_repo"))
    }

    /// The repository's default branch (`git symbolic-ref HEAD`), `None` on
    /// a detached or empty repository.
    async fn default_branch(&self, repo: &Path) -> GitResult<Option<String>> {
        let _ = repo;
        Ok(None)
    }

    /// Every local branch of `repo` (its own `refs/heads/*`; for a bare
    /// clone of a host repo this already is "remote" branches, since
    /// `git clone --bare` places them directly under `refs/heads/*`).
    async fn list_branches(&self, repo: &Path, default_branch: &str) -> GitResult<Vec<BranchInfo>> {
        let _ = (repo, default_branch);
        Ok(Vec::new())
    }

    /// Resolves a ref (branch name, tag, sha, …) to its commit sha.
    async fn resolve_ref(&self, repo: &Path, refname: &str) -> GitResult<Sha> {
        let _ = (repo, refname);
        Err(GitError::Unsupported("resolve_ref"))
    }

    /// One commit's metadata.
    async fn commit(&self, repo: &Path, sha: &Sha) -> GitResult<CommitLog> {
        let _ = (repo, sha);
        Err(GitError::Unsupported("commit"))
    }

    /// Commits reachable from `head` but not `base` (`git log base..head`).
    async fn commit_log(&self, repo: &Path, base: &Sha, head: &Sha) -> GitResult<Vec<CommitLog>> {
        let _ = (repo, base, head);
        Ok(Vec::new())
    }

    /// Co-change history for grouping: the last `max_commits` non-merge
    /// commits reachable from `rev`, each reduced to the subset of `paths`
    /// it touched. Commits touching more than `max_files` files in total
    /// (bulk reformat, vendoring) and commits touching none of `paths` are
    /// left out. Empty by default.
    async fn co_change_commits(
        &self,
        repo: &Path,
        rev: &Sha,
        paths: &[String],
        max_commits: usize,
        max_files: usize,
    ) -> GitResult<Vec<Vec<String>>> {
        let _ = (repo, rev, paths, max_commits, max_files);
        Ok(Vec::new())
    }

    /// Deletes the cached clone/mirror of `repo` and any worktrees under it.
    /// Never touches a local folder project's own checkout.
    async fn remove_repo(&self, repo: &RepoRef) -> GitResult<()> {
        let _ = repo;
        Ok(())
    }

    /// Builds a synthetic PR's detail + diff for reviewing `branch` against
    /// `base` with no host PR behind it (plan "projects" `open_branch`).
    /// The default composes the primitives above; a `GitPort` only needs to
    /// override those, not this.
    async fn describe_branch(
        &self,
        repo: &Path,
        repo_ref: &RepoRef,
        number: u64,
        branch: &str,
        base: &str,
    ) -> GitResult<(PrDetail, Revision)> {
        let head = self.resolve_ref(repo, branch).await?;
        let base_sha = self.resolve_ref(repo, base).await?;
        let head_commit = self.commit(repo, &head).await?;
        let commits = self.commit_log(repo, &base_sha, &head).await?;
        let revision = self
            .diff(repo, &base_sha, &head, &DiffOptions::default())
            .await?;
        let additions = revision.files.iter().map(|f| f.additions).sum();
        let deletions = revision.files.iter().map(|f| f.deletions).sum();
        let changed_files = revision.files.len() as u32;
        let pr = PrRef {
            repo: repo_ref.clone(),
            number,
            host_id: Some(branch.to_string()),
        };
        let summary = PrSummary {
            pr,
            title: head_commit.title.clone(),
            author: person_of(&head_commit),
            state: PrState::Open,
            base_ref: base.to_string(),
            head_ref: branch.to_string(),
            head_sha: head,
            additions,
            deletions,
            changed_files,
            reasons: Vec::new(),
            checks: CheckRollup::None,
            approvals: 0,
            required_approvals: None,
            updated_at: head_commit.at,
            url: String::new(),
            review_decision: None,
            conflicts: false,
            created_at: Some(head_commit.at),
            stack: None,
        };
        let detail = PrDetail {
            summary,
            body: String::new(),
            base_sha,
            commits: commits
                .into_iter()
                .map(|c| Commit {
                    sha: c.sha.clone(),
                    title: c.title.clone(),
                    author: person_of(&c),
                    at: c.at,
                    // A local branch review has no host-side reviews to
                    // measure freshness against.
                    after_my_review: false,
                })
                .collect(),
            checks: Vec::new(),
            reviewers: Vec::new(),
            assignees: Vec::new(),
            labels: Vec::new(),
            threads: Vec::new(),
            viewed: Vec::new(),
        };
        Ok((detail, revision))
    }

    /// Whole-word, extended-regex search of `worktree`'s tracked files
    /// (plan "symbols": go-to-definition / find-references without a full
    /// LSP). `Unsupported` by default; only a `GitPort` backed by the real
    /// git CLI can grep a working tree.
    async fn grep(&self, worktree: &Path, pattern: &str) -> GitResult<Vec<GrepHit>> {
        let _ = (worktree, pattern);
        Err(GitError::Unsupported("grep"))
    }
    /// Conflicting paths of merging `head` into `base` (`git merge-tree
    /// --write-tree --name-only`, git >= 2.38): no worktree checkout, no
    /// index changes (plan "conflicts"). `Unsupported` by default, and on
    /// an older local `git`.
    async fn merge_tree_conflicts(
        &self,
        repo: &Path,
        base: &Sha,
        head: &Sha,
    ) -> GitResult<Vec<ConflictEntry>> {
        let _ = (repo, base, head);
        Err(GitError::Unsupported("merge_tree_conflicts"))
    }

    /// A local folder project's own checkout: its checked-out branch and how
    /// many paths are uncommitted. Read-only: never takes the index lock or
    /// refreshes the index (`--no-optional-locks`). `Unsupported` by
    /// default; only a `GitPort` backed by the real git CLI can read a
    /// working copy.
    async fn working_state(&self, folder: &Path) -> GitResult<WorkingState> {
        let _ = folder;
        Err(GitError::Unsupported("working_state"))
    }

    // ── spaces (docs/spaces.md) ──────────────────────────────────────────

    /// The detached worktree of space `space` (`<root>/spaces/<space>`),
    /// created at the space's checkpoint ref (`refs/theodore/spaces/<space>`)
    /// if one exists, else at `base` — which then becomes the ref. An
    /// existing worktree is returned as is: it may hold the agent's
    /// uncommitted edits, so it is never reset. A directory there that is
    /// not a worktree of Theodore's (its `.git` gone) is moved aside and
    /// the worktree recreated. `repo` is the bare cache.
    async fn space_worktree(&self, repo: &Path, space: &str, base: &Sha) -> GitResult<Worktree> {
        let _ = (repo, space, base);
        Err(GitError::Unsupported("space_worktree"))
    }

    /// Snapshots everything in a space's worktree (tracked and untracked,
    /// `.gitignore` honored) through a temporary index into a commit on top
    /// of the worktree's `HEAD`, and points `refs/theodore/spaces/<space>`
    /// at it. The worktree's `HEAD`, branch and index stay as they are, so
    /// the agent's own commits are kept as it made them. A worktree that
    /// matches the last checkpoint gets that checkpoint again; one that
    /// matches `HEAD` gets `HEAD`. Snapshots nothing, with `Busy`, while a
    /// rebase, merge or conflict is unfinished there; `NotAWorktree` when
    /// git would resolve another repository from `worktree`.
    async fn space_checkpoint(
        &self,
        worktree: &Path,
        space: &str,
        message: &str,
    ) -> GitResult<SpaceCheckpoint> {
        let _ = (worktree, space, message);
        Err(GitError::Unsupported("space_checkpoint"))
    }

    /// Moves a space back to the earlier checkpoint `to`: `HEAD` is reset
    /// (mixed) to `to.head` and the worktree made to match `to.sha`,
    /// writing only the files whose content differs and removing untracked
    /// files that weren't in it (ignored files stay);
    /// `refs/theodore/spaces/<space>` points at `to.sha` again. Same
    /// refusals as `space_checkpoint`.
    async fn space_reset(
        &self,
        worktree: &Path,
        space: &str,
        to: &SpaceCheckpoint,
    ) -> GitResult<()> {
        let _ = (worktree, space, to);
        Err(GitError::Unsupported("space_reset"))
    }

    /// Makes `paths` in a space's worktree as they are in commit `from`:
    /// written from it, or removed when `from` has no such file. `HEAD`,
    /// the index and refs stay; the caller checkpoints. Same refusals as
    /// `space_checkpoint`; a path that leaves the worktree is `Refused`.
    async fn space_restore_paths(
        &self,
        worktree: &Path,
        paths: &[String],
        from: &Sha,
    ) -> GitResult<()> {
        let _ = (worktree, paths, from);
        Err(GitError::Unsupported("space_restore_paths"))
    }

    /// A cheap fingerprint of a space worktree's changes (`HEAD`, what
    /// `status` lists, and those files' size and mtime): equal while
    /// nothing changed. Read only; never writes the index.
    async fn space_fingerprint(&self, worktree: &Path) -> GitResult<String> {
        let _ = worktree;
        Err(GitError::Unsupported("space_fingerprint"))
    }

    /// The git operation unfinished in a space's `worktree`, if any. Read
    /// only. `Ok(None)` by default (fakes); the git CLI checks the
    /// worktree's root first (`NotAWorktree`).
    async fn worktree_busy(&self, worktree: &Path) -> GitResult<Option<WorktreeBusy>> {
        let _ = worktree;
        Ok(None)
    }

    /// Whether two commits hold the same tree. By the shas by default (fakes).
    async fn same_tree(&self, repo: &Path, a: &Sha, b: &Sha) -> GitResult<bool> {
        let _ = repo;
        Ok(a == b)
    }

    /// Keeps `paths` (relative to the worktree) out of every checkpoint: the
    /// ones git would otherwise pick up, untracked and not ignored, are
    /// excluded for the cache's worktrees. Tracked and ignored ones are left
    /// as they are. `Ok` by default (fakes).
    async fn space_exclude(&self, worktree: &Path, paths: &[String]) -> GitResult<()> {
        let _ = (worktree, paths);
        Ok(())
    }

    /// The space's worktree as it is now, as a commit on top of `HEAD`:
    /// staged through a temporary index, so the worktree's own index, its
    /// `HEAD` and every ref stay as they were. `HEAD` itself when nothing
    /// is uncommitted; the same worktree on the same `HEAD` gives the same
    /// commit (its dates are `HEAD`'s).
    async fn space_snapshot(&self, worktree: &Path) -> GitResult<Sha> {
        let _ = worktree;
        Err(GitError::Unsupported("space_snapshot"))
    }
    /// Update from base (docs/spaces.md §6): merges `tip` into a space's
    /// worktree (never a rebase, so pushed history stays). Clean: Theodore
    /// commits it as a checkpoint (`message`) and `Merged` is the new
    /// head, ref moved. Conflicts: the merge is left in progress for the
    /// agent and `Conflicts` lists the files. `Busy` when another operation
    /// is unfinished there already.
    async fn space_merge(
        &self,
        worktree: &Path,
        space: &str,
        tip: &Sha,
        message: &str,
    ) -> GitResult<SpaceMerge> {
        let _ = (worktree, space, tip, message);
        Err(GitError::Unsupported("space_merge"))
    }

    /// Completes the merge `space_merge` left in progress: once neither a
    /// file in `conflicts` (what it listed, staged or not) nor one still
    /// unmerged has conflict markers, everything is staged and the merge
    /// commit written as a checkpoint (`Merged`); otherwise `Conflicts` with
    /// what is left. A merge concluded in the worktree by hand is
    /// checkpointed as is; `NotMerging` when it was aborted there.
    async fn space_merge_finish(
        &self,
        worktree: &Path,
        space: &str,
        tip: &Sha,
        conflicts: &[String],
        message: &str,
    ) -> GitResult<SpaceMerge> {
        let _ = (worktree, space, tip, conflicts, message);
        Err(GitError::Unsupported("space_merge_finish"))
    }

    /// Abandons an update from base: `merge --abort`, then the worktree
    /// and `refs/theodore/spaces/<space>` back at checkpoint `to`.
    async fn space_merge_abort(&self, worktree: &Path, space: &str, to: &Sha) -> GitResult<()> {
        let _ = (worktree, space, to);
        Err(GitError::Unsupported("space_merge_abort"))
    }

    /// Removes a space's worktree directory. The checkpoint ref stays, so
    /// `space_worktree` can bring the space back. A directory that isn't
    /// a worktree of Theodore's is moved aside, never deleted.
    async fn remove_space_worktree(&self, repo: &Path, space: &str) -> GitResult<()> {
        let _ = (repo, space);
        Err(GitError::Unsupported("remove_space_worktree"))
    }

    // ── shipping a space (docs/spaces.md §6) ─────────────────────────────

    /// The user's own git identity (`user.name` / `user.email` as `git`
    /// resolves them for `repo`), `None` when either is unset.
    async fn user_identity(&self, repo: &Path) -> GitResult<Option<Identity>> {
        let _ = repo;
        Ok(None)
    }

    /// Writes one commit with `c.head`'s tree on top of `c.parent`, authored
    /// and committed by `c.author`, and points `refs/theodore/shipped/<space>`
    /// at it. With `c.replay_from`, the tree is instead `c.parent` plus the
    /// change `replay_from..head` (a three-way merge, no worktree);
    /// `Refused` when that conflicts. Never touches a worktree or a branch.
    async fn space_commit(&self, repo: &Path, space: &str, c: &SpaceCommit) -> GitResult<Sha> {
        let _ = (repo, space, c);
        Err(GitError::Unsupported("space_commit"))
    }

    /// Fetches `branch` from `repo_ref`'s remote into the cache and returns
    /// its tip, `None` when the remote has no such branch.
    async fn remote_branch(
        &self,
        repo: &Path,
        repo_ref: &RepoRef,
        branch: &str,
    ) -> GitResult<Option<Sha>> {
        let _ = (repo, repo_ref, branch);
        Err(GitError::Unsupported("remote_branch"))
    }

    /// Pushes `sha` to `refs/heads/<branch>` on `repo_ref`'s remote. Never
    /// forced: a branch that moved elsewhere is rejected by the remote.
    async fn push_branch(
        &self,
        repo: &Path,
        repo_ref: &RepoRef,
        sha: &Sha,
        branch: &str,
    ) -> GitResult<()> {
        let _ = (repo, repo_ref, sha, branch);
        Err(GitError::Unsupported("push_branch"))
    }

    /// Merges the space's shipped commit (`refs/theodore/shipped/<space>` in
    /// the cache `repo`) into the branch checked out in the local `folder`,
    /// with a normal `git merge`. `Refused`, with the folder untouched, on
    /// a dirty working tree, a detached `HEAD` or a conflicting merge.
    /// Returns the folder's new `HEAD`.
    async fn merge_into_folder(
        &self,
        folder: &Path,
        repo: &Path,
        space: &str,
        message: &str,
    ) -> GitResult<Sha> {
        let _ = (folder, repo, space, message);
        Err(GitError::Unsupported("merge_into_folder"))
    }

    /// Creates `refs/heads/<branch>` in the local `folder` at the space's
    /// shipped commit. Only a ref is written; `Refused` when the branch
    /// already exists.
    async fn write_folder_branch(
        &self,
        folder: &Path,
        repo: &Path,
        space: &str,
        branch: &str,
    ) -> GitResult<()> {
        let _ = (folder, repo, space, branch);
        Err(GitError::Unsupported("write_folder_branch"))
    }

    // ── mirroring a space into a local folder (docs/spaces.md) ───────────

    /// Writes the space's checkpoint `to` into `folder`'s working tree
    /// (docs/spaces.md "Mirror into a local folder"): only the paths that
    /// differ between `from` and `to` are rewritten or removed (the first
    /// sync passes the space's own `base_sha`, so only what the agent
    /// actually changed is written, not the whole tree). The folder's own
    /// index, branch and `HEAD` are never touched — a temporary index does
    /// the lookup. Returns the paths written or removed, for the caller to
    /// fold into the running set mirroring has ever touched (`mirror_edited`
    /// / `mirror_restore` work from that set, not from a single diff: a
    /// path the agent added and then removed again has no net difference
    /// between the session's endpoints, but still needs to be watched and
    /// restored).
    async fn mirror_sync(
        &self,
        folder: &Path,
        repo: &Path,
        space: &str,
        to: &Sha,
        from: &Sha,
    ) -> GitResult<Vec<String>> {
        let _ = (folder, repo, space, to, from);
        Err(GitError::Unsupported("mirror_sync"))
    }

    /// Which of `paths` (the running set `mirror_sync` has ever touched) no
    /// longer match what mirroring wrote there, checked against `to`'s
    /// tree (the last checkpoint mirrored) by content — never a timestamp.
    async fn mirror_edited(
        &self,
        folder: &Path,
        repo: &Path,
        space: &str,
        to: &Sha,
        paths: &[String],
    ) -> GitResult<Vec<String>> {
        let _ = (folder, repo, space, to, paths);
        Err(GitError::Unsupported("mirror_edited"))
    }

    /// Restores every path in `paths` to its content at `from` (the
    /// folder's own remembered `HEAD`, already local — no fetch needed)
    /// and touches nothing else: the folder's branch, `HEAD` and index
    /// stay exactly as they were. Returns the paths restored or removed.
    async fn mirror_restore(
        &self,
        folder: &Path,
        from: &Sha,
        paths: &[String],
    ) -> GitResult<Vec<String>> {
        let _ = (folder, from, paths);
        Err(GitError::Unsupported("mirror_restore"))
    }

    // ── saving a project's config (docs/config.md) ───────────────────────

    /// Writes one commit on top of `parent` in the bare cache `repo` whose
    /// tree is `parent`'s with the file at `path` set to `content`,
    /// authored and committed by `author`. Built from trees directly
    /// (`hash-object`, `mktree`, `commit-tree`): no worktree, no checkout,
    /// no index, no ref moved. Returns the new commit.
    async fn commit_file(
        &self,
        repo: &Path,
        parent: &Sha,
        path: &str,
        content: &[u8],
        message: &str,
        author: &Identity,
    ) -> GitResult<Sha> {
        let _ = (repo, parent, path, content, message, author);
        Err(GitError::Unsupported("commit_file"))
    }

    /// Every path `git status` reports in a local folder's checkout
    /// (staged, unstaged, untracked; both sides of a rename). Read-only.
    async fn dirty_paths(&self, folder: &Path) -> GitResult<Vec<String>> {
        let _ = folder;
        Err(GitError::Unsupported("dirty_paths"))
    }

    /// `git add` then `git commit -- <paths>` in a local folder: only
    /// those paths, the user's own identity and hooks. Returns `HEAD`.
    async fn commit_paths(&self, folder: &Path, paths: &[String], message: &str) -> GitResult<Sha> {
        let _ = (folder, paths, message);
        Err(GitError::Unsupported("commit_paths"))
    }
}

/// An author for a commit Theodore writes on the user's behalf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub name: String,
    pub email: String,
}

/// One squashed commit of a space, for `GitPort::space_commit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceCommit {
    /// The checkpoint whose tree is shipped.
    pub head: Sha,
    /// The new commit's parent: the base on the first ship, the shipped
    /// branch's tip after that.
    pub parent: Sha,
    /// The checkpoint `parent`'s content was last shipped from, when
    /// `parent` has moved on since (someone else pushed to the branch):
    /// only `replay_from..head` is applied on top of `parent`.
    pub replay_from: Option<Sha>,
    /// The base tip merged into the space since (Update from base): a
    /// second parent, so the host sees the base merged in and the PR's
    /// diff stays the change alone. Dropped when `parent` already
    /// contains it.
    pub merge_parent: Option<Sha>,
    pub message: String,
    pub author: Identity,
}

/// What `GitPort::space_merge` / `space_merge_finish` left in a space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpaceMerge {
    /// The merge is committed; the new checkpoint.
    Merged(Sha),
    /// Still in progress: these files have conflicts.
    Conflicts(Vec<String>),
    /// No merge in progress any more, and the base isn't in `HEAD`: it
    /// was aborted in the worktree.
    NotMerging,
}

/// Where a space's last shipped (squashed) commit is kept in the cache.
pub fn shipped_ref(space: &str) -> String {
    format!("refs/theodore/shipped/{space}")
}

/// The ref a space's checkpoints live on (docs/spaces.md §6): outside
/// `refs/heads/*`, which the cache force-fetches and prunes.
pub fn space_ref(space: &str) -> String {
    format!("{SPACE_REF_PREFIX}{space}")
}

pub const SPACE_REF_PREFIX: &str = "refs/theodore/spaces/";

/// Keeps one checkpoint of a space reachable (docs/spaces.md §6): each
/// snapshot's parent is the worktree's `HEAD`, not the checkpoint before
/// it, so without these refs an older checkpoint could be pruned by `gc`
/// and Undo, rewind or Push would lose it.
pub fn space_checkpoint_ref(space: &str, sha: &Sha) -> String {
    format!("refs/theodore/checkpoints/{space}/{}", sha.as_str())
}

/// A local folder's checkout, from `GitPort::working_state`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkingState {
    /// The checked-out branch; `None` on a detached HEAD.
    pub current_branch: Option<String>,
    /// Paths `git status` reports: staged, unstaged and untracked.
    pub dirty_files: u32,
}

/// One locally-detected conflicting path, from `merge_tree_conflicts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictEntry {
    pub path: String,
    pub kind: ConflictKind,
}

/// The `CONFLICT (<kind>): ...` tag `git merge-tree` reports for a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictKind {
    Content,
    AddAdd,
    ModifyDelete,
    RenameDelete,
    RenameRename,
    DirectoryFile,
    Submodule,
    Other,
}

fn person_of(c: &CommitLog) -> Person {
    Person {
        login: c.author_email.clone(),
        name: Some(c.author_name.clone()),
        avatar_url: None,
    }
}
