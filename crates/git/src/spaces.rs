//! Space worktrees (docs/spaces.md §6): one detached worktree per space
//! under `<root>/spaces/<id>`, with its checkpoints on a private ref
//! `refs/theodore/spaces/<id>` in the bare cache.
//!
//! Detached, not a branch: the cache force-fetches `refs/heads/*` with
//! `--prune`, which would refuse to touch a branch checked out in a worktree
//! and would delete one that was never pushed. Lives outside `<root>/wt`, so
//! the PR worktree sweep (`gc_worktrees`) never sees it.

use std::path::{Path, PathBuf};

use theodore_domain::Sha;
use theodore_ports::{GitError, GitResult, Worktree, space_ref};

use crate::GitCli;
use crate::process::GitProc;
use crate::safety::END_OF_OPTIONS;
use crate::space_guard::SpaceGit;

/// A space id becomes a path component and part of a ref name: lowercase
/// ascii letters, digits and `-` only, never leading with `-`.
pub(crate) fn validate_space_id(id: &str) -> GitResult<()> {
    let ok = !id.is_empty()
        && id.len() <= 64
        && !id.starts_with('-')
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(GitError::Parse(format!("bad space id {id:?}")))
    }
}

impl GitCli {
    pub(crate) fn spaces_root(&self) -> PathBuf {
        self.root().join("spaces")
    }

    fn space_path(&self, id: &str) -> GitResult<PathBuf> {
        validate_space_id(id)?;
        Ok(self.spaces_root().join(id))
    }

    pub(crate) async fn space_worktree_op(
        &self,
        repo: &Path,
        id: &str,
        base: &Sha,
    ) -> GitResult<Worktree> {
        let askpass = self.ensure_askpass()?;
        let path = self.space_path(id)?;
        let refname = space_ref(id);
        std::fs::create_dir_all(self.spaces_root())?;
        // Worktree before repo, as everywhere (`locks`).
        let _wt = crate::locks::lock(&path).await;

        if std::fs::symlink_metadata(&path).is_ok() {
            let git = self.space_git(&path, &askpass);
            match git.verify_root().await {
                Ok(()) => {
                    let head = git.run_text(&["rev-parse", "--verify", "HEAD"]).await?;
                    return Ok(Worktree {
                        path,
                        head: Sha::new(head.trim().to_string()),
                    });
                }
                // Its `.git` is gone: whatever is there is kept, out of
                // the way, and the worktree comes back from the ref.
                Err(GitError::NotAWorktree(_)) => {
                    self.move_aside(&path, id)?;
                }
                Err(e) => return Err(e),
            }
        }

        let _repo = crate::locks::lock(repo).await;
        // Resume from the last checkpoint when the space already has one
        // (its worktree was archived or deleted by hand), else start at base.
        let existing = GitProc {
            cwd: repo,
            askpass: &askpass,
            args: &["rev-parse", "--verify", "--quiet", END_OF_OPTIONS, &refname],
            token: None,
            stdin: None,
        }
        .run_raw()
        .await?;
        let start = if existing.status == 0 {
            String::from_utf8_lossy(&existing.stdout).trim().to_string()
        } else {
            base.as_str().to_string()
        };

        let path_str = path
            .to_str()
            .ok_or_else(|| GitError::Parse("non-utf8 space path".into()))?
            .to_string();
        let add = ["worktree", "add", "--detach", path_str.as_str(), &start];
        let first = GitProc {
            cwd: repo,
            askpass: &askpass,
            args: &add,
            token: None,
            stdin: None,
        }
        .run()
        .await;
        if first.is_err() {
            // A stale administrative entry for a directory removed by hand.
            let _ = GitProc {
                cwd: repo,
                askpass: &askpass,
                args: &["worktree", "prune"],
                token: None,
                stdin: None,
            }
            .run()
            .await;
            GitProc {
                cwd: repo,
                askpass: &askpass,
                args: &add,
                token: None,
                stdin: None,
            }
            .run()
            .await?;
        }
        GitProc {
            cwd: repo,
            askpass: &askpass,
            args: &["update-ref", &refname, &start],
            token: None,
            stdin: None,
        }
        .run()
        .await?;
        Ok(Worktree {
            path,
            head: Sha::new(start),
        })
    }

