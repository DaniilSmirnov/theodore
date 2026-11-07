//! Update from base (docs/spaces.md §6) on real git: the base's new tip is
//! merged into a space's worktree — clean, with conflicts the agent
//! resolves, aborted — and shipping after it still pushes one commit,
//! fast-forward, whose PR diff is the space's change alone.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use tempfile::TempDir;
use theodore_domain::{HostKind, RepoRef, Sha};
use theodore_git::GitCli;
use theodore_ports::{
    CredentialSource, GitError, GitPort, Identity, SpaceCommit, SpaceMerge, WorktreeBusy, space_ref,
};

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

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(PLAIN)
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
    git(dir, &["commit", "-q", "-m", message]);
}

fn repo() -> RepoRef {
    RepoRef {
        host: HostKind::Github,
        owner: "acme".into(),
        name: "widget".into(),
    }
}

fn me() -> Identity {
    Identity {
        name: "Ada Lovelace".into(),
        email: "ada@example.com".into(),
    }
}

struct Host {
    _tmp: TempDir,
    git: GitCli,
    /// A clone of the remote where "someone else" pushes to `main`.
    work: PathBuf,
    bare: PathBuf,
    cache: PathBuf,
    base: Sha,
}

async fn host() -> Host {
    let tmp = TempDir::new().unwrap();
    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "-q", "-b", "main"]);
    std::fs::write(work.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    commit_all(&work, "base");
    let bare = tmp.path().join("remote.git");
    git(
        tmp.path(),
        &["clone", "-q", "--bare", "work", bare.to_str().unwrap()],
    );
    git(&work, &["remote", "add", "origin", bare.to_str().unwrap()]);
    let url = format!("file://{}", bare.display());
    let cli = GitCli::new(tmp.path().join("root"), Arc::new(NoAuth))
        .with_remote_urls(Arc::new(move |_r: &RepoRef| Some(url.clone())));
    let cache = cli.ensure_repo(&repo()).await.expect("clone");
    let base = cli.resolve_ref(&cache, "main").await.unwrap();
    Host {
        _tmp: tmp,
        git: cli,
        work,
        bare,
        cache,
        base,
    }
}

impl Host {
    /// Someone pushes `file` = `content` to `main`; the cache fetches it.
    async fn base_moves(&self, file: &str, content: &str) -> Sha {
        std::fs::write(self.work.join(file), content).unwrap();
        commit_all(&self.work, &format!("base: {file}"));
        git(&self.work, &["push", "-q", "origin", "main"]);
        self.git.ensure_repo(&repo()).await.unwrap();
        self.git.resolve_ref(&self.cache, "main").await.unwrap()
    }

    /// A space with one turn that wrote `file` = `content`.
    async fn space_with(&self, id: &str, file: &str, content: &str) -> (PathBuf, Sha) {
        let wt = self
            .git
            .space_worktree(&self.cache, id, &self.base)
            .await
            .unwrap();
        std::fs::write(wt.path.join(file), content).unwrap();
        let head = self
            .git
            .space_checkpoint(&wt.path, id, "turn 1")
            .await
            .unwrap();
        (wt.path, head.sha)
    }
}

fn parents(dir: &Path, rev: &str) -> Vec<String> {
    git(dir, &["rev-list", "--parents", "-n1", rev])
        .split_whitespace()
        .skip(1)
        .map(str::to_string)
        .collect()
}

#[tokio::test]
async fn a_clean_update_is_a_merge_checkpoint() {
    let h = host().await;
    let (wt, turn) = h.space_with("s1", "b.txt", "space\n").await;
    let tip = h.base_moves("c.txt", "base\n").await;

    let merged = h
        .git
        .space_merge(&wt, "s1", &tip, "s1: merge main")
        .await
        .unwrap();
    let SpaceMerge::Merged(head) = merged else {
        panic!("{merged:?}")
    };
    assert_eq!(parents(&wt, "HEAD"), vec![turn.0.clone(), tip.0.clone()]);
    assert_eq!(
        git(&wt, &["log", "-1", "--format=%an %s"]),
        "Theodore s1: merge main"
    );
    assert_eq!(git(&wt, &["rev-parse", &space_ref("s1")]), head.0);
    assert!(wt.join("c.txt").exists() && wt.join("b.txt").exists());
    // Diff vs the new base: the space's change alone.
    assert_eq!(git(&wt, &["diff", "--name-only", &tip.0, "HEAD"]), "b.txt");
}

