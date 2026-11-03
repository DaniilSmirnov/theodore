//! GitPort over the git CLI: bare clones, worktrees, diff parsing.
//!
//! Layout under `root` (normally `~/.theodore`):
//! ```text
//! repos/<host>/<owner>/<repo>.git   bare clone
//! wt/<host>-<owner>-<repo>-<n>/     one worktree per PR, at its head
//! wt/.meta/<dirname>.json           sidecar: which RepoRef/PR a worktree is
//! spaces/<id>/                      one detached worktree per space
//! .askpass.sh                       GIT_ASKPASS helper (holds no secret)
//! ```
//! Auth never touches argv, a clone URL, or `.git/config` (see `auth.rs`).

mod auth;
mod branches;
mod config_file;
mod conflicts;
mod diff;
mod generated;
mod grep;
mod history;
mod local;
mod locks;
mod meta;
mod mirror;
mod parse;
mod process;
pub use process::isolate_from_user_config;
mod safety;
mod ship;
mod space_guard;
mod space_update;
mod spaces;
mod worktree;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use theodore_domain::{PrRef, RepoRef, Revision, Sha};
use theodore_ports::{
    BranchInfo, CommitLog, ConflictEntry, CredentialSource, DiffOptions, GitError, GitPort,
    GitResult, GrepHit, Identity, SpaceCommit, WorkingState, Worktree,
};

use process::GitProc;

/// Where a repo's remote lives, overriding the host's real URL (the sandbox
/// points every repo at a local `file://` bare repository).
pub type RemoteUrls = Arc<dyn Fn(&RepoRef) -> Option<String> + Send + Sync>;

/// How long a refresh of a repo's branches counts as current for
/// `fetch_pr`, which then fetches the PR ref alone.
const BRANCHES_FRESH_FOR: Duration = Duration::from_secs(10);

/// GitPort over the system `git` binary. Bare clones under `<root>/repos`,
/// one worktree per PR under `<root>/wt`.
pub struct GitCli {
    root: PathBuf,
    creds: Arc<dyn CredentialSource>,
    remote_urls: Option<RemoteUrls>,
    /// Bare repo → when its last successful full refresh started.
    refreshed: Mutex<HashMap<PathBuf, Instant>>,
    /// Clones and `refs/heads/*` fetches run so far (tests).
    full_fetches: AtomicU64,
}

impl GitCli {
    pub fn new(root: PathBuf, creds: Arc<dyn CredentialSource>) -> Self {
        Self {
            root,
            creds,
            remote_urls: None,
            refreshed: Mutex::new(HashMap::new()),
            full_fetches: AtomicU64::new(0),
        }
    }

    /// Every clone, fetch and push goes to `urls(repo)` instead of the
    /// host's own URL or the stored `origin`. Authoritative: a repo it
    /// returns `None` for has no remote at all — never a fallback to the
    /// real host.
    pub fn with_remote_urls(mut self, urls: RemoteUrls) -> Self {
        self.remote_urls = Some(urls);
        self
    }

    /// `Ok(None)`: no override, use the host URL / `origin` as usual.
    pub(crate) fn remote_override(&self, repo: &RepoRef) -> GitResult<Option<String>> {
        match &self.remote_urls {
            None => Ok(None),
            Some(f) => f(repo).map(Some).ok_or_else(|| {
                GitError::Parse(format!("no remote configured for {}", repo.slug()))
            }),
        }
    }

    pub(crate) fn ensure_askpass(&self) -> GitResult<PathBuf> {
        auth::ensure_askpass(&self.root)
    }

