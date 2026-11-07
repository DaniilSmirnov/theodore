//! Mirrors a local folder project into Theodore's own cache (plan
//! "projects"): a bare, hardlink-free clone, refreshed by fetching straight
//! from the folder's path. The folder's own `.git` is only ever read
//! (`clone`/`fetch` never write to the source) — never cloned with
//! hardlinks into it, never given a worktree, never touched by push.

use std::path::{Path, PathBuf};

use theodore_domain::RepoRef;
use theodore_ports::{GitError, GitResult, WorkingState};

use crate::GitCli;
use crate::process::GitProc;

impl GitCli {
    pub(crate) async fn ensure_local_mirror_op(
        &self,
        source: &Path,
        repo: &RepoRef,
    ) -> GitResult<PathBuf> {
        let askpass = self.ensure_askpass()?;
        let dir = self.repo_dir(repo)?;
        let source_str = source
            .to_str()
            .ok_or_else(|| GitError::Parse("non-utf8 source path".to_string()))?;
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let _repo = crate::locks::lock(&dir).await;

        if dir.join("HEAD").exists() {
            let args = ["fetch", source_str, "+refs/heads/*:refs/heads/*"];
            GitProc {
                cwd: &dir,
                askpass: &askpass,
                args: &args,
                token: None,
                stdin: None,
            }
            .run()
            .await?;
            return Ok(dir);
        }

        let dir_str = dir
            .to_str()
            .ok_or_else(|| GitError::Parse("non-utf8 repo path".to_string()))?;
        let args = ["clone", "--bare", "--no-hardlinks", source_str, dir_str];
        GitProc {
            cwd: self.root(),
            askpass: &askpass,
            args: &args,
            token: None,
            stdin: None,
        }
        .run()
        .await?;
        Ok(dir)
    }

    pub(crate) async fn is_repo_op(&self, path: &Path) -> GitResult<bool> {
        let askpass = self.ensure_askpass()?;
        let out = GitProc {
            cwd: path,
            askpass: &askpass,
            args: &["rev-parse", "--git-dir"],
            token: None,
            stdin: None,
        }
        .run()
        .await;
        Ok(out.is_ok())
    }

    pub(crate) async fn is_tracked_op(&self, folder: &Path, path: &str) -> GitResult<bool> {
        let askpass = self.ensure_askpass()?;
        let out = GitProc {
            cwd: folder,
            askpass: &askpass,
            args: &["ls-files", "--error-unmatch", "--", path],
            token: None,
            stdin: None,
        }
        .run_raw()
        .await?;
        Ok(out.status == 0)
    }

    pub(crate) async fn exclude_locally_op(&self, folder: &Path, pattern: &str) -> GitResult<()> {
        let askpass = self.ensure_askpass()?;
        let out = GitProc {
            cwd: folder,
            askpass: &askpass,
            args: &[
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                "info/exclude",
            ],
            token: None,
            stdin: None,
        }
        .run_text()
        .await?;
        let path = std::path::PathBuf::from(out.trim());
        let text = match tokio::fs::read_to_string(&path).await {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(GitError::Io(e)),
        };
        if text.lines().any(|l| l.trim() == pattern) {
            return Ok(());
        }
        if let Some(dir) = path.parent() {
            tokio::fs::create_dir_all(dir).await.map_err(GitError::Io)?;
        }
        let sep = if text.is_empty() || text.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        tokio::fs::write(&path, format!("{text}{sep}{pattern}\n"))
            .await
            .map_err(GitError::Io)
    }

    pub(crate) async fn remove_repo_op(&self, repo: &RepoRef) -> GitResult<()> {
        let askpass = self.ensure_askpass()?;
        let repo_dir = self.repo_dir(repo)?;
        for (dirname, meta) in crate::meta::list(&self.meta_root()) {
            if meta.repo == *repo {
                self.drop_worktree_dir(&askpass, &repo_dir, &dirname).await;
                crate::meta::remove(&self.meta_root(), &dirname);
            }
        }
        let _repo = crate::locks::lock(&repo_dir).await;
        if repo_dir.exists() {
            std::fs::remove_dir_all(&repo_dir)?;
        }
        Ok(())
    }

    /// Reads the folder's own checkout. Both commands are read-only:
    /// `symbolic-ref` with no new value only prints, and
    /// `--no-optional-locks` keeps `status` from refreshing (rewriting) the
    /// index the way a plain `git status` would.
    pub(crate) async fn working_state_op(&self, folder: &Path) -> GitResult<WorkingState> {
        let askpass = self.ensure_askpass()?;
        let head = GitProc {
            cwd: folder,
            askpass: &askpass,
            args: &["symbolic-ref", "--quiet", "--short", "HEAD"],
            token: None,
            stdin: None,
        }
        .run_raw()
        .await?;
        let current_branch = match head.status {
            0 => Some(String::from_utf8_lossy(&head.stdout).trim().to_string())
                .filter(|s| !s.is_empty()),
            // 1: HEAD isn't a symbolic ref (detached).
            1 => None,
            code => {
                return Err(GitError::Command {
                    args: "symbolic-ref --quiet --short HEAD".into(),
                    code,
                    stderr: String::from_utf8_lossy(&head.stderr).into_owned(),
                });
            }
        };
        let status = GitProc {
            cwd: folder,
            askpass: &askpass,
            args: &["--no-optional-locks", "status", "--porcelain=v1"],
            token: None,
            stdin: None,
        }
        .run_text()
        .await?;
        Ok(WorkingState {
            current_branch,
            dirty_files: count_status_entries(&status),
        })
    }
}

/// `status --porcelain=v1` prints one line per path (a rename is one
/// `old -> new` line; odd names are quoted, never split across lines).
fn count_status_entries(porcelain: &str) -> u32 {
    porcelain.lines().filter(|l| !l.trim().is_empty()).count() as u32
}

#[cfg(test)]
mod tests {
    use super::count_status_entries;

    #[test]
    fn counts_one_entry_per_porcelain_line() {
        assert_eq!(count_status_entries(""), 0);
        assert_eq!(
            count_status_entries(" M src/a.rs\nR  old.rs -> new.rs\n?? \"we\\nird\"\n"),
            3
        );
    }
}
