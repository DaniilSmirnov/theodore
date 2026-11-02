//! Space worktree safety (docs/spaces.md §5, §6) on real git: a space dir
//! whose `.git` is gone never lets git walk up into a parent repository,
//! and a checkpoint or Undo waits while a git operation is unfinished.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;

use tempfile::TempDir;
use theodore_domain::{HostKind, RepoRef, Sha};
use theodore_git::GitCli;
use theodore_ports::{CredentialSource, GitError, GitPort, WorktreeBusy};

struct NoAuth;
impl CredentialSource for NoAuth {
    fn token_for(&self, _repo: &RepoRef) -> Option<String> {
        None
    }
}

/// No user hooks, signing or identity from the machine running the tests.
const PLAIN: [&str; 8] = [
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "commit.gpgsign=false",
    "-c",
    "user.name=test",
    "-c",
    "user.email=a@b.com",
];

fn git_out(dir: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .current_dir(dir)
        .args(PLAIN)
        .args(args)
        .output()
        .expect("git installed")
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = git_out(dir, args);
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit_all(dir: &Path, message: &str) -> String {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
    git(dir, &["rev-parse", "HEAD"])
}

fn init_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a.txt"), "one\n").unwrap();
    commit_all(dir, "base");
}

fn repo() -> RepoRef {
    RepoRef {
        host: HostKind::Github,
        owner: "acme".into(),
        name: "widget".into(),
    }
}

struct Host {
    tmp: TempDir,
    git: GitCli,
    cache: PathBuf,
    base: Sha,
}

/// Theodore's root inside `home`, which may itself be a git repository.
async fn host_in(tmp: TempDir, home: &Path) -> Host {
    let work = tmp.path().join("work");
    init_repo(&work);
    let bare = tmp.path().join("remote.git");
    git(
        tmp.path(),
        &["clone", "-q", "--bare", "work", bare.to_str().unwrap()],
    );
    let url = format!("file://{}", bare.display());
    let cli = GitCli::new(home.join(".theodore"), Arc::new(NoAuth))
        .with_remote_urls(Arc::new(move |_r: &RepoRef| Some(url.clone())));
    let cache = cli.ensure_repo(&repo()).await.expect("clone");
    let base = cli.resolve_ref(&cache, "main").await.unwrap();
    Host {
        tmp,
        git: cli,
        cache,
        base,
    }
}

async fn host() -> Host {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    host_in(tmp, &home).await
}

/// A git-controlled home holds Theodore's root; the space's `.git` file is
/// deleted. Without the guard, `add -A` / `reset --hard` / `clean` would
/// run on the home repository.
#[tokio::test]
async fn a_space_dir_without_git_never_touches_the_parent_repo() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    init_repo(&home);
    // Theodore's own files change under it; the user's must not.
    std::fs::write(home.join(".git/info/exclude"), ".theodore/\n").unwrap();
    std::fs::write(home.join("notes.txt"), "mine, untracked\n").unwrap();
    std::fs::write(home.join("a.txt"), "one\nedited, not staged\n").unwrap();
    let h = host_in(tmp, &home).await;
    let home_head = git(&home, &["rev-parse", "HEAD"]);
    let home_status = git(&home, &["status", "--porcelain", "--untracked-files=all"]);

    let wt = h.git.space_worktree(&h.cache, "s1", &h.base).await.unwrap();
    std::fs::write(wt.path.join("a.txt"), "one\ntwo\n").unwrap();
    let turn1 = h
        .git
        .space_checkpoint(&wt.path, "s1", "turn 1")
        .await
        .unwrap();
    std::fs::write(wt.path.join("draft.txt"), "by hand\n").unwrap();
    std::fs::remove_file(wt.path.join(".git")).unwrap();
    // Sanity: plain git from there does resolve the home repository.
    assert!(
        git_out(&wt.path, &["rev-parse", "--show-toplevel"])
            .status
            .success()
    );

    let err = h
        .git
        .space_checkpoint(&wt.path, "s1", "turn 2")
        .await
        .unwrap_err();
    assert!(matches!(err, GitError::NotAWorktree(_)), "{err:?}");
    let err = h
        .git
        .space_reset(&wt.path, "s1", &h.base)
        .await
        .unwrap_err();
    assert!(matches!(err, GitError::NotAWorktree(_)), "{err:?}");
    let err = h.git.worktree_busy(&wt.path).await.unwrap_err();
    assert!(matches!(err, GitError::NotAWorktree(_)), "{err:?}");

    // The home repository is exactly as it was: no commit, nothing staged
    // or cleaned, and the space's files are still there.
    assert_eq!(git(&home, &["rev-parse", "HEAD"]), home_head);
    assert_eq!(
        git(&home, &["status", "--porcelain", "--untracked-files=all"]),
        home_status
    );
    assert!(home.join("notes.txt").exists());
    assert!(wt.path.join("draft.txt").exists());
    assert_eq!(
        git(&h.cache, &["rev-parse", "refs/theodore/spaces/s1"]),
        turn1.as_str()
    );

    // Recovery: the dir is moved aside, never deleted, and the worktree
    // comes back from the checkpoint ref.
    let again = h.git.space_worktree(&h.cache, "s1", &h.base).await.unwrap();
    assert_eq!(again.path, wt.path);
    assert_eq!(again.head, turn1);
    assert!(again.path.join(".git").is_file());
    assert!(!again.path.join("draft.txt").exists());
    let aside = home.join(".theodore/spaces/.moved-aside");
    let kept: Vec<PathBuf> = std::fs::read_dir(&aside)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(kept.len(), 1);
    assert!(kept[0].join("draft.txt").exists());
    assert_eq!(git(&home, &["rev-parse", "HEAD"]), home_head);
    let _ = &h.tmp;
}