    pub(crate) fn token_for(&self, repo: &RepoRef) -> Option<String> {
        self.creds.token_for(repo)
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn repos_root(&self) -> PathBuf {
        self.root.join("repos")
    }

    pub(crate) fn wt_root(&self) -> PathBuf {
        self.root.join("wt")
    }

    pub(crate) fn meta_root(&self) -> PathBuf {
        self.wt_root().join(".meta")
    }

    /// `owner`/`name` become path components here, so both are validated
    /// first (`safety::validate_repo_ref`): a malicious owner/name (e.g.
    /// `..` or a leading `-`) must never reach the filesystem or a `git`
    /// argv unchecked.
    pub(crate) fn repo_dir(&self, repo: &RepoRef) -> GitResult<PathBuf> {
        safety::validate_repo_ref(repo)?;
        Ok(self
            .repos_root()
            .join(auth::host_dir(repo.host))
            .join(&repo.owner)
            .join(format!("{}.git", repo.name)))
    }

    pub(crate) fn wt_dirname(repo: &RepoRef, number: u64) -> GitResult<String> {
        safety::validate_repo_ref(repo)?;
        Ok(format!(
            "{}-{}-{}-{}",
            auth::host_dir(repo.host),
            repo.owner,
            repo.name,
            number
        ))
    }

    pub(crate) fn wt_path(&self, pr: &PrRef) -> GitResult<PathBuf> {
        Ok(self.wt_root().join(Self::wt_dirname(&pr.repo, pr.number)?))
    }

    /// `dir`'s parent made, so its lock key is stable from before the clone
    /// that creates `dir` (`locks::key_of`).
    fn repo_dir_for_lock(&self, repo: &RepoRef) -> GitResult<PathBuf> {
        let dir = self.repo_dir(repo)?;
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(dir)
    }

    /// When the last full refresh of `dir` that succeeded started.
    fn refreshed_at(&self, dir: &Path) -> Option<Instant> {
        self.refreshed
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(dir)
            .copied()
    }

    /// Bare clone under `repos/...`, or an in-place ref refresh if present.
    /// Serialized per repo (`locks`); a caller that waited while a refresh
    /// which started after its own call ran gets that one instead of
    /// fetching again.
    pub(crate) async fn ensure_repo_op(&self, repo: &RepoRef) -> GitResult<PathBuf> {
        let called_at = Instant::now();
        let dir = self.repo_dir_for_lock(repo)?;
        let _repo = locks::lock(&dir).await;
        if self.refreshed_at(&dir).is_some_and(|at| at >= called_at) && dir.join("HEAD").exists() {
            return Ok(dir);
        }
        self.refresh_locked(repo, &dir, None).await?;
        Ok(dir)
    }

    /// Under `dir`'s lock: clones it if missing, else one `fetch` of
    /// `refs/heads/*` (plus `extra`, a PR head refspec, in the same run).
    async fn refresh_locked(
        &self,
        repo: &RepoRef,
        dir: &Path,
        extra: Option<&str>,
    ) -> GitResult<()> {
        let askpass = self.ensure_askpass()?;
        let token = self.token_for(repo);
        let remote = self.remote_override(repo)?;

        if dir.join("HEAD").exists() {
            let started = Instant::now();
            let mut args = vec![
                "fetch",
                remote.as_deref().unwrap_or("origin"),
                "+refs/heads/*:refs/heads/*",
            ];
            args.extend(extra);
            args.extend(["--prune", "--tags"]);
            self.full_fetches.fetch_add(1, Ordering::Relaxed);
            GitProc {
                cwd: dir,
                askpass: &askpass,
                args: &args,
                token: token.as_deref(),
                stdin: None,
            }
            .run()
            .await?;
            self.refreshed
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(dir.to_path_buf(), started);
            return Ok(());
        }

        let started = Instant::now();
        self.clone_locked(repo, dir, &askpass, token.as_deref(), remote)
            .await?;
        self.refreshed
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(dir.to_path_buf(), started);
        Ok(())
    }

    async fn clone_locked(
        &self,
        repo: &RepoRef,
        dir: &Path,
        askpass: &Path,
        token: Option<&str>,
        remote: Option<String>,
    ) -> GitResult<()> {
        let url = match remote {
            Some(url) => url,
            None => auth::clone_url(repo.host, &repo.owner, &repo.name).ok_or_else(|| {
                GitError::Parse(format!("{:?} repos have no remote to clone", repo.host))
            })?,
        };
        let dir_str = dir
            .to_str()
            .ok_or_else(|| GitError::Parse("non-utf8 repo path".to_string()))?;
        let args = ["clone", "--bare", url.as_str(), dir_str];
        self.full_fetches.fetch_add(1, Ordering::Relaxed);
        GitProc {
            cwd: &self.root,
            askpass,
            args: &args,
            token,
            stdin: None,
        }
        .run()
        .await?;
        Ok(())
    }

    /// Fetches the PR/MR head into `refs/theodore/pr/<n>` and returns its
    /// sha. One `git fetch` either way: just the PR ref when the branches
    /// were refreshed a moment ago (opening a PR runs `ensure_repo` right
    /// before this), else the branches and the PR ref together — a moved
    /// base must be here for the diff too.
    pub(crate) async fn fetch_pr_op(&self, pr: &PrRef) -> GitResult<Sha> {
        let dir = self.repo_dir_for_lock(&pr.repo)?;
        let remote_ref = auth::remote_pr_ref(pr.repo.host, pr.number);
        let local_ref = auth::local_pr_ref(pr.number);
        let refspec = format!("+{remote_ref}:{local_ref}");
        let askpass = self.ensure_askpass()?;

        let _repo = locks::lock(&dir).await;
        if !dir.join("HEAD").exists() {
            // The clone brings the branches, then the PR ref alone below.
            self.refresh_locked(&pr.repo, &dir, None).await?;
        }
        let fresh = self
            .refreshed_at(&dir)
            .is_some_and(|at| at.elapsed() < BRANCHES_FRESH_FOR);
        if fresh {
            let token = self.token_for(&pr.repo);
            let remote = self.remote_override(&pr.repo)?;
            let fetch_args = [
                "fetch",
                remote.as_deref().unwrap_or("origin"),
                refspec.as_str(),
            ];
            GitProc {
                cwd: &dir,
                askpass: &askpass,
                args: &fetch_args,
                token: token.as_deref(),
                stdin: None,
            }
            .run()
            .await?;
        } else {
            self.refresh_locked(&pr.repo, &dir, Some(&refspec)).await?;
        }
        let rev_args = ["rev-parse", local_ref.as_str()];
        let out = GitProc {
            cwd: &dir,
            askpass: &askpass,
            args: &rev_args,
            token: None,
            stdin: None,
        }
        .run_text()
        .await?;
        Ok(Sha::new(out.trim().to_string()))
    }
}

#[async_trait]
impl GitPort for GitCli {
    async fn ensure_repo(&self, repo: &RepoRef) -> GitResult<PathBuf> {
        self.ensure_repo_op(repo).await
    }

