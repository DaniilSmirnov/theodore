//! `interdiff` against a real temp git repo and the real `git` binary
//! (`theodore_git::GitCli`), not `testkit::FakeGit` — `FakeGit::diff` always
//! returns one canned `Revision` regardless of which heads are asked for, so
//! it can't stand in for "two different revisions of the same PR base", the
//! exact thing this command compares.

use std::path::Path;
use std::process::Command;

use theodore_contract::{InterdiffClass, InterdiffRequest, OpenPrRequest};
use theodore_domain::{HostKind, LineKind, RepoRef, Sha};
use theodore_ports::{CredentialSource, Fetched};

use crate::Core;
use crate::testkit::{self, fixtures};

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
    assert!(out.status.success(), "git {args:?} failed");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Four independent, widely-spaced regions of one file so each gets its own
/// hunk under the default 3-line diff context, covering every classification:
/// `r1` changes again between pushes (`Changed`), `r2` only changes on the
/// second push (`New`), `r3` changes on the first push then reverts on the
/// second (`Vanished` — no longer differs from base at all), `r4` changes
/// once and stays that way (`Unchanged` between the two diffs).
fn region(label: &str, mid: &str) -> Vec<String> {
    vec![format!("{label}-one"), mid.into(), format!("{label}-three")]
}

fn filler(prefix: &str) -> Vec<String> {
    (1..=12).map(|i| format!("{prefix}{i}")).collect()
}

fn notes_txt(r1: &str, r2: &str, r3: &str, r4: &str) -> String {
    let mut lines: Vec<String> = vec!["head1".into(), "head2".into(), "head3".into()];
    lines.extend(region("r1", r1));
    lines.extend(filler("f"));
    lines.extend(region("r2", r2));
    lines.extend(filler("g"));
    lines.extend(region("r3", r3));
    lines.extend(filler("h"));
    lines.extend(region("r4", r4));
    lines.extend(["tail1".into(), "tail2".into(), "tail3".into()]);
    lines.join("\n") + "\n"
}

struct Fixture {
    _tmp: tempfile::TempDir,
    root: std::path::PathBuf,
    repo: RepoRef,
    base_sha: String,
    /// The head the reviewer last looked at — the first push.
    baseline_sha: String,
    /// The PR's current head — the second push.
    head_sha: String,
}

/// A local "origin" with `main` at `base_sha`, and `feature` two commits
/// ahead of it: `baseline_sha` (first push) then `head_sha` (second push),
/// with `refs/pull/1/head` left at `head_sha` (as if the reviewer visited
/// after the first push and is now re-reviewing after the second).
fn setup() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    std::fs::create_dir_all(&origin).unwrap();
    git(&origin, &["init", "-q", "-b", "main"]);
    git(&origin, &["config", "user.email", "a@b.com"]);
    git(&origin, &["config", "user.name", "test"]);

    let base = notes_txt("r1-two", "r2-two", "r3-two", "r4-two");
    std::fs::write(origin.join("notes.txt"), &base).unwrap();
    git(&origin, &["add", "-A"]);
    git(&origin, &["commit", "-q", "-m", "base"]);
    let base_sha = git_out(&origin, &["rev-parse", "HEAD"]);

    git(&origin, &["checkout", "-q", "-b", "feature"]);
    let push1 = notes_txt("r1-two V1", "r2-two", "r3-two CHANGED", "r4-two CHANGED");
    std::fs::write(origin.join("notes.txt"), &push1).unwrap();
    git(&origin, &["add", "-A"]);
    git(&origin, &["commit", "-q", "-m", "push 1"]);
    let baseline_sha = git_out(&origin, &["rev-parse", "HEAD"]);

    // r1 changes again, r2 changes for the first time, r3 reverts to base,
    // r4 stays exactly as push 1 left it.
    let push2 = notes_txt("r1-two V2", "r2-two CHANGED", "r3-two", "r4-two CHANGED");
    std::fs::write(origin.join("notes.txt"), &push2).unwrap();
    git(&origin, &["add", "-A"]);
    git(&origin, &["commit", "-q", "-m", "push 2"]);
    let head_sha = git_out(&origin, &["rev-parse", "HEAD"]);
    git(&origin, &["update-ref", "refs/pull/1/head", &head_sha]);
    git(&origin, &["checkout", "-q", "main"]);

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
        baseline_sha,
        head_sha,
    }
}

