//! Shipping a space (docs/spaces.md §6): its checkpoints squashed into one
//! commit authored by the user, then pushed to the host (Open PR / Push) or
//! brought into a local folder (Merge locally / Keep as branch).
//!
//! The squashed commit is written with `commit-tree` straight into the bare
//! cache and kept on `refs/theodore/shipped/<id>`: no worktree, no branch,
//! nothing the cache's force-fetch could clobber. The only operations that
//! write outside Theodore's cache are `push_branch` (the remote) and the
//! two folder operations, each behind an explicit click.

use std::path::Path;

use theodore_domain::{RepoRef, Sha};
use theodore_ports::{GitError, GitResult, Identity, SpaceCommit, WorkingState, shipped_ref};

use crate::GitCli;
use crate::process::{GitProc, RawOutput};
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

fn stderr_of(out: &RawOutput) -> String {
    String::from_utf8_lossy(&out.stderr).trim().to_string()
}

fn path_str(p: &Path) -> GitResult<&str> {
    p.to_str()
        .ok_or_else(|| GitError::Parse(format!("non-utf8 path {}", p.display())))
}

/// Same rules as a space id (`spaces::validate_space_id`), kept local so a
/// ship op never builds a ref from an unchecked id.
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

/// The `Refused` text for a folder whose checkout can't take a merge.
pub(crate) fn folder_refusal(state: &WorkingState) -> Option<String> {
    if state.current_branch.is_none() {
        return Some("the folder has a detached HEAD; check out a branch first".into());
    }
    if state.dirty_files > 0 {
        let n = state.dirty_files;
        return Some(format!(
            "the folder has {n} uncommitted change{}; commit or stash {} first",
            if n == 1 { "" } else { "s" },
            if n == 1 { "it" } else { "them" },
        ));
    }
    None
}

/// `merge-tree --write-tree --name-only --no-messages` stdout: the tree
/// oid, then one conflicted path per line.
fn conflicted_paths(stdout: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(stdout);
    let mut paths: Vec<String> = text
        .lines()
        .skip(1)
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    paths.dedup();
    paths
}

