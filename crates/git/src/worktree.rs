//! Worktree lifecycle: one detached worktree per PR under `<root>/wt`, with a
//! JSON sidecar (`meta.rs`) so `gc_worktrees` can route a stale directory
//! back to the bare repo that owns it.
//!
//! Never touches a user's own checkout or branches — every worktree here is
//! Theodore's own, detached, and disposable.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use theodore_domain::{PrRef, RepoRef, Sha};
use theodore_ports::{GitResult, Worktree};

use crate::GitCli;
use crate::locks;
use crate::meta::{self, WorktreeMeta};
use crate::process::GitProc;

impl GitCli {
    /// Commits every change in `worktree` as `author` and pushes it to
    /// `branch` on `repo`'s remote (plan §4 "Fix target": own or local PR).
    /// The worktree is always a detached, Theodore-owned checkout, so `git
    /// add -A` + commit is safe — there is no user branch to disturb.
    pub(crate) async fn commit_and_push_op(
        &self,
        worktree: &Path,
        repo: &RepoRef,
        branch: &str,
        message: &str,
        author: &theodore_ports::Identity,
    ) -> GitResult<Sha> {
        let name = format!("user.name={}", author.name);
        let email = format!("user.email={}", author.email);
        let askpass = self.ensure_askpass()?;
        GitProc {
            cwd: worktree,
            askpass: &askpass,
            args: &["add", "-A"],
            token: None,
            stdin: None,
        }
        .run()
        .await?;
        GitProc {
            cwd: worktree,
            askpass: &askpass,
            args: &["-c", &name, "-c", &email, "commit", "-m", message],
            token: None,
            stdin: None,
        }
        .run()
        .await?;
        let token = self.token_for(repo);
        let refspec = format!("HEAD:refs/heads/{branch}");
        let remote = self.remote_override(repo)?;
        GitProc {
            cwd: worktree,
            askpass: &askpass,
            args: &["push", remote.as_deref().unwrap_or("origin"), &refspec],
            token: token.as_deref(),
            stdin: None,
        }
        .run()
        .await?;
        let out = GitProc {
            cwd: worktree,
            askpass: &askpass,
            args: &["rev-parse", "HEAD"],
            token: None,
            stdin: None,
        }
        .run_text()
        .await?;
        Ok(Sha::new(out.trim().to_string()))
    }
}

impl GitCli {
    pub(crate) async fn worktree_op(&self, pr: &PrRef, head: &Sha) -> GitResult<Worktree> {
        let askpass = self.ensure_askpass()?;
        let repo_dir = self.repo_dir(&pr.repo)?;
        let wt_path = self.wt_path(pr)?;
        let dirname = Self::wt_dirname(&pr.repo, pr.number)?;
        let head_str = head.as_str().to_string();
        // One checkout at a time per worktree (`locks`): two at once fight
        // over its `index.lock`, and whichever ran last would win anyway.
        if let Some(parent) = wt_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let _wt = locks::lock(&wt_path).await;

        if wt_path.join(".git").exists() {
            // Theodore's own throwaway worktree: moving it to a new head with
            // a hard reset is safe (never the user's checkout or branch).
            GitProc {
                cwd: &wt_path,
                askpass: &askpass,
                args: &["checkout", "--detach", &head_str],
                token: None,
                stdin: None,
            }
            .run()
            .await?;
            GitProc {
                cwd: &wt_path,
                askpass: &askpass,
                args: &["reset", "--hard", &head_str],
                token: None,
                stdin: None,
            }
            .run()
            .await?;
            GitProc {
                cwd: &wt_path,
                askpass: &askpass,
                args: &["clean", "-fdx"],
                token: None,
                stdin: None,
            }
            .run()
            .await?;
        } else {
            // `worktree add` writes the bare repo's `worktrees/` admin dir,
            // which a concurrent `worktree prune` would sweep.
            let _repo = locks::lock(&repo_dir).await;
            let path_str = wt_path
                .to_str()
                .ok_or_else(|| {
                    theodore_ports::GitError::Parse("non-utf8 worktree path".to_string())
                })?
                .to_string();
            let args = [
                "worktree",
                "add",
                "--detach",
                path_str.as_str(),
                head_str.as_str(),
            ];
            let add_result = GitProc {
                cwd: &repo_dir,
                askpass: &askpass,
                args: &args,
                token: None,
                stdin: None,
            }
            .run()
            .await;
            if add_result.is_err() {
                // Stale administrative entry from a worktree dir removed by
                // hand outside Theodore; prune once and retry.
                let _ = GitProc {
                    cwd: &repo_dir,
                    askpass: &askpass,
                    args: &["worktree", "prune"],
                    token: None,
                    stdin: None,
                }
                .run()
                .await;
                GitProc {
                    cwd: &repo_dir,
                    askpass: &askpass,
                    args: &args,
                    token: None,
                    stdin: None,
                }
                .run()
                .await?;
            }
            meta::write(
                &self.meta_root(),
                &dirname,
                &WorktreeMeta {
                    repo: pr.repo.clone(),
                    number: pr.number,
                },
            )?;
        }

        Ok(Worktree {
            path: wt_path,
            head: head.clone(),
        })
    }

