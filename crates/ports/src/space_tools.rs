//! What a space's agent gets from the `space-tools` MCP server
//! (docs/spaces.md "Tools the agent gets"): the space's configured
//! commands, never an arbitrary shell. `app` produces these, the MCP
//! server sends them back as JSON.
//!
//! ```text
//! agent ─▶ run_check ─────────────▶ check, now ─▶ exit code + output tail
//!       ─▶ start_app / stop_app ──▶ `run`      ─▶ running?, ports, URLs
//!       ─▶ app_status · read_logs · space_info   (read only)
//!       ─▶ ask_user ─▶ card in the space ─▶ you answer ─▶ the answers
//!                                        └─ Stop / timeout ─▶ an error
//! a repo command not allowed yet ─▶ NotAllowed, nothing runs
//! ```

use serde::{Deserialize, Serialize};

/// One of a space's commands (docs/config.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpaceCommandName {
    Setup,
    Run,
    Check,
    Archive,
}

impl SpaceCommandName {
    pub const ALL: [SpaceCommandName; 4] = [Self::Setup, Self::Run, Self::Check, Self::Archive];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Run => "run",
            Self::Check => "check",
            Self::Archive => "archive",
        }
    }
}

/// `run_check`: how the check ended and its last lines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckOutcome {
    pub passed: bool,
    /// `None` when killed, timed out or never started.
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    /// One line: "check finished", "check exited with 1", …
    pub summary: String,
    /// The last lines of output, oldest first (bounded).
    pub output: Vec<String>,
}

/// `app_status` (and what `start_app` / `stop_app` return): the space's
/// `run` command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppStatus {
    /// A `spaces.run` command is set.
    pub configured: bool,
    /// …and may run (not a repo command the user hasn't allowed yet).
    pub allowed: bool,
    pub running: bool,
    /// The space's reserved ports (`$THEODORE_PORT`, `_1`, …).
    pub ports: Vec<u16>,
    /// `http://localhost:<port>` for each port.
    pub urls: Vec<String>,
    /// How the last run ended, when it has: "exited", "stopped", "failed".
    pub last_status: Option<String>,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
}

/// `read_logs`: the last lines of one command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LogTail {
    pub kind: SpaceCommandName,
    /// Oldest first; stderr lines start with `! `.
    pub lines: Vec<String>,
    /// Lines of this command Theodore still holds (`lines` is the tail).
    pub available: usize,
}

/// One command in `space_info`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandInfo {
    pub kind: SpaceCommandName,
    pub configured: bool,
    pub allowed: bool,
}

/// `space_info`. Environment variable names only, never their values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SpaceInfo {
    pub name: String,
    pub branch: String,
    pub base: String,
    pub worktree: String,
    pub ports: Vec<u16>,
    pub urls: Vec<String>,
    pub env: Vec<String>,
    pub commands: Vec<CommandInfo>,
}

/// How long `ask_user` waits for you before the agent is told to go on
/// without an answer.
pub const ASK_USER_TIMEOUT_MS: u64 = 30 * 60 * 1000;

/// `ask_user`: 1–3 questions at once ([`ASK_USER_MAX_QUESTIONS`]).
pub const ASK_USER_MAX_QUESTIONS: usize = 3;
/// …each with 2–4 options; "Other" (free text) is always offered on top.
pub const ASK_USER_OPTIONS: std::ops::RangeInclusive<usize> = 2..=4;

/// One question the agent asks you (`ask_user`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserQuestion {
    pub question: String,
    pub options: Vec<String>,
    /// Several options may be picked.
    #[serde(default)]
    pub multi: bool,
}

/// Your answer to one [`UserQuestion`], in the order they were asked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserAnswer {
    pub question: String,
    /// The options you picked, as written.
    pub selected: Vec<String>,
    /// What you typed under "Other".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub other: Option<String>,
}

/// Why a set of questions can't be asked, `None` when it can.
pub fn invalid_questions(questions: &[UserQuestion]) -> Option<String> {
    if questions.is_empty() || questions.len() > ASK_USER_MAX_QUESTIONS {
        return Some(format!(
            "ask 1 to {ASK_USER_MAX_QUESTIONS} questions at once"
        ));
    }
    for q in questions {
        if q.question.trim().is_empty() {
            return Some("a question is empty".into());
        }
        if !ASK_USER_OPTIONS.contains(&q.options.len()) {
            return Some(format!(
                "give {} to {} options per question (\"{}\" has {})",
                ASK_USER_OPTIONS.start(),
                ASK_USER_OPTIONS.end(),
                q.question,
                q.options.len()
            ));
        }
        if q.options.iter().any(|o| o.trim().is_empty()) {
            return Some(format!("an option of \"{}\" is empty", q.question));
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpaceToolError {
    /// The trust gate: a repo command the user hasn't allowed (or said
    /// "Not now" to). Nothing ran.
    #[error("not allowed by the user yet: {0}")]
    NotAllowed(String),
    /// Not configured, archived, unknown space…
    #[error("{0}")]
    Unavailable(String),
}

/// `http://localhost:<port>` for each port.
pub fn local_urls(ports: &[u16]) -> Vec<String> {
    ports
        .iter()
        .map(|p| format!("http://localhost:{p}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(options: &[&str]) -> UserQuestion {
        UserQuestion {
            question: "Which?".into(),
            options: options.iter().map(|o| o.to_string()).collect(),
            multi: false,
        }
    }

    #[test]
    fn questions_are_bounded() {
        assert_eq!(invalid_questions(&[q(&["a", "b"])]), None);
        assert_eq!(invalid_questions(&[q(&["a", "b", "c", "d"])]), None);
        assert!(invalid_questions(&[]).is_some());
        assert!(invalid_questions(&[q(&["a"])]).is_some());
        assert!(invalid_questions(&[q(&["a", "b", "c", "d", "e"])]).is_some());
        assert!(invalid_questions(&[q(&["a", " "])]).is_some());
        let four = vec![q(&["a", "b"]); 4];
        assert!(invalid_questions(&four).is_some());
    }
}