    async fn fetch_pr(&self, pr: &PrRef) -> GitResult<Sha> {
        self.fetch_pr_op(pr).await
    }

    async fn worktree(&self, pr: &PrRef, head: &Sha) -> GitResult<Worktree> {
        self.worktree_op(pr, head).await
    }

    async fn remove_worktree(&self, pr: &PrRef) -> GitResult<()> {
        self.remove_worktree_op(pr).await
    }

    async fn gc_worktrees(&self, keep: &[PrRef]) -> GitResult<u32> {
        self.gc_worktrees_op(keep).await
    }

    async fn merge_base(&self, repo: &Path, a: &Sha, b: &Sha) -> GitResult<Sha> {
        let askpass = self.ensure_askpass()?;
        let args = ["merge-base", a.as_str(), b.as_str()];
        let out = GitProc {
            cwd: repo,
            askpass: &askpass,
            args: &args,
            token: None,
            stdin: None,
        }
        .run_text()
        .await?;
        Ok(Sha::new(out.trim().to_string()))
    }

    async fn diff(
        &self,
        repo: &Path,
        base: &Sha,
        head: &Sha,
        opts: &DiffOptions,
    ) -> GitResult<Revision> {
        self.diff_op(repo, base, head, opts).await
    }

    async fn show(&self, repo: &Path, rev: &Sha, path: &str) -> GitResult<Option<Vec<u8>>> {
        self.show_op(repo, rev, path).await
    }

    async fn cached_repo(&self, repo: &RepoRef) -> GitResult<Option<PathBuf>> {
        let dir = self.repo_dir(repo)?;
        Ok(dir.join("HEAD").exists().then_some(dir))
    }

    async fn has_commit(&self, repo: &Path, sha: &Sha) -> GitResult<bool> {
        // Only a hex object id: never anything `cat-file` could read as a
        // flag or a ref expression.
        let s = sha.as_str();
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(false);
        }
        let askpass = self.ensure_askpass()?;
        let object = format!("{s}^{{commit}}");
        let out = GitProc {
            cwd: repo,
            askpass: &askpass,
            args: &["cat-file", "-e", &object],
            token: None,
            stdin: None,
        }
        .run_raw()
        .await?;
        Ok(out.status == 0)
    }

