//! Process lifecycle tests against `tests/fake_agent.sh`: a stand-in for a
//! real CLI so these never touch the network or spend money.

use std::path::PathBuf;
use std::time::Duration;

use theodore_agents::process::{LineParser, SpawnSpec, reap_orphans, spawn_and_stream};
use theodore_ports::agent::AgentEvent;

fn fake_agent() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fake_agent.sh")
}

/// Minimal parser: `{"line":N}` → `Text`, anything else ignored. Matches
/// `LineParser`'s contract (pure per call) without pulling in a real CLI's
/// schema.
struct EchoParser;

impl LineParser for EchoParser {
    fn on_line(&mut self, line: &str) -> Vec<AgentEvent> {
        if line.trim().is_empty() {
            return vec![];
        }
        vec![AgentEvent::Text {
            text: line.to_string(),
        }]
    }

    fn on_exit(&mut self, success: bool, stderr_tail: &str) -> AgentEvent {
        if success {
            AgentEvent::Done {
                final_text: "done".into(),
                usage: Default::default(),
            }
        } else {
            AgentEvent::Error {
                message: "fake agent exited non-zero".into(),
                stderr_tail: stderr_tail.into(),
            }
        }
    }
}

fn spec(mode: &str, stall_after_ms: u64) -> SpawnSpec {
    spec_with_runtime_cap(mode, stall_after_ms, 0)
}

fn spec_with_runtime_cap(mode: &str, stall_after_ms: u64, max_runtime_ms: u64) -> SpawnSpec {
    SpawnSpec {
        pid_dir: None,
        program: fake_agent(),
        args: vec![mode.to_string()],
        cwd: std::env::temp_dir(),
        env: vec![],
        stall_after_ms,
        max_runtime_ms,
        guards: vec![],
        agent: "fake".into(),
        version: None,
    }
}

async fn collect(mut rx: tokio::sync::mpsc::Receiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        let terminal = matches!(ev, AgentEvent::Done { .. } | AgentEvent::Error { .. });
        events.push(ev);
        if terminal {
            break;
        }
    }
    events
}

#[tokio::test]
async fn normal_run_ends_in_exactly_one_done() {
    let run = spawn_and_stream(spec("normal", 5_000), Box::new(EchoParser)).unwrap();
    let events = collect(run.events).await;
    assert_eq!(
        events,
        vec![
            AgentEvent::Text {
                text: r#"{"line":1}"#.into()
            },
            AgentEvent::Done {
                final_text: "done".into(),
                usage: Default::default(),
            }
        ]
    );
}

#[tokio::test]
async fn crash_exit_becomes_one_error_with_stderr_tail() {
    let run = spawn_and_stream(spec("crash", 5_000), Box::new(EchoParser)).unwrap();
    let events = collect(run.events).await;
    assert_eq!(events.len(), 1);
    match &events[0] {
        AgentEvent::Error {
            message,
            stderr_tail,
        } => {
            assert_eq!(message, "fake agent exited non-zero");
            assert!(
                stderr_tail.contains("something broke"),
                "tail was: {stderr_tail:?}"
            );
        }
        other => panic!("expected Error, got {other:?}"),
    }
}

#[tokio::test]
async fn a_connection_failure_on_stderr_ends_as_offline_not_a_crash() {
    let run = spawn_and_stream(spec("offline", 5_000), Box::new(EchoParser)).unwrap();
    let events = collect(run.events).await;
    match events.as_slice() {
        [
            AgentEvent::Error {
                message,
                stderr_tail,
            },
        ] => {
            assert_eq!(
                message,
                "Couldn't reach Fake's servers — check your connection"
            );
            assert!(stderr_tail.contains("ENOTFOUND"), "{stderr_tail:?}");
        }
        other => panic!("expected one offline Error, got {other:?}"),
    }
}

