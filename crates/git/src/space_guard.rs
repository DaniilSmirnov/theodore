//! Guards for every git command Theodore runs inside a space worktree
//! (docs/spaces.md §5 Safety, §6). The repository git resolves there must
//! be that very worktree, or `add -A` / `reset --hard` / `clean` would act
//! on whatever repo sits above it (a git-controlled home, say); and nothing
//! is committed or reset while a git operation is unfinished in it.
//!
//! ```text
//! cwd = <root>/spaces/<id>, GIT_CEILING_DIRECTORIES = <root>/spaces
//!   `.git` a file, rev-parse --show-toplevel == the worktree,
//!   common dir under <root>/repos                    ─▶ run the command
//!   anything else                                    ─▶ NotAWorktree, nothing run
//! ```

use std::path::{Path, PathBuf};

use theodore_ports::{GitError, GitResult, WorktreeBusy};

use crate::GitCli;
use crate::process::{GitProc, RawOutput};

/// `git` with cwd = one space worktree, never discovering a repository
/// above it.
pub(crate) struct SpaceGit<'a> {
    worktree: &'a Path,
    askpass: &'a Path,
    /// Git may look in the worktree itself, never in its parent or above.
    ceiling: PathBuf,
    /// Theodore's bare caches: a space worktree belongs to one of them.
    repos: PathBuf,
}

impl GitCli {
    pub(crate) fn space_git<'a>(&self, worktree: &'a Path, askpass: &'a Path) -> SpaceGit<'a> {
        let ceiling = worktree.parent().unwrap_or(worktree);
        SpaceGit {
            worktree,
            askpass,
            ceiling: physical(ceiling),
            repos: physical(&self.repos_root()),
        }
    }
}

/// The path with symlinks resolved; as given when it doesn't exist.
fn physical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

impl SpaceGit<'_> {
    fn proc<'b>(&'b self, args: &'b [&'b str]) -> GitProc<'b> {
        GitProc {
            cwd: self.worktree,
            askpass: self.askpass,
            args,
            token: None,
            stdin: None,
        }
    }

    pub(crate) async fn run(&self, args: &[&str]) -> GitResult<Vec<u8>> {
        self.proc(args).run_below(&self.ceiling).await
    }

    pub(crate) async fn run_text(&self, args: &[&str]) -> GitResult<String> {
        Ok(String::from_utf8_lossy(&self.run(args).await?).into_owned())
    }

    /// `run` with extra variables on the child (`GIT_INDEX_FILE`).
    pub(crate) async fn run_env(&self, args: &[&str], env: &[(&str, &Path)]) -> GitResult<Vec<u8>> {
        self.proc(args).run_env_below(&self.ceiling, env).await
    }

    pub(crate) async fn run_raw(&self, args: &[&str]) -> GitResult<RawOutput> {
        self.proc(args).run_raw_below(&self.ceiling).await
    }

    fn lost(&self, why: &str) -> GitError {
        GitError::NotAWorktree(format!(
            "{} is no longer the space's git worktree ({why}); nothing was changed",
            self.worktree.display()
        ))
    }

    /// `Ok` only when git, run in the worktree, resolves exactly that
    /// worktree of one of Theodore's caches. Read only.
    pub(crate) async fn verify_root(&self) -> GitResult<()> {
        // A real directory, not a link to one somewhere else.
        match std::fs::symlink_metadata(self.worktree) {
            Ok(m) if m.file_type().is_dir() => {}
            Ok(_) => return Err(self.lost("it is not a directory")),
            Err(_) => return Err(self.lost("it is gone")),
        }
        // A linked worktree's `.git` is a file; a directory there is a
        // repository someone created in its place.
        if !self.worktree.join(".git").is_file() {
            return Err(self.lost("its .git file is missing"));
        }
        let out = self
            .run_raw(&[
                "rev-parse",
                "--path-format=absolute",
                "--show-toplevel",
                "--git-common-dir",
            ])
            .await?;
        if out.status != 0 {
            return Err(self.lost("git finds no repository there"));
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let mut lines = text.lines().map(|l| physical(Path::new(l.trim())));
        let (Some(top), Some(common)) = (lines.next(), lines.next()) else {
            return Err(self.lost("git finds no repository there"));
        };
        if top != physical(self.worktree) {
            return Err(self.lost(&format!("git resolves {} instead", top.display())));
        }
        if !common.starts_with(&self.repos) {
            return Err(self.lost(&format!(
                "it belongs to {}, not to Theodore's cache",
                common.display()
            )));
        }
        Ok(())
    }

    /// Whether a merge is in progress (`MERGE_HEAD`). Read only; call
    /// after `verify_root`.
    pub(crate) async fn merging(&self) -> GitResult<bool> {
        let p = self
            .run_text(&[
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                "MERGE_HEAD",
            ])
            .await?;
        let p = Path::new(p.trim());
        Ok(if p.is_absolute() {
            p.exists()
        } else {
            self.worktree.join(p).exists()
        })
    }

    /// The git operation unfinished in the worktree, if any. Read only;
    /// call after `verify_root`.
    pub(crate) async fn busy(&self) -> GitResult<Option<WorktreeBusy>> {
        const MARKERS: [(&str, WorktreeBusy); 5] = [
            ("rebase-merge", WorktreeBusy::Rebase),
            ("rebase-apply", WorktreeBusy::Rebase),
            ("MERGE_HEAD", WorktreeBusy::Merge),
            ("CHERRY_PICK_HEAD", WorktreeBusy::CherryPick),
            ("REVERT_HEAD", WorktreeBusy::Revert),
        ];
        let mut args = vec!["rev-parse", "--path-format=absolute"];
        for (name, _) in MARKERS {
            args.extend(["--git-path", name]);
        }
        let paths = self.run_text(&args).await?;
        for (line, (_, busy)) in paths.lines().zip(MARKERS) {
            let p = Path::new(line.trim());
            let p = if p.is_absolute() {
                p.to_path_buf()
            } else {
                self.worktree.join(p)
            };
            if p.exists() {
                return Ok(Some(busy));
            }
        }
        let unmerged = self.run(&["ls-files", "-u", "-z"]).await?;
        Ok((!unmerged.is_empty()).then_some(WorktreeBusy::Conflicts))
    }
}