fn conflict_list(paths: &[String]) -> String {
    const SHOWN: usize = 5;
    let mut s = paths
        .iter()
        .take(SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if paths.len() > SHOWN {
        s.push_str(&format!(" and {} more", paths.len() - SHOWN));
    }
    s
}

impl GitCli {
    async fn check_branch(&self, cwd: &Path, branch: &str) -> GitResult<String> {
        let askpass = self.ensure_askpass()?;
        let full = format!("refs/heads/{branch}");
        let out = proc(cwd, &askpass, &["check-ref-format", &full])
            .run_raw()
            .await?;
        if out.status != 0 || branch.starts_with('-') {
            return Err(GitError::Parse(format!("bad branch name {branch:?}")));
        }
        Ok(full)
    }

    pub(crate) async fn user_identity_op(&self, repo: &Path) -> GitResult<Option<Identity>> {
        let askpass = self.ensure_askpass()?;
        let get = |key: &'static str| {
            let askpass = askpass.clone();
            async move {
                let out = proc(repo, &askpass, &["config", "--get", key])
                    .run_raw()
                    .await?;
                Ok::<_, GitError>(match out.status {
                    0 => Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
                        .filter(|s| !s.is_empty()),
                    _ => None,
                })
            }
        };
        let name = get("user.name").await?;
        let email = get("user.email").await?;
        Ok(match (name, email) {
            (Some(name), Some(email)) => Some(Identity { name, email }),
            _ => None,
        })
    }

    pub(crate) async fn space_commit_op(
        &self,
        repo: &Path,
        space: &str,
        c: &SpaceCommit,
    ) -> GitResult<Sha> {
        check_space(space)?;
        let askpass = self.ensure_askpass()?;
        let tree = match &c.replay_from {
            Some(from) => {
                let out = proc(
                    repo,
                    &askpass,
                    &[
                        "merge-tree",
                        "--write-tree",
                        "--name-only",
                        "--no-messages",
                        "--merge-base",
                        from.as_str(),
                        c.parent.as_str(),
                        c.head.as_str(),
                    ],
                )
                .run_raw()
                .await?;
                match out.status {
                    0 => String::from_utf8_lossy(&out.stdout)
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .trim()
                        .to_string(),
                    1 => {
                        return Err(GitError::Refused(format!(
                            "the branch changed on the host and conflicts with the new \
                             turns: {}",
                            conflict_list(&conflicted_paths(&out.stdout))
                        )));
                    }
                    code => {
                        return Err(GitError::Command {
                            args: "merge-tree --write-tree".into(),
                            code,
                            stderr: stderr_of(&out),
                        });
                    }
                }
            }
            None => {
                let spec = format!("{}^{{tree}}", c.head.as_str());
                proc(repo, &askpass, &["rev-parse", "--verify", &spec])
                    .run_text()
                    .await?
                    .trim()
                    .to_string()
            }
        };
        if tree.is_empty() {
            return Err(GitError::Parse("no tree for the shipped commit".into()));
        }
        // The base merged in since (Update from base) as a second parent,
        // unless `parent`'s history already has it.
        let merge_parent = match &c.merge_parent {
            Some(base) if base != &c.parent => {
                let has = proc(
                    repo,
                    &askpass,
                    &[
                        "merge-base",
                        "--is-ancestor",
                        END_OF_OPTIONS,
                        base.as_str(),
                        c.parent.as_str(),
                    ],
                )
                .run_raw()
                .await?;
                match has.status {
                    0 => None,
                    1 => Some(base.as_str()),
                    code => {
                        return Err(GitError::Command {
                            args: "merge-base --is-ancestor".into(),
                            code,
                            stderr: stderr_of(&has),
                        });
                    }
                }
            }
            _ => None,
        };
        let name = format!("user.name={}", c.author.name);
        let email = format!("user.email={}", c.author.email);
        // Author and committer are the user. Unsigned: Theodore can't
        // answer a passphrase prompt (docs/spaces.md §6).
        let mut args = vec![
            "-c",
            &name,
            "-c",
            &email,
            "-c",
            "commit.gpgsign=false",
            "commit-tree",
            &tree,
            "-p",
            c.parent.as_str(),
        ];
        if let Some(base) = merge_parent {
            args.extend(["-p", base]);
        }
        args.extend(["-m", &c.message]);
        let sha = proc(repo, &askpass, &args)
            .run_text()
            .await?
            .trim()
            .to_string();
        {
            let _repo = crate::locks::lock(repo).await;
            proc(repo, &askpass, &["update-ref", &shipped_ref(space), &sha])
                .run()
                .await?;
        }
        Ok(Sha::new(sha))
    }

    pub(crate) async fn remote_branch_op(
        &self,
        repo: &Path,
        repo_ref: &RepoRef,
        branch: &str,
    ) -> GitResult<Option<Sha>> {
        let full = self.check_branch(repo, branch).await?;
        let askpass = self.ensure_askpass()?;
        let token = self.token_for(repo_ref);
        let remote = self.remote_override(repo_ref)?;
        let remote = remote.as_deref().unwrap_or("origin");
        let listed = GitProc {
            cwd: repo,
            askpass: &askpass,
            args: &["ls-remote", remote, &full],
            token: token.as_deref(),
            stdin: None,
        }
        .run_text()
        .await?;
        let Some(tip) = listed
            .lines()
            .find(|l| l.split_whitespace().nth(1) == Some(full.as_str()))
            .and_then(|l| l.split_whitespace().next())
            .map(str::to_string)
        else {
            return Ok(None);
        };
        // Same mapping as the cache's own refresh (`+refs/heads/*`), so the
        // tip's objects are here and nothing new is invented.
        let refspec = format!("+{full}:{full}");
        let _repo = crate::locks::lock(repo).await;
        GitProc {
            cwd: repo,
            askpass: &askpass,
            args: &["fetch", "--no-tags", remote, &refspec],
            token: token.as_deref(),
            stdin: None,
        }
        .run()
        .await?;
        Ok(Some(Sha::new(tip)))
    }

    pub(crate) async fn push_branch_op(
        &self,
        repo: &Path,
        repo_ref: &RepoRef,
        sha: &Sha,
        branch: &str,
    ) -> GitResult<()> {
        let full = self.check_branch(repo, branch).await?;
        let askpass = self.ensure_askpass()?;
        let token = self.token_for(repo_ref);
        let remote = self.remote_override(repo_ref)?;
        let refspec = format!("{}:{full}", sha.as_str());
        let out = GitProc {
            cwd: repo,
            askpass: &askpass,
            // The cache is Theodore's own bare clone: hooks written for the
            // user's checkout (a pre-push lint) can't run here.
            args: &[
                "-c",
                "core.hooksPath=/dev/null",
                "push",
                remote.as_deref().unwrap_or("origin"),
                &refspec,
            ],
            token: token.as_deref(),
            stdin: None,
        }
        .run()
        .await;
        match out {
            Err(GitError::Command { stderr, .. })
                if stderr.contains("non-fast-forward")
                    || stderr.contains("fetch first")
                    || stderr.contains("[rejected]") =>
            {
                Err(GitError::Refused(format!(
                    "{branch} changed on the host since the last push; nothing was overwritten"
                )))
            }
            other => other.map(|_| ()),
        }
    }

    pub(crate) async fn merge_into_folder_op(
        &self,
        folder: &Path,
        repo: &Path,
        space: &str,
        message: &str,
    ) -> GitResult<Sha> {
        check_space(space)?;
        let askpass = self.ensure_askpass()?;
        let state = self.working_state_op(folder).await?;
        if let Some(why) = folder_refusal(&state) {
            return Err(GitError::Refused(why));
        }
        let refname = shipped_ref(space);
        let sha = proc(repo, &askpass, &["rev-parse", "--verify", &refname])
            .run_text()
            .await?
            .trim()
            .to_string();
        // Objects only: no FETCH_HEAD, no ref in the folder.
        let repo_str = path_str(repo)?;
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
        // Dry run first: a conflicting merge is refused before the
        // folder's index or files are touched at all.
        let dry = proc(
            folder,
            &askpass,
            &[
                "merge-tree",
                "--write-tree",
                "--name-only",
                "--no-messages",
                "HEAD",
                &sha,
            ],
        )
        .run_raw()
        .await?;
        match dry.status {
            0 => {}
            1 => {
                return Err(GitError::Refused(format!(
                    "merging into {} would conflict: {}",
                    state.current_branch.unwrap_or_default(),
                    conflict_list(&conflicted_paths(&dry.stdout))
                )));
            }
            code => {
                return Err(GitError::Command {
                    args: "merge-tree --write-tree".into(),
                    code,
                    stderr: stderr_of(&dry),
                });
            }
        }
        let merged = proc(
            folder,
            &askpass,
            &["merge", "--no-edit", "-m", message, END_OF_OPTIONS, &sha],
        )
        .run_raw()
        .await?;
        if merged.status != 0 {
            // Leave the tree as it was (a hook or identity error mid-merge).
            let _ = proc(folder, &askpass, &["merge", "--abort"])
                .run_raw()
                .await;
            return Err(GitError::Command {
                args: "merge".into(),
                code: merged.status,
                stderr: stderr_of(&merged),
            });
        }
        let head = proc(folder, &askpass, &["rev-parse", "HEAD"])
            .run_text()
            .await?;
        Ok(Sha::new(head.trim().to_string()))
    }

    pub(crate) async fn write_folder_branch_op(
        &self,
        folder: &Path,
        repo: &Path,
        space: &str,
        branch: &str,
    ) -> GitResult<()> {
        check_space(space)?;
        let full = self.check_branch(folder, branch).await?;
        let askpass = self.ensure_askpass()?;
        let exists = proc(
            folder,
            &askpass,
            &["rev-parse", "--verify", "--quiet", END_OF_OPTIONS, &full],
        )
        .run_raw()
        .await?;
        if exists.status == 0 {
            return Err(GitError::Refused(format!(
                "a branch named {branch} already exists in the folder"
            )));
        }
        let refspec = format!("{}:{full}", shipped_ref(space));
        proc(
            folder,
            &askpass,
            &[
                "fetch",
                "--no-tags",
                "--no-write-fetch-head",
                END_OF_OPTIONS,
                path_str(repo)?,
                &refspec,
            ],
        )
        .run()
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_must_be_clean_and_on_a_branch() {
        let clean = WorkingState {
            current_branch: Some("main".into()),
            dirty_files: 0,
        };
        assert_eq!(folder_refusal(&clean), None);
        let detached = WorkingState {
            current_branch: None,
            dirty_files: 0,
        };
        assert!(folder_refusal(&detached).unwrap().contains("detached"));
        let dirty = WorkingState {
            current_branch: Some("main".into()),
            dirty_files: 2,
        };
        assert!(
            folder_refusal(&dirty)
                .unwrap()
                .contains("2 uncommitted changes")
        );
    }

    #[test]
    fn conflicts_are_listed_without_the_tree_line() {
        let out = b"4b825dc642cb6eb9a060e54bf8d69288fbee4904\na.txt\nb.txt\n";
        assert_eq!(conflicted_paths(out), vec!["a.txt", "b.txt"]);
        let many: Vec<String> = (0..7).map(|i| format!("f{i}")).collect();
        assert_eq!(conflict_list(&many), "f0, f1, f2, f3, f4 and 2 more");
    }
}
