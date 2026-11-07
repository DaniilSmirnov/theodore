//! What a running agent shows in the window while it works: its answer as
//! `Ephemeral::AgentDelta`, its tool calls and thinking as `Ephemeral::RunLog`
//! (a `thinking: ` line stays out of the UI's tool log). One
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
//!
//! The timeline reads its rows from their own events, the same data a reply
//! stores (`turn_steps`). The tool log (`RunLog`, the AI panel's) stays one
//! line per call:
//!
//! ```text
//! TextStart          ──▶ RunTextBreak       the next delta is a new row
//! ToolCall t1        ──▶ RunLog  RunStep    a row, with its id
//! ToolResult t1      ──▶ RunLog  RunStepDone marks t1, whatever ran since
//! Thinking           ──▶ RunStep
//! Nested { Text }    ──▶ RunStep            under its Agent call
//! Nested { ToolCall }──▶ RunLog  RunStep    under its Agent call
//! ```

use std::collections::{HashMap, HashSet};

use theodore_contract::{Ephemeral, TurnStep};
use theodore_domain::AgentId;
use theodore_ports::AgentEvent;

use crate::core::Core;
use crate::usecases::agents::{tool_label, tool_log_line_with};
use crate::usecases::turn_steps::{
    THINKING_MAX, call_step, capped, handback, launched_in_background, result_summary,
};

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
    /// A call's label by its id, for what its result says.
    tools: HashMap<String, String>,
    /// Handback calls: no row, so their results mark nothing.
    hidden: HashSet<String>,
}

impl LiveRun {
    pub(crate) fn new(run: impl Into<String>, agent: AgentId) -> Self {
        Self {
            run: run.into(),
            agent,
            thinking: false,
            streamed: String::new(),
            tools: HashMap::new(),
            hidden: HashSet::new(),
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
            AgentEvent::TextStart => vec![self.text_break()],
            AgentEvent::Text { text } => match self.whole(text) {
                // A block no pieces came for is its own, marked or not.
                Some((t, true)) => vec![self.text_break(), self.text(t)],
                Some((t, false)) => vec![self.text(t)],
                None => vec![],
            },
            AgentEvent::Nested { parent, event } => self.nested(parent, event),
            AgentEvent::Thinking { text } if self.thinking => vec![self.text(text.clone())],
            AgentEvent::Thinking { text } if !text.trim().is_empty() => {
                vec![self.step(TurnStep::Thinking {
                    text: capped(text.trim(), THINKING_MAX),
                    parent: None,
                })]
            }
            AgentEvent::ToolCall { id, name, input } => self.call(id, None, name, input),
            AgentEvent::ToolResult { id, ok, output } => self.result(id, *ok, output),
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
            | AgentEvent::Plan { .. }
            | AgentEvent::Done { .. }
            | AgentEvent::Error { .. } => vec![],
        }
    }

    /// A subagent's: a row under its call, never the run's own text.
    fn nested(&mut self, parent: &str, event: &AgentEvent) -> Vec<Ephemeral> {
        match event {
            AgentEvent::Text { text } if !text.trim().is_empty() => {
                vec![self.step(TurnStep::Text {
                    text: capped(text.trim(), THINKING_MAX),
                    parent: Some(parent.to_string()),
                })]
            }
            AgentEvent::Thinking { text } if !text.trim().is_empty() => {
                vec![self.step(TurnStep::Thinking {
                    text: capped(text.trim(), THINKING_MAX),
                    parent: Some(parent.to_string()),
                })]
            }
            AgentEvent::ToolCall { id, name, input } => self.call(id, Some(parent), name, input),
            AgentEvent::ToolResult { id, ok, output } => self.result(id, *ok, output),
            _ => vec![],
        }
    }

    fn call(
        &mut self,
        id: &str,
        parent: Option<&str>,
        name: &str,
        input: &serde_json::Value,
    ) -> Vec<Ephemeral> {
        if let Some(report) = handback(name, input) {
            self.hidden.insert(id.to_string());
            return match (parent, report) {
                (Some(of), Some(report)) => vec![Ephemeral::RunStepDone {
                    run: self.run.clone(),
                    id: of.to_string(),
                    ok: None,
                    summary: Some(report),
                    background: false,
                }],
                _ => vec![],
            };
        }
        self.tools
            .insert(id.to_string(), tool_label(name).to_string());
        vec![
            self.log(tool_log_line_with(name, input)),
            self.step(call_step(id, parent, name, input)),
        ]
    }

    fn result(&mut self, id: &str, ok: bool, output: &str) -> Vec<Ephemeral> {
        if self.hidden.contains(id) {
            return vec![];
        }
        let tool = self.tools.get(id);
        let summary = tool.and_then(|t| result_summary(t, output));
        let background = tool.is_some_and(|t| launched_in_background(t, output));
        vec![
            self.log(format!("tool result ok={ok}")),
            Ephemeral::RunStepDone {
                run: self.run.clone(),
                id: id.to_string(),
                ok: Some(ok),
                summary,
                background,
            },
        ]
    }

    /// The part of a whole message the deltas didn't show, and whether it is
    /// a block of its own. Deltas cover the start of it, all of it, or it and
    /// the next block of the same message: only the rest is new. A message
    /// that matches them but for surrounding whitespace is the same one;
    /// anything else is a message they weren't about, shown whole.
    fn whole(&mut self, text: &str) -> Option<(String, bool)> {
        if self.streamed.trim().is_empty() {
            self.streamed.clear();
            return (!text.is_empty()).then(|| (text.to_string(), true));
        }
        if let Some(rest) = self.streamed.strip_prefix(text) {
            self.streamed = rest.to_string();
            return None;
        }
        let streamed = std::mem::take(&mut self.streamed);
        if let Some(rest) = text.strip_prefix(streamed.as_str()) {
            return (!rest.is_empty()).then(|| (rest.to_string(), false));
        }
        (text.trim() != streamed.trim()).then(|| (text.to_string(), true))
    }

