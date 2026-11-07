//! Mirroring a space into a local folder (docs/spaces.md "Mirror into a
//! local folder") on real git: only changed paths are rewritten, a turn
//! off restores exactly, and a folder edited meanwhile is caught.
//!
//! The app usecase tracks the running set of paths mirroring has ever
//! touched (the union of every `mirror_sync` call's result) and passes it
//! to `mirror_edited` / `mirror_restore`; these tests do the same by hand.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::SystemTime;

use tempfile::TempDir;
use theodore_domain::{HostKind, RepoRef, Sha};
use theodore_git::GitCli;
use theodore_ports::{CredentialSource, GitError, GitPort};

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

fn mtime(p: &Path) -> SystemTime {
    std::fs::metadata(p).unwrap().modified().unwrap()
}

struct Env {
    _tmp: TempDir,
    git: GitCli,
    folder: PathBuf,
    cache: PathBuf,
    base: Sha,
}

/// A local folder project, mirrored into Theodore's cache.
async fn env() -> Env {
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
    Env {
        _tmp: tmp,
        git: cli,
        folder,
        cache,
        base,
    }
}

/// One checkpoint for `space` in its own space worktree, from `base`.
async fn checkpoint(e: &Env, space: &str, base: &Sha, edit: impl FnOnce(&Path)) -> Sha {
    let wt = e.git.space_worktree(&e.cache, space, base).await.unwrap();
    edit(&wt.path);
    e.git
        .space_checkpoint(&wt.path, space, "turn")
        .await
        .unwrap()
        .sha
}

/// `mirror_sync`, folding its result into the running set of touched
/// paths the way the app usecase does.
async fn sync(
    e: &Env,
    space: &str,
    touched: &mut Vec<String>,
    from: &Sha,
    to: &Sha,
) -> Vec<String> {
    let changed = e
        .git
        .mirror_sync(&e.folder, &e.cache, space, to, from)
        .await
        .unwrap();
    for p in &changed {
        if !touched.contains(p) {
            touched.push(p.clone());
        }
    }
    changed
}

