//! Space checkpoints (docs/spaces.md §6): the worktree as a commit taken
//! through a temporary index, and the way back to one that writes only the
//! files that differ.
//!
//! ```text
//! checkpoint  index ─copy─▶ tmp index ─add -A─▶ write-tree ─▶ commit-tree -p HEAD
//!             ─▶ refs/theodore/spaces/<id>, refs/theodore/checkpoints/<id>/<sha>
//!             (the worktree's HEAD, branch and index stay as they are)
//! reset       now = the worktree's tree ─diff-tree─▶ to's tree
//!             ─▶ rm what to lacks, checkout-index what differs
//!             ─▶ reset --mixed to.head ─▶ refs/theodore/spaces/<id> = to
//! ```

use std::ffi::OsStr;
use std::hash::{Hash, Hasher};
use std::path::{Component, Path, PathBuf};

use theodore_domain::Sha;
use theodore_ports::{GitError, GitResult, SpaceCheckpoint, space_checkpoint_ref, space_ref};

use crate::GitCli;
use crate::space_guard::SpaceGit;
use crate::spaces::validate_space_id;

/// A gitlink (submodule) entry: never written or removed here.
const GITLINK: &str = "160000";

/// A temporary index under the worktree's git dir, removed when dropped.
struct TempIndex(PathBuf);

impl TempIndex {
    fn new(git_dir: &Path, name: &str) -> Self {
        let path = git_dir.join(name);
        let _ = std::fs::remove_file(&path);
        Self(path)
    }

