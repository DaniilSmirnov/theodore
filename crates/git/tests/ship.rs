//! Shipping a space (docs/spaces.md §6) on real git: the squashed commit,
//! pushing it to a bare "remote" without force, replaying new turns onto a
//! branch that moved, and the two local-folder end states.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use tempfile::TempDir;
use theodore_domain::{HostKind, RepoRef, Sha};
use theodore_git::GitCli;
use theodore_ports::{CredentialSource, GitError, GitPort, Identity, SpaceCommit, shipped_ref};

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
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit_all(dir: &Path, message: &str) {
    git(dir, &["add", "-A"]);
    git(
        dir,
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            message,
        ],
    );
}

fn init_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "a@b.com"]);
    git(dir, &["config", "user.name", "test"]);
    std::fs::write(dir.join("a.txt"), "one\n").unwrap();
    std::fs::write(dir.join("b.txt"), "bee\n").unwrap();
    commit_all(dir, "base");
}

fn me() -> Identity {
    Identity {
        name: "Ada Lovelace".into(),
        email: "ada@example.com".into(),
    }
}

fn repo() -> RepoRef {
    RepoRef {
        host: HostKind::Github,
        owner: "acme".into(),
        name: "widget".into(),
    }
}

struct Host {
    _tmp: TempDir,
    git: GitCli,
    bare: PathBuf,
    cache: PathBuf,
    base: Sha,
}

/// A host project: a bare "remote" with `main`, and Theodore's cache of it.
async fn host() -> Host {
    let tmp = TempDir::new().unwrap();
    let work = tmp.path().join("work");
    init_repo(&work);
    let bare = tmp.path().join("remote.git");
    git(
        tmp.path(),
        &["clone", "-q", "--bare", "work", bare.to_str().unwrap()],
    );
    let url = format!("file://{}", bare.display());
    let cli = GitCli::new(tmp.path().join("root"), Arc::new(NoAuth))
        .with_remote_urls(Arc::new(move |_r: &RepoRef| Some(url.clone())));
    let cache = cli.ensure_repo(&repo()).await.expect("clone");
    let base = cli.resolve_ref(&cache, "main").await.unwrap();
    Host {
        _tmp: tmp,
        git: cli,
        bare,
        cache,
        base,
    }
}

/// Two agent turns in space `s1`: returns the last checkpoint.
async fn two_turns(h: &Host, space: &str) -> (PathBuf, Sha) {
    let wt = h
        .git
        .space_worktree(&h.cache, space, &h.base)
        .await
        .unwrap();
    std::fs::write(wt.path.join("a.txt"), "one\ntwo\n").unwrap();
    h.git
        .space_checkpoint(&wt.path, space, "turn 1")
        .await
        .unwrap();
    std::fs::write(wt.path.join("c.txt"), "new\n").unwrap();
    let head = h
        .git
        .space_checkpoint(&wt.path, space, "turn 2")
        .await
        .unwrap();
    (wt.path, head)
}

fn squash(head: &Sha, parent: &Sha, message: &str) -> SpaceCommit {
    SpaceCommit {
        head: head.clone(),
        parent: parent.clone(),
        replay_from: None,
        merge_parent: None,
        message: message.into(),
        author: me(),
    }
}

#[tokio::test]
async fn checkpoints_squash_into_one_commit_by_the_user() {
    let h = host().await;
    let (_wt, head) = two_turns(&h, "s1").await;

    let sha = h
        .git
        .space_commit(&h.cache, "s1", &squash(&head, &h.base, "Retry on 429s"))
        .await
        .unwrap();

    assert_eq!(
        git(&h.cache, &["rev-parse", &shipped_ref("s1")]),
        sha.as_str()
    );
    assert_eq!(
        git(&h.cache, &["rev-parse", &format!("{sha}^@")]),
        h.base.as_str(),
        "one parent: the base"
    );
    assert_eq!(
        git(&h.cache, &["rev-parse", &format!("{sha}^{{tree}}")]),
        git(&h.cache, &["rev-parse", &format!("{head}^{{tree}}")]),
    );
    assert_eq!(
        git(
            &h.cache,
            &["log", "-1", "--format=%an <%ae>|%cn|%s", sha.as_str()]
        ),
        "Ada Lovelace <ada@example.com>|Ada Lovelace|Retry on 429s"
    );
}

