//! Mirroring a space into the user's own local folder (docs/spaces.md
//! "Mirror into a local folder"): some projects only run from their own
//! path (fixed ports, one local database, an IDE run config bound to it),
//! so instead of reviewing the space's worktree, the user runs the project
//! from the folder itself while the space keeps writing into it.
//!
//! ```text
//! Mirror on  ─▶ folder clean ─▶ remember its HEAD + branch
//!            ─▶ write the space's checkpoint into the folder (every path)
//! turn ends / "Sync now" ─▶ re-sync: only the paths that changed are rewritten
//! Mirror off ─▶ restore every mirrored path to its content at the remembered HEAD
//!            ─▶ refused if the folder disagrees with what mirroring wrote there
//!               (unless Discard)
//! ```
//!
//! No index of the folder's own is ever opened. Every operation below
//! fetches the space's checkpoint ref into the folder first (objects only,
//! like `merge_into_folder`/`write_folder_branch`: no `FETCH_HEAD`, no new
//! ref), then reads and writes through a temporary index
//! (`GIT_INDEX_FILE`) built with `read-tree` from one tree at a time —
//! the folder's real index, branch and `HEAD` are never opened. Which
//! paths to touch always comes from `git diff --name-only` between two
//! commits (cheap: both are already local), never from walking the
//! working tree by hand.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use theodore_domain::Sha;
use theodore_ports::{GitError, GitResult};

use crate::GitCli;
use crate::config_file::segments;
use crate::process::{GitProc, RawOutput, io_err, isolated};
use crate::safety::END_OF_OPTIONS;

