//! One short, read-only, non-streaming agent turn with no tools and no
//! session: the building block behind a space's name and chat title
//! (`space_naming`) and its handoff summary (`space_handoff`). Never the
//! agent that writes code for the space — always the chat agent, and
//! always its fastest, cheapest model, so these calls stay cheap and
//! `stream_text: false` keeps them off the UI (and, in tests, off the
//! fake agent's turn-reply queue — see `testkit::FakeAgent`).

use std::path::Path;

use theodore_domain::AgentId;
use theodore_ports::{AgentEvent, AgentMode, AgentRequest};

use crate::core::Core;

/// Claude Code's `haiku` alias is documented as "Fastest, cheapest"
/// (`crates/agents/src/models.rs::claude_aliases`); Codex and Gemini have
/// no such alias in their CLIs, so effort is turned down instead where
/// that's possible.
fn cheapest(agent: AgentId) -> (Option<String>, Option<String>) {
    match agent {
        AgentId::Claude => (Some("haiku".into()), None),
        AgentId::Codex => (None, Some("low".into())),
        AgentId::Gemini => (None, None),
    }
}

/// Runs `prompt` to completion and returns its final text, trimmed;
/// `None` on any failure — the CLI missing, offline, cancelled before an
/// answer, or an empty one — so callers fall back to what they had
/// without surfacing an error.
pub(crate) async fn run(
    core: &Core,
    agent: AgentId,
    system: &str,
    prompt: &str,
    cwd: &Path,
) -> Option<String> {
    let (model, effort) = cheapest(agent);
    let req = AgentRequest {
        agent,
        prompt: prompt.to_string(),
        system: Some(system.to_string()),
        cwd: cwd.to_path_buf(),
        mode: AgentMode::ReadOnly,
        mcp: None,
        model,
        effort,
        stall_after_ms: core.stall_after_ms().await,
        env: Vec::new(),
        user_mcp: false,
        attachments: Vec::new(),
        resume: None,
        stream_text: false,
    };
    let mut run = core.0.deps.agents.run(req).await.ok()?;
    let mut text = String::new();
    loop {
        match run.events.recv().await {
            Some(AgentEvent::Text { text: t }) => text.push_str(&t),
            Some(AgentEvent::Done { final_text, .. }) => {
                if !final_text.is_empty() {
                    text = final_text;
                }
                break;
            }
            Some(AgentEvent::Error { .. }) => return None,
            Some(_) => {}
            None => break,
        }
    }
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}
