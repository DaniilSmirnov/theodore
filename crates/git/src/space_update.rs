//! Update from base (docs/spaces.md §6): the base's new tip merged into a
//! space's worktree — a merge, never a rebase, so what was pushed and the
//! comments on it stay put.
//!
//! ```text
//! space_merge         git merge <tip> ─┬─ clean ─▶ merge commit = checkpoint ─▶ Merged
//!                                      └─ conflicts ─▶ left in progress ────▶ Conflicts
//! space_merge_finish  no markers left in the listed files, staged or not
//!                                                         ─▶ add -A, commit ─▶ Merged
//!                     markers left ───────────────────────────────────────▶ Conflicts
//! space_merge_abort   merge --abort, reset --hard <checkpoint>, clean -fd
//! ```

use std::path::Path;

use theodore_domain::Sha;
use theodore_ports::{GitError, GitResult, SpaceMerge, WorktreeBusy, space_ref};

use crate::GitCli;
use crate::safety::END_OF_OPTIONS;
use crate::space_guard::SpaceGit;
use crate::spaces::{snapshot, validate_space_id};

/// Theodore's own commits in a space: no user hooks or signing prompts, a
/// fixed identity (as for checkpoints).
const THEODORE: [&str; 8] = [
    "-c",
    "user.name=Theodore",
    "-c",
    "user.email=theodore@localhost",
    "-c",
    "commit.gpgsign=false",
    "-c",
    "core.hooksPath=/dev/null",
];

/// Paths with unmerged index entries, each once, in index order.
async fn unmerged(git: &SpaceGit<'_>) -> GitResult<Vec<String>> {
    let out = git.run(&["ls-files", "-u", "-z"]).await?;
    let mut paths: Vec<String> = Vec::new();
    // `<mode> <sha> <stage>\t<path>` per entry, NUL-terminated.
    for entry in out.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let Some(tab) = entry.iter().position(|b| *b == b'\t') else {
            continue;
        };
        let path = String::from_utf8_lossy(&entry[tab + 1..]).into_owned();
        if paths.last() != Some(&path) {
            paths.push(path);
        }
    }
    Ok(paths)
}

/// Whether `text` still has a conflict marker line (`<<<<<<<`, `|||||||`,
/// `>>>>>>>`; `=======` alone is too common in docs to count).
pub(crate) fn has_conflict_markers(text: &[u8]) -> bool {
    text.split(|b| *b == b'\n').any(|line| {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        [b"<<<<<<<", b"|||||||", b">>>>>>>"].iter().any(|m| {
            line.strip_prefix(&m[..])
                .is_some_and(|rest| rest.is_empty() || rest[0] == b' ')
        })
    })
}

impl GitCli {
    pub(crate) async fn space_merge_op(
        &self,
        worktree: &Path,
        id: &str,
        tip: &Sha,
        message: &str,
    ) -> GitResult<SpaceMerge> {
        validate_space_id(id)?;
        let askpass = self.ensure_askpass()?;
        let _wt = crate::locks::lock(worktree).await;
        let git = self.space_git(worktree, &askpass);
        git.verify_root().await?;
        if let Some(busy) = git.busy().await? {
            return Err(GitError::Busy(busy));
        }
        // Checkpoints leave edits uncommitted (`checkpoints`); a merge
        // wants them in `HEAD`.
        crate::checkpoints::commit_edits(&git, id, "edits before updating from the base").await?;
        let mut args: Vec<&str> = THEODORE.to_vec();
        args.extend([
            // The common ancestor's lines between the two sides: what each
            // side changed is readable from the markers alone.
            "-c",
            "merge.conflictStyle=diff3",
            "merge",
            "--no-edit",
            "--no-stat",
            "--ff",
            "-m",
            message,
            END_OF_OPTIONS,
            tip.as_str(),
        ]);
        let out = git.run_raw(&args).await?;
        if out.status == 0 {
            let head = git.run_text(&["rev-parse", "--verify", "HEAD"]).await?;
            let head = head.trim().to_string();
            git.run(&["update-ref", &space_ref(id), &head]).await?;
            return Ok(SpaceMerge::Merged(Sha::new(head)));
        }
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if git.merging().await? {
            let paths = unmerged(&git).await?;
            if !paths.is_empty() {
                return Ok(SpaceMerge::Conflicts(paths));
            }
            // Stopped for another reason: leave nothing half-done.
            let _ = git.run_raw(&["merge", "--abort"]).await;
        }
        // Refused before it started (local changes it would overwrite,
        // unrelated histories): the worktree is as it was.
        Err(GitError::Command {
            args: "merge".into(),
            code: out.status,
            stderr,
        })
    }