#[tokio::test]
async fn mirror_on_writes_only_what_the_checkpoint_changed_from_base() {
    let e = env().await;
    let head = checkpoint(&e, "s1", &e.base, |p| {
        std::fs::write(p.join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(p.join("c.txt"), "new\n").unwrap();
    })
    .await;

    let mut touched = Vec::new();
    let mut written = sync(&e, "s1", &mut touched, &e.base, &head).await;
    written.sort();
    assert_eq!(written, vec!["a.txt", "c.txt"]);
    assert_eq!(
        std::fs::read_to_string(e.folder.join("a.txt")).unwrap(),
        "one\ntwo\n"
    );
    assert_eq!(
        std::fs::read_to_string(e.folder.join("c.txt")).unwrap(),
        "new\n"
    );
    // b.txt never changed, so it was never rewritten at all.
    assert_eq!(
        std::fs::read_to_string(e.folder.join("b.txt")).unwrap(),
        "bee\n"
    );
    // The folder's own git state (branch, HEAD, index) never moved.
    assert_eq!(git(&e.folder, &["rev-parse", "HEAD"]), e.base.as_str());
    assert_eq!(git(&e.folder, &["symbolic-ref", "--short", "HEAD"]), "main");
    // The real index was never touched: a.txt (content rewritten) now
    // reads as modified against it, c.txt as untracked — exactly what a
    // normal edit in the folder would look like.
    assert_eq!(
        git(&e.folder, &["status", "--porcelain"]),
        "M a.txt\n?? c.txt"
    );
}

#[tokio::test]
async fn a_resync_only_rewrites_what_changed() {
    let e = env().await;
    let head1 = checkpoint(&e, "s1", &e.base, |p| {
        std::fs::write(p.join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(p.join("c.txt"), "new\n").unwrap();
    })
    .await;
    let mut touched = Vec::new();
    sync(&e, "s1", &mut touched, &e.base, &head1).await;
    let a_before = mtime(&e.folder.join("a.txt"));
    let c_before = mtime(&e.folder.join("c.txt"));
    let b_before = mtime(&e.folder.join("b.txt"));
    // Give the filesystem a moment so a real rewrite would show a newer
    // mtime than "unchanged".
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let head2 = checkpoint(&e, "s1", &head1, |p| {
        std::fs::write(p.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    })
    .await;
    let changed = sync(&e, "s1", &mut touched, &head1, &head2).await;
    assert_eq!(changed, vec!["a.txt"]);
    assert!(mtime(&e.folder.join("a.txt")) > a_before, "a.txt rewritten");
    assert_eq!(mtime(&e.folder.join("c.txt")), c_before, "c.txt untouched");
    assert_eq!(mtime(&e.folder.join("b.txt")), b_before, "b.txt untouched");
    assert_eq!(
        std::fs::read_to_string(e.folder.join("a.txt")).unwrap(),
        "one\ntwo\nthree\n"
    );
}

#[tokio::test]
async fn a_path_the_new_checkpoint_drops_is_removed_from_the_folder() {
    let e = env().await;
    let head1 = checkpoint(&e, "s1", &e.base, |p| {
        std::fs::write(p.join("c.txt"), "new\n").unwrap();
    })
    .await;
    let mut touched = Vec::new();
    sync(&e, "s1", &mut touched, &e.base, &head1).await;
    assert!(e.folder.join("c.txt").exists());

    let head2 = checkpoint(&e, "s1", &head1, |p| {
        std::fs::remove_file(p.join("c.txt")).unwrap();
    })
    .await;
    let changed = sync(&e, "s1", &mut touched, &head1, &head2).await;
    assert_eq!(changed, vec!["c.txt"]);
    assert!(!e.folder.join("c.txt").exists());
    assert_eq!(git(&e.folder, &["rev-parse", "HEAD"]), e.base.as_str());
}

#[tokio::test]
async fn a_sync_refuses_to_overwrite_a_hand_edit_in_the_folder() {
    let e = env().await;
    let first = checkpoint(&e, "s1", &e.base, |p| {
        std::fs::write(p.join("a.txt"), "first\n").unwrap();
    })
    .await;
    let mut touched = Vec::new();
    sync(&e, "s1", &mut touched, &e.base, &first).await;

    std::fs::write(e.folder.join("a.txt"), "by hand\n").unwrap();
    let second = checkpoint(&e, "s1", &e.base, |p| {
        std::fs::write(p.join("a.txt"), "second\n").unwrap();
    })
    .await;
    let err = e
        .git
        .mirror_sync(&e.folder, &e.cache, "s1", &second, &first)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, GitError::Refused(m) if m.contains("a.txt")),
        "{err:?}"
    );
    assert_eq!(
        std::fs::read_to_string(e.folder.join("a.txt")).unwrap(),
        "by hand\n"
    );
}

#[tokio::test]
async fn a_first_sync_from_a_base_the_folder_isnt_on_is_no_hand_edit() {
    let e = env().await;
    let fork = e.base.clone();
    // The folder moves on; the space still forks from the older base.
    std::fs::write(e.folder.join("a.txt"), "folder moved on\n").unwrap();
    commit_all(&e.folder, "folder moves on");
    let head = checkpoint(&e, "s1", &fork, |p| {
        std::fs::write(p.join("a.txt"), "from the space\n").unwrap();
    })
    .await;
    let mut touched = Vec::new();
    sync(&e, "s1", &mut touched, &fork, &head).await;
    assert_eq!(
        std::fs::read_to_string(e.folder.join("a.txt")).unwrap(),
        "from the space\n"
    );
}

#[tokio::test]
async fn a_rename_removes_the_old_path_from_the_folder() {
    let e = env().await;
    let first = checkpoint(&e, "s1", &e.base, |p| {
        std::fs::write(p.join("x.txt"), "a file long enough to be a rename\n").unwrap();
    })
    .await;
    let mut touched = Vec::new();
    sync(&e, "s1", &mut touched, &e.base, &first).await;

    let second = checkpoint(&e, "s1", &e.base, |p| {
        std::fs::rename(p.join("x.txt"), p.join("y.txt")).unwrap();
    })
    .await;
    sync(&e, "s1", &mut touched, &first, &second).await;
    assert!(!e.folder.join("x.txt").exists());
    assert!(e.folder.join("y.txt").exists());
    assert_eq!(
        e.git
            .mirror_edited(&e.folder, &e.cache, "s1", &second, &touched)
            .await
            .unwrap(),
        Vec::<String>::new()
    );
}

#[tokio::test]
async fn mirror_off_restores_every_mirrored_path_exactly() {
    let e = env().await;
    let before_a = std::fs::read_to_string(e.folder.join("a.txt")).unwrap();
    let head1 = checkpoint(&e, "s1", &e.base, |p| {
        std::fs::write(p.join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(p.join("c.txt"), "new\n").unwrap();
    })
    .await;
    let mut touched = Vec::new();
    sync(&e, "s1", &mut touched, &e.base, &head1).await;
    let head2 = checkpoint(&e, "s1", &head1, |p| {
        std::fs::write(p.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    })
    .await;
    sync(&e, "s1", &mut touched, &head1, &head2).await;

    let mut restored = e
        .git
        .mirror_restore(&e.folder, &e.base, &touched)
        .await
        .unwrap();
    restored.sort();
    assert_eq!(restored, vec!["a.txt", "c.txt"]);
    assert_eq!(
        std::fs::read_to_string(e.folder.join("a.txt")).unwrap(),
        before_a
    );
    assert!(!e.folder.join("c.txt").exists());
    assert_eq!(git(&e.folder, &["rev-parse", "HEAD"]), e.base.as_str());
    assert_eq!(git(&e.folder, &["status", "--porcelain"]), "");
}

#[tokio::test]
async fn an_untouched_folder_mirrors_as_not_edited() {
    let e = env().await;
    let head = checkpoint(&e, "s1", &e.base, |p| {
        std::fs::write(p.join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(p.join("c.txt"), "new\n").unwrap();
    })
    .await;
    let mut touched = Vec::new();
    sync(&e, "s1", &mut touched, &e.base, &head).await;

    let edited = e
        .git
        .mirror_edited(&e.folder, &e.cache, "s1", &head, &touched)
        .await
        .unwrap();
    assert_eq!(edited, Vec::<String>::new());
}

#[tokio::test]
async fn editing_a_mirrored_file_in_the_folder_is_caught_by_content() {
    let e = env().await;
    let head = checkpoint(&e, "s1", &e.base, |p| {
        std::fs::write(p.join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(p.join("c.txt"), "new\n").unwrap();
    })
    .await;
    let mut touched = Vec::new();
    sync(&e, "s1", &mut touched, &e.base, &head).await;

    // Rewriting the exact same bytes is not an edit (content-based, not a
    // timestamp check).
    std::fs::write(e.folder.join("a.txt"), "one\ntwo\n").unwrap();
    assert_eq!(
        e.git
            .mirror_edited(&e.folder, &e.cache, "s1", &head, &touched)
            .await
            .unwrap(),
        Vec::<String>::new()
    );

    std::fs::write(e.folder.join("a.txt"), "edited by hand\n").unwrap();
    let edited = e
        .git
        .mirror_edited(&e.folder, &e.cache, "s1", &head, &touched)
        .await
        .unwrap();
    assert_eq!(edited, vec!["a.txt"]);
}

#[tokio::test]
async fn recreating_a_mirror_deleted_file_is_caught_too() {
    let e = env().await;
    let head1 = checkpoint(&e, "s1", &e.base, |p| {
        std::fs::write(p.join("c.txt"), "new\n").unwrap();
    })
    .await;
    let mut touched = Vec::new();
    sync(&e, "s1", &mut touched, &e.base, &head1).await;
    let head2 = checkpoint(&e, "s1", &head1, |p| {
        std::fs::remove_file(p.join("c.txt")).unwrap();
    })
    .await;
    sync(&e, "s1", &mut touched, &head1, &head2).await;
    assert!(!e.folder.join("c.txt").exists());
    // c.txt has no net difference between base and head2 (added, then
    // removed): the running `touched` set is what still remembers it.
    assert_eq!(touched, vec!["c.txt"]);

    // The user brings it back by hand after mirroring removed it.
    std::fs::write(e.folder.join("c.txt"), "mine again\n").unwrap();
    let edited = e
        .git
        .mirror_edited(&e.folder, &e.cache, "s1", &head2, &touched)
        .await
        .unwrap();
    assert_eq!(edited, vec!["c.txt"]);

    // Mirror off still removes it again — restore, not Discard, since
    // nothing legitimate is being overwritten here from mirroring's own
    // point of view (the app usecase is what actually gates this on
    // `mirror_edited` first).
    let restored = e
        .git
        .mirror_restore(&e.folder, &e.base, &touched)
        .await
        .unwrap();
    assert_eq!(restored, vec!["c.txt"]);
    assert!(!e.folder.join("c.txt").exists());
}

#[tokio::test]
async fn restore_overwrites_a_hand_edit_when_asked_to() {
    // mirror_restore itself always rewrites the paths it is given; the
    // app usecase is what checks `mirror_edited` first and offers
    // Discard — this proves the git layer's half of that still works.
    let e = env().await;
    let head = checkpoint(&e, "s1", &e.base, |p| {
        std::fs::write(p.join("c.txt"), "new\n").unwrap();
    })
    .await;
    let mut touched = Vec::new();
    sync(&e, "s1", &mut touched, &e.base, &head).await;
    std::fs::write(e.folder.join("c.txt"), "edited by hand\n").unwrap();

    let restored = e
        .git
        .mirror_restore(&e.folder, &e.base, &touched)
        .await
        .unwrap();
    assert_eq!(restored, vec!["c.txt"]);
    assert!(!e.folder.join("c.txt").exists());
}

#[tokio::test]
async fn switching_to_another_space_restores_the_first_then_mirrors_the_second() {
    let e = env().await;
    let head1 = checkpoint(&e, "s1", &e.base, |p| {
        std::fs::write(p.join("c.txt"), "from s1\n").unwrap();
    })
    .await;
    let mut touched1 = Vec::new();
    sync(&e, "s1", &mut touched1, &e.base, &head1).await;
    assert_eq!(
        std::fs::read_to_string(e.folder.join("c.txt")).unwrap(),
        "from s1\n"
    );

    // Mirror off s1 (no edits in the folder meanwhile): back to the base.
    e.git
        .mirror_restore(&e.folder, &e.base, &touched1)
        .await
        .unwrap();
    assert!(!e.folder.join("c.txt").exists());

    // Mirror on s2, from the same remembered base.
    let head2 = checkpoint(&e, "s2", &e.base, |p| {
        std::fs::write(p.join("d.txt"), "from s2\n").unwrap();
    })
    .await;
    let mut touched2 = Vec::new();
    let written = sync(&e, "s2", &mut touched2, &e.base, &head2).await;
    assert_eq!(written, vec!["d.txt"]);
    assert_eq!(
        std::fs::read_to_string(e.folder.join("d.txt")).unwrap(),
        "from s2\n"
    );
    assert!(!e.folder.join("c.txt").exists(), "s1's file stays gone");
}

#[tokio::test]
async fn mirroring_never_touches_a_repository_above_a_moved_folder() {
    let e = env().await;
    let head = checkpoint(&e, "s1", &e.base, |p| {
        std::fs::write(p.join("c.txt"), "new\n").unwrap();
    })
    .await;
    // The folder stopped being its own git working copy (`.git` removed):
    // mirroring must refuse, not fall back to a parent repository.
    std::fs::remove_dir_all(e.folder.join(".git")).unwrap();
    let err = e
        .git
        .mirror_sync(&e.folder, &e.cache, "s1", &head, &e.base)
        .await
        .unwrap_err();
    assert!(matches!(err, GitError::NotAWorktree(_)), "{err}");
}

#[tokio::test]
async fn is_tracked_and_exclude_locally_leave_the_checkout_clean() {
    let e = env().await;
    assert!(e.git.is_tracked(&e.folder, "a.txt").await.unwrap());
    assert!(!e.git.is_tracked(&e.folder, "nope.txt").await.unwrap());

    std::fs::write(e.folder.join("mine.yaml"), "x").unwrap();
    assert_ne!(git(&e.folder, &["status", "--porcelain"]), "");
    e.git.exclude_locally(&e.folder, "mine.yaml").await.unwrap();
    e.git.exclude_locally(&e.folder, "mine.yaml").await.unwrap();
    assert_eq!(
        git(&e.folder, &["status", "--porcelain"]),
        "",
        "ignored, no .gitignore"
    );
    assert_eq!(
        std::fs::read_to_string(e.folder.join(".git/info/exclude"))
            .unwrap()
            .matches("mine.yaml")
            .count(),
        1,
        "once"
    );
}