#[tokio::test]
async fn going_quiet_while_failing_to_connect_ends_the_run_as_offline() {
    // "offline_hang" sleeps 60 s after its connection error: the first
    // stall must end it instead of waiting for the runtime cap.
    let run = spawn_and_stream(spec("offline_hang", 200), Box::new(EchoParser)).unwrap();
    let events = tokio::time::timeout(Duration::from_secs(10), collect(run.events))
        .await
        .expect("an offline stall must end the run well under the agent's own sleep");
    match events.last() {
        Some(AgentEvent::Error { message, .. }) => {
            assert!(message.starts_with("Couldn't reach"), "{message:?}");
        }
        other => panic!("expected an offline Error, got {other:?}"),
    }
}

#[tokio::test]
async fn output_after_a_connection_error_means_it_recovered() {
    // Wide margins: stderr and stdout are read separately, so under load a
    // tight gap can reorder the error and the first output.
    let run = spawn_and_stream(spec("offline_recovered", 500), Box::new(EchoParser)).unwrap();
    let events = collect(run.events).await;
    assert!(events.contains(&AgentEvent::Stalled), "{events:?}");
    assert!(
        matches!(events.last(), Some(AgentEvent::Done { .. })),
        "{events:?}"
    );
}

#[tokio::test]
async fn silent_period_emits_stalled_but_keeps_running() {
    // "stall" mode sleeps 1s between its two lines; a 100ms stall threshold
    // must fire at least one `Stalled` without ending the run.
    let run = spawn_and_stream(spec("stall", 100), Box::new(EchoParser)).unwrap();
    let mut rx = run.events;
    let mut saw_stalled = false;
    let mut saw_second_line = false;
    let mut saw_done = false;
    while let Some(ev) = rx.recv().await {
        match ev {
            AgentEvent::Stalled => saw_stalled = true,
            AgentEvent::Text { ref text } if text.contains("\"line\":2") => saw_second_line = true,
            AgentEvent::Done { .. } => {
                saw_done = true;
                break;
            }
            _ => {}
        }
    }
    assert!(saw_stalled, "expected at least one Stalled event");
    assert!(saw_second_line, "run must keep going after a stall");
    assert!(saw_done, "run must still reach a terminal event");
}

#[tokio::test]
async fn cancel_kills_a_hanging_process_well_before_its_own_sleep() {
    let run = spawn_and_stream(spec("hang", 5_000), Box::new(EchoParser)).unwrap();
    let cancel = run.cancel.clone();
    let mut rx = run.events;

    tokio::time::sleep(Duration::from_millis(50)).await;
    cancel.cancel();

    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let mut last = None;
        while let Some(ev) = rx.recv().await {
            last = Some(ev);
        }
        last
    })
    .await;

    let last = result.expect("cancellation must end the run well under fake_agent.sh's 60s sleep");
    assert!(
        matches!(last, Some(AgentEvent::Error { .. })),
        "expected a terminal Error after cancel, got {last:?}"
    );
}

#[tokio::test]
async fn exactly_one_terminal_event_even_with_extra_output_after() {
    // Regression guard: terminal-event dedup lives in the run loop, not in
    // any one parser, so exercise it here instead of per-CLI.
    let run = spawn_and_stream(spec("normal", 5_000), Box::new(EchoParser)).unwrap();
    let events = collect(run.events).await;
    let terminals = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::Done { .. } | AgentEvent::Error { .. }))
        .count();
    assert_eq!(terminals, 1);
}

#[tokio::test]
async fn stderr_tail_is_bounded_even_for_output_far_larger_than_the_cap() {
    // Regression guard for the `read_to_end`-before-trimming bug: this
    // writes ~20KB to stderr, five times the 4KB tail, so a reintroduced
    // unbounded buffer would still "pass" functionally but defeat the point.
    let run = spawn_and_stream(spec("bigstderr", 5_000), Box::new(EchoParser)).unwrap();
    let events = collect(run.events).await;
    match events.last() {
        Some(AgentEvent::Error { stderr_tail, .. }) => {
            assert!(
                stderr_tail.len() <= 4096,
                "tail was {} bytes",
                stderr_tail.len()
            );
            assert!(
                stderr_tail.ends_with("END"),
                "tail must keep the most recent bytes: {stderr_tail:?}"
            );
        }
        other => panic!("expected a terminal Error, got {other:?}"),
    }
}

