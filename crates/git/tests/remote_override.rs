//! `GitCli::with_remote_urls`: every clone, fetch and push goes to the
//! override (a local bare repo here), never to the host's real URL.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use tempfile::TempDir;
use theodore_domain::{HostKind, PrRef, RepoRef};
use theodore_git::GitCli;
use theodore_ports::{CredentialSource, GitPort};

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

fn repo() -> RepoRef {
    RepoRef {
        host: HostKind::Github,
        owner: "acme".into(),
        name: "widget".into(),
    }
}

/// A bare "remote" with `main` and a PR branch, plus `refs/pull/1/head`.
fn bare_remote(tmp: &Path) -> (std::path::PathBuf, String) {
    let work = tmp.join("work");
    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "-q", "-b", "main"]);
    git(&work, &["config", "user.email", "a@b.com"]);
    git(&work, &["config", "user.name", "test"]);
    std::fs::write(work.join("a.txt"), "one\n").unwrap();
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-q", "-m", "base"]);
    git(&work, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(work.join("a.txt"), "one\ntwo\n").unwrap();
    git(&work, &["commit", "-qam", "pr"]);
    let head = git(&work, &["rev-parse", "HEAD"]);
    let bare = tmp.join("remote.git");
    git(
        tmp,
        &["clone", "-q", "--bare", "work", bare.to_str().unwrap()],
    );
    git(&bare, &["update-ref", "refs/pull/1/head", &head]);
    (bare, head)
}

#[tokio::test]
async fn clone_fetch_and_push_go_to_the_override() {
    let tmp = TempDir::new().unwrap();
    let (bare, head) = bare_remote(tmp.path());
    let url = format!("file://{}", bare.display());
    let git_cli = GitCli::new(tmp.path().join("root"), Arc::new(NoAuth))
        .with_remote_urls(Arc::new(move |_r: &RepoRef| Some(url.clone())));

    let repo_dir = git_cli.ensure_repo(&repo()).await.expect("clone");
    assert!(repo_dir.join("HEAD").exists());
    // Second call takes the fetch path, also against the override.
    git_cli.ensure_repo(&repo()).await.expect("fetch");

    let pr = PrRef {
        repo: repo(),
        number: 1,
        host_id: None,
    };
    let sha = git_cli.fetch_pr(&pr).await.expect("fetch pr");
    assert_eq!(sha.as_str(), head);

    let wt = git_cli.worktree(&pr, &sha).await.expect("worktree");
    std::fs::write(wt.path.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    let me = theodore_ports::Identity {
        name: "Pat Author".into(),
        email: "pat@example.com".into(),
    };
    let pushed = git_cli
        .commit_and_push(&wt.path, &repo(), "feature", "fix: three", &me)
        .await
        .expect("push");
    assert_eq!(git(&bare, &["rev-parse", "refs/heads/feature"]), pushed.0);
    assert_eq!(
        git(
            &bare,
            &[
                "log",
                "-1",
                "--format=%an <%ae> / %cn <%ce>",
                "refs/heads/feature"
            ]
        ),
        "Pat Author <pat@example.com> / Pat Author <pat@example.com>",
        "authored and committed as the user"
    );
}

#[tokio::test]
async fn an_override_without_this_repo_never_falls_back_to_the_host() {
    let tmp = TempDir::new().unwrap();
    let git_cli = GitCli::new(tmp.path().join("root"), Arc::new(NoAuth))
        .with_remote_urls(Arc::new(|_r: &RepoRef| None));
    let err = git_cli.ensure_repo(&repo()).await.unwrap_err();
    assert!(err.to_string().contains("no remote configured"), "{err}");
}
