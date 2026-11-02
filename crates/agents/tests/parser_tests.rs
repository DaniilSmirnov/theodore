//! Fixture-driven parser tests: feed each CLI's normalizer every line of a
//! recorded (or, where noted, documented-schema) transcript and check the
//! fold ends in exactly one terminal event with the expected payload.

use theodore_agents::{claude, codex, gemini};
use theodore_domain::Usage;
use theodore_ports::agent::AgentEvent;

fn terminals(events: &[AgentEvent]) -> Vec<&AgentEvent> {
    events
        .iter()
        .filter(|e| matches!(e, AgentEvent::Done { .. } | AgentEvent::Error { .. }))
        .collect()
}

#[test]
fn claude_real_text_run_fixture() {
    let fixture = include_str!("fixtures/claude_result.jsonl");
    let events: Vec<AgentEvent> = fixture.lines().flat_map(claude::parse_line).collect();
    let terms = terminals(&events);
    assert_eq!(terms.len(), 1, "exactly one terminal event, got {terms:?}");
    assert!(events.contains(&AgentEvent::Text { text: "ok".into() }));
    assert_eq!(
        terms[0],
        &AgentEvent::Done {
            final_text: "ok".into(),
            usage: Usage {
                input_tokens: 2 + 11543 + 10122,
                output_tokens: 82,
                usd: 0.0969684
            },
        }
    );
}

#[test]
fn claude_documented_tool_call_fixture() {
    let fixture = include_str!("fixtures/claude_tool_call.jsonl");
    let events: Vec<AgentEvent> = fixture.lines().flat_map(claude::parse_line).collect();
    let terms = terminals(&events);
    assert_eq!(terms.len(), 1);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolCall { name, .. } if name == "Bash"))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolResult { ok: true, .. }))
    );
}

#[test]
fn codex_real_auth_error_fixture() {
    let fixture = include_str!("fixtures/codex_auth_error.jsonl");
    let mut last = String::new();
    let events: Vec<AgentEvent> = fixture
        .lines()
        .flat_map(|l| codex::parse_line(l, &mut last))
        .collect();
    let terms = terminals(&events);
    assert_eq!(terms.len(), 1, "exactly one terminal event, got {terms:?}");
    assert!(matches!(terms[0], AgentEvent::Error { .. }));
}

#[test]
fn codex_documented_success_fixture() {
    let fixture = include_str!("fixtures/codex_success.jsonl");
    let mut last = String::new();
    let events: Vec<AgentEvent> = fixture
        .lines()
        .flat_map(|l| codex::parse_line(l, &mut last))
        .collect();
    let terms = terminals(&events);
    assert_eq!(terms.len(), 1);
    assert_eq!(
        terms[0],
        &AgentEvent::Done {
            final_text: "ok".into(),
            usage: Usage {
                input_tokens: 120,
                output_tokens: 8,
                usd: 0.0
            },
        }
    );
}

/// gemini-cli v0.66.0's own stream-json snapshot
/// (`packages/cli/src/__snapshots__/nonInteractiveCli.test.ts.snap`),
/// timestamps filled in — not a recorded run.
#[test]
fn gemini_documented_stream_fixture() {
    let fixture = include_str!("fixtures/gemini_stream.jsonl");
    let mut parser = gemini::GeminiParser::new(false);
    let events: Vec<AgentEvent> = fixture.lines().flat_map(|l| parser.parse_line(l)).collect();
    let terms = terminals(&events);
    assert_eq!(terms.len(), 1);
    assert!(
        matches!(terms[0], AgentEvent::Done { final_text, .. } if final_text == "Final answer"),
        "{terms:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolCall { name, .. } if name == "testTool"))
    );
}