fn proc<'a>(cwd: &'a Path, askpass: &'a Path, args: &'a [&'a str]) -> GitProc<'a> {
    GitProc {
        cwd,
        askpass,
        args,
        token: None,
        stdin: None,
    }
}

/// Same rules as a space id (`spaces::validate_space_id`), kept local so a
/// mirror op never builds a ref from an unchecked id.
fn check_space(id: &str) -> GitResult<()> {
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

/// The path with symlinks resolved; as given when it doesn't exist.
fn physical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// `git` run with a caller-chosen env on top of the usual safety defaults,
/// no askpass and no token: every call here is local index/tree plumbing,
/// never the network.
async fn run_env(cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> GitResult<RawOutput> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.current_dir(cwd)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_PAGER", "cat")
        .env("LC_ALL", "C")
        .envs(isolated().then_some(("GIT_CONFIG_GLOBAL", "/dev/null")))
        .envs(envs.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = cmd.spawn().map_err(io_err)?;
    let output = child.wait_with_output().await.map_err(io_err)?;
    Ok(RawOutput {
        status: output.status.code().unwrap_or(-1),
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

fn require_ok(out: RawOutput, args: &str) -> GitResult<Vec<u8>> {
    if out.status == 0 {
        Ok(out.stdout)
    } else {
        Err(GitError::Command {
            args: args.into(),
            code: out.status,
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

/// A path under the OS temp dir no other call in this process can collide
/// with: the pid plus a counter, not a timestamp (two calls can land in
/// the same nanosecond).
fn temp_index_path() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!("theodore-mirror-index-{}-{n}", std::process::id()))
}

/// `-z`-terminated plain paths (`diff --name-only -z`, `ls-tree -z
/// --name-only`): split on NUL, drop the trailing empty field.
fn parse_paths_z(out: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(out)
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect()
}

impl GitCli {
    /// Brings the space's checkpoint objects into `folder` without writing
    /// `FETCH_HEAD` or any new ref there (same shape as `merge_into_folder`
    /// / `write_folder_branch`).
    async fn fetch_space_ref(&self, folder: &Path, repo: &Path, space: &str) -> GitResult<()> {
        let askpass = self.ensure_askpass()?;
        let refname = theodore_ports::space_ref(space);
        let repo_str = repo
            .to_str()
            .ok_or_else(|| GitError::Parse("non-utf8 repo path".to_string()))?;
        proc(
            folder,
            &askpass,
            &[
                "fetch",
                "--no-tags",
                "--no-write-fetch-head",
                END_OF_OPTIONS,
                repo_str,
                &refname,
            ],
        )
        .run()
        .await?;
        Ok(())
    }

    /// `folder` is a real working copy, and git run there resolves exactly
    /// it — never a repository above it, never a worktree pointing
    /// elsewhere. Read only.
    async fn verify_folder(&self, folder: &Path) -> GitResult<()> {
        let askpass = self.ensure_askpass()?;
        let out = proc(
            folder,
            &askpass,
            &["rev-parse", "--path-format=absolute", "--show-toplevel"],
        )
        .run_raw()
        .await?;
        if out.status != 0 {
            return Err(GitError::NotAWorktree(format!(
                "{} is no longer a git working copy; nothing was changed",
                folder.display()
            )));
        }
        let top = physical(Path::new(String::from_utf8_lossy(&out.stdout).trim()));
        if top != physical(folder) {
            return Err(GitError::NotAWorktree(format!(
                "{} resolves {} instead of itself; nothing was changed",
                folder.display(),
                top.display()
            )));
        }
        Ok(())
    }

    /// Paths of `tree` (`ls-tree -r --name-only -z`), the mirrored
    /// membership check: is a path present at this commit at all.
    async fn tree_paths(&self, folder: &Path, tree: &Sha) -> GitResult<Vec<String>> {
        let askpass = self.ensure_askpass()?;
        let out = proc(
            folder,
            &askpass,
            &["ls-tree", "-r", "--name-only", "-z", tree.as_str()],
        )
        .run()
        .await?;
        Ok(parse_paths_z(&out))
    }

    /// Paths that differ between two commits (`git diff --name-only -z`):
    /// the set mirroring has ever touched between them, added, changed or
    /// removed alike.
    async fn mirror_diff(&self, folder: &Path, a: &Sha, b: &Sha) -> GitResult<Vec<String>> {
        let askpass = self.ensure_askpass()?;
        let out = proc(
            folder,
            &askpass,
            &["diff", "--name-only", "-z", a.as_str(), b.as_str()],
        )
        .run()
        .await?;
        Ok(parse_paths_z(&out))
    }

    /// Builds a temporary index at `tree`, runs `with_index` against it
    /// (`GIT_DIR`/`GIT_WORK_TREE` pointing at `folder`, `GIT_INDEX_FILE` at
    /// a path only this call uses), then removes the index file. The
    /// folder's own index is never opened.
    async fn with_tree_index<T>(
        &self,
        folder: &Path,
        tree: &Sha,
        with_index: impl AsyncFnOnce(&[(&str, &str)]) -> GitResult<T>,
    ) -> GitResult<T> {
        let askpass = self.ensure_askpass()?;
        let git_dir = proc(
            folder,
            &askpass,
            &["rev-parse", "--path-format=absolute", "--git-dir"],
        )
        .run_text()
        .await?
        .trim()
        .to_string();
        let work_tree = folder
            .to_str()
            .ok_or_else(|| GitError::Parse("non-utf8 folder path".to_string()))?
            .to_string();
        let index = temp_index_path();
        let index_str = index
            .to_str()
            .ok_or_else(|| GitError::Parse("non-utf8 temp index path".to_string()))?
            .to_string();
        let envs = [
            ("GIT_DIR", git_dir.as_str()),
            ("GIT_WORK_TREE", work_tree.as_str()),
            ("GIT_INDEX_FILE", index_str.as_str()),
        ];
        let result = async {
            let out = run_env(folder, &["read-tree", tree.as_str()], &envs).await?;
            require_ok(out, "read-tree")?;
            with_index(&envs).await
        }
        .await;
        let _ = std::fs::remove_file(&index);
        result
    }

    /// Checks out `paths` (already validated as safe components) from
    /// `tree` into `folder`'s working tree, through a temporary index that
    /// only this call ever opens. Paths absent from `tree` are left alone
    /// by this step — the caller removes those itself.
    async fn checkout_from_tree(&self, folder: &Path, tree: &Sha, paths: &[&str]) -> GitResult<()> {
        if paths.is_empty() {
            return Ok(());
        }
        self.with_tree_index(folder, tree, async |envs| {
            let mut args: Vec<&str> = vec!["checkout-index", "-f", END_OF_OPTIONS];
            args.extend(paths.iter().copied());
            let out = run_env(folder, &args, envs).await?;
            require_ok(out, "checkout-index").map(|_| ())
        })
        .await
    }

    /// `paths` in `folder`'s working tree whose content no longer matches
    /// `tree` (a temporary index of `tree`, then `git diff --name-only -z`
    /// against the working tree: no revision but a pathspec, the default
    /// "index vs worktree" comparison).
    async fn differs_from_tree(
        &self,
        folder: &Path,
        tree: &Sha,
        paths: &[&str],
    ) -> GitResult<Vec<String>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        self.with_tree_index(folder, tree, async |envs| {
            let mut args: Vec<&str> = vec!["diff", "--name-only", "-z", END_OF_OPTIONS];
            args.extend(paths.iter().copied());
            let out = run_env(folder, &args, envs).await?;
            let bytes = require_ok(out, "diff")?;
            Ok(parse_paths_z(&bytes))
        })
        .await
    }

    /// Removes `path` from `folder`'s working tree, pruning now-empty
    /// parent directories up to (not including) `folder` itself. `path` is
    /// already validated; best-effort beyond "the file itself is gone".
    fn remove_mirrored(&self, folder: &Path, path: &str) {
        let full = folder.join(path);
        let _ = std::fs::remove_file(&full);
        let mut dir = full.parent().map(Path::to_path_buf);
        while let Some(d) = dir.filter(|d| d != folder && d.starts_with(folder)) {
            if std::fs::read_dir(&d).is_ok_and(|mut e| e.next().is_none()) {
                let _ = std::fs::remove_dir(&d);
                dir = d.parent().map(Path::to_path_buf);
            } else {
                break;
            }
        }
    }

    /// `changed`, split into paths present in `tree` (to check out) and
    /// paths absent from it (to remove) — the one membership lookup every
    /// op here needs after it has its path list.
    async fn split_by_tree<'a>(
        &self,
        folder: &Path,
        tree: &Sha,
        changed: &'a [String],
    ) -> GitResult<(Vec<&'a str>, Vec<&'a str>)> {
        let present = self.tree_paths(folder, tree).await?;
        let present: std::collections::HashSet<&str> = present.iter().map(String::as_str).collect();
        Ok(changed
            .iter()
            .map(String::as_str)
            .partition(|p| present.contains(p)))
    }

    pub(crate) async fn mirror_sync_op(
        &self,
        folder: &Path,
        repo: &Path,
        space: &str,
        to: &Sha,
        from: &Sha,
    ) -> GitResult<Vec<String>> {
        check_space(space)?;
        self.verify_folder(folder).await?;
        self.fetch_space_ref(folder, repo, space).await?;
        let _folder = crate::locks::lock(folder).await;
        let changed = self.mirror_diff(folder, from, to).await?;
        if changed.is_empty() {
            return Ok(Vec::new());
        }
        for p in &changed {
            segments(p)?;
        }
        let (to_checkout, to_delete) = self.split_by_tree(folder, to, &changed).await?;
        self.checkout_from_tree(folder, to, &to_checkout).await?;
        for p in &to_delete {
            self.remove_mirrored(folder, p);
        }
        Ok(changed)
    }

    pub(crate) async fn mirror_edited_op(
        &self,
        folder: &Path,
        repo: &Path,
        space: &str,
        to: &Sha,
        paths: &[String],
    ) -> GitResult<Vec<String>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        check_space(space)?;
        self.verify_folder(folder).await?;
        self.fetch_space_ref(folder, repo, space).await?;
        for p in paths {
            segments(p)?;
        }
        let (tracked, deleted) = self.split_by_tree(folder, to, paths).await?;
        let mut edited = self.differs_from_tree(folder, to, &tracked).await?;
        for p in deleted {
            if folder.join(p).exists() {
                edited.push(p.to_string());
            }
        }
        edited.sort();
        edited.dedup();
        Ok(edited)
    }

    pub(crate) async fn mirror_restore_op(
        &self,
        folder: &Path,
        from: &Sha,
        paths: &[String],
    ) -> GitResult<Vec<String>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        self.verify_folder(folder).await?;
        for p in paths {
            segments(p)?;
        }
        let _folder = crate::locks::lock(folder).await;
        let (to_checkout, to_delete) = self.split_by_tree(folder, from, paths).await?;
        self.checkout_from_tree(folder, from, &to_checkout).await?;
        for p in &to_delete {
            self.remove_mirrored(folder, p);
        }
        Ok(paths.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nul_separated_paths_split_cleanly() {
        assert_eq!(parse_paths_z(b""), Vec::<String>::new());
        assert_eq!(
            parse_paths_z(b"a.txt\0dir/b.txt\0"),
            vec!["a.txt", "dir/b.txt"]
        );
    }

    #[test]
    fn space_ids_are_checked_like_everywhere_else() {
        assert!(check_space("fix-login-1").is_ok());
        assert!(check_space("").is_err());
        assert!(check_space("-x").is_err());
        assert!(check_space("Has-Caps").is_err());
    }
}