    async fn commit_and_push(
        &self,
        worktree: &Path,
        repo: &RepoRef,
        branch: &str,
        message: &str,
    ) -> GitResult<Sha> {
        self.commit_and_push_op(worktree, repo, branch, message)
            .await
    }

    async fn ensure_local_mirror(&self, source: &Path, repo: &RepoRef) -> GitResult<PathBuf> {
        self.ensure_local_mirror_op(source, repo).await
    }

    async fn is_repo(&self, path: &Path) -> GitResult<bool> {
        self.is_repo_op(path).await
    }

    async fn remove_repo(&self, repo: &RepoRef) -> GitResult<()> {
        self.remove_repo_op(repo).await
    }

    async fn default_branch(&self, repo: &Path) -> GitResult<Option<String>> {
        self.default_branch_op(repo).await
    }

    async fn resolve_ref(&self, repo: &Path, refname: &str) -> GitResult<Sha> {
        self.resolve_ref_op(repo, refname).await
    }

    async fn commit(&self, repo: &Path, sha: &Sha) -> GitResult<CommitLog> {
        self.commit_op(repo, sha).await
    }

    async fn commit_log(&self, repo: &Path, base: &Sha, head: &Sha) -> GitResult<Vec<CommitLog>> {
        self.commit_log_op(repo, base, head).await
    }

    async fn co_change_commits(
        &self,
        repo: &Path,
        rev: &Sha,
        paths: &[String],
        max_commits: usize,
        max_files: usize,
    ) -> GitResult<Vec<Vec<String>>> {
        self.co_change_commits_op(repo, rev, paths, max_commits, max_files)
            .await
    }

    async fn list_branches(&self, repo: &Path, default_branch: &str) -> GitResult<Vec<BranchInfo>> {
        self.list_branches_op(repo, default_branch).await
    }

    async fn grep(&self, worktree: &Path, pattern: &str) -> GitResult<Vec<GrepHit>> {
        self.grep_op(worktree, pattern).await
    }
    async fn merge_tree_conflicts(
        &self,
        repo: &Path,
        base: &Sha,
        head: &Sha,
    ) -> GitResult<Vec<ConflictEntry>> {
        self.merge_tree_conflicts_op(repo, base, head).await
    }

    async fn working_state(&self, folder: &Path) -> GitResult<WorkingState> {
        self.working_state_op(folder).await
    }

    async fn space_worktree(&self, repo: &Path, space: &str, base: &Sha) -> GitResult<Worktree> {
        self.space_worktree_op(repo, space, base).await
    }

    async fn space_checkpoint(
        &self,
        worktree: &Path,
        space: &str,
        message: &str,
    ) -> GitResult<Sha> {
        self.space_checkpoint_op(worktree, space, message).await
    }

    async fn space_reset(&self, worktree: &Path, space: &str, to: &Sha) -> GitResult<()> {
        self.space_reset_op(worktree, space, to).await
    }

    async fn worktree_busy(
        &self,
        worktree: &Path,
    ) -> GitResult<Option<theodore_ports::WorktreeBusy>> {
        self.worktree_busy_op(worktree).await
    }

    async fn space_snapshot(&self, worktree: &Path) -> GitResult<Sha> {
        self.space_snapshot_op(worktree).await
    }
    async fn space_merge(
        &self,
        worktree: &Path,
        space: &str,
        tip: &Sha,
        message: &str,
    ) -> GitResult<theodore_ports::SpaceMerge> {
        self.space_merge_op(worktree, space, tip, message).await
    }

    async fn space_merge_finish(
        &self,
        worktree: &Path,
        space: &str,
        tip: &Sha,
        message: &str,
    ) -> GitResult<theodore_ports::SpaceMerge> {
        self.space_merge_finish_op(worktree, space, tip, message)
            .await
    }

    async fn space_merge_abort(&self, worktree: &Path, space: &str, to: &Sha) -> GitResult<()> {
        self.space_merge_abort_op(worktree, space, to).await
    }

    async fn remove_space_worktree(&self, repo: &Path, space: &str) -> GitResult<()> {
        self.remove_space_worktree_op(repo, space).await
    }

    async fn user_identity(&self, repo: &Path) -> GitResult<Option<Identity>> {
        self.user_identity_op(repo).await
    }