#[tokio::test]
async fn conflicts_wait_for_the_agent_and_theodore_commits_the_merge() {
    let h = host().await;
    let (wt, turn) = h
        .space_with("s1", "a.txt", "one\nTWO (space)\nthree\n")
        .await;
    let tip = h.base_moves("a.txt", "one\nTWO (base)\nthree\n").await;

    let merged = h.git.space_merge(&wt, "s1", &tip, "s1: merge main").await;
    assert_eq!(merged.unwrap(), SpaceMerge::Conflicts(vec!["a.txt".into()]));
    let text = std::fs::read_to_string(wt.join("a.txt")).unwrap();
    assert!(
        text.contains("<<<<<<<") && text.contains("|||||||"),
        "diff3: {text}"
    );
    // The normal checkpoint waits; the ref stays at the turn.
    let err = h.git.space_checkpoint(&wt, "s1", "x").await.unwrap_err();
    assert!(matches!(err, GitError::Busy(WorktreeBusy::Merge)), "{err}");
    assert_eq!(git(&wt, &["rev-parse", &space_ref("s1")]), turn.0);

    // Markers still there: nothing committed.
    let again = h
        .git
        .space_merge_finish(&wt, "s1", &tip, &["a.txt".into()], "s1: merge main")
        .await;
    assert_eq!(again.unwrap(), SpaceMerge::Conflicts(vec!["a.txt".into()]));

    // The agent edits the file (no `git add`), and adds one of its own.
    std::fs::write(wt.join("a.txt"), "one\nTWO (space + base)\nthree\n").unwrap();
    std::fs::write(wt.join("note.txt"), "why\n").unwrap();
    let done = h
        .git
        .space_merge_finish(&wt, "s1", &tip, &["a.txt".into()], "s1: merge main")
        .await
        .unwrap();
    let SpaceMerge::Merged(head) = done else {
        panic!("{done:?}")
    };
    assert_eq!(parents(&wt, "HEAD"), vec![turn.0.clone(), tip.0.clone()]);
    assert_eq!(git(&wt, &["rev-parse", &space_ref("s1")]), head.0);
    assert_eq!(h.git.worktree_busy(&wt).await.unwrap(), None);
    assert_eq!(git(&wt, &["status", "--porcelain"]), "");
    assert_eq!(
        git(&wt, &["show", "HEAD:a.txt"]),
        "one\nTWO (space + base)\nthree"
    );
}

#[tokio::test]
async fn a_merge_concluded_or_aborted_by_hand_is_recognised() {
    let h = host().await;
    let (wt, turn) = h.space_with("s1", "a.txt", "one\nspace\nthree\n").await;
    let tip = h.base_moves("a.txt", "one\nbase\nthree\n").await;

    h.git.space_merge(&wt, "s1", &tip, "m").await.unwrap();
    git(&wt, &["merge", "--abort"]);
    let left = h.git.space_merge_finish(&wt, "s1", &tip, &[], "m").await;
    assert_eq!(left.unwrap(), SpaceMerge::NotMerging);

    h.git.space_merge(&wt, "s1", &tip, "m").await.unwrap();
    std::fs::write(wt.join("a.txt"), "one\nboth\nthree\n").unwrap();
    git(&wt, &["commit", "-qam", "agent's own merge commit"]);
    let done = h
        .git
        .space_merge_finish(&wt, "s1", &tip, &[], "m")
        .await
        .unwrap();
    let SpaceMerge::Merged(head) = done else {
        panic!("{done:?}")
    };
    assert_eq!(parents(&wt, &head.0), vec![turn.0, tip.0]);
}

#[tokio::test]
async fn abort_goes_back_to_the_checkpoint_it_started_from() {
    let h = host().await;
    let (wt, turn) = h.space_with("s1", "a.txt", "one\nspace\nthree\n").await;
    let tip = h.base_moves("a.txt", "one\nbase\nthree\n").await;
    h.git.space_merge(&wt, "s1", &tip, "m").await.unwrap();
    // Half resolved, plus a stray file.
    std::fs::write(wt.join("a.txt"), "half\n").unwrap();
    std::fs::write(wt.join("stray.txt"), "x\n").unwrap();

    h.git.space_merge_abort(&wt, "s1", &turn).await.unwrap();
    assert_eq!(git(&wt, &["rev-parse", "HEAD"]), turn.0);
    assert_eq!(git(&wt, &["rev-parse", &space_ref("s1")]), turn.0);
    assert_eq!(git(&wt, &["status", "--porcelain"]), "");
    assert_eq!(h.git.worktree_busy(&wt).await.unwrap(), None);
    assert_eq!(
        std::fs::read_to_string(wt.join("a.txt")).unwrap(),
        "one\nspace\nthree\n"
    );
}