/// A `git init` in place of the worktree's `.git` file is not the space's.
#[tokio::test]
async fn a_repository_created_in_the_space_dir_is_refused() {
    let h = host().await;
    let wt = h.git.space_worktree(&h.cache, "s1", &h.base).await.unwrap();
    std::fs::remove_file(wt.path.join(".git")).unwrap();
    git(&wt.path, &["init", "-q"]);
    let err = h
        .git
        .space_checkpoint(&wt.path, "s1", "turn 1")
        .await
        .unwrap_err();
    assert!(matches!(err, GitError::NotAWorktree(_)), "{err:?}");
}

/// Archive on a dir that isn't a worktree: moved aside, not `rm -rf`.
#[tokio::test]
async fn removing_a_space_dir_that_is_not_a_worktree_moves_it_aside() {
    let h = host().await;
    let wt = h.git.space_worktree(&h.cache, "s1", &h.base).await.unwrap();
    std::fs::remove_file(wt.path.join(".git")).unwrap();
    std::fs::write(wt.path.join("keep.txt"), "x\n").unwrap();
    h.git
        .remove_space_worktree(&h.cache, "s1")
        .await
        .expect("remove");
    assert!(!wt.path.exists());
    let aside = wt.path.parent().unwrap().join(".moved-aside");
    let kept: Vec<PathBuf> = std::fs::read_dir(&aside)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(kept.len(), 1);
    assert!(kept[0].join("keep.txt").exists());
}

/// A conflicting cherry-pick, merge or rebase left in the worktree: no
/// checkpoint (conflict markers stay out of the history), no Undo.
#[tokio::test]
async fn an_unfinished_git_operation_skips_the_checkpoint_and_refuses_undo() {
    let h = host().await;
    let wt = h.git.space_worktree(&h.cache, "s1", &h.base).await.unwrap();
    let p = &wt.path;
    assert_eq!(h.git.worktree_busy(p).await.unwrap(), None);
    std::fs::write(p.join("a.txt"), "one\nA\n").unwrap();
    let turn1 = h.git.space_checkpoint(p, "s1", "turn 1").await.unwrap();
    // A commit off the base that conflicts with turn 1.
    git(p, &["checkout", "-q", "--detach", h.base.as_str()]);
    std::fs::write(p.join("a.txt"), "one\nB\n").unwrap();
    let other = commit_all(p, "other");
    git(p, &["checkout", "-q", "--detach", turn1.as_str()]);

    let cases: [(&[&str], &[&str], WorktreeBusy); 3] = [
        (
            &["cherry-pick", &other],
            &["cherry-pick", "--abort"],
            WorktreeBusy::CherryPick,
        ),
        (
            &["merge", "--no-edit", &other],
            &["merge", "--abort"],
            WorktreeBusy::Merge,
        ),
        (
            &["rebase", &other],
            &["rebase", "--abort"],
            WorktreeBusy::Rebase,
        ),
    ];
    for (start, abort, busy) in cases {
        assert!(!git_out(p, start).status.success(), "{start:?} conflicts");
        assert_eq!(h.git.worktree_busy(p).await.unwrap(), Some(busy));
        let err = h.git.space_checkpoint(p, "s1", "turn 2").await.unwrap_err();
        assert!(matches!(err, GitError::Busy(b) if b == busy), "{err:?}");
        let err = h.git.space_reset(p, "s1", &h.base).await.unwrap_err();
        assert!(matches!(err, GitError::Busy(b) if b == busy), "{err:?}");
        assert_eq!(
            git(&h.cache, &["rev-parse", "refs/theodore/spaces/s1"]),
            turn1.as_str()
        );
        git(p, abort);
        assert_eq!(h.git.worktree_busy(p).await.unwrap(), None);
    }

    // Unmerged entries alone (a conflicting `stash pop`) count too.
    std::fs::write(p.join("a.txt"), "one\nC\n").unwrap();
    git(p, &["stash", "push", "-q"]);
    std::fs::write(p.join("a.txt"), "one\nD\n").unwrap();
    commit_all(p, "d");
    assert!(!git_out(p, &["stash", "pop", "-q"]).status.success());
    assert_eq!(
        h.git.worktree_busy(p).await.unwrap(),
        Some(WorktreeBusy::Conflicts)
    );
    git(p, &["reset", "-q", "--hard"]);
    assert_eq!(h.git.worktree_busy(p).await.unwrap(), None);
    h.git
        .space_checkpoint(p, "s1", "turn 2")
        .await
        .expect("checkpoint once finished");
}