    async fn space_commit(&self, repo: &Path, space: &str, c: &SpaceCommit) -> GitResult<Sha> {
        self.space_commit_op(repo, space, c).await
    }

    async fn remote_branch(
        &self,
        repo: &Path,
        repo_ref: &RepoRef,
        branch: &str,
    ) -> GitResult<Option<Sha>> {
        self.remote_branch_op(repo, repo_ref, branch).await
    }

    async fn push_branch(
        &self,
        repo: &Path,
        repo_ref: &RepoRef,
        sha: &Sha,
        branch: &str,
    ) -> GitResult<()> {
        self.push_branch_op(repo, repo_ref, sha, branch).await
    }

    async fn merge_into_folder(
        &self,
        folder: &Path,
        repo: &Path,
        space: &str,
        message: &str,
    ) -> GitResult<Sha> {
        self.merge_into_folder_op(folder, repo, space, message)
            .await
    }

    async fn write_folder_branch(
        &self,
        folder: &Path,
        repo: &Path,
        space: &str,
        branch: &str,
    ) -> GitResult<()> {
        self.write_folder_branch_op(folder, repo, space, branch)
            .await
    }

    async fn mirror_sync(
        &self,
        folder: &Path,
        repo: &Path,
        space: &str,
        to: &Sha,
        from: &Sha,
    ) -> GitResult<Vec<String>> {
        self.mirror_sync_op(folder, repo, space, to, from).await
    }

    async fn mirror_edited(
        &self,
        folder: &Path,
        repo: &Path,
        space: &str,
        to: &Sha,
        paths: &[String],
    ) -> GitResult<Vec<String>> {
        self.mirror_edited_op(folder, repo, space, to, paths).await
    }

    async fn mirror_restore(
        &self,
        folder: &Path,
        from: &Sha,
        paths: &[String],
    ) -> GitResult<Vec<String>> {
        self.mirror_restore_op(folder, from, paths).await
    }

    async fn commit_file(
        &self,
        repo: &Path,
        parent: &Sha,
        path: &str,
        content: &[u8],
        message: &str,
        author: &Identity,
    ) -> GitResult<Sha> {
        self.commit_file_op(repo, parent, path, content, message, author)
            .await
    }

    async fn dirty_paths(&self, folder: &Path) -> GitResult<Vec<String>> {
        self.dirty_paths_op(folder).await
    }

    async fn commit_paths(&self, folder: &Path, paths: &[String], message: &str) -> GitResult<Sha> {
        self.commit_paths_op(folder, paths, message).await
    }
}

/// Parse a standalone patch (e.g. `git diff > f.patch`) without a repository:
/// counts come from the hunks and no `.gitattributes` apply.
pub fn parse_patch(
    text: &str,
    base: &theodore_domain::Sha,
    head: &theodore_domain::Sha,
    max_file_lines: u32,
) -> theodore_domain::Revision {
    let attrs = generated::GitAttributes::parse("");
    let mut files: Vec<_> = parse::parse_unified_diff(text, &[], &attrs, max_file_lines)
        .into_iter()
        .map(|p| p.file)
        .collect();
    theodore_domain::interdiff::dedupe_hunk_ids(&mut files);
    theodore_domain::Revision {
        base: base.clone(),
        head: head.clone(),
        files,
    }
}

#[cfg(test)]
mod fetch_tests {
    use super::*;
    use std::process::Command;
    use theodore_domain::HostKind;

    struct NoAuth;
    impl CredentialSource for NoAuth {
        fn token_for(&self, _repo: &RepoRef) -> Option<String> {
            None
        }
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .expect("git installed");
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn repo() -> RepoRef {
        RepoRef {
            host: HostKind::Github,
            owner: "acme".into(),
            name: "widget".into(),
        }
    }

    fn pr() -> PrRef {
        PrRef {
            repo: repo(),
            number: 1,
            host_id: None,
        }
    }