    pub(crate) async fn space_checkpoint_op(
        &self,
        worktree: &Path,
        id: &str,
        message: &str,
    ) -> GitResult<Sha> {
        validate_space_id(id)?;
        let askpass = self.ensure_askpass()?;
        let _wt = crate::locks::lock(worktree).await;
        let git = self.space_git(worktree, &askpass);
        git.verify_root().await?;
        // Mid-rebase or with conflict markers in the files, a snapshot
        // would record half an operation: the next turn checkpoints it.
        if let Some(busy) = git.busy().await? {
            return Err(GitError::Busy(busy));
        }
        snapshot(&git, id, message).await
    }

    pub(crate) async fn worktree_busy_op(
        &self,
        worktree: &Path,
    ) -> GitResult<Option<theodore_ports::WorktreeBusy>> {
        let askpass = self.ensure_askpass()?;
        let git = self.space_git(worktree, &askpass);
        git.verify_root().await?;
        git.busy().await
    }

    /// Moves a space directory that isn't its worktree any more to
    /// `<root>/spaces/.moved-aside/<id>-<millis>`: never deleted, since
    /// nothing proves it is Theodore's. Returns where it went.
    fn move_aside(&self, path: &Path, id: &str) -> GitResult<PathBuf> {
        let aside = self.spaces_root().join(".moved-aside");
        std::fs::create_dir_all(&aside)?;
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or_default();
        let to = aside.join(format!("{id}-{millis}"));
        std::fs::rename(path, &to)?;
        tracing::warn!(
            error.code = "space.worktree_moved_aside",
            space = id,
            to = %to.display(),
            "space directory was not its git worktree; moved aside"
        );
        Ok(to)
    }

    pub(crate) async fn space_reset_op(
        &self,
        worktree: &Path,
        id: &str,
        to: &Sha,
    ) -> GitResult<()> {
        validate_space_id(id)?;
        let askpass = self.ensure_askpass()?;
        let refname = space_ref(id);
        let _wt = crate::locks::lock(worktree).await;
        let git = self.space_git(worktree, &askpass);
        git.verify_root().await?;
        // Undo would throw the unfinished operation away with the turn.
        if let Some(busy) = git.busy().await? {
            return Err(GitError::Busy(busy));
        }
        let steps: [&[&str]; 3] = [
            &["reset", "-q", "--hard", to.as_str()],
            // Untracked files the turn left behind; ignored ones (deps,
            // build output) stay, so the next turn needn't reinstall.
            &["clean", "-q", "-fd"],
            &["update-ref", &refname, to.as_str()],
        ];
        for args in steps {
            git.run(args).await?;
        }
        Ok(())
    }

    /// `GitPort::space_snapshot`: a copy of the worktree's index (so `add`
    /// reuses its stat cache) under the worktree's git dir, `add -A` into
    /// it, then `write-tree` and `commit-tree`. No ref moves; the copy is
    /// removed again.
    pub(crate) async fn space_snapshot_op(&self, worktree: &Path) -> GitResult<Sha> {
        let askpass = self.ensure_askpass()?;
        let _wt = crate::locks::lock(worktree).await;
        let git = self.space_git(worktree, &askpass);
        git.verify_root().await?;
        let head = git.run_text(&["rev-parse", "--verify", "HEAD"]).await?;
        let head = head.trim().to_string();
        let git_dir = git.run_text(&["rev-parse", "--absolute-git-dir"]).await?;
        let git_dir = PathBuf::from(git_dir.trim());
        let index = git_dir.join("theodore-snapshot.index");
        let real = git_dir.join("index");
        let tree = async {
            if real.exists() {
                std::fs::copy(&real, &index)?;
            } else {
                let _ = std::fs::remove_file(&index);
                git.run_env(&["read-tree", "HEAD"], &[("GIT_INDEX_FILE", &index)])
                    .await?;
            }
            git.run_env(&["add", "-A"], &[("GIT_INDEX_FILE", &index)])
                .await?;
            let out = git
                .run_env(&["write-tree"], &[("GIT_INDEX_FILE", &index)])
                .await?;
            Ok::<_, GitError>(String::from_utf8_lossy(&out).trim().to_string())
        }
        .await;
        let _ = std::fs::remove_file(&index);
        let tree = tree?;
        let head_tree = git.run_text(&["rev-parse", "HEAD^{tree}"]).await?;
        if head_tree.trim() == tree {
            return Ok(Sha::new(head));
        }
        // Theodore's own identity, as for checkpoints: never the user's
        // signing or hooks for a commit no ref points at.
        let sha = git
            .run_text(&[
                "-c",
                "user.name=Theodore",
                "-c",
                "user.email=theodore@localhost",
                "commit-tree",
                &tree,
                "-p",
                &head,
                "-m",
                "snapshot of the worktree",
            ])
            .await?;
        Ok(Sha::new(sha.trim().to_string()))
    }