#[tokio::test]
async fn interdiff_classifies_every_hunk_against_real_git_history() {
    let fx = setup();
    let f = fixtures();
    let git_cli = theodore_git::GitCli::new(fx.root.clone(), std::sync::Arc::new(NoAuth));
    let deps = crate::Deps {
        git: std::sync::Arc::new(git_cli),
        ..f.deps()
    };

    let pr_ref = testkit::sample_pr_ref("acme", "widget", 1);
    assert_eq!(pr_ref.repo, fx.repo);
    let base = Sha::new(fx.base_sha.clone());
    let head = Sha::new(fx.head_sha.clone());
    let detail = testkit::sample_pr_detail(&pr_ref, &base, &head);
    f.host.push_get_pr(Ok(Fetched::Modified {
        etag: Some("etag-1".into()),
        value: detail,
    }));

    let (core, _recv) = Core::start(deps);
    let resp = core
        .open_pr(OpenPrRequest {
            url: "https://github.com/acme/widget/pull/1".into(),
        })
        .await
        .expect("open_pr");
    assert_eq!(resp.head, head);

    let entries = core
        .interdiff(InterdiffRequest {
            pr: resp.pr,
            base_sha: Sha::new(fx.baseline_sha.clone()),
            file: None,
        })
        .await
        .expect("interdiff");

    assert_eq!(entries.len(), 4, "one entry per region: {entries:?}");
    assert!(entries.iter().all(|e| e.path == "notes.txt"));

    let changed: Vec<_> = entries
        .iter()
        .filter(|e| e.class == InterdiffClass::Changed)
        .collect();
    assert_eq!(changed.len(), 1, "r1 changed again: {entries:?}");
    assert!(changed[0].previous.is_some());
    let deleted: String = changed[0]
        .words
        .iter()
        .filter(|w| w.kind == LineKind::Del)
        .map(|w| w.text.as_str())
        .collect();
    let added: String = changed[0]
        .words
        .iter()
        .filter(|w| w.kind == LineKind::Add)
        .map(|w| w.text.as_str())
        .collect();
    assert!(deleted.contains("V1"), "deleted word span: {deleted:?}");
    assert!(added.contains("V2"), "added word span: {added:?}");

    let new: Vec<_> = entries
        .iter()
        .filter(|e| e.class == InterdiffClass::New)
        .collect();
    assert_eq!(new.len(), 1, "r2 is new since the baseline: {entries:?}");
    assert!(new[0].previous.is_none());

    let vanished: Vec<_> = entries
        .iter()
        .filter(|e| e.class == InterdiffClass::Vanished)
        .collect();
    assert_eq!(
        vanished.len(),
        1,
        "r3 reverted to base, no longer in the diff: {entries:?}"
    );

    let unchanged: Vec<_> = entries
        .iter()
        .filter(|e| e.class == InterdiffClass::Unchanged)
        .collect();
    assert_eq!(unchanged.len(), 1, "r4 settled after push 1: {entries:?}");
}

#[tokio::test]
async fn interdiff_file_filter_excludes_other_paths() {
    let fx = setup();
    let f = fixtures();
    let git_cli = theodore_git::GitCli::new(fx.root.clone(), std::sync::Arc::new(NoAuth));
    let deps = crate::Deps {
        git: std::sync::Arc::new(git_cli),
        ..f.deps()
    };

    let pr_ref = testkit::sample_pr_ref("acme", "widget", 1);
    let base = Sha::new(fx.base_sha.clone());
    let head = Sha::new(fx.head_sha.clone());
    let detail = testkit::sample_pr_detail(&pr_ref, &base, &head);
    f.host.push_get_pr(Ok(Fetched::Modified {
        etag: Some("etag-1".into()),
        value: detail,
    }));

    let (core, _recv) = Core::start(deps);
    let resp = core
        .open_pr(OpenPrRequest {
            url: "https://github.com/acme/widget/pull/1".into(),
        })
        .await
        .expect("open_pr");

    let none = core
        .interdiff(InterdiffRequest {
            pr: resp.pr.clone(),
            base_sha: Sha::new(fx.baseline_sha.clone()),
            file: Some("other.txt".into()),
        })
        .await
        .expect("interdiff");
    assert!(none.is_empty());

    let some = core
        .interdiff(InterdiffRequest {
            pr: resp.pr,
            base_sha: Sha::new(fx.baseline_sha.clone()),
            file: Some("notes.txt".into()),
        })
        .await
        .expect("interdiff");
    assert_eq!(some.len(), 4);
}

/// A last-seen sha that isn't in the repo (force-pushed away, a fresh
/// clone) answers a typed "commit not found" the UI falls back from by
/// forgetting it, not git's "Invalid symmetric difference expression".
#[tokio::test]
async fn interdiff_from_a_commit_not_in_the_repo_is_commit_not_found() {
    let fx = setup();
    let f = fixtures();
    let git_cli = theodore_git::GitCli::new(fx.root.clone(), std::sync::Arc::new(NoAuth));
    let deps = crate::Deps {
        git: std::sync::Arc::new(git_cli),
        ..f.deps()
    };

    let pr_ref = testkit::sample_pr_ref("acme", "widget", 1);
    let base = Sha::new(fx.base_sha.clone());
    let head = Sha::new(fx.head_sha.clone());
    let detail = testkit::sample_pr_detail(&pr_ref, &base, &head);
    f.host.push_get_pr(Ok(Fetched::Modified {
        etag: Some("etag-1".into()),
        value: detail,
    }));

    let (core, _recv) = Core::start(deps);
    let resp = core
        .open_pr(OpenPrRequest {
            url: "https://github.com/acme/widget/pull/1".into(),
        })
        .await
        .expect("open_pr");

    let stale = "b6cef3e".repeat(5) + "b6cef";
    let err = core
        .interdiff(InterdiffRequest {
            pr: resp.pr.clone(),
            base_sha: Sha::new(stale.clone()),
            file: None,
        })
        .await
        .unwrap_err();
    assert!(
        matches!(
            &err,
            crate::AppError::Git(theodore_ports::GitError::MissingCommit(s)) if *s == stale
        ),
        "{err:?}"
    );
    assert!(err.is_expected());
}