    pub(crate) async fn space_merge_finish_op(
        &self,
        worktree: &Path,
        id: &str,
        tip: &Sha,
        conflicts: &[String],
        message: &str,
    ) -> GitResult<SpaceMerge> {
        validate_space_id(id)?;
        let askpass = self.ensure_askpass()?;
        let _wt = crate::locks::lock(worktree).await;
        let git = self.space_git(worktree, &askpass);
        git.verify_root().await?;
        if !git.merging().await? {
            // Concluded or aborted in the worktree, not by Theodore.
            match git.busy().await? {
                None => {}
                Some(busy) => return Err(GitError::Busy(busy)),
            }
            let has_tip = git
                .run_raw(&[
                    "merge-base",
                    "--is-ancestor",
                    END_OF_OPTIONS,
                    tip.as_str(),
                    "HEAD",
                ])
                .await?;
            return match has_tip.status {
                0 => Ok(SpaceMerge::Merged(snapshot(&git, id, message).await?)),
                1 => Ok(SpaceMerge::NotMerging),
                code => Err(GitError::Command {
                    args: "merge-base --is-ancestor".into(),
                    code,
                    stderr: String::from_utf8_lossy(&has_tip.stderr).into_owned(),
                }),
            };
        }
        // Resolved = no markers left in the file (or the file deleted);
        // the agent needn't `git add` it, its sandbox may not let it. And
        // adding it resolves nothing: a staged file is no longer unmerged,
        // so the files the merge listed are read too.
        let mut paths = unmerged(&git).await?;
        for path in conflicts {
            if !paths.contains(path) {
                paths.push(path.clone());
            }
        }
        let mut left = Vec::new();
        for path in paths {
            match std::fs::read(worktree.join(&path)) {
                Ok(text) if has_conflict_markers(&text) => left.push(path),
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        if !left.is_empty() {
            return Ok(SpaceMerge::Conflicts(left));
        }
        let head = snapshot(&git, id, message).await?;
        Ok(SpaceMerge::Merged(head))
    }

    pub(crate) async fn space_merge_abort_op(
        &self,
        worktree: &Path,
        id: &str,
        to: &Sha,
    ) -> GitResult<()> {
        validate_space_id(id)?;
        let askpass = self.ensure_askpass()?;
        let _wt = crate::locks::lock(worktree).await;
        let git = self.space_git(worktree, &askpass);
        git.verify_root().await?;
        if git.merging().await? {
            // Best effort: the reset below clears a merge either way.
            let _ = git.run_raw(&["merge", "--abort"]).await;
        }
        // A rebase or cherry-pick the agent started meanwhile isn't ours
        // to throw away.
        match git.busy().await? {
            None | Some(WorktreeBusy::Merge) | Some(WorktreeBusy::Conflicts) => {}
            Some(busy) => return Err(GitError::Busy(busy)),
        }
        let refname = space_ref(id);
        let steps: [&[&str]; 3] = [
            &["reset", "-q", "--hard", to.as_str()],
            &["clean", "-q", "-fd"],
            &["update-ref", &refname, to.as_str()],
        ];
        for args in steps {
            git.run(args).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflict_markers_are_found_at_line_starts_only() {
        assert!(has_conflict_markers(
            b"a\n<<<<<<< HEAD\nx\n||||||| base\ny\n=======\nz\n>>>>>>> theirs\n"
        ));
        assert!(has_conflict_markers(b"<<<<<<<\r\n"));
        assert!(!has_conflict_markers(b"a\n=======\nb\n"));
        assert!(!has_conflict_markers(b"  <<<<<<< indented\n"));
        assert!(!has_conflict_markers(b"<<<<<<<<< longer\n"));
        assert!(!has_conflict_markers(b"resolved\n"));
    }
}
