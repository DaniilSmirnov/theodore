//! What a running agent shows in the window while it works: its answer as
//! `Ephemeral::AgentDelta`, its tool calls as `Ephemeral::RunLog`. One
//! mapping for every run (chat asks, skills, space turns, fix tasks), so a
//! runner that streams its text (`AgentRequest::stream_text`) streams it
//! everywhere, and none shows a message twice.
//!
//! A streaming runner reports a message twice: piece by piece
//! (`TextDelta`), then whole (`Text`). The UI appends every `AgentDelta`,
//! so the whole one only adds what the pieces didn't already show:
//!
//! ```text
//! TextDelta "Hel"  ──▶ AgentDelta "Hel"     shown  "Hel"
//! TextDelta "lo"   ──▶ AgentDelta "lo"      shown  "Hello"
//! Text "Hello"     ──▶ (nothing)            shown  "Hello"
//! Text "Next."     ──▶ AgentDelta "Next."   no pieces came: shown whole
//! ```

use theodore_contract::Ephemeral;
use theodore_domain::AgentId;
use theodore_ports::AgentEvent;

use crate::core::Core;
use crate::usecases::agents::tool_log_line_with;

/// One run's live view: maps its `AgentEvent`s to the `Ephemeral`s the UI
/// shows under `run`.
pub(crate) struct LiveRun {
    run: String,
    agent: AgentId,
    /// Thinking shows as text too (a skill's or a fix's progress); a chat
    /// answer keeps to the answer.
    thinking: bool,
    /// What deltas showed that no `Text` has accounted for yet.
    streamed: String,
}

impl LiveRun {
    pub(crate) fn new(run: impl Into<String>, agent: AgentId) -> Self {
        Self {
            run: run.into(),
            agent,
            thinking: false,
            streamed: String::new(),
        }
    }

    pub(crate) fn with_thinking(mut self) -> Self {
        self.thinking = true;
        self
    }

    /// Emits what `event` shows.
    pub(crate) fn show(&mut self, core: &Core, event: &AgentEvent) {
        for e in self.map(event) {
            core.emit(e);
        }
    }

    /// Pure: what `event` shows, in order.
    pub(crate) fn map(&mut self, event: &AgentEvent) -> Vec<Ephemeral> {
        match event {
            AgentEvent::TextDelta { text } => {
                self.streamed.push_str(text);
                vec![self.text(text.clone())]
            }
            AgentEvent::Text { text } => {
                self.whole(text).map(|t| self.text(t)).into_iter().collect()
            }
            AgentEvent::Thinking { text } if self.thinking => vec![self.text(text.clone())],
            AgentEvent::ToolCall { name, input, .. } => {
                vec![self.log(tool_log_line_with(name, input))]
            }
            AgentEvent::ToolResult { ok, .. } => vec![self.log(format!("tool result ok={ok}"))],
            AgentEvent::Stalled => vec![
                self.log("stalled".into()),
                Ephemeral::RunStalled {
                    run: self.run.clone(),
                    agent: self.agent,
                },
            ],
            AgentEvent::Thinking { .. }
            | AgentEvent::Usage { .. }
            | AgentEvent::Session { .. }
            | AgentEvent::Done { .. }
            | AgentEvent::Error { .. } => vec![],
        }
    }

    /// The part of a whole message the deltas didn't show. Deltas cover
    /// the start of it, all of it, or it and the next block of the same
    /// message: only the rest is new. A message that matches them but for
    /// surrounding whitespace is the same one; anything else is a message
    /// they weren't about, shown whole.
    fn whole(&mut self, text: &str) -> Option<String> {
        if self.streamed.trim().is_empty() {
            self.streamed.clear();
            return (!text.is_empty()).then(|| text.to_string());
        }
        if let Some(rest) = self.streamed.strip_prefix(text) {
            self.streamed = rest.to_string();
            return None;
        }
        let streamed = std::mem::take(&mut self.streamed);
        if let Some(rest) = text.strip_prefix(streamed.as_str()) {
            return (!rest.is_empty()).then(|| rest.to_string());
        }
        (text.trim() != streamed.trim()).then(|| text.to_string())
    }

    fn text(&self, text: String) -> Ephemeral {
        Ephemeral::AgentDelta {
            run: self.run.clone(),
            agent: self.agent,
            text,
        }
    }