/// Open PR, then the base moves and the space updates, then Push: one more
/// commit, fast-forward, with the base as its second parent, so the
/// host's PR diff (vs the merge base) is still only the space's change.
#[tokio::test]
async fn push_after_an_update_merges_the_base_into_the_pr_branch() {
    let h = host().await;
    let (wt, turn) = h.space_with("s1", "b.txt", "space\n").await;
    let branch = "ada/s1";
    let first = h
        .git
        .space_commit(
            &h.cache,
            "s1",
            &SpaceCommit {
                head: turn.clone(),
                parent: h.base.clone(),
                replay_from: None,
                merge_parent: None,
                message: "Open".into(),
                author: me(),
            },
        )
        .await
        .unwrap();
    h.git
        .push_branch(&h.cache, &repo(), &first, branch)
        .await
        .unwrap();

    let tip = h.base_moves("c.txt", "base\n").await;
    let SpaceMerge::Merged(_) = h.git.space_merge(&wt, "s1", &tip, "m").await.unwrap() else {
        panic!("clean")
    };
    std::fs::write(wt.join("b.txt"), "space, more\n").unwrap();
    let head = h
        .git
        .space_checkpoint(&wt, "s1", "turn 2")
        .await
        .unwrap()
        .sha;

    let pushed = h
        .git
        .space_commit(
            &h.cache,
            "s1",
            &SpaceCommit {
                head: head.clone(),
                parent: first.clone(),
                replay_from: None,
                merge_parent: Some(tip.clone()),
                message: "Push".into(),
                author: me(),
            },
        )
        .await
        .unwrap();
    // Fast-forward: a force would be needed otherwise, and the remote
    // refuses non-fast-forward pushes by default here too.
    h.git
        .push_branch(&h.cache, &repo(), &pushed, branch)
        .await
        .unwrap();
    let remote = format!("refs/heads/{branch}");
    assert_eq!(
        parents(&h.bare, &remote),
        vec![first.0.clone(), tip.0.clone()]
    );
    assert_eq!(git(&h.bare, &["merge-base", "main", &remote]), tip.0);
    assert_eq!(
        git(
            &h.bare,
            &["diff", "--name-only", &format!("main...{remote}")]
        ),
        "b.txt"
    );
    assert_eq!(
        git(&h.bare, &["show", &format!("{remote}:b.txt")]),
        "space, more"
    );

    // Nothing new on the base since: no second parent this time.
    std::fs::write(wt.join("b.txt"), "space, again\n").unwrap();
    let head3 = h
        .git
        .space_checkpoint(&wt, "s1", "turn 3")
        .await
        .unwrap()
        .sha;
    let third = h
        .git
        .space_commit(
            &h.cache,
            "s1",
            &SpaceCommit {
                head: head3,
                parent: pushed.clone(),
                replay_from: None,
                merge_parent: Some(tip.clone()),
                message: "Push again".into(),
                author: me(),
            },
        )
        .await
        .unwrap();
    assert_eq!(parents(&h.cache, &third.0), vec![pushed.0]);
}

/// `git add` clears the unmerged entry, not the markers in the file.
#[tokio::test]
async fn staged_conflict_markers_are_not_committed() {
    let h = host().await;
    let (wt, _) = h
        .space_with("s1", "a.txt", "one\nTWO (space)\nthree\n")
        .await;
    let tip = h.base_moves("a.txt", "one\nTWO (base)\nthree\n").await;
    let merged = h.git.space_merge(&wt, "s1", &tip, "s1: merge main").await;
    assert_eq!(merged.unwrap(), SpaceMerge::Conflicts(vec!["a.txt".into()]));

    git(&wt, &["add", "a.txt"]);
    let left = h
        .git
        .space_merge_finish(&wt, "s1", &tip, &["a.txt".into()], "s1: merge main")
        .await
        .unwrap();
    assert_eq!(left, SpaceMerge::Conflicts(vec!["a.txt".into()]));
    assert_eq!(
        h.git.worktree_busy(&wt).await.unwrap(),
        Some(WorktreeBusy::Merge),
        "still merging"
    );
}

/// Open PR after an update: one commit on the new base, nothing else.
#[tokio::test]
async fn the_first_ship_after_an_update_sits_on_the_new_base() {
    let h = host().await;
    let (wt, _) = h.space_with("s1", "b.txt", "space\n").await;
    let tip = h.base_moves("c.txt", "base\n").await;
    let SpaceMerge::Merged(head) = h.git.space_merge(&wt, "s1", &tip, "m").await.unwrap() else {
        panic!("clean")
    };
    let sha = h
        .git
        .space_commit(
            &h.cache,
            "s1",
            &SpaceCommit {
                head,
                parent: tip.clone(),
                replay_from: None,
                merge_parent: None,
                message: "Open".into(),
                author: me(),
            },
        )
        .await
        .unwrap();
    assert_eq!(parents(&h.cache, &sha.0), vec![tip.0.clone()]);
    assert_eq!(
        git(&h.cache, &["diff", "--name-only", &tip.0, &sha.0]),
        "b.txt"
    );
}