    fn env(&self) -> [(&'static str, &OsStr); 1] {
        [("GIT_INDEX_FILE", self.0.as_os_str())]
    }
}

impl Drop for TempIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

async fn git_dir(git: &SpaceGit<'_>) -> GitResult<PathBuf> {
    let dir = git.run_text(&["rev-parse", "--absolute-git-dir"]).await?;
    Ok(PathBuf::from(dir.trim()))
}

/// `rev-parse --verify --quiet <spec>`, `None` when it names nothing.
async fn rev(git: &SpaceGit<'_>, spec: &str) -> GitResult<Option<String>> {
    let out = git
        .run_raw(&["rev-parse", "--verify", "--quiet", spec])
        .await?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok((out.status == 0 && !text.is_empty()).then_some(text))
}

async fn must_rev(git: &SpaceGit<'_>, spec: &str) -> GitResult<String> {
    rev(git, spec)
        .await?
        .ok_or_else(|| GitError::Parse(format!("{spec} names no object")))
}

/// The whole worktree as a tree: tracked and untracked files, `.gitignore`
/// honored. Staged in a copy of the worktree's index, so `add` reuses its
/// stat cache and the index itself is never written.
async fn worktree_tree(git: &SpaceGit<'_>, git_dir: &Path) -> GitResult<String> {
    exclude_junk(git).await?;
    let index = TempIndex::new(git_dir, "theodore-snapshot.index");
    let real = git_dir.join("index");
    if real.exists() {
        std::fs::copy(&real, &index.0)?;
    } else {
        git.run_env(&["read-tree", "HEAD"], &index.env()).await?;
    }
    git.run_env(&["add", "-A"], &index.env()).await?;
    let out = git.run_env(&["write-tree"], &index.env()).await?;
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

/// A commit of Theodore's own: a fixed identity, never the user's hooks or
/// signing. `date` (git's `@<secs> <tz>`) pins both dates, so the same
/// tree on the same parent is the same commit.
async fn commit_tree(
    git: &SpaceGit<'_>,
    tree: &str,
    parent: &str,
    message: &str,
    date: Option<&str>,
) -> GitResult<String> {
    let args = [
        "-c",
        "user.name=Theodore",
        "-c",
        "user.email=theodore@localhost",
        "commit-tree",
        tree,
        "-p",
        parent,
        "-m",
        message,
    ];
    let out = match date {
        Some(d) => {
            let d = OsStr::new(d);
            git.run_env(&args, &[("GIT_AUTHOR_DATE", d), ("GIT_COMMITTER_DATE", d)])
                .await?
        }
        None => git.run(&args).await?,
    };
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

/// `path` relative to the worktree, made only of plain components and
/// never inside `.git`; anything else is refused.
fn safe_rel(path: &str) -> GitResult<PathBuf> {
    let p = Path::new(path);
    let plain = !path.is_empty()
        && p.components().all(|c| match c {
            Component::Normal(n) => n != ".git",
            _ => false,
        });
    if plain {
        Ok(p.to_path_buf())
    } else {
        Err(GitError::Refused(format!(
            "{path:?} is not a path inside the space"
        )))
    }
}

/// Whether a directory on the way to `rel` is a symlink: writing or
/// removing through it would leave the worktree.
fn through_symlink(root: &Path, rel: &Path) -> bool {
    let mut at = root.to_path_buf();
    let mut parts = rel.components().peekable();
    while let Some(c) = parts.next() {
        if parts.peek().is_none() {
            break;
        }
        at.push(c);
        if std::fs::symlink_metadata(&at).is_ok_and(|m| m.file_type().is_symlink()) {
            return true;
        }
    }
    false
}

/// Removes the file (or symlink) at `rel`, then the directories it leaves
/// empty. A directory there is left alone.
fn remove_file(root: &Path, rel: &Path) -> GitResult<()> {
    if through_symlink(root, rel) {
        return Ok(());
    }
    let p = root.join(rel);
    match std::fs::symlink_metadata(&p) {
        Ok(m) if !m.is_dir() => std::fs::remove_file(&p)?,
        _ => return Ok(()),
    }
    let mut dir = p.parent();
    while let Some(d) = dir {
        if d == root || std::fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
    Ok(())
}

/// Writes `paths` from `tree` into the worktree with `checkout-index`,
/// through a temporary index: the worktree's own index stays as it is.
async fn write_paths(
    git: &SpaceGit<'_>,
    git_dir: &Path,
    tree: &str,
    paths: &[String],
) -> GitResult<()> {
    let paths: Vec<&String> = paths
        .iter()
        .filter(|p| !through_symlink(git.root(), Path::new(p)))
        .collect();
    if paths.is_empty() {
        return Ok(());
    }
    let index = TempIndex::new(git_dir, "theodore-restore.index");
    git.run_env(&["read-tree", tree], &index.env()).await?;
    let mut stdin = Vec::new();
    for p in &paths {
        stdin.extend_from_slice(p.as_bytes());
        stdin.push(0);
    }
    git.run_env_stdin(
        &["checkout-index", "-f", "-z", "--stdin"],
        &index.env(),
        &stdin,
    )
    .await?;
    Ok(())
}

/// The paths `diff-tree -r` lists between two trees, split into the ones
/// `to` has (written) and the ones it lacks (removed). Submodules skipped.
async fn changed_paths(
    git: &SpaceGit<'_>,
    from: &str,
    to: &str,
) -> GitResult<(Vec<String>, Vec<String>)> {
    let out = git
        .run(&["diff-tree", "-r", "-z", "--no-renames", from, to])
        .await?;
    let mut write = Vec::new();
    let mut remove = Vec::new();
    let mut fields = out.split(|b| *b == 0).filter(|f| !f.is_empty());
    while let (Some(meta), Some(path)) = (fields.next(), fields.next()) {
        // ":<old mode> <new mode> <old sha> <new sha> <status>"
        let meta = String::from_utf8_lossy(meta);
        let mut m = meta.trim_start_matches(':').split(' ');
        let (old_mode, new_mode) = (m.next().unwrap_or(""), m.next().unwrap_or(""));
        if old_mode == GITLINK || new_mode == GITLINK {
            continue;
        }
        let path = String::from_utf8_lossy(path).into_owned();
        safe_rel(&path)?;
        if new_mode.bytes().all(|b| b == b'0') {
            remove.push(path);
        } else {
            write.push(path);
        }
    }
    Ok((write, remove))
}

/// Makes the worktree's uncommitted edits `HEAD`'s own: the snapshot of
/// them (the last checkpoint when it matches) becomes `HEAD` with a mixed
/// reset, so no file is written. An update from base merges into a clean
/// tree, and Abort comes back to exactly this commit. Returns `HEAD`. The
/// caller holds the worktree lock and has checked it isn't busy.
pub(crate) async fn commit_edits(git: &SpaceGit<'_>, id: &str, message: &str) -> GitResult<Sha> {
    let head = must_rev(git, "HEAD").await?;
    let tree = worktree_tree(git, &git_dir(git).await?).await?;
    if must_rev(git, "HEAD^{tree}").await? == tree {
        return Ok(Sha::new(head));
    }
    let refname = space_ref(id);
    let last = rev(git, &refname).await?;
    let same = match &last {
        Some(last) => {
            rev(git, &format!("{last}^{{tree}}")).await?.as_deref() == Some(&tree)
                && rev(git, &format!("{last}^1")).await?.as_deref() == Some(&head)
        }
        None => false,
    };
    let sha = match last {
        Some(last) if same => last,
        _ => commit_tree(git, &tree, &head, message, None).await?,
    };
    git.run(&["reset", "-q", "--mixed", &sha]).await?;
    let sha = Sha::new(sha);
    git.run(&["update-ref", &refname, sha.as_str()]).await?;
    git.run(&["update-ref", &space_checkpoint_ref(id, &sha), sha.as_str()])
        .await?;
    Ok(sha)
}

/// Build output no repo means to ship, left out of checkpoints (so of what
/// ships) even without a `.gitignore`. Untracked files only: a file the
/// repo tracks stays tracked.
const JUNK: &[&str] = &["__pycache__/", "*.py[co]", "node_modules/", ".DS_Store"];
/// Cargo's build output, only with a `Cargo.toml` at the root.
const CARGO_TARGET: &str = "/target/";

/// Puts `JUNK` (and `CARGO_TARGET` for a Cargo repo) in the cache's
/// `info/exclude`, the file the default `.env*` exclusion uses.
async fn exclude_junk(git: &SpaceGit<'_>) -> GitResult<()> {
    let mut lines: Vec<String> = JUNK.iter().map(|l| l.to_string()).collect();
    if git.root().join("Cargo.toml").is_file() {
        lines.push(CARGO_TARGET.to_string());
    }
    append_excludes(git, &lines).await
}

/// Adds the `lines` the worktree's `info/exclude` (the cache's, shared by
/// its worktrees) doesn't have yet.
async fn append_excludes(git: &SpaceGit<'_>, lines: &[String]) -> GitResult<()> {
    let file = git
        .run_text(&["rev-parse", "--git-path", "info/exclude"])
        .await?;
    let file = git.root().join(file.trim());
    // Shared by the cache's worktrees.
    let _file = crate::locks::lock(&file).await;
    let mut text = std::fs::read_to_string(&file).unwrap_or_default();
    let known: Vec<&str> = text.lines().collect();
    let new: Vec<&String> = lines
        .iter()
        .filter(|l| !known.contains(&l.as_str()))
        .collect();
    if new.is_empty() {
        return Ok(());
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    for line in new {
        text.push_str(line);
        text.push('\n');
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&file, text)?;
    Ok(())
}

/// `path` as one line of an exclude file: anchored at the worktree's root,
/// nothing in it read as a pattern. `None` for a name a line can't hold.
fn exclude_line(path: &str) -> Option<String> {
    if path.contains(['\n', '\r']) {
        return None;
    }
    let mut line = String::from("/");
    for c in path.chars() {
        if matches!(c, '\\' | '*' | '?' | '[' | ' ' | '\t') {
            line.push('\\');
        }
        line.push(c);
    }
    Some(line)
}

impl GitCli {
    /// `GitPort::space_exclude`. Through the cache's `info/exclude`, the one
    /// exclude file a worktree reads that isn't in its tree: git itself then
    /// leaves the files alone, in `add -A`, `status` and `clean` alike, and
    /// the agent's own commits too. Each file by name, so anything new next
    /// to them still counts.
    pub(crate) async fn space_exclude_op(
        &self,
        worktree: &Path,
        paths: &[String],
    ) -> GitResult<()> {
        if paths.is_empty() {
            return Ok(());
        }
        let askpass = self.ensure_askpass()?;
        let _wt = crate::locks::lock(worktree).await;
        let git = self.space_git(worktree, &askpass);
        git.verify_root().await?;
        let others = git
            .run(&["ls-files", "-z", "--others", "--exclude-standard"])
            .await?;
        let others: std::collections::HashSet<String> = others
            .split(|b| *b == 0)
            .filter(|f| !f.is_empty())
            .map(|f| String::from_utf8_lossy(f).into_owned())
            .collect();
        let lines: Vec<String> = paths
            .iter()
            .filter(|p| others.contains(*p))
            .filter_map(|p| exclude_line(p))
            .collect();
        if lines.is_empty() {
            return Ok(());
        }
        append_excludes(&git, &lines).await
    }

    pub(crate) async fn space_checkpoint_op(
        &self,
        worktree: &Path,
        id: &str,
        message: &str,
    ) -> GitResult<SpaceCheckpoint> {
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
        let head = must_rev(&git, "HEAD").await?;
        let tree = worktree_tree(&git, &git_dir(&git).await?).await?;
        let refname = space_ref(id);
        let sha = if must_rev(&git, "HEAD^{tree}").await? == tree {
            head.clone()
        } else {
            // The worktree as it was at the last checkpoint, on the same
            // `HEAD`: that checkpoint again, not a new commit for nothing.
            let last = rev(&git, &refname).await?;
            let same = match &last {
                Some(last) => {
                    rev(&git, &format!("{last}^{{tree}}")).await?.as_deref() == Some(&tree)
                        && rev(&git, &format!("{last}^1")).await?.as_deref() == Some(&head)
                }
                None => false,
            };
            match last {
                Some(last) if same => last,
                _ => commit_tree(&git, &tree, &head, message, None).await?,
            }
        };
        let sha = Sha::new(sha);
        // Refs outside `refs/heads` and `refs/worktree` are shared by every
        // worktree, so these land in the bare cache.
        git.run(&["update-ref", &refname, sha.as_str()]).await?;
        git.run(&["update-ref", &space_checkpoint_ref(id, &sha), sha.as_str()])
            .await?;
        Ok(SpaceCheckpoint {
            sha,
            head: Sha::new(head),
        })
    }

    pub(crate) async fn space_reset_op(
        &self,
        worktree: &Path,
        id: &str,
        to: &SpaceCheckpoint,
    ) -> GitResult<()> {
        validate_space_id(id)?;
        let askpass = self.ensure_askpass()?;
        let _wt = crate::locks::lock(worktree).await;
        let git = self.space_git(worktree, &askpass);
        git.verify_root().await?;
        // Undo would throw the unfinished operation away with the turn.
        if let Some(busy) = git.busy().await? {
            return Err(GitError::Busy(busy));
        }
        let git_dir = git_dir(&git).await?;
        let target = must_rev(&git, &format!("{}^{{tree}}", to.sha.as_str())).await?;
        must_rev(&git, &format!("{}^{{commit}}", to.head.as_str())).await?;
        let now = worktree_tree(&git, &git_dir).await?;
        if now != target {
            let (write, remove) = changed_paths(&git, &now, &target).await?;
            // Removed first: a file may give way to a directory and back.
            // Untracked files are in `now`, so the ones `to` lacks go too;
            // ignored ones never are, and stay.
            for p in &remove {
                remove_file(worktree, Path::new(p))?;
            }
            write_paths(&git, &git_dir, &target, &write).await?;
        }
        // The index follows `HEAD`, the files stay as written above.
        git.run(&["reset", "-q", "--mixed", to.head.as_str()])
            .await?;
        git.run(&["update-ref", &space_ref(id), to.sha.as_str()])
            .await?;
        // A target no checkpoint made (Continue's replay onto a new base)
        // is kept reachable like one.
        git.run(&[
            "update-ref",
            &space_checkpoint_ref(id, &to.sha),
            to.sha.as_str(),
        ])
        .await?;
        Ok(())
    }

    pub(crate) async fn space_restore_paths_op(
        &self,
        worktree: &Path,
        paths: &[String],
        from: &Sha,
    ) -> GitResult<()> {
        let rels = paths
            .iter()
            .map(|p| safe_rel(p))
            .collect::<GitResult<Vec<_>>>()?;
        if rels.is_empty() {
            return Ok(());
        }
        let askpass = self.ensure_askpass()?;
        let _wt = crate::locks::lock(worktree).await;
        let git = self.space_git(worktree, &askpass);
        git.verify_root().await?;
        if let Some(busy) = git.busy().await? {
            return Err(GitError::Busy(busy));
        }
        let tree = must_rev(&git, &format!("{}^{{tree}}", from.as_str())).await?;
        let mut args = vec!["ls-tree", "-r", "-z", "--name-only", tree.as_str(), "--"];
        args.extend(paths.iter().map(String::as_str));
        let listed = git.run(&args).await?;
        let present: Vec<String> = listed
            .split(|b| *b == 0)
            .filter(|f| !f.is_empty())
            .map(|f| String::from_utf8_lossy(f).into_owned())
            .collect();
        for (path, rel) in paths.iter().zip(&rels) {
            let dir = format!("{path}/");
            if !present.iter().any(|p| p == path || p.starts_with(&dir)) {
                remove_file(worktree, rel)?;
            }
        }
        // Only what differs: an unchanged file keeps its mtime.
        let git_dir = git_dir(&git).await?;
        let now = worktree_tree(&git, &git_dir).await?;
        let (differ, _) = changed_paths(&git, &now, &tree).await?;
        let write: Vec<String> = present.into_iter().filter(|p| differ.contains(p)).collect();
        write_paths(&git, &git_dir, &tree, &write).await
    }

    /// `GitPort::space_snapshot`: the worktree's tree (`worktree_tree`) as
    /// a commit on `HEAD`, dated as `HEAD` so the same worktree is the same
    /// commit. No ref moves.
    pub(crate) async fn space_snapshot_op(&self, worktree: &Path) -> GitResult<Sha> {
        let askpass = self.ensure_askpass()?;
        let _wt = crate::locks::lock(worktree).await;
        let git = self.space_git(worktree, &askpass);
        git.verify_root().await?;
        let head = must_rev(&git, "HEAD").await?;
        let tree = worktree_tree(&git, &git_dir(&git).await?).await?;
        if must_rev(&git, "HEAD^{tree}").await? == tree {
            return Ok(Sha::new(head));
        }
        let secs = git
            .run_text(&["show", "-s", "--format=%ct", "HEAD"])
            .await?;
        let date = format!("@{} +0000", secs.trim());
        let sha = commit_tree(&git, &tree, &head, "snapshot of the worktree", Some(&date)).await?;
        Ok(Sha::new(sha))
    }

    pub(crate) async fn space_fingerprint_op(&self, worktree: &Path) -> GitResult<String> {
        let askpass = self.ensure_askpass()?;
        let git = self.space_git(worktree, &askpass);
        git.verify_root().await?;
        let head = rev(&git, "HEAD").await?.unwrap_or_default();
        // No optional locks: `status` would otherwise refresh and write
        // the worktree's index.
        let status = git
            .run(&[
                "--no-optional-locks",
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
            ])
            .await?;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        head.hash(&mut h);
        status.hash(&mut h);
        // `status` names a changed file, not how often it changed since:
        // its size and mtime do.
        for entry in status.split(|b| *b == 0).filter(|e| !e.is_empty()) {
            let path = if entry.len() > 3 && entry[2] == b' ' {
                &entry[3..]
            } else {
                entry
            };
            let p = worktree.join(String::from_utf8_lossy(path).as_ref());
            match std::fs::symlink_metadata(&p) {
                Ok(m) => {
                    m.len().hash(&mut h);
                    m.modified().ok().hash(&mut h);
                }
                Err(_) => 0u8.hash(&mut h),
            }
        }
        Ok(format!("{:016x}", h.finish()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_exclude_line_names_one_file_and_no_pattern() {
        assert_eq!(exclude_line(".env").as_deref(), Some("/.env"));
        assert_eq!(exclude_line("a/b c*.x").as_deref(), Some("/a/b\\ c\\*.x"));
        assert_eq!(exclude_line("a\nb"), None);
    }

    #[test]
    fn only_plain_paths_inside_the_space_are_accepted() {
        assert!(safe_rel("src/a.rs").is_ok());
        assert!(safe_rel("a").is_ok());
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "a/../../x",
            ".git/config",
            "a/.git/x",
            "./a",
        ] {
            assert!(safe_rel(bad).is_err(), "{bad:?}");
        }
    }
}