#[tokio::test]
async fn push_never_forces_and_new_turns_go_on_top() {
    let h = host().await;
    let (wt, head) = two_turns(&h, "s1").await;
    let branch = "ada/retry";

    assert_eq!(
        h.git
            .remote_branch(&h.cache, &repo(), branch)
            .await
            .unwrap(),
        None
    );
    let first = h
        .git
        .space_commit(&h.cache, "s1", &squash(&head, &h.base, "Retry"))
        .await
        .unwrap();
    h.git
        .push_branch(&h.cache, &repo(), &first, branch)
        .await
        .unwrap();
    assert_eq!(
        git(&h.bare, &["rev-parse", &format!("refs/heads/{branch}")]),
        first.as_str()
    );
    assert_eq!(
        h.git
            .remote_branch(&h.cache, &repo(), branch)
            .await
            .unwrap(),
        Some(first.clone())
    );

    // Turn 3, then Push: a new commit whose parent is the pushed one.
    std::fs::write(wt.join("c.txt"), "newer\n").unwrap();
    let head3 = h.git.space_checkpoint(&wt, "s1", "turn 3").await.unwrap();
    let second = h
        .git
        .space_commit(&h.cache, "s1", &squash(&head3, &first, "Address review"))
        .await
        .unwrap();
    h.git
        .push_branch(&h.cache, &repo(), &second, branch)
        .await
        .unwrap();
    assert_eq!(
        git(&h.bare, &["rev-parse", &format!("refs/heads/{branch}~1")]),
        first.as_str()
    );

    // A commit that doesn't descend from the remote tip is refused.
    let stray = h
        .git
        .space_commit(&h.cache, "s1", &squash(&head3, &h.base, "stray"))
        .await
        .unwrap();
    let err = h
        .git
        .push_branch(&h.cache, &repo(), &stray, branch)
        .await
        .unwrap_err();
    assert!(matches!(err, GitError::Refused(_)), "{err}");
    assert_eq!(
        git(&h.bare, &["rev-parse", &format!("refs/heads/{branch}")]),
        second.as_str(),
        "the remote branch is untouched"
    );
}

#[tokio::test]
async fn new_turns_replay_onto_a_branch_someone_else_moved() {
    let h = host().await;
    let (wt, head) = two_turns(&h, "s1").await;
    let branch = "ada/retry";
    let first = h
        .git
        .space_commit(&h.cache, "s1", &squash(&head, &h.base, "Retry"))
        .await
        .unwrap();
    h.git
        .push_branch(&h.cache, &repo(), &first, branch)
        .await
        .unwrap();

    // A reviewer pushes a fix to b.txt on the PR branch.
    let other = h._tmp.path().join("other");
    git(
        h._tmp.path(),
        &[
            "clone",
            "-q",
            "-b",
            branch,
            h.bare.to_str().unwrap(),
            "other",
        ],
    );
    git(&other, &["config", "user.email", "r@b.com"]);
    git(&other, &["config", "user.name", "reviewer"]);
    std::fs::write(other.join("b.txt"), "bee fixed\n").unwrap();
    commit_all(&other, "reviewer fix");
    git(&other, &["push", "-q", "origin", branch]);

    std::fs::write(wt.join("c.txt"), "newer\n").unwrap();
    let head3 = h.git.space_checkpoint(&wt, "s1", "turn 3").await.unwrap();
    let tip = h
        .git
        .remote_branch(&h.cache, &repo(), branch)
        .await
        .unwrap()
        .expect("tip");
    assert_ne!(tip, first);
    let replayed = h
        .git
        .space_commit(
            &h.cache,
            "s1",
            &SpaceCommit {
                replay_from: Some(head.clone()),
                ..squash(&head3, &tip, "Address review")
            },
        )
        .await
        .unwrap();
    h.git
        .push_branch(&h.cache, &repo(), &replayed, branch)
        .await
        .unwrap();
    let show = |p: &str| git(&h.bare, &["show", &format!("refs/heads/{branch}:{p}")]);
    assert_eq!(show("b.txt"), "bee fixed", "the reviewer's commit is kept");
    assert_eq!(show("c.txt"), "newer");

    // A turn that edits the same line as someone else is refused.
    git(&other, &["pull", "-q", "--ff-only", "origin", branch]);
    std::fs::write(other.join("c.txt"), "theirs\n").unwrap();
    commit_all(&other, "reviewer c");
    git(&other, &["push", "-q", "origin", branch]);
    std::fs::write(wt.join("c.txt"), "ours\n").unwrap();
    let head4 = h.git.space_checkpoint(&wt, "s1", "turn 4").await.unwrap();
    let tip = h
        .git
        .remote_branch(&h.cache, &repo(), branch)
        .await
        .unwrap()
        .unwrap();
    let err = h
        .git
        .space_commit(
            &h.cache,
            "s1",
            &SpaceCommit {
                replay_from: Some(head3),
                ..squash(&head4, &tip, "x")
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&err, GitError::Refused(m) if m.contains("c.txt")),
        "{err}"
    );
}