    pub(crate) async fn remove_space_worktree_op(&self, repo: &Path, id: &str) -> GitResult<()> {
        let askpass = self.ensure_askpass()?;
        let path = self.space_path(id)?;
        if std::fs::symlink_metadata(&path).is_err() {
            return Ok(());
        }
        let _wt = crate::locks::lock(&path).await;
        let _repo = crate::locks::lock(repo).await;
        match self.space_git(&path, &askpass).verify_root().await {
            Ok(()) => {
                if let Some(path_str) = path.to_str() {
                    let _ = GitProc {
                        cwd: repo,
                        askpass: &askpass,
                        args: &["worktree", "remove", "--force", path_str],
                        token: None,
                        stdin: None,
                    }
                    .run()
                    .await;
                }
                // Verified ours above: what `worktree remove` left goes.
                if path.exists() {
                    std::fs::remove_dir_all(&path)?;
                }
            }
            Err(GitError::NotAWorktree(_)) => {
                self.move_aside(&path, id)?;
            }
            Err(e) => return Err(e),
        }
        let _ = GitProc {
            cwd: repo,
            askpass: &askpass,
            args: &["worktree", "prune"],
            token: None,
            stdin: None,
        }
        .run()
        .await;
        Ok(())
    }
}

/// `add -A`, a commit when anything is staged (with a merge in progress,
/// its merge commit), and the space ref at `HEAD`: one checkpoint. The
/// caller holds the worktree lock and has run `verify_root`.
pub(crate) async fn snapshot(git: &SpaceGit<'_>, id: &str, message: &str) -> GitResult<Sha> {
    git.run(&["add", "-A"]).await?;
    // Exit 1 = something is staged.
    let staged = git.run_raw(&["diff", "--cached", "--quiet"]).await?;
    // A resolved merge may stage nothing new over `HEAD`: still a commit.
    if staged.status == 1 || (staged.status == 0 && git.merging().await?) {
        // Theodore's own snapshot: no user hooks or signing prompts on
        // every agent turn, a fixed identity like other Theodore commits.
        git.run(&[
            "-c",
            "user.name=Theodore",
            "-c",
            "user.email=theodore@localhost",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "-q",
            "--no-edit",
            "-m",
            message,
        ])
        .await?;
    } else if staged.status != 0 {
        return Err(GitError::Command {
            args: "diff --cached --quiet".into(),
            code: staged.status,
            stderr: String::from_utf8_lossy(&staged.stderr).into_owned(),
        });
    }
    let head = git.run_text(&["rev-parse", "--verify", "HEAD"]).await?;
    let head = head.trim().to_string();
    // Refs outside `refs/heads` and `refs/worktree` are shared by every
    // worktree, so this lands in the bare cache.
    git.run(&["update-ref", &space_ref(id), &head]).await?;
    Ok(Sha::new(head))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn space_ids_are_safe_path_and_ref_components() {
        assert!(validate_space_id("a1b2c3d4e5f6").is_ok());
        assert!(validate_space_id("fix-login").is_ok());
        for bad in ["", "-x", "../x", "a/b", "A", "a b", "a.lock"] {
            assert!(validate_space_id(bad).is_err(), "{bad:?}");
        }
    }
}