    fn text(&self, text: String) -> Ephemeral {
        Ephemeral::AgentDelta {
            run: self.run.clone(),
            agent: self.agent,
            text,
        }
    }

    fn text_break(&self) -> Ephemeral {
        Ephemeral::RunTextBreak {
            run: self.run.clone(),
        }
    }

    fn step(&self, step: TurnStep) -> Ephemeral {
        Ephemeral::RunStep {
            run: self.run.clone(),
            step,
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
    fn thinking_is_a_step_unless_it_shows_as_text_and_never_in_the_log() {
        let events = [thinking(" hmm "), thinking("  ")];
        let mut l = live();
        let out: Vec<_> = events.iter().flat_map(|e| l.map(e)).collect();
        assert_eq!(
            out,
            vec![Ephemeral::RunStep {
                run: "r1".into(),
                step: TurnStep::Thinking {
                    text: "hmm".into(),
                    parent: None,
                },
            }]
        );
        assert!(shown(&mut live().with_thinking(), &events).1.is_empty());
    }

    #[test]
    fn a_block_start_breaks_the_text_and_the_text_stays_as_it_was() {
        let mut l = live();
        let out: Vec<_> = [
            AgentEvent::TextStart,
            delta("A"),
            text("A"),
            AgentEvent::TextStart,
            delta("B"),
        ]
        .iter()
        .flat_map(|e| l.map(e))
        .collect();
        let breaks = out
            .iter()
            .filter(|e| matches!(e, Ephemeral::RunTextBreak { .. }))
            .count();
        assert_eq!(breaks, 2);
        assert!(matches!(out[0], Ephemeral::RunTextBreak { .. }));
        assert!(matches!(out[2], Ephemeral::RunTextBreak { .. }));
    }

    /// A CLI that only sends whole blocks needs no marks: each is its own row.
    #[test]
    fn a_whole_block_no_pieces_came_for_starts_its_own_row() {
        let mut l = live();
        let out = l.map(&text("One."));
        assert!(matches!(out[0], Ephemeral::RunTextBreak { .. }));
        assert!(matches!(&out[1], Ephemeral::AgentDelta { text, .. } if text == "One."));
    }

    /// Parallel calls: a result marks its own call by id, whichever came last.
    #[test]
    fn a_result_names_its_own_call_with_what_it_found() {
        let mut l = live();
        let read = AgentEvent::ToolCall {
            id: "r".into(),
            name: "Read".into(),
            input: serde_json::json!({ "file_path": "a.rs" }),
        };
        l.map(&read);
        l.map(&tool("Bash"));
        let out = l.map(&AgentEvent::ToolResult {
            id: "r".into(),
            ok: true,
            output: "1\tx\n2\ty\n".into(),
        });
        assert_eq!(
            out[1],
            Ephemeral::RunStepDone {
                run: "r1".into(),
                id: "r".into(),
                ok: Some(true),
                summary: Some("2 lines".into()),
                background: false,
            }
        );
    }

    /// The AI panel's log reads a subagent's call as it always did; the
    /// timeline gets it under its parent, and its text stays out of the answer.
    #[test]
    fn a_subagents_steps_are_rows_under_their_parent() {
        let nested = |event| AgentEvent::Nested {
            parent: "ag".into(),
            event: Box::new(event),
        };
        let mut l = live();
        let out = l.map(&nested(tool("Read")));
        assert!(
            matches!(&out[0], Ephemeral::RunLog { line, .. } if line == "tool: Read · src/lib.rs")
        );
        assert!(matches!(
            &out[1],
            Ephemeral::RunStep { step: TurnStep::Tool { parent: Some(p), .. }, .. } if p == "ag"
        ));
        let out = l.map(&nested(text("Found it.")));
        assert!(matches!(
            out.as_slice(),
            [Ephemeral::RunStep {
                step: TurnStep::Text {
                    parent: Some(_),
                    ..
                },
                ..
            }]
        ));
    }

    #[test]
    fn a_handback_is_no_row_and_tells_its_agents_row() {
        let mut l = live();
        let out = l.map(&AgentEvent::Nested {
            parent: "ag".into(),
            event: Box::new(AgentEvent::ToolCall {
                id: "h".into(),
                name: "SubagentHandback".into(),
                input: serde_json::json!({ "message": "All fine." }),
            }),
        });
        assert_eq!(
            out,
            vec![Ephemeral::RunStepDone {
                run: "r1".into(),
                id: "ag".into(),
                ok: None,
                summary: Some("All fine.".into()),
                background: false,
            }]
        );
        let out = l.map(&AgentEvent::ToolResult {
            id: "h".into(),
            ok: true,
            output: "{}".into(),
        });
        assert!(out.is_empty());
    }

    /// The launch note is the CLI's plumbing: it says a subagent runs in the
    /// background, and no text of it reaches the row.
    #[test]
    fn a_background_launch_is_marked_and_says_nothing() {
        let mut l = live();
        l.map(&AgentEvent::ToolCall {
            id: "ag".into(),
            name: "Agent".into(),
            input: serde_json::json!({ "description": "Read it", "run_in_background": true }),
        });
        let out = l.map(&AgentEvent::ToolResult {
            id: "ag".into(),
            ok: true,
            output: "Async agent launched successfully.\nagentId: a1\n(This tool result is intended for the agent.)".into(),
        });
        assert_eq!(
            out[1],
            Ephemeral::RunStepDone {
                run: "r1".into(),
                id: "ag".into(),
                ok: Some(true),
                summary: None,
                background: true,
            }
        );
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