    /// `work` (a checkout) pushed to `remote.git`, which has a PR ref.
    fn remote(tmp: &Path) -> (PathBuf, PathBuf) {
        let work = tmp.join("work");
        std::fs::create_dir_all(&work).unwrap();
        git(&work, &["init", "-q", "-b", "main"]);
        git(&work, &["config", "user.email", "a@b.com"]);
        git(&work, &["config", "user.name", "test"]);
        std::fs::write(work.join("a.txt"), "one\n").unwrap();
        git(&work, &["add", "-A"]);
        git(&work, &["commit", "-q", "-m", "base"]);
        let bare = tmp.join("remote.git");
        git(
            tmp,
            &["clone", "-q", "--bare", "work", bare.to_str().unwrap()],
        );
        let head = git(&work, &["rev-parse", "HEAD"]);
        git(&bare, &["update-ref", "refs/pull/1/head", &head]);
        (work, bare)
    }

    fn cli(root: PathBuf, bare: &Path) -> GitCli {
        let url = format!("file://{}", bare.display());
        GitCli::new(root, Arc::new(NoAuth))
            .with_remote_urls(Arc::new(move |_r: &RepoRef| Some(url.clone())))
    }

    fn full_fetches(cli: &GitCli) -> u64 {
        cli.full_fetches.load(Ordering::Relaxed)
    }

    #[tokio::test]
    async fn opening_a_pr_refreshes_the_branches_once() {
        let tmp = tempfile::tempdir().unwrap();
        let (_work, bare) = remote(tmp.path());
        let cli = cli(tmp.path().join("root"), &bare);
        cli.ensure_repo_op(&repo()).await.expect("clone");
        let before = full_fetches(&cli);

        // What `open_ref` runs: ensure_repo, then fetch_pr.
        cli.ensure_repo_op(&repo()).await.expect("refresh");
        let sha = cli.fetch_pr_op(&pr()).await.expect("fetch pr");

        assert_eq!(full_fetches(&cli) - before, 1);
        assert_eq!(sha.as_str(), git(&bare, &["rev-parse", "refs/pull/1/head"]));
    }

    #[tokio::test]
    async fn fetch_pr_on_its_own_still_brings_a_moved_base() {
        let tmp = tempfile::tempdir().unwrap();
        let (work, bare) = remote(tmp.path());
        let root = tmp.path().join("root");
        cli(root.clone(), &bare)
            .ensure_repo_op(&repo())
            .await
            .expect("clone");
        std::fs::write(work.join("a.txt"), "two\n").unwrap();
        git(&work, &["commit", "-qam", "base moved"]);
        git(&work, &["push", "-q", bare.to_str().unwrap(), "main"]);
        let moved = git(&work, &["rev-parse", "HEAD"]);

        // A later session (no refresh on record): the poller's push
        // handler calls fetch_pr alone.
        let later = cli(root, &bare);
        let dir = later.repo_dir(&repo()).unwrap();
        later.fetch_pr_op(&pr()).await.expect("fetch pr");

        assert_eq!(full_fetches(&later), 1, "branches and PR ref in one fetch");
        assert_eq!(git(&dir, &["rev-parse", "refs/heads/main"]), moved);
    }

    #[tokio::test]
    async fn callers_waiting_on_a_refresh_share_the_next_one() {
        let tmp = tempfile::tempdir().unwrap();
        let (_work, bare) = remote(tmp.path());
        let cli = Arc::new(cli(tmp.path().join("root"), &bare));
        cli.ensure_repo_op(&repo()).await.expect("clone");
        let before = full_fetches(&cli);

        let calls: Vec<_> = (0..5)
            .map(|_| {
                let cli = cli.clone();
                tokio::spawn(async move { cli.ensure_repo_op(&repo()).await.map(|_| ()) })
            })
            .collect();
        for c in calls {
            c.await.unwrap().expect("refresh");
        }

        let ran = full_fetches(&cli) - before;
        assert!((1..=2).contains(&ran), "{ran} fetches for 5 callers");
    }

    #[tokio::test]
    async fn an_unreachable_remote_is_a_network_error() {
        let tmp = tempfile::tempdir().unwrap();
        // Nothing listens on port 9 (discard) here: connection refused.
        let cli = GitCli::new(tmp.path().join("root"), Arc::new(NoAuth)).with_remote_urls(
            Arc::new(|_r: &RepoRef| Some("http://127.0.0.1:9/acme/widget.git".into())),
        );
        let err = cli.ensure_repo_op(&repo()).await.unwrap_err();
        assert!(matches!(err, GitError::Network(_)), "{err:?}");
        assert_eq!(err.code(), "git.network");
    }
}