struct Folder {
    _tmp: TempDir,
    git: GitCli,
    folder: PathBuf,
    cache: PathBuf,
    head: Sha,
}

/// A local folder project with one shipped space commit (edits a.txt,
/// adds c.txt) in the mirror.
async fn folder() -> Folder {
    let tmp = TempDir::new().unwrap();
    let folder = tmp.path().join("app");
    init_repo(&folder);
    let cli = GitCli::new(tmp.path().join("root"), Arc::new(NoAuth));
    let local = RepoRef {
        host: HostKind::Local,
        owner: "local".into(),
        name: "app".into(),
    };
    let cache = cli.ensure_local_mirror(&folder, &local).await.unwrap();
    let base = cli.resolve_ref(&cache, "main").await.unwrap();
    let wt = cli.space_worktree(&cache, "s1", &base).await.unwrap();
    std::fs::write(wt.path.join("a.txt"), "one\ntwo\n").unwrap();
    std::fs::write(wt.path.join("c.txt"), "new\n").unwrap();
    let head = cli
        .space_checkpoint(&wt.path, "s1", "turn 1")
        .await
        .unwrap();
    cli.space_commit(&cache, "s1", &squash(&head, &base, "Add c"))
        .await
        .unwrap();
    let head = Sha::new(git(&folder, &["rev-parse", "HEAD"]));
    Folder {
        _tmp: tmp,
        git: cli,
        folder,
        cache,
        head,
    }
}

#[tokio::test]
async fn merge_locally_merges_into_the_checked_out_branch() {
    let f = folder().await;
    // The folder moved on meanwhile: a real merge commit, not a ff.
    std::fs::write(f.folder.join("b.txt"), "bee 2\n").unwrap();
    commit_all(&f.folder, "local work");

    let head = f
        .git
        .merge_into_folder(&f.folder, &f.cache, "s1", "Merge space add-c")
        .await
        .unwrap();
    assert_eq!(git(&f.folder, &["rev-parse", "HEAD"]), head.as_str());
    assert_eq!(
        git(&f.folder, &["log", "-1", "--format=%s"]),
        "Merge space add-c"
    );
    assert_eq!(git(&f.folder, &["symbolic-ref", "--short", "HEAD"]), "main");
    assert_eq!(
        std::fs::read_to_string(f.folder.join("c.txt")).unwrap(),
        "new\n"
    );
    assert_eq!(git(&f.folder, &["status", "--porcelain"]), "");
    assert!(!f.folder.join(".git/FETCH_HEAD").exists());
}

#[tokio::test]
async fn merge_locally_refuses_a_dirty_folder_and_leaves_it_alone() {
    let f = folder().await;
    std::fs::write(f.folder.join("b.txt"), "uncommitted\n").unwrap();
    let err = f
        .git
        .merge_into_folder(&f.folder, &f.cache, "s1", "m")
        .await
        .unwrap_err();
    assert!(
        matches!(&err, GitError::Refused(m) if m.contains("uncommitted")),
        "{err}"
    );
    assert_eq!(git(&f.folder, &["rev-parse", "HEAD"]), f.head.as_str());
    assert_eq!(git(&f.folder, &["status", "--porcelain"]), "M b.txt");
}

