//! The task suite (`docs/eval/agent-tools/tasks.yaml`): parsing and
//! validation. One task is one prompt on one repo at one commit, scored
//! 0/1 by its check.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// Seconds a run may take when the task names none.
pub const DEFAULT_TIMEOUT_S: u64 = 600;
/// The longest a task may ask for.
pub const MAX_TIMEOUT_S: u64 = 4 * 3600;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskKind {
    /// A question: read-only tools, scored on the final answer.
    Answer,
    /// A change: the space's Edit mode, scored by a command in the worktree.
    Edit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum Check {
    /// The final answer contains `value` (case-sensitive).
    Contains { value: String },
    /// The final answer matches `value` (Rust `regex`; `(?i)` for case).
    Regex { value: String },
    /// `bash -c <run>` in the worktree after the agent; exit 0 = pass.
    Command { run: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub id: String,
    /// A git URL, or a local path relative to the repo root.
    pub repo: String,
    /// A full commit sha.
    pub rev: String,
    pub kind: TaskKind,
    pub prompt: String,
    pub check: Check,
    #[serde(default = "default_timeout")]
    pub timeout_s: u64,
}

fn default_timeout() -> u64 {
    DEFAULT_TIMEOUT_S
}

impl Task {
    /// A URL git clones as is (`https://`, `ssh://`, `git@…`, `file://`).
    pub fn repo_is_url(&self) -> bool {
        is_url(&self.repo)
    }

    /// Where to clone from: the URL, or the local path under `repo_root`.
    pub fn repo_source(&self, repo_root: &Path) -> String {
        if self.repo_is_url() {
            self.repo.clone()
        } else {
            let rel = self.repo.trim_start_matches("./").trim_end_matches('/');
            match rel {
                "." | "" => repo_root.display().to_string(),
                rel => repo_root.join(rel).display().to_string(),
            }
        }
    }
}

fn is_url(repo: &str) -> bool {
    ["https://", "http://", "ssh://", "git://", "file://"]
        .iter()
        .any(|p| repo.starts_with(p))
        || (repo.starts_with("git@") && repo.contains(':'))
}

/// Parses and validates a whole suite: every problem, one per line.
pub fn parse(yaml: &str) -> Result<Vec<Task>> {
    let tasks: Vec<Task> = serde_yaml::from_str(yaml).context("tasks file is not a task list")?;
    let mut problems: Vec<String> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for (i, t) in tasks.iter().enumerate() {
        let at = if t.id.is_empty() {
            format!("task #{}", i + 1)
        } else {
            format!("task {}", t.id)
        };
        for p in problems_of(t) {
            problems.push(format!("{at}: {p}"));
        }
        if !t.id.is_empty() && !seen.insert(t.id.as_str()) {
            problems.push(format!("{at}: id is not unique"));
        }
    }
    if tasks.is_empty() {
        problems.push("no tasks".into());
    }
    if !problems.is_empty() {
        bail!("invalid tasks file:\n  {}", problems.join("\n  "));
    }
    Ok(tasks)
}

fn problems_of(t: &Task) -> Vec<String> {
    let mut out = Vec::new();
    let id_ok = !t.id.is_empty()
        && t.id.len() <= 64
        && t.id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !id_ok {
        out.push("id must be 1-64 chars of a-z, 0-9, - or _".into());
    }
    if t.repo.trim().is_empty() {
        out.push("repo is empty".into());
    } else if !is_url(&t.repo) {
        let p = Path::new(&t.repo);
        if p.is_absolute() || p.components().any(|c| c == std::path::Component::ParentDir) {
            out.push("a local repo is a path inside the repo root (relative, no ..)".into());
        }
    }
    if t.rev.len() != 40 || !t.rev.chars().all(|c| c.is_ascii_hexdigit()) {
        out.push(format!(
            "rev must be a full 40-char commit sha, got {:?}",
            t.rev
        ));
    }
    if t.prompt.trim().is_empty() {
        out.push("prompt is empty".into());
    }
    match &t.check {
        Check::Contains { value } if value.is_empty() => {
            out.push("check.value is empty".into());
        }
        Check::Regex { value } => {
            if let Err(e) = regex::Regex::new(value) {
                out.push(format!("check.value is not a regex: {e}"));
            }
        }
        Check::Command { run } if run.trim().is_empty() => {
            out.push("check.run is empty".into());
        }
        _ => {}
    }
    if t.kind == TaskKind::Edit && !matches!(t.check, Check::Command { .. }) {
        out.push("an edit task is scored by a command check".into());
    }
    if t.timeout_s == 0 || t.timeout_s > MAX_TIMEOUT_S {
        out.push(format!("timeout_s must be 1..={MAX_TIMEOUT_S}"));
    }
    out
}

/// Reads and validates `path`.
pub fn load(path: &Path) -> Result<Vec<Task>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    parse(&text).with_context(|| path.display().to_string())
}

/// `tasks` narrowed to `only` (ids, in the file's order); an unknown id
/// is an error.
pub fn select(tasks: Vec<Task>, only: &[String]) -> Result<Vec<Task>> {
    if only.is_empty() {
        return Ok(tasks);
    }
    let unknown: Vec<&String> = only
        .iter()
        .filter(|id| !tasks.iter().any(|t| &t.id == *id))
        .collect();
    if !unknown.is_empty() {
        bail!("--only names unknown tasks: {unknown:?}");
    }
    Ok(tasks.into_iter().filter(|t| only.contains(&t.id)).collect())
}

/// Scores a final answer against an answer check; `None` for a command
/// check (run in the worktree instead).
pub fn score_answer(check: &Check, answer: &str) -> Option<bool> {
    match check {
        Check::Contains { value } => Some(answer.contains(value.as_str())),
        // Validated when the file was loaded.
        Check::Regex { value } => Some(regex::Regex::new(value).is_ok_and(|r| r.is_match(answer))),
        Check::Command { .. } => None,
    }
}

/// The check command with the relative paths it names made absolute
/// where they are the suite's: under `suite_dir` (the tasks file's
/// directory, relative to `eval_root`), or missing from the worktree but in
/// `eval_root`. Check scripts live with the suite; the worktree is the
/// task's repo at another commit, which may have an older copy or none.
pub fn resolve_command(run: &str, worktree: &Path, eval_root: &Path, suite_dir: &Path) -> String {
    run.split(' ')
        .map(|word| {
            let rel = Path::new(word);
            let pathlike = word.contains('/') && rel.is_relative() && !word.starts_with('-');
            let ours = eval_root.join(rel);
            let suites = !suite_dir.as_os_str().is_empty() && rel.starts_with(suite_dir);
            if pathlike && ours.exists() && (suites || !worktree.join(rel).exists()) {
                shell_quote(&ours)
            } else {
                word.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn suite(body: &str) -> String {
        body.replace("SHA", SHA)
    }

    #[test]
    fn parses_the_contract() {
        let yaml = suite(
            r#"
- id: refs-open-workspace
  repo: https://github.com/JeanIsahakyan/theodore
  rev: SHA
  kind: answer
  prompt: |
    Who calls open_workspace?
  check:
    type: contains
    value: "open_code_workspace"
- id: edit-rename
  repo: .
  rev: SHA
  kind: edit
  prompt: rename it
  check:
    type: command
    run: "bash docs/eval/agent-tools/checks/x.sh"
  timeout_s: 900
- id: regex-one
  repo: git@github.com:o/r.git
  rev: SHA
  kind: answer
  prompt: where?
  check: { type: regex, value: "(?i)src/.*\\.rs" }
"#,
        );
        let tasks = parse(&yaml).unwrap();
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[0].kind, TaskKind::Answer);
        assert_eq!(tasks[0].timeout_s, DEFAULT_TIMEOUT_S);
        assert_eq!(tasks[0].prompt, "Who calls open_workspace?\n");
        assert!(tasks[0].repo_is_url());
        assert_eq!(tasks[1].timeout_s, 900);
        assert!(!tasks[1].repo_is_url());
        assert_eq!(tasks[1].repo_source(Path::new("/r")), "/r".to_string());
        assert!(tasks[2].repo_is_url());
        assert!(matches!(tasks[1].check, Check::Command { .. }));
    }

    #[test]
    fn reports_every_problem_at_once() {
        let yaml = r#"
- id: Bad Id
  repo: ../outside
  rev: abc
  kind: edit
  prompt: " "
  check: { type: contains, value: "" }
  timeout_s: 0
- id: dup
  repo: .
  rev: 0123456789abcdef0123456789abcdef01234567
  kind: answer
  prompt: p
  check: { type: regex, value: "(" }
- id: dup
  repo: .
  rev: 0123456789abcdef0123456789abcdef01234567
  kind: answer
  prompt: p
  check: { type: command, run: "" }
"#;
        let err = format!("{:#}", parse(yaml).unwrap_err());
        for want in [
            "task Bad Id: id must be",
            "relative, no ..",
            "full 40-char commit sha",
            "prompt is empty",
            "check.value is empty",
            "edit task is scored by a command",
            "timeout_s must be",
            "task dup: check.value is not a regex",
            "task dup: check.run is empty",
            "task dup: id is not unique",
        ] {
            assert!(err.contains(want), "missing {want:?} in:\n{err}");
        }
    }

    #[test]
    fn unknown_fields_and_kinds_are_refused() {
        let extra = suite(
            "- {id: a, repo: ., rev: SHA, kind: answer, prompt: p, check: {type: contains, value: v}, model: x}",
        );
        assert!(parse(&extra).is_err());
        let kind = suite(
            "- {id: a, repo: ., rev: SHA, kind: review, prompt: p, check: {type: contains, value: v}}",
        );
        assert!(parse(&kind).is_err());
        let check = suite(
            "- {id: a, repo: ., rev: SHA, kind: answer, prompt: p, check: {type: contains, run: v}}",
        );
        assert!(parse(&check).is_err());
        assert!(parse("[]").is_err(), "an empty suite is refused");
    }

    #[test]
    fn only_narrows_and_rejects_unknown_ids() {
        let yaml = suite(
            "- {id: a, repo: ., rev: SHA, kind: answer, prompt: p, check: {type: contains, value: v}}\n\
             - {id: b, repo: ., rev: SHA, kind: answer, prompt: p, check: {type: contains, value: v}}",
        );
        let tasks = parse(&yaml).unwrap();
        let only = select(tasks.clone(), &["b".into()]).unwrap();
        assert_eq!(only.len(), 1);
        assert_eq!(only[0].id, "b");
        assert_eq!(select(tasks.clone(), &[]).unwrap().len(), 2);
        assert!(select(tasks, &["zz".into()]).is_err());
    }

    #[test]
    fn answers_are_scored_by_contains_and_regex() {
        let c = Check::Contains {
            value: "Core::open".into(),
        };
        assert_eq!(score_answer(&c, "it is Core::open, in core.rs"), Some(true));
        assert_eq!(score_answer(&c, "core::open"), Some(false));
        let r = Check::Regex {
            value: r"(?i)crates/app/src/\w+\.rs".into(),
        };
        assert_eq!(score_answer(&r, "In CRATES/app/src/core.rs"), Some(true));
        assert_eq!(score_answer(&r, "nowhere"), Some(false));
        let cmd = Check::Command { run: "true".into() };
        assert_eq!(score_answer(&cmd, "x"), None);
    }

    #[test]
    fn check_scripts_resolve_against_the_suite() {
        let suite_root = tempfile::tempdir().unwrap();
        let wt = tempfile::tempdir().unwrap();
        for dir in [suite_root.path(), wt.path()] {
            std::fs::create_dir_all(dir.join("suite/checks")).unwrap();
            std::fs::write(dir.join("suite/checks/x.sh"), "exit 0").unwrap();
            std::fs::create_dir_all(dir.join("src")).unwrap();
            std::fs::write(dir.join("src/a.rs"), "").unwrap();
        }
        std::fs::create_dir_all(suite_root.path().join("tools")).unwrap();
        std::fs::write(suite_root.path().join("tools/y.sh"), "").unwrap();
        let cmd = resolve_command(
            "bash suite/checks/x.sh src/a.rs tools/y.sh --flag",
            wt.path(),
            suite_root.path(),
            Path::new("suite"),
        );
        // The suite's script even though the worktree has a copy; the
        // worktree's own file; a script only the suite has.
        let want = format!(
            "bash '{}' src/a.rs '{}' --flag",
            suite_root.path().join("suite/checks/x.sh").display(),
            suite_root.path().join("tools/y.sh").display()
        );
        assert_eq!(cmd, want);
    }
}