    fn log(&self, line: String) -> Ephemeral {
        Ephemeral::RunLog {
            run: self.run.clone(),
            agent: self.agent,
            line,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delta(t: &str) -> AgentEvent {
        AgentEvent::TextDelta { text: t.into() }
    }
    fn text(t: &str) -> AgentEvent {
        AgentEvent::Text { text: t.into() }
    }
    fn thinking(t: &str) -> AgentEvent {
        AgentEvent::Thinking { text: t.into() }
    }
    fn tool(name: &str) -> AgentEvent {
        AgentEvent::ToolCall {
            id: "t1".into(),
            name: name.into(),
            input: serde_json::json!({ "file_path": "src/lib.rs" }),
        }
    }

    /// Everything the run showed, as the UI builds it: text appended, log
    /// lines one by one.
    fn shown(live: &mut LiveRun, events: &[AgentEvent]) -> (String, Vec<String>) {
        let mut text = String::new();
        let mut log = Vec::new();
        for e in events {
            for out in live.map(e) {
                match out {
                    Ephemeral::AgentDelta { run, text: t, .. } => {
                        assert_eq!(run, "r1");
                        text.push_str(&t);
                    }
                    Ephemeral::RunLog { line, .. } => log.push(line),
                    _ => {}
                }
            }
        }
        (text, log)
    }

    fn live() -> LiveRun {
        LiveRun::new("r1", AgentId::Claude)
    }

    #[test]
    fn deltas_then_the_whole_message_show_it_once() {
        let mut l = live();
        let out: Vec<_> = [delta("Hel"), delta("lo"), text("Hello")]
            .iter()
            .flat_map(|e| l.map(e))
            .collect();
        // Each delta as it comes, and nothing for the whole message.
        assert_eq!(out.len(), 2);
        assert_eq!(
            shown(&mut live(), &[delta("Hel"), delta("lo"), text("Hello")]).0,
            "Hello"
        );
    }

    #[test]
    fn a_message_without_deltas_shows_whole() {
        let (t, _) = shown(&mut live(), &[text("Hello")]);
        assert_eq!(t, "Hello");
    }

    #[test]
    fn a_turn_of_several_messages_shows_each_once_in_order() {
        let events = [
            delta("Let me "),
            delta("look."),
            text("Let me look."),
            tool("Read"),
            AgentEvent::ToolResult {
                id: "t1".into(),
                ok: true,
                output: String::new(),
            },
            delta("Found "),
            delta("it."),
            text("Found it."),
        ];
        let (t, log) = shown(&mut live(), &events);
        // Same separators as unstreamed messages: none added.
        assert_eq!(t, "Let me look.Found it.");
        assert_eq!(log, vec!["tool: Read · src/lib.rs", "tool result ok=true"]);
        // Unstreamed, the same turn shows the same.
        let (plain, _) = shown(
            &mut live(),
            &[text("Let me look."), tool("Read"), text("Found it.")],
        );
        assert_eq!(plain, t);
    }

    #[test]
    fn only_the_tail_the_deltas_missed_is_added() {
        let (t, _) = shown(&mut live(), &[delta("Hel"), text("Hello"), text("Bye")]);
        assert_eq!(t, "HelloBye");
    }

    #[test]
    fn deltas_spanning_two_blocks_of_one_message_show_each_once() {
        let (t, _) = shown(
            &mut live(),
            &[delta("A"), delta("B"), text("A"), text("B"), text("C")],
        );
        assert_eq!(t, "ABC");
    }

    #[test]
    fn whitespace_around_a_streamed_message_is_not_a_new_one() {
        let (t, _) = shown(&mut live(), &[delta("Hello "), text("Hello"), text("Next")]);
        assert_eq!(t, "Hello Next");
        let (t, _) = shown(&mut live(), &[delta("\nHi\n"), text("Hi")]);
        assert_eq!(t, "\nHi\n");
    }

    #[test]
    fn a_message_the_deltas_were_not_about_still_shows() {
        let (t, _) = shown(&mut live(), &[delta("abc"), text("xyz")]);
        assert_eq!(t, "abcxyz");
    }

    #[test]
    fn thinking_shows_only_where_asked_and_never_counts_as_streamed() {
        let events = [thinking("hmm"), delta("Hi"), text("Hi")];
        assert_eq!(shown(&mut live(), &events).0, "Hi");
        assert_eq!(shown(&mut live().with_thinking(), &events).0, "hmmHi");
    }

    #[test]
    fn deltas_never_confirmed_by_a_whole_message_stay_shown() {
        // A run that fails (or ends) mid-message: what streamed is all
        // there is, and nothing takes it back.
        let error = AgentEvent::Error {
            message: "boom".into(),
            stderr_tail: String::new(),
        };
        let (t, _) = shown(&mut live(), &[delta("Half "), delta("an answer"), error]);
        assert_eq!(t, "Half an answer");
        let done = AgentEvent::Done {
            final_text: String::new(),
            usage: Default::default(),
        };
        let (t, _) = shown(&mut live(), &[delta("All of it"), done]);
        assert_eq!(t, "All of it");
    }

    #[test]
    fn a_runner_that_never_streams_shows_each_message_whole() {
        let (t, log) = shown(
            &mut live(),
            &[text("Looking."), tool("Read"), text("Done.")],
        );
        assert_eq!(t, "Looking.Done.");
        assert_eq!(log, vec!["tool: Read · src/lib.rs"]);
    }

    #[test]
    fn a_stall_is_logged_and_flagged() {
        let out = live().map(&AgentEvent::Stalled);
        assert_eq!(
            out,
            vec![
                Ephemeral::RunLog {
                    run: "r1".into(),
                    agent: AgentId::Claude,
                    line: "stalled".into()
                },
                Ephemeral::RunStalled {
                    run: "r1".into(),
                    agent: AgentId::Claude
                },
            ]
        );
    }

    #[test]
    fn terminal_and_bookkeeping_events_show_nothing() {
        let mut l = live();
        for e in [
            AgentEvent::Session { id: "s".into() },
            AgentEvent::Usage {
                usage: Default::default(),
            },
            AgentEvent::Done {
                final_text: "x".into(),
                usage: Default::default(),
            },
            AgentEvent::Error {
                message: "x".into(),
                stderr_tail: String::new(),
            },
        ] {
            assert!(l.map(&e).is_empty(), "{e:?}");
        }
    }
}
