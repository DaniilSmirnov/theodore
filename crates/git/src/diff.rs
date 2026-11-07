//! Orchestrates `diff`/`show`: runs the right `git` invocations, then hands
//! the raw text to the pure parser in `parse.rs`.

use std::collections::HashMap;
use std::path::Path;

use theodore_domain::{FileFlag, Revision, Sha};
use theodore_ports::{DiffOptions, GitError, GitPort, GitResult};

use crate::GitCli;
use crate::generated::GitAttributes;
use crate::parse::{self, ParsedFile};
use crate::process::GitProc;

impl GitCli {
    pub(crate) async fn diff_op(
        &self,
        repo: &Path,
        base: &Sha,
        head: &Sha,
        opts: &DiffOptions,
    ) -> GitResult<Revision> {
        let askpass = self.ensure_askpass()?;
        let range = format!("{}...{}", base.as_str(), head.as_str());
        let context_flag = format!("-U{}", opts.context);

        let mut common: Vec<&str> = vec!["diff", "--no-color", "--no-ext-diff"];
        if opts.find_renames {
            common.push("--find-renames");
            common.push("--find-copies-harder");
        }

        let mut numstat_args = common.clone();
        numstat_args.push("--numstat");
        numstat_args.push("-z");
        numstat_args.push(&range);
        let numstat_bytes = match (GitProc {
            cwd: repo,
            askpass: &askpass,
            args: &numstat_args,
            token: None,
            stdin: None,
        })
        .run()
        .await
        {
            Ok(bytes) => bytes,
            Err(e @ GitError::Command { .. }) => {
                // A stale sha (a last-seen head force-pushed away) is "commit
                // not found", which callers fall back from; anything else
                // stays git's own failure.
                for sha in [base, head] {
                    if !self.has_commit(repo, sha).await.unwrap_or(true) {
                        return Err(GitError::MissingCommit(sha.as_str().to_string()));
                    }
                }
                return Err(e);
            }
            Err(e) => return Err(e),
        };
        let numstat = parse::parse_numstat(&numstat_bytes);

        let mut full_args = common;
        full_args.push(&context_flag);
        full_args.push(&range);
        let full_text = GitProc {
            cwd: repo,
            askpass: &askpass,
            args: &full_args,
            token: None,
            stdin: None,
        }
        .run_text()
        .await?;

        let attrs_bytes = self
            .show_op(repo, head, ".gitattributes")
            .await
            .unwrap_or(None);
        let attrs_text = attrs_bytes
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        let attrs = GitAttributes::parse(&attrs_text);

        let mut parsed =
            parse::parse_unified_diff(&full_text, &numstat, &attrs, opts.max_file_lines);
        self.resolve_binary_sizes(repo, &askpass, &mut parsed)
            .await?;

        let mut files: Vec<_> = parsed.into_iter().map(|p| p.file).collect();
        theodore_domain::interdiff::dedupe_hunk_ids(&mut files);
        Ok(Revision {
            base: base.clone(),
            head: head.clone(),
            files,
        })
    }

    pub(crate) async fn numstat_op(
        &self,
        repo: &Path,
        from: &Sha,
        to: &Sha,
    ) -> GitResult<Vec<theodore_ports::FileStat>> {
        let askpass = self.ensure_askpass()?;
        let args = [
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--find-renames",
            "--numstat",
            "-z",
            from.as_str(),
            to.as_str(),
        ];
        let bytes = GitProc {
            cwd: repo,
            askpass: &askpass,
            args: &args,
            token: None,
            stdin: None,
        }
        .run()
        .await?;
        Ok(parse::parse_numstat(&bytes)
            .into_iter()
            .map(|n| theodore_ports::FileStat {
                path: n.path,
                additions: n.additions.unwrap_or(0),
                deletions: n.deletions.unwrap_or(0),
            })
            .collect())
    }

    pub(crate) async fn show_op(
        &self,
        repo: &Path,
        rev: &Sha,
        path: &str,
    ) -> GitResult<Option<Vec<u8>>> {
        let askpass = self.ensure_askpass()?;
        let spec = format!("{}:{}", rev.as_str(), path);
        let args = ["show", spec.as_str()];
        let result = GitProc {
            cwd: repo,
            askpass: &askpass,
            args: &args,
            token: None,
            stdin: None,
        }
        .run()
        .await;
        match result {
            Ok(bytes) => Ok(Some(bytes)),
            Err(GitError::Command { stderr, .. }) if looks_missing(&stderr) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Unified-diff text carries no byte sizes for binary files; batch-resolve
    /// them from the blob shas on each file's `index` line.
    async fn resolve_binary_sizes(
        &self,
        repo: &Path,
        askpass: &Path,
        parsed: &mut [ParsedFile],
    ) -> GitResult<()> {
        let mut shas: Vec<String> = Vec::new();
        for p in parsed.iter() {
            if !p
                .file
                .flags
                .iter()
                .any(|f| matches!(f, FileFlag::Binary { .. }))
            {
                continue;
            }
            for sha in [&p.old_sha, &p.new_sha].into_iter().flatten() {
                if !is_zero_sha(sha) {
                    shas.push(sha.clone());
                }
            }
        }
        if shas.is_empty() {
            return Ok(());
        }

        let stdin_data = (shas.join("\n") + "\n").into_bytes();
        let args = ["cat-file", "--batch-check=%(objectsize)"];
        let out = GitProc {
            cwd: repo,
            askpass,
            args: &args,
            token: None,
            stdin: Some(stdin_data.as_slice()),
        }
        .run_text()
        .await?;

        let mut sizes: HashMap<&str, u64> = HashMap::new();
        for (sha, line) in shas.iter().zip(out.lines()) {
            if let Ok(n) = line.trim().parse::<u64>() {
                sizes.insert(sha.as_str(), n);
            }
        }

        for p in parsed.iter_mut() {
            let old_sha = p.old_sha.clone();
            let new_sha = p.new_sha.clone();
            if let Some(FileFlag::Binary { old_size, new_size }) = p
                .file
                .flags
                .iter_mut()
                .find(|f| matches!(f, FileFlag::Binary { .. }))
            {
                if let Some(s) = old_sha.as_deref().filter(|s| !is_zero_sha(s)) {
                    *old_size = sizes.get(s).copied();
                }
                if let Some(s) = new_sha.as_deref().filter(|s| !is_zero_sha(s)) {
                    *new_size = sizes.get(s).copied();
                }
            }
        }
        Ok(())
    }
}

fn is_zero_sha(sha: &str) -> bool {
    !sha.is_empty() && sha.chars().all(|c| c == '0')
}

fn looks_missing(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    s.contains("does not exist")
        || s.contains("exists on disk, but not in")
        || s.contains("bad revision")
        || s.contains("invalid object name")
        || s.contains("unknown revision")
}
