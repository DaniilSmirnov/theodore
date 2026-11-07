//! Snapshot checkpoints on real git (docs/spaces.md §6): a checkpoint is
//! the worktree as a commit on top of its `HEAD`, taken through a temporary
//! index; the worktree's `HEAD`, branch and index stay as the agent left
//! them. Going back writes only the files that differ.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tempfile::TempDir;
use theodore_domain::{HostKind, RepoRef, Sha};
use theodore_git::GitCli;
use theodore_ports::{
    CredentialSource, GitError, GitPort, Identity, SpaceCheckpoint, SpaceCommit,
    space_checkpoint_ref, space_ref,
};

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

fn commit_all(dir: &Path, message: &str) -> String {
    git(dir, &["add", "-A"]);
    git(
        dir,
        &[
            "-c",
            "user.name=agent",
            "-c",
            "user.email=agent@x",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            message,
        ],
    );
    git(dir, &["rev-parse", "HEAD"])
}

struct Space {
    _tmp: TempDir,
    git: GitCli,
    cache: PathBuf,
    base: Sha,
    wt: PathBuf,
}

/// A space `s1` at `main` of a repo with `a.txt`, `b.txt` and a
/// `.gitignore` for `deps/`.
async fn space() -> Space {
    let tmp = TempDir::new().unwrap();
    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "-q", "-b", "main"]);
    std::fs::write(work.join("a.txt"), "one\n").unwrap();
    std::fs::write(work.join("b.txt"), "bee\n").unwrap();
    std::fs::write(work.join(".gitignore"), "deps/\n").unwrap();
    commit_all(&work, "base");
    let bare = tmp.path().join("remote.git");
    git(
        tmp.path(),
        &["clone", "-q", "--bare", "work", bare.to_str().unwrap()],
    );
    let url = format!("file://{}", bare.display());
    let cli = GitCli::new(tmp.path().join("root"), Arc::new(NoAuth))
        .with_remote_urls(Arc::new(move |_r: &RepoRef| Some(url.clone())));
    let repo = RepoRef {
        host: HostKind::Github,
        owner: "acme".into(),
        name: "widget".into(),
    };
    let cache = cli.ensure_repo(&repo).await.unwrap();
    let base = cli.resolve_ref(&cache, "main").await.unwrap();
    let wt = cli.space_worktree(&cache, "s1", &base).await.unwrap().path;
    Space {
        _tmp: tmp,
        git: cli,
        cache,
        base,
        wt,
    }
}

impl Space {
    async fn checkpoint(&self, message: &str) -> SpaceCheckpoint {
        self.git
            .space_checkpoint(&self.wt, "s1", message)
            .await
            .unwrap()
    }

