//! A turn that can't resume its agent session — another agent, Gemini
//! (never resumes), after Undo — replays the transcript instead
//! (`spaces::turn_prompt`, capped at the last `HISTORY_MESSAGES`
//! messages). Once a space has outgrown that window, replaying only the
//! window drops everything before it; this module prepends a summary of
//! that earlier part, generated once by the chat agent's cheapest model
//! and cached on the space (`Space::handoff_summary`), ahead of the
//! replayed window (docs/spaces.md §5 "One session per space").

use std::path::Path;

use theodore_contract::{ChatMessage, Space, models};
use theodore_domain::AgentId;
use theodore_ports::{NewAction, SyncActionCode};

use crate::core::Core;
use crate::usecases::one_shot;
use crate::usecases::spaces::{HISTORY_MESSAGES, turn_prompt};

/// Marks a handoff-summary call for a test's `push_tagged`, distinct from
/// `space_naming`'s own system prompt.
pub(crate) const SUMMARY_SYSTEM: &str = "Summarize an earlier part of a conversation between someone and \
     a coding agent, so a different agent session can pick up the work without the full \
     transcript. Cover the goal, decisions made, files or areas touched, and what is left to \
     do. Plain prose, under 12 lines, no headings, no code blocks, nothing else.";

/// Older messages fed to the summarizer, most recent first truncated:
/// enough to carry the thread of a long space without an unbounded prompt.
const SUMMARY_INPUT_MESSAGES: usize = 40;
const SUMMARY_INPUT_CHARS: usize = 2_000;
/// The cached summary itself, so a stale space never grows the prompt
/// without bound.
const SUMMARY_MAX_CHARS: usize = 1_500;

/// The prompt a turn that can't resume its session sends: `text`, preceded
/// by the recent transcript and, once the space has outgrown that window,
/// a summary of what came before it.
pub(crate) async fn replay_prompt(
    core: &Core,
    space: &Space,
    agent: AgentId,
    cwd: &Path,
    history: &[ChatMessage],
    text: &str,
) -> String {
    let recent = turn_prompt(history, text);
    if history.len() <= HISTORY_MESSAGES {
        return recent;
    }
    let older = &history[..history.len() - HISTORY_MESSAGES];
    match handoff_summary(core, space, agent, cwd, older).await {
        Some(summary) => {
            format!("Summary of the conversation before this point:\n{summary}\n\n{recent}")
        }
        // Today's plain replay: everything the window drops is simply
        // not there this turn, as before this module existed.
        None => recent,
    }
}

/// The cached summary, or a freshly generated one on its first use —
/// cached on the space so it's never asked for twice. `None` on any
/// failure (offline, no CLI, an empty answer): the caller falls back to
/// a plain replay.
async fn handoff_summary(
    core: &Core,
    space: &Space,
    agent: AgentId,
    cwd: &Path,
    older: &[ChatMessage],
) -> Option<String> {
    if let Some(cached) = &space.handoff_summary {
        return Some(cached.clone());
    }
    let transcript = render(older);
    let answer = one_shot::run(core, agent, SUMMARY_SYSTEM, &transcript, cwd).await?;
    let summary: String = answer.chars().take(SUMMARY_MAX_CHARS).collect();
    let _ = core
        .apply(vec![NewAction {
            model: models::SPACE.into(),
            model_id: space.id.clone(),
            action: SyncActionCode::C,
            data: Some(serde_json::json!({ "handoff_summary": summary })),
        }])
        .await;
    Some(summary)
}

/// `older`, oldest first within the bound, as plain `[who]\ntext` turns —
/// the same shape `turn_prompt` replays the recent window in.
fn render(older: &[ChatMessage]) -> String {
    let bounded = &older[older.len().saturating_sub(SUMMARY_INPUT_MESSAGES)..];
    let mut out = String::new();
    for m in bounded {
        let who = match m.author {
            theodore_domain::Author::Me => "user".to_string(),
            theodore_domain::Author::Agent(a) => a.as_str().to_string(),
        };
        let body: String = m.text.chars().take(SUMMARY_INPUT_CHARS).collect();
        out.push_str(&format!("[{who}]\n{body}\n\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(author: theodore_domain::Author, text: &str, at: i64) -> ChatMessage {
        ChatMessage {
            id: at.to_string(),
            pr: crate::usecases::spaces::space_chat_key("s"),
            author,
            text: text.into(),
            refs: vec![],
            at,
            failed: None,
            attachments: vec![],
            queued: None,
        }
    }

    #[test]
    fn render_keeps_only_the_most_recent_bounded_messages() {
        let many: Vec<_> = (0..50)
            .map(|i| msg(theodore_domain::Author::Me, &format!("m{i}"), i))
            .collect();
        let out = render(&many);
        assert!(!out.contains("[user]\nm9\n"), "{out}");
        assert!(out.contains("[user]\nm49\n"));
    }

    #[test]
    fn render_caps_each_message() {
        let long = "x".repeat(SUMMARY_INPUT_CHARS + 500);
        let out = render(&[msg(theodore_domain::Author::Me, &long, 0)]);
        let body = out.strip_prefix("[user]\n").unwrap().trim();
        assert_eq!(body.len(), SUMMARY_INPUT_CHARS);
    }
}