    pub(crate) async fn remove_worktree_op(&self, pr: &PrRef) -> GitResult<()> {
        let askpass = self.ensure_askpass()?;
        let dirname = Self::wt_dirname(&pr.repo, pr.number)?;
        let repo_dir = self.repo_dir(&pr.repo)?;
        self.drop_worktree_dir(&askpass, &repo_dir, &dirname).await;
        meta::remove(&self.meta_root(), &dirname);
        Ok(())
    }

    pub(crate) async fn gc_worktrees_op(&self, keep: &[PrRef]) -> GitResult<u32> {
        let askpass = self.ensure_askpass()?;
        let meta_root = self.meta_root();
        // A `keep` entry whose `RepoRef` fails validation can't have a real
        // worktree dirname either; dropping it from the set only risks an
        // over-eager sweep of a worktree that could never have existed.
        let keep_names: HashSet<String> = keep
            .iter()
            .filter_map(|pr| Self::wt_dirname(&pr.repo, pr.number).ok())
            .collect();
        let mut removed = 0u32;
        let mut handled: HashSet<String> = HashSet::new();

        for (dirname, wt_meta) in meta::list(&meta_root) {
            handled.insert(dirname.clone());
            if keep_names.contains(&dirname) {
                continue;
            }
            // Theodore only ever wrote this sidecar after a successful
            // `repo_dir` validation; a failure here means the data is
            // foreign or stale — skip it rather than abort the whole sweep.
            let Ok(repo_dir) = self.repo_dir(&wt_meta.repo) else {
                tracing::warn!(%dirname, "skipping gc: sidecar has an unsafe RepoRef");
                continue;
            };
            self.drop_worktree_dir(&askpass, &repo_dir, &dirname).await;
            meta::remove(&meta_root, &dirname);
            removed += 1;
        }

        // Directories with no sidecar (e.g. left by a crashed run) can't be
        // routed back to their bare repo; reclaim the disk space directly and
        // let the `worktree prune` sweep below clean the administrative side.
        if let Ok(entries) = std::fs::read_dir(self.wt_root()) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if name == ".meta" || keep_names.contains(name) || handled.contains(name) {
                    continue;
                }
                if std::fs::remove_dir_all(&path).is_ok() {
                    removed += 1;
                }
            }
        }

        self.prune_all_repos(&askpass).await;
        Ok(removed)
    }

    pub(crate) async fn drop_worktree_dir(&self, askpass: &Path, repo_dir: &Path, dirname: &str) {
        let wt_path = self.wt_root().join(dirname);
        if !wt_path.exists() {
            return;
        }
        let _wt = locks::lock(&wt_path).await;
        let _repo = locks::lock(repo_dir).await;
        if let Some(path_str) = wt_path.to_str() {
            let _ = GitProc {
                cwd: repo_dir,
                askpass,
                args: &["worktree", "remove", "--force", path_str],
                token: None,
                stdin: None,
            }
            .run()
            .await;
        }
        if wt_path.exists() {
            let _ = std::fs::remove_dir_all(&wt_path);
        }
    }

    async fn prune_all_repos(&self, askpass: &Path) {
        for repo_dir in self.all_repo_dirs() {
            let _repo = locks::lock(&repo_dir).await;
            let _ = GitProc {
                cwd: &repo_dir,
                askpass,
                args: &["worktree", "prune"],
                token: None,
                stdin: None,
            }
            .run()
            .await;
        }
    }

    /// Every `repos/<host>/<owner>/<repo>.git` bare repo Theodore manages.
    fn all_repo_dirs(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let Ok(hosts) = std::fs::read_dir(self.repos_root()) else {
            return out;
        };
        for host in hosts.flatten() {
            let Ok(owners) = std::fs::read_dir(host.path()) else {
                continue;
            };
            for owner in owners.flatten() {
                let Ok(repos) = std::fs::read_dir(owner.path()) else {
                    continue;
                };
                for repo in repos.flatten() {
                    let p = repo.path();
                    if p.extension().and_then(|e| e.to_str()) == Some("git") {
                        out.push(p);
                    }
                }
            }
        }
        out
    }
}