    fn write(&self, path: &str, text: &str) {
        let p = self.wt.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn read(&self, path: &str) -> Option<String> {
        std::fs::read_to_string(self.wt.join(path)).ok()
    }

    fn space_ref(&self) -> String {
        git(&self.cache, &["rev-parse", &space_ref("s1")])
    }

    fn refs(&self) -> String {
        git(
            &self.cache,
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        )
    }
}

fn mtime(p: &Path) -> SystemTime {
    std::fs::metadata(p).unwrap().modified().unwrap()
}

/// An old mtime, so a rewrite of the file can't land on the same second.
fn age(p: &Path) -> SystemTime {
    let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    std::fs::File::options()
        .write(true)
        .open(p)
        .unwrap()
        .set_modified(old)
        .unwrap();
    old
}

#[tokio::test]
async fn a_checkpoint_snapshots_the_worktree_and_leaves_head_and_index_alone() {
    let s = space().await;
    // Nothing changed: the checkpoint is `HEAD`.
    let none = s.checkpoint("turn 0").await;
    assert_eq!(none.sha, s.base);
    assert_eq!(none.head, s.base);

    s.write("a.txt", "one\ntwo\n");
    s.write("c.txt", "new\n");
    s.write("deps/lib.js", "ignored\n");
    std::fs::remove_file(s.wt.join("b.txt")).unwrap();
    // The agent staged one edit itself: it stays staged, and only that.
    git(&s.wt, &["add", "a.txt"]);
    let status = git(&s.wt, &["status", "--porcelain"]);

    let cp = s.checkpoint("turn 1").await;
    assert_ne!(cp.sha, s.base);
    assert_eq!(cp.head, s.base);
    assert_eq!(git(&s.wt, &["rev-parse", "HEAD"]), s.base.as_str());
    assert_eq!(git(&s.wt, &["status", "--porcelain"]), status);
    assert_eq!(s.space_ref(), cp.sha.as_str());
    assert_eq!(
        git(&s.cache, &["rev-parse", &format!("{}^", cp.sha.as_str())]),
        s.base.as_str(),
        "the snapshot sits on HEAD"
    );
    let files = git(&s.cache, &["ls-tree", "-r", "--name-only", cp.sha.as_str()]);
    assert_eq!(files, ".gitignore\na.txt\nc.txt", "ignored files stay out");
    assert_eq!(
        git(
            &s.cache,
            &["rev-parse", &space_checkpoint_ref("s1", &cp.sha)]
        ),
        cp.sha.as_str(),
        "kept reachable"
    );

    // The same worktree again: the same checkpoint, not a new commit.
    let again = s.checkpoint("turn 2").await;
    assert_eq!(again, cp);
}

#[tokio::test]
async fn the_agents_own_commits_are_kept_as_it_made_them() {
    let s = space().await;
    s.write("a.txt", "one\ncommitted\n");
    let agent = commit_all(&s.wt, "agent: fix a");
    s.write("c.txt", "uncommitted\n");

    let cp = s.checkpoint("turn 1").await;
    assert_eq!(cp.head.as_str(), agent);
    assert_eq!(git(&s.wt, &["rev-parse", "HEAD"]), agent);
    assert_eq!(
        git(&s.wt, &["log", "-1", "--format=%s %an"]),
        "agent: fix a agent"
    );
    assert_eq!(
        git(&s.cache, &["rev-parse", &format!("{}^", cp.sha.as_str())]),
        agent
    );
    assert_eq!(git(&s.wt, &["status", "--porcelain"]), "?? c.txt");

    // Everything committed: the checkpoint is the agent's commit itself.
    let agent2 = commit_all(&s.wt, "agent: add c");
    let cp2 = s.checkpoint("turn 2").await;
    assert_eq!(cp2.sha.as_str(), agent2);
    assert_eq!(cp2.head.as_str(), agent2);
}

#[tokio::test]
async fn undo_restores_only_what_differs_and_keeps_mtimes() {
    let s = space().await;
    s.write("a.txt", "one\ntwo\n");
    let turn1 = s.checkpoint("turn 1").await;
    let b_old = age(&s.wt.join("b.txt"));
    let a_old = age(&s.wt.join("a.txt"));

    // Turn 2: the agent commits, edits, adds and deletes; then leaves an
    // untracked file and installs ignored deps.
    s.write("c.txt", "new\n");
    commit_all(&s.wt, "agent: add c");
    s.write("a.txt", "one\ntwo\nthree\n");
    std::fs::remove_file(s.wt.join(".gitignore")).unwrap();
    s.checkpoint("turn 2").await;
    std::fs::write(s.wt.join(".gitignore"), "deps/\n").unwrap();
    s.write("stray/notes.txt", "left behind\n");
    s.write("deps/lib.js", "x\n");

    s.git.space_reset(&s.wt, "s1", &turn1).await.expect("reset");

    assert_eq!(git(&s.wt, &["rev-parse", "HEAD"]), s.base.as_str());
    assert_eq!(s.space_ref(), turn1.sha.as_str());
    assert_eq!(s.read("a.txt").as_deref(), Some("one\ntwo\n"));
    assert_eq!(s.read("c.txt"), None);
    assert!(!s.wt.join("stray").exists(), "untracked leftovers go");
    assert_eq!(
        s.read("deps/lib.js").as_deref(),
        Some("x\n"),
        "ignored stay"
    );
    assert_eq!(
        mtime(&s.wt.join("b.txt")),
        b_old,
        "unchanged file untouched"
    );
    assert_ne!(mtime(&s.wt.join("a.txt")), a_old, "changed file rewritten");
    assert_eq!(git(&s.wt, &["status", "--porcelain"]), "M a.txt");
}

#[tokio::test]
async fn rewinding_several_turns_goes_straight_back() {
    let s = space().await;
    let mut cps = Vec::new();
    for n in 1..=3 {
        s.write(&format!("t{n}.txt"), "x\n");
        if n == 2 {
            commit_all(&s.wt, "agent: turn 2");
        }
        cps.push(s.checkpoint(&format!("turn {n}")).await);
    }
    s.git
        .space_reset(&s.wt, "s1", &cps[0])
        .await
        .expect("rewind");
    assert_eq!(git(&s.wt, &["rev-parse", "HEAD"]), s.base.as_str());
    assert_eq!(s.read("t1.txt").as_deref(), Some("x\n"));
    assert_eq!(s.read("t2.txt"), None);
    assert_eq!(s.read("t3.txt"), None);
    assert_eq!(s.space_ref(), cps[0].sha.as_str());

    // Forward again works too (the later checkpoints are kept reachable).
    s.git
        .space_reset(&s.wt, "s1", &cps[2])
        .await
        .expect("forward");
    assert_eq!(git(&s.wt, &["rev-parse", "HEAD"]), cps[1].head.as_str());
    assert_eq!(s.read("t3.txt").as_deref(), Some("x\n"));
}

/// A space from before snapshots: its checkpoints were commits that were
/// `HEAD`, so `head == sha` brings it back the same way.
#[tokio::test]
async fn old_commit_checkpoints_still_undo() {
    let s = space().await;
    s.write("a.txt", "one\nold turn 1\n");
    let old1 = Sha::new(commit_all(&s.wt, "s1: turn 1"));
    s.write("a.txt", "one\nold turn 2\n");
    commit_all(&s.wt, "s1: turn 2");
    s.write("new.txt", "after\n");
    s.git
        .space_reset(
            &s.wt,
            "s1",
            &SpaceCheckpoint {
                sha: old1.clone(),
                head: old1.clone(),
            },
        )
        .await
        .unwrap();
    assert_eq!(git(&s.wt, &["rev-parse", "HEAD"]), old1.as_str());
    assert_eq!(s.read("a.txt").as_deref(), Some("one\nold turn 1\n"));
    assert_eq!(s.read("new.txt"), None);
    assert_eq!(git(&s.wt, &["status", "--porcelain"]), "");
}

#[tokio::test]
async fn reverting_a_file_restores_it_from_a_commit_and_nothing_else() {
    let s = space().await;
    s.write("a.txt", "one\nmine\n");
    s.write("b.txt", "bee changed\n");
    s.write("c.txt", "new\n");
    let cp = s.checkpoint("turn 1").await;
    let b_old = age(&s.wt.join("b.txt"));

    s.git
        .space_restore_paths(&s.wt, &["a.txt".into(), "c.txt".into()], &s.base)
        .await
        .unwrap();
    assert_eq!(s.read("a.txt").as_deref(), Some("one\n"));
    assert_eq!(s.read("c.txt"), None, "not in the base: removed");
    assert_eq!(mtime(&s.wt.join("b.txt")), b_old);
    assert_eq!(s.space_ref(), cp.sha.as_str(), "the caller checkpoints");
    assert_eq!(git(&s.wt, &["status", "--porcelain"]), "M b.txt");

    let err = s
        .git
        .space_restore_paths(&s.wt, &["../outside".into()], &s.base)
        .await
        .unwrap_err();
    assert!(matches!(err, GitError::Refused(_)), "{err:?}");
}

#[tokio::test]
async fn reverting_a_directory_leaves_its_unchanged_files_alone() {
    let s = space().await;
    s.write("d/same.txt", "same\n");
    s.write("d/edited.txt", "first\n");
    let cp = s.checkpoint("turn 1").await;
    s.write("d/edited.txt", "second\n");
    s.checkpoint("turn 2").await;
    let same_old = age(&s.wt.join("d/same.txt"));
    let edited_old = age(&s.wt.join("d/edited.txt"));

    s.git
        .space_restore_paths(&s.wt, &["d".into()], &cp.sha)
        .await
        .unwrap();
    assert_eq!(s.read("d/edited.txt").as_deref(), Some("first\n"));
    assert_ne!(mtime(&s.wt.join("d/edited.txt")), edited_old);
    assert_eq!(
        mtime(&s.wt.join("d/same.txt")),
        same_old,
        "its content didn't change, so neither does its mtime"
    );
}

#[tokio::test]
async fn a_live_snapshot_moves_no_ref_and_repeats_itself() {
    let s = space().await;
    let refs = s.refs();
    let quiet = s.git.space_fingerprint(&s.wt).await.unwrap();
    assert_eq!(s.git.space_fingerprint(&s.wt).await.unwrap(), quiet);

    s.write("a.txt", "one\nlive\n");
    let edited = s.git.space_fingerprint(&s.wt).await.unwrap();
    assert_ne!(edited, quiet);
    let snap = s.git.space_snapshot(&s.wt).await.unwrap();
    assert_eq!(s.git.space_snapshot(&s.wt).await.unwrap(), snap);
    // A second edit of a file `status` already lists still shows.
    s.write("a.txt", "one\nlive, longer\n");
    assert_ne!(s.git.space_fingerprint(&s.wt).await.unwrap(), edited);
    assert_ne!(s.git.space_snapshot(&s.wt).await.unwrap(), snap);
    assert_eq!(s.refs(), refs, "no ref created or moved");
    assert_eq!(git(&s.wt, &["status", "--porcelain"]), "M a.txt");
}

#[tokio::test]
async fn shipping_squashes_the_snapshot_tree_onto_the_base() {
    let s = space().await;
    s.write("a.txt", "one\ncommitted\n");
    commit_all(&s.wt, "agent: a");
    s.write("c.txt", "uncommitted\n");
    let cp = s.checkpoint("turn 1").await;
    let shipped = s
        .git
        .space_commit(
            &s.cache,
            "s1",
            &SpaceCommit {
                head: cp.sha.clone(),
                parent: s.base.clone(),
                replay_from: None,
                merge_parent: None,
                message: "Ship it".into(),
                author: Identity {
                    name: "Ada".into(),
                    email: "ada@x".into(),
                },
            },
        )
        .await
        .unwrap();
    let show = |spec: &str| git(&s.cache, &["show", "-s", "--format=%P %an %s", spec]);
    assert_eq!(
        show(shipped.as_str()),
        format!("{} Ada Ship it", s.base.as_str())
    );
    assert_eq!(
        git(&s.cache, &["show", &format!("{}:c.txt", shipped.as_str())]),
        "uncommitted"
    );
    assert_eq!(
        git(&s.cache, &["show", &format!("{}:a.txt", shipped.as_str())]),
        "one\ncommitted"
    );
}

#[tokio::test]
async fn build_junk_stays_out_without_a_gitignore_but_tracked_files_stay_tracked() {
    let s = space().await;
    // The agent tracked one compiled file itself: it stays tracked.
    s.write("keep.pyc", "v1\n");
    git(&s.wt, &["add", "-f", "keep.pyc"]);
    commit_all(&s.wt, "agent: keep a pyc");
    s.write("keep.pyc", "v2\n");
    s.write("game.py", "print('hi')\n");
    s.write("__pycache__/game.cpython-312.pyc", "junk\n");
    s.write("lib/__pycache__/x.pyc", "junk\n");
    s.write("stray.pyc", "junk\n");
    s.write("node_modules/left-pad/index.js", "junk\n");
    s.write(".DS_Store", "junk\n");
    s.write("sub/.DS_Store", "junk\n");
    // No Cargo.toml: a `target/` is the repo's own.
    s.write("target/notes.txt", "kept\n");

    let cp = s.checkpoint("turn 1").await;
    let files = git(&s.cache, &["ls-tree", "-r", "--name-only", cp.sha.as_str()]);
    assert_eq!(
        files,
        ".gitignore\na.txt\nb.txt\ngame.py\nkeep.pyc\ntarget/notes.txt"
    );
    assert_eq!(
        git(
            &s.cache,
            &["show", &format!("{}:keep.pyc", cp.sha.as_str())]
        ),
        "v2"
    );

    // With a Cargo.toml at the root, `target/` is its build output.
    s.write("Cargo.toml", "[package]\nname = \"x\"\n");
    s.write("target/debug/x", "junk\n");
    let cp = s.checkpoint("turn 2").await;
    let files = git(&s.cache, &["ls-tree", "-r", "--name-only", cp.sha.as_str()]);
    assert!(files.contains("Cargo.toml"));
    assert!(!files.contains("target/debug/x"), "{files}");
}
