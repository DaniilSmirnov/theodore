//! `space_snapshot` on real git: the worktree's uncommitted edits (new,
//! changed and deleted files) become a commit no ref points at, while the
//! worktree's own index, `HEAD` and the checkpoint ref stay put.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use tempfile::TempDir;
use theodore_domain::{FileStatus, HostKind, RepoRef};
use theodore_git::GitCli;
use theodore_ports::{CredentialSource, DiffOptions, GitPort, space_ref};

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

#[tokio::test]
async fn a_snapshot_holds_uncommitted_edits_and_moves_nothing() {
    let tmp = TempDir::new().unwrap();
    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "-q", "-b", "main"]);
    git(&work, &["config", "user.email", "a@b.com"]);
    git(&work, &["config", "user.name", "test"]);
    std::fs::write(work.join("a.txt"), "one\n").unwrap();
    std::fs::write(work.join("b.txt"), "bee\n").unwrap();
    git(&work, &["add", "-A"]);
    git(
        &work,
        &["-c", "commit.gpgsign=false", "commit", "-q", "-m", "base"],
    );
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
    let wt = cli.space_worktree(&cache, "s1", &base).await.unwrap();

    // Nothing changed: the snapshot is `HEAD`.
    assert_eq!(cli.space_snapshot(&wt.path).await.unwrap(), base);

    std::fs::write(wt.path.join("a.txt"), "one\ntwo\n").unwrap();
    std::fs::write(wt.path.join("c.txt"), "new\n").unwrap();
    std::fs::remove_file(wt.path.join("b.txt")).unwrap();
    // A staged edit of the agent's own stays staged, and only that.
    git(&wt.path, &["add", "a.txt"]);
    let status_before = git(&wt.path, &["status", "--porcelain"]);

    let snap = cli.space_snapshot(&wt.path).await.unwrap();
    assert_ne!(snap, base);
    let rev = cli
        .diff(&cache, &base, &snap, &DiffOptions::default())
        .await
        .unwrap();
    let mut files: Vec<(&str, FileStatus)> = rev
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.status))
        .collect();
    files.sort_by_key(|f| f.0);
    assert_eq!(
        files,
        [
            ("a.txt", FileStatus::Modified),
            ("b.txt", FileStatus::Deleted),
            ("c.txt", FileStatus::Added),
        ]
    );

    assert_eq!(git(&wt.path, &["status", "--porcelain"]), status_before);
    assert_eq!(git(&wt.path, &["rev-parse", "HEAD"]), base.as_str());
    assert_eq!(git(&cache, &["rev-parse", &space_ref("s1")]), base.as_str());
    let git_dir = git(&wt.path, &["rev-parse", "--absolute-git-dir"]);
    assert!(!Path::new(&git_dir).join("theodore-snapshot.index").exists());
}