#[tokio::test]
async fn merge_locally_refuses_a_conflict_before_touching_anything() {
    let f = folder().await;
    std::fs::write(f.folder.join("a.txt"), "one\nmine\n").unwrap();
    commit_all(&f.folder, "conflicting work");
    let before = git(&f.folder, &["rev-parse", "HEAD"]);

    let err = f
        .git
        .merge_into_folder(&f.folder, &f.cache, "s1", "m")
        .await
        .unwrap_err();
    assert!(
        matches!(&err, GitError::Refused(m) if m.contains("conflict") && m.contains("a.txt")),
        "{err}"
    );
    assert_eq!(git(&f.folder, &["rev-parse", "HEAD"]), before);
    assert_eq!(git(&f.folder, &["status", "--porcelain"]), "");
    assert!(!f.folder.join(".git/MERGE_HEAD").exists());
}

#[tokio::test]
async fn keep_as_branch_writes_only_a_ref() {
    let f = folder().await;
    f.git
        .write_folder_branch(&f.folder, &f.cache, "s1", "space/add-c")
        .await
        .unwrap();
    assert_eq!(
        git(&f.folder, &["rev-parse", "space/add-c"]),
        git(&f.cache, &["rev-parse", &shipped_ref("s1")])
    );
    assert_eq!(git(&f.folder, &["rev-parse", "HEAD"]), f.head.as_str());
    assert_eq!(git(&f.folder, &["symbolic-ref", "--short", "HEAD"]), "main");
    assert_eq!(git(&f.folder, &["status", "--porcelain"]), "");

    let err = f
        .git
        .write_folder_branch(&f.folder, &f.cache, "s1", "space/add-c")
        .await
        .unwrap_err();
    assert!(matches!(err, GitError::Refused(_)), "{err}");
}

#[tokio::test]
async fn the_user_identity_comes_from_git_config() {
    let f = folder().await;
    // The folder's own config sets it.
    let id = f.git.user_identity(&f.folder).await.unwrap();
    assert_eq!(
        id,
        Some(Identity {
            name: "test".into(),
            email: "a@b.com".into()
        })
    );
}

/// Undo last turn: the worktree and the checkpoint ref go back to the turn
/// before; the turn's untracked leftovers go too, ignored files stay.
#[tokio::test]
async fn space_reset_moves_the_worktree_and_ref_back_a_turn() {
    let h = host().await;
    let wt = h.git.space_worktree(&h.cache, "s1", &h.base).await.unwrap();
    std::fs::write(wt.path.join(".gitignore"), "deps/\n").unwrap();
    std::fs::write(wt.path.join("a.txt"), "one\ntwo\n").unwrap();
    let turn1 = h
        .git
        .space_checkpoint(&wt.path, "s1", "turn 1")
        .await
        .unwrap();
    std::fs::write(wt.path.join("c.txt"), "new\n").unwrap();
    h.git
        .space_checkpoint(&wt.path, "s1", "turn 2")
        .await
        .unwrap();
    std::fs::create_dir_all(wt.path.join("deps")).unwrap();
    std::fs::write(wt.path.join("deps/lib.js"), "x\n").unwrap();
    std::fs::write(wt.path.join("stray.txt"), "left behind\n").unwrap();

    h.git
        .space_reset(&wt.path, "s1", &turn1)
        .await
        .expect("reset");

    assert_eq!(git(&wt.path, &["rev-parse", "HEAD"]), turn1.as_str());
    assert_eq!(
        git(&h.cache, &["rev-parse", "refs/theodore/spaces/s1"]),
        turn1.as_str()
    );
    assert!(!wt.path.join("c.txt").exists());
    assert!(!wt.path.join("stray.txt").exists());
    assert!(wt.path.join("deps/lib.js").exists(), "ignored files stay");
    assert_eq!(
        std::fs::read_to_string(wt.path.join("a.txt")).unwrap(),
        "one\ntwo\n"
    );
    assert_eq!(git(&wt.path, &["status", "--porcelain"]), "");
}