#[tokio::test]
async fn oversized_stdout_line_is_truncated_not_unbounded() {
    let run = spawn_and_stream(spec("bigline", 5_000), Box::new(EchoParser)).unwrap();
    let events = collect(run.events).await;
    let text = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::Text { text } => Some(text.clone()),
            _ => None,
        })
        .expect("expected a Text event for the oversized line");
    assert_eq!(
        text.len(),
        1024 * 1024,
        "an oversized line must be capped at MAX_STDOUT_LINE_BYTES, not dropped or left unbounded"
    );
    assert!(matches!(events.last(), Some(AgentEvent::Done { .. })));
}

#[tokio::test]
async fn runtime_cap_terminates_a_hanging_process_well_before_its_own_sleep() {
    // "hang" sleeps 60s; a 100ms cap must end it long before that, the same
    // way `cancel` does, but driven by the cap instead of an explicit cancel.
    let run = spawn_and_stream(
        spec_with_runtime_cap("hang", 5_000, 100),
        Box::new(EchoParser),
    )
    .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(10), collect(run.events)).await;
    let events = result.expect("the runtime cap must end the run well under 10s");
    match events.last() {
        Some(AgentEvent::Error { message, .. }) => {
            assert!(
                message.contains("runtime cap"),
                "expected a runtime-cap message, got {message:?}"
            );
        }
        other => panic!("expected a terminal Error, got {other:?}"),
    }
}

#[tokio::test]
async fn reap_orphans_is_async_and_cleans_up_stale_markers() {
    // `reap_orphans` is `async` precisely so its `ps`/`kill` calls (now
    // `tokio::process`, not `std::process`) never block the executing
    // worker thread; this just exercises it as an async fn end to end.
    let dir = tempfile::tempdir().unwrap();
    // A pid astronomically unlikely to be a real, running process.
    std::fs::write(dir.path().join("999999999"), "claude").unwrap();

    let killed = reap_orphans(dir.path()).await;

    assert_eq!(killed, 0, "no live process should match that pid");
    assert!(
        !dir.path().join("999999999").exists(),
        "the stale marker must be removed regardless"
    );
}

// ── stdin input (steerable runs) ─────────────────────────────────────────

fn steerable(
    work_secs: &str,
) -> (
    theodore_ports::agent::AgentRun,
    theodore_ports::agent::Steer,
) {
    use theodore_agents::claude::{ClaudeParser, user_message_line};
    let (steer, messages) = theodore_ports::agent::Steer::channel();
    let input = theodore_agents::process::Input {
        first: user_message_line("first"),
        messages,
        encode: user_message_line,
    };
    let spec = SpawnSpec {
        program: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fake_claude_stream.sh"),
        args: vec![work_secs.to_string()],
        ..spec("normal", 5_000)
    };
    let run = theodore_agents::process::spawn_with_input(
        spec,
        Box::new(ClaudeParser::with_stdin_input()),
        input,
    )
    .unwrap();
    (run, steer)
}

fn final_text(events: &[AgentEvent]) -> String {
    match events.last() {
        Some(AgentEvent::Done { final_text, .. }) => final_text.clone(),
        other => panic!("expected Done last, got {other:?}"),
    }
}

#[tokio::test]
async fn a_steered_message_joins_the_running_turn() {
    let (run, steer) = steerable("1");
    assert!(steer.send("second".into()).await, "the run took it");
    let events = tokio::time::timeout(Duration::from_secs(10), collect(run.events))
        .await
        .expect("the run ends once both are answered");
    assert_eq!(final_text(&events), "turn 1 took 2");
    assert!(
        !steer.send("too late".into()).await,
        "a finished run refuses"
    );
}

#[tokio::test]
async fn an_unsteered_run_ends_after_its_one_turn() {
    let (run, _steer) = steerable("1");
    let events = tokio::time::timeout(Duration::from_secs(10), collect(run.events))
        .await
        .expect("stdin closes after the result");
    assert_eq!(final_text(&events), "turn 1 took 1");
}
