//! Which languages a worktree uses, by file count (docs/code-intel.md §3.1).
//!
//! ```text
//! git ls-files -z ─▶ paths ─▶ first language whose glob matches ─▶ counts
//! ```

use std::collections::HashMap;
use std::path::Path;

use theodore_ports::code_intel::{CodeIntelError, CodeIntelResult, LanguageId};

use crate::manifest::Manifest;

impl Manifest {
    /// Files per language among `paths` (repo-relative, `/` separators), most
    /// files first, then by id. Languages with no file are left out.
    pub fn detect<S: AsRef<str>>(&self, paths: &[S]) -> Vec<(LanguageId, usize)> {
        let mut counts: HashMap<&LanguageId, usize> = HashMap::new();
        for path in paths {
            if let Some(lang) = self.language_for_path(path.as_ref()) {
                *counts.entry(&lang.id).or_default() += 1;
            }
        }
        let mut out: Vec<_> = counts.into_iter().map(|(id, n)| (id.clone(), n)).collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out
    }
}

/// Tracked files of the worktree at `dir`: `git ls-files -z`.
pub async fn git_ls_files(dir: &Path) -> CodeIntelResult<Vec<String>> {
    let out = tokio::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["ls-files", "-z"])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| CodeIntelError::Other(format!("git ls-files: {e}")))?;
    if !out.status.success() {
        return Err(CodeIntelError::Other(format!(
            "git ls-files in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(out
        .stdout
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect())
}

/// Shell-style glob over a whole string: `*` any run of chars, `?` one char,
/// everything else literal. Case-sensitive.
pub(crate) fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    // Last `*` seen and the text position it was tried at, for backtracking.
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        match p.get(pi) {
            Some('*') => {
                star = Some((pi, ti));
                pi += 1;
            }
            Some(&c) if c == '?' || c == t[ti] => {
                pi += 1;
                ti += 1;
            }
            _ => match star {
                Some((sp, st)) => {
                    pi = sp + 1;
                    ti = st + 1;
                    star = Some((sp, st + 1));
                }
                None => return false,
            },
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn globs() {
        assert!(glob_match("*.rs", "main.rs"));
        assert!(!glob_match("*.rs", "main.rsx"));
        assert!(glob_match("Dockerfile", "Dockerfile"));
        assert!(!glob_match("Dockerfile", "dockerfile"));
        assert!(glob_match("Dockerfile.*", "Dockerfile.dev"));
        assert!(glob_match("*.d.ts", "index.d.ts"));
        assert!(glob_match("a?c", "abc"));
        assert!(glob_match("*a*b*", "xxaxxbxx"));
        assert!(!glob_match("*a*b", "xxaxxbxx"));
        assert!(glob_match("*", ""));
        assert!(glob_match("docs/*.md", "docs/a/b.md"));
    }

    #[test]
    fn counts_files_per_language() {
        let m = Manifest::builtin();
        let paths = [
            "src/main.rs",
            "src/lib.rs",
            "crates/x/src/a.rs",
            "ui/app.tsx",
            "ui/index.ts",
            "ui/types.d.ts",
            "include/a.h",
            "src/b.cpp",
            "Dockerfile",
            "deploy/Dockerfile.prod",
            "README.md",
            "LICENSE",
            "Cargo.lock",
        ];
        let got = m.detect(&paths);
        let want: Vec<(LanguageId, usize)> = [
            ("rust", 3),
            ("dockerfile", 2),
            ("typescript", 2),
            ("c", 1),
            ("cpp", 1),
            ("markdown", 1),
            ("tsx", 1),
        ]
        .into_iter()
        .map(|(id, n)| (LanguageId::new(id), n))
        .collect();
        assert_eq!(got, want);
    }

    #[tokio::test]
    async fn ls_files_in_a_repo() {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q"]);
        std::fs::write(dir.path().join("a.rs"), "").unwrap();
        std::fs::write(dir.path().join("b c.py"), "").unwrap();
        std::fs::write(dir.path().join("untracked.go"), "").unwrap();
        git(&["add", "a.rs", "b c.py"]);
        let mut files = git_ls_files(dir.path()).await.unwrap();
        files.sort();
        assert_eq!(files, vec!["a.rs".to_string(), "b c.py".to_string()]);
    }
}
