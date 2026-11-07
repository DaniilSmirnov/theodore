//! End-to-end exercise of `GitCli` against real temp repos and the real
//! `git` binary: no network, no GitHub — a local path stands in for
//! `origin`, and a hand-written `refs/pull/<n>/head` stands in for a PR.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use tempfile::TempDir;
use theodore_domain::{FileStatus, HostKind, PrRef, RepoRef};
use theodore_git::GitCli;
use theodore_ports::{CredentialSource, DiffOptions, GitPort};

struct NoAuth;
impl CredentialSource for NoAuth {
    fn token_for(&self, _repo: &RepoRef) -> Option<String> {
        None
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("git installed");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("git installed");
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A local "origin" with a `main` branch and a `refs/pull/1/head` ref ahead
/// of it by one commit, plus the bare clone Theodore would have made of it
/// (hand-placed at the exact path `GitCli` computes, so `ensure_repo`'s
/// "already cloned" refresh path is what gets exercised, not the
/// GitHub-only clone URL).
struct Fixture {
    _tmp: TempDir,
    root: std::path::PathBuf,
    repo: RepoRef,
    base_sha: String,
    head_sha: String,
}

fn setup() -> Fixture {
    let tmp = TempDir::new().unwrap();
    let origin = tmp.path().join("origin");
    std::fs::create_dir_all(&origin).unwrap();
    git(&origin, &["init", "-q", "-b", "main"]);
    git(&origin, &["config", "user.email", "a@b.com"]);
    git(&origin, &["config", "user.name", "test"]);
    std::fs::write(origin.join("a.txt"), "line1\nline2\nline3\n").unwrap();
    std::fs::write(origin.join("keep.txt"), "hello\n").unwrap();
    git(&origin, &["add", "-A"]);
    git(&origin, &["commit", "-q", "-m", "base"]);
    let base_sha = git_out(&origin, &["rev-parse", "HEAD"]);

    std::fs::write(origin.join("a.txt"), "line1\nline2 CHANGED\nline3\n").unwrap();
    git(&origin, &["mv", "keep.txt", "moved.txt"]);
    git(&origin, &["add", "-A"]);
    git(&origin, &["commit", "-q", "-m", "pr change"]);
    let head_sha = git_out(&origin, &["rev-parse", "HEAD"]);

    // Fake PR head ref — stands in for GitHub's `refs/pull/<n>/head`.
    git(&origin, &["update-ref", "refs/pull/1/head", &head_sha]);
    // Move main back to base so the PR's head is genuinely ahead of it.
    git(&origin, &["update-ref", "refs/heads/main", &base_sha]);

    let root = tmp.path().join("theodore-root");
    std::fs::create_dir_all(&root).unwrap();
    let repo = RepoRef {
        host: HostKind::Github,
        owner: "acme".to_string(),
        name: "widget".to_string(),
    };
    let repo_dir = root
        .join("repos")
        .join("github")
        .join("acme")
        .join("widget.git");
    std::fs::create_dir_all(repo_dir.parent().unwrap()).unwrap();
    git(
        tmp.path(),
        &[
            "clone",
            "-q",
            "--bare",
            origin.to_str().unwrap(),
            repo_dir.to_str().unwrap(),
        ],
    );

    Fixture {
        _tmp: tmp,
        root,
        repo,
        base_sha,
        head_sha,
    }
}

impl Fixture {
    fn cli(&self) -> GitCli {
        GitCli::new(self.root.clone(), Arc::new(NoAuth))
    }
    fn repo_dir(&self) -> std::path::PathBuf {
        self.root
            .join("repos")
            .join("github")
            .join("acme")
            .join("widget.git")
    }
    fn pr(&self, number: u64) -> PrRef {
        PrRef {
            repo: self.repo.clone(),
            number,
            host_id: None,
        }
    }
}

#[tokio::test]
async fn ensure_repo_refreshes_an_existing_clone() {
    let fx = setup();
    let cli = fx.cli();
    let dir = cli.ensure_repo(&fx.repo).await.unwrap();
    assert_eq!(dir, fx.repo_dir());
    // "main" was wound back to base_sha in the origin; a refresh must see it.
    let main_sha = git_out(&dir, &["rev-parse", "refs/heads/main"]);
    assert_eq!(main_sha, fx.base_sha);
}

#[tokio::test]
async fn fetch_pr_resolves_the_head_sha() {
    let fx = setup();
    let cli = fx.cli();
    let sha = cli.fetch_pr(&fx.pr(1)).await.unwrap();
    assert_eq!(sha.as_str(), fx.head_sha);
}

#[tokio::test]
async fn worktree_checks_out_the_pr_head_and_can_be_moved_and_removed() {
    let fx = setup();
    let cli = fx.cli();
    let head = cli.fetch_pr(&fx.pr(1)).await.unwrap();

    let wt = cli.worktree(&fx.pr(1), &head).await.unwrap();
    assert!(wt.path.join("a.txt").exists());
    assert!(wt.path.join("moved.txt").exists());
    assert!(!wt.path.join("keep.txt").exists());
    assert_eq!(
        std::fs::read_to_string(wt.path.join("a.txt")).unwrap(),
        "line1\nline2 CHANGED\nline3\n"
    );

    // Moving to the same head is idempotent and doesn't error.
    let wt2 = cli.worktree(&fx.pr(1), &head).await.unwrap();
    assert_eq!(wt2.path, wt.path);

    cli.remove_worktree(&fx.pr(1)).await.unwrap();
    assert!(!wt.path.exists());
}

#[tokio::test]
async fn gc_worktrees_drops_everything_not_kept() {
    let fx = setup();
    let cli = fx.cli();
    let head = cli.fetch_pr(&fx.pr(1)).await.unwrap();
    let wt1 = cli.worktree(&fx.pr(1), &head).await.unwrap();

    // A second PR number, reusing the same head for simplicity.
    let pr2 = fx.pr(2);
    git(
        &fx.repo_dir(),
        &["update-ref", "refs/theodore/pr/2", &fx.head_sha],
    );
    let wt2 = cli.worktree(&pr2, &head).await.unwrap();

    assert!(wt1.path.exists());
    assert!(wt2.path.exists());

    let removed = cli.gc_worktrees(std::slice::from_ref(&pr2)).await.unwrap();
    assert_eq!(removed, 1);
    assert!(!wt1.path.exists());
    assert!(wt2.path.exists());
}

#[tokio::test]
async fn diff_parses_a_real_git_diff_end_to_end() {
    let fx = setup();
    let cli = fx.cli();
    let repo_dir = fx.repo_dir();
    let base = theodore_domain::Sha::new(fx.base_sha.clone());
    let head = theodore_domain::Sha::new(fx.head_sha.clone());

    let rev = cli
        .diff(&repo_dir, &base, &head, &DiffOptions::default())
        .await
        .unwrap();
    assert_eq!(rev.files.len(), 2);

    let a = rev.files.iter().find(|f| f.path == "a.txt").unwrap();
    assert_eq!(a.status, FileStatus::Modified);
    assert_eq!(a.additions, 1);
    assert_eq!(a.deletions, 1);
    assert_eq!(a.hunks.len(), 1);
    assert!(!a.hunks[0].id.as_str().is_empty());

    let moved = rev.files.iter().find(|f| f.path == "moved.txt").unwrap();
    assert_eq!(moved.status, FileStatus::Renamed);
    assert_eq!(moved.old_path.as_deref(), Some("keep.txt"));
}

/// A stale sha (a last-seen head force-pushed away) is a typed "commit not
/// found", not git's "Invalid symmetric difference expression".
#[tokio::test]
async fn diff_with_a_commit_not_in_the_repo_is_commit_not_found() {
    let fx = setup();
    let cli = fx.cli();
    let repo_dir = fx.repo_dir();
    let base = theodore_domain::Sha::new(fx.base_sha.clone());
    let gone = theodore_domain::Sha::new("b6cef3e".repeat(5) + "b6cef");

    let err = cli
        .diff(&repo_dir, &base, &gone, &DiffOptions::default())
        .await
        .unwrap_err();
    assert!(
        matches!(&err, theodore_ports::GitError::MissingCommit(s) if *s == gone.as_str()),
        "{err:?}"
    );
    let err = cli
        .diff(&repo_dir, &gone, &base, &DiffOptions::default())
        .await
        .unwrap_err();
    assert!(
        matches!(err, theodore_ports::GitError::MissingCommit(_)),
        "{err:?}"
    );
}

#[tokio::test]
async fn show_reads_file_content_and_none_for_missing_path() {
    let fx = setup();
    let cli = fx.cli();
    let repo_dir = fx.repo_dir();
    let head = theodore_domain::Sha::new(fx.head_sha.clone());

    let content = cli.show(&repo_dir, &head, "a.txt").await.unwrap();
    assert_eq!(content.unwrap(), b"line1\nline2 CHANGED\nline3\n");

    let missing = cli
        .show(&repo_dir, &head, "does-not-exist.txt")
        .await
        .unwrap();
    assert_eq!(missing, None);
}

#[tokio::test]
async fn merge_base_finds_the_common_ancestor() {
    let fx = setup();
    let cli = fx.cli();
    let repo_dir = fx.repo_dir();
    let base = theodore_domain::Sha::new(fx.base_sha.clone());
    let head = theodore_domain::Sha::new(fx.head_sha.clone());

    let mb = cli.merge_base(&repo_dir, &base, &head).await.unwrap();
    assert_eq!(mb.as_str(), fx.base_sha);
}

#[tokio::test]
async fn cached_repo_and_has_commit_read_the_clone_without_fetching() {
    let fx = setup();
    let cli = fx.cli();
    let dir = cli.cached_repo(&fx.repo).await.unwrap().expect("cloned");
    assert_eq!(dir, fx.repo_dir());
    let other = RepoRef {
        name: "absent".into(),
        ..fx.repo.clone()
    };
    assert!(cli.cached_repo(&other).await.unwrap().is_none());

    let base = theodore_domain::Sha::new(fx.base_sha.clone());
    let head = theodore_domain::Sha::new(fx.head_sha.clone());
    cli.fetch_pr(&fx.pr(1)).await.unwrap();
    assert!(cli.has_commit(&dir, &base).await.unwrap());
    assert!(cli.has_commit(&dir, &head).await.unwrap());

    let absent = theodore_domain::Sha::new("0123456789abcdef0123456789abcdef01234567");
    assert!(!cli.has_commit(&dir, &absent).await.unwrap());
    let flag = theodore_domain::Sha::new("--all");
    assert!(!cli.has_commit(&dir, &flag).await.unwrap());
}
