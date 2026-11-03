//! The space tools (docs/spaces.md "Tools the agent gets") against `Core`
//! and the testkit fakes: the trust gate, the check's result, start, stop
//! and status, the log tail, the terminal tail, `space_info` without env
//! values, and which runs get which tools registered.

use std::time::Duration;

use theodore_contract::{AskRequest, RunSkillRequest, SpaceTrustRequest};
use theodore_domain::AgentId;
use theodore_ports::{SpaceCommandName, SpaceToolError};

use crate::space_commands_tests::{done, start_env, wait_until};
use crate::testkit::{FakeScript, ToolRegistration};

#[tokio::test]
async fn repo_commands_not_allowed_yet_are_refused_and_never_run() {
    let e = start_env(false).await;
    e.repo_file(
        ".theodore/config.yaml",
        "spaces:\n  check: make check\n  run: pnpm dev\n",
    );
    let s = e.create("").await;
    for refused in [
        e.core.space_tool_check(&s.id, "no-run").await.map(|_| ()),
        e.core.space_tool_start(&s.id).await.map(|_| ()),
    ] {
        match refused {
            Err(SpaceToolError::NotAllowed(why)) => {
                let text = SpaceToolError::NotAllowed(why).to_string();
                assert!(text.starts_with("not allowed by the user yet"), "{text}");
            }
            other => panic!("expected NotAllowed, got {other:?}"),
        }
    }
    assert!(e.f.commands.specs().is_empty(), "nothing ran");
    let status = e.core.space_tool_status(&s.id).await.unwrap();
    assert!(status.configured && !status.allowed && !status.running);

    // Allowed: the same call runs the check.
    let hash = e
        .core
        .space_config(&s.id)
        .await
        .unwrap()
        .repo_commands
        .unwrap()
        .hash;
    e.core
        .space_trust(SpaceTrustRequest {
            space: s.id.clone(),
            allow: true,
            hash,
        })
        .await
        .unwrap();
    let check = e.core.space_tool_check(&s.id, "no-run").await.unwrap();
    assert!(check.passed);
    assert_eq!(e.f.commands.scripts(), ["make check"]);
}

#[tokio::test]
async fn an_unset_command_is_unavailable_not_a_shell() {
    let e = start_env(false).await;
    let s = e.create("").await;
    let err = e.core.space_tool_check(&s.id, "r").await.unwrap_err();
    assert_eq!(
        err,
        SpaceToolError::Unavailable(
            "no `spaces.check` command is configured for this project".into()
        )
    );
    assert!(matches!(
        e.core.space_tool_start(&s.id).await,
        Err(SpaceToolError::Unavailable(_))
    ));
    assert!(e.f.commands.specs().is_empty());
}

#[tokio::test]
async fn run_check_returns_the_exit_and_a_bounded_tail() {
    let e = start_env(false).await;
    e.global("spaces: {check: make check}");
    let lines: Vec<String> = (1..=100).map(|i| format!("line {i}")).collect();
    e.f.commands.on(
        "make check",
        FakeScript::Exit {
            lines: lines.clone(),
            code: 2,
        },
    );
    let s = e.create("").await;
    let check = e.core.space_tool_check(&s.id, "r").await.unwrap();
    assert!(!check.passed);
    assert_eq!(check.exit_code, Some(2));
    assert!(!check.timed_out);
    assert_eq!(check.summary, "check exited with 2");
    assert_eq!(
        check.output.len(),
        crate::usecases::space_commands::CHECK_TAIL
    );
    assert_eq!(check.output.last().map(String::as_str), Some("line 100"));

    // It ran in the space, with the space's variables, and is in the logs.
    let spec = &e.f.commands.specs()[0];
    assert_eq!(spec.cwd, std::path::Path::new(&s.worktree));
    assert!(spec.env.iter().any(|(k, _)| k == "THEODORE_SPACE"));
    let logs = e
        .core
        .space_tool_logs(&s.id, SpaceCommandName::Check, 3)
        .await
        .unwrap();
    assert_eq!(logs.available, 102, "the `$` line, 100 lines, the summary");
    assert_eq!(
        logs.lines,
        ["line 99", "line 100", "! ── check exited with 2"]
    );
}

#[tokio::test]
async fn start_stop_and_status_follow_the_run_command() {
    let e = start_env(false).await;
    e.global("spaces: {ports: 2, run: pnpm dev}");
    e.f.commands.on(
        "pnpm dev",
        FakeScript::Hang {
            lines: vec!["ready".into()],
        },
    );
    let s = e.create("").await;
    let before = e.core.space_tool_status(&s.id).await.unwrap();
    assert!(before.configured && before.allowed && !before.running);
    assert_eq!(before.last_status, None);
    assert_eq!(
        before.urls,
        s.ports
            .iter()
            .map(|p| format!("http://localhost:{p}"))
            .collect::<Vec<_>>()
    );

    let started = e.core.space_tool_start(&s.id).await.unwrap();
    assert!(started.running);
    assert_eq!(started.ports, s.ports);
    let again = e.core.space_tool_start(&s.id).await.unwrap();
    assert!(again.running);
    assert_eq!(
        e.f.commands.specs().len(),
        1,
        "already running: not started twice"
    );
    wait_until(|| {
        e.core
            .with_runtime(&s.id, |rt| rt.logs.iter().any(|l| l.line == "ready"))
    })
    .await;
    let logs = e
        .core
        .space_tool_logs(&s.id, SpaceCommandName::Run, 100)
        .await
        .unwrap();
    assert_eq!(logs.lines, ["$ pnpm dev", "ready"]);

    let stopped = e.core.space_tool_stop(&s.id).await.unwrap();
    assert!(!stopped.running);
    assert_eq!(stopped.last_status.as_deref(), Some("stopped"));
    assert!(
        e.core
            .space_tool_status(&s.id)
            .await
            .unwrap()
            .last_status
            .is_some()
    );
}

#[tokio::test]
async fn read_logs_returns_one_commands_tail() {
    let e = start_env(false).await;
    e.global("spaces: {check: make check, run: pnpm dev}");
    e.f.commands.on(
        "make check",
        FakeScript::Exit {
            lines: vec!["a".into(), "b".into(), "c".into()],
            code: 0,
        },
    );
    let s = e.create("").await;
    e.core.space_tool_check(&s.id, "r").await.unwrap();
    let all = e
        .core
        .space_tool_logs(&s.id, SpaceCommandName::Check, 400)
        .await
        .unwrap();
    assert_eq!(
        all.lines,
        ["$ make check", "a", "b", "c", "── check finished"]
    );
    assert_eq!(all.available, 5);
    let one = e
        .core
        .space_tool_logs(&s.id, SpaceCommandName::Check, 1)
        .await
        .unwrap();
    assert_eq!(one.lines, ["── check finished"]);
    let run = e
        .core
        .space_tool_logs(&s.id, SpaceCommandName::Run, 100)
        .await
        .unwrap();
    assert!(run.lines.is_empty() && run.available == 0);
}

#[tokio::test]
async fn read_terminal_defaults_to_the_most_recently_active_one() {
    let e = start_env(false).await;
    let s = e.create("").await;

    // No terminal open yet: a note, not an error.
    let none = e
        .core
        .space_tool_read_terminal(&s.id, None, 200)
        .await
        .unwrap();
    assert_eq!(none.id, None);
    assert_eq!(
        none.note.as_deref(),
        Some("no terminal is open in this space")
    );
    assert!(none.lines.is_empty());

    let t1 = e.core.open_space_terminal(&s.id, 80, 24).await.unwrap();
    e.f.clock.advance(1_000);
    let t2 = e.core.open_space_terminal(&s.id, 80, 24).await.unwrap();
    // t2 opened later, so it's the default until t1 is written to
    // (`FakeTerminals` echoes input back as output).
    let latest = e
        .core
        .space_tool_read_terminal(&s.id, None, 10)
        .await
        .unwrap();
    assert_eq!(latest.id, Some(t2.id.clone()));

    e.f.clock.advance(1_000);
    e.core
        .write_space_terminal(&t1.id, "hello\n")
        .await
        .unwrap();
    let mut tail = e
        .core
        .space_tool_read_terminal(&s.id, Some(t1.id.as_str()), 10)
        .await
        .unwrap();
    for _ in 0..200 {
        if tail.lines.iter().any(|l| l.contains("hello")) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
        tail = e
            .core
            .space_tool_read_terminal(&s.id, Some(t1.id.as_str()), 10)
            .await
            .unwrap();
    }
    assert_eq!(tail.id, Some(t1.id.clone()));
    assert_eq!(tail.title, Some(t1.title.clone()));
    assert_eq!(tail.running, Some(true));
    assert!(tail.lines.iter().any(|l| l.contains("hello")), "{tail:?}");

    // Writing to t1 made it the space's most recently active terminal.
    let latest = e
        .core
        .space_tool_read_terminal(&s.id, None, 10)
        .await
        .unwrap();
    assert_eq!(latest.id, Some(t1.id.clone()));

    // Naming a terminal overrides the default.
    let named = e
        .core
        .space_tool_read_terminal(&s.id, Some(t2.id.as_str()), 10)
        .await
        .unwrap();
    assert_eq!(named.id, Some(t2.id));
    assert!(named.lines.is_empty());

    // An unknown id is refused, not silently redirected to the default.
    let err = e
        .core
        .space_tool_read_terminal(&s.id, Some("nope"), 10)
        .await
        .unwrap_err();
    assert!(matches!(err, SpaceToolError::Unavailable(_)), "{err:?}");
}

#[tokio::test]
async fn space_info_names_env_vars_but_never_their_values() {
    let e = start_env(false).await;
    e.global("spaces:\n  ports: 1\n  check: make check\n  env: {API_TOKEN: hunter2-secret}\n");
    let s = e.create("").await;
    let info = e.core.space_tool_info(&s.id).await.unwrap();
    assert_eq!(info.name, s.name);
    assert_eq!(info.branch, s.branch);
    assert_eq!(info.base, "main");
    assert_eq!(info.worktree, s.worktree);
    assert_eq!(info.ports, s.ports);
    assert!(info.env.iter().any(|k| k == "API_TOKEN"), "{:?}", info.env);
    assert!(info.env.iter().any(|k| k == "THEODORE_PORT"));
    let json = serde_json::to_string(&info).unwrap();
    assert!(!json.contains("hunter2"), "{json}");
    let configured: Vec<(&str, bool, bool)> = info
        .commands
        .iter()
        .map(|c| (c.kind.as_str(), c.configured, c.allowed))
        .collect();
    assert_eq!(
        configured,
        [
            ("setup", false, false),
            ("run", false, false),
            ("check", true, true),
            ("archive", false, false),
        ]
    );
}

#[tokio::test]
async fn a_space_turn_gets_space_tools_and_is_told_about_them() {
    let e = start_env(true).await;
    e.f.agents.push_script(done("ok"));
    let s = e.create("Fix it").await;
    let s = e.until(&s.id, |s| s.turns == 1 && s.run.is_none()).await;
    let regs = e.f.tools.registered();
    assert_eq!(regs.len(), 1, "{regs:?}");
    let ToolRegistration {
        run_id,
        name,
        pr,
        space,
        code,
    } = regs[0].clone();
    assert!(run_id.starts_with(&format!("space:{}:", s.id)), "{run_id}");
    assert_eq!(name, "space-tools");
    assert_eq!(pr, None);
    assert_eq!(space.as_deref(), Some(s.id.as_str()));
    assert_eq!(
        code,
        crate::deps::CodeTools::Off,
        "no code intelligence wired"
    );

    let req = e.f.agents.last_request().unwrap();
    assert_eq!(
        req.mcp.as_ref().map(|m| m.name.as_str()),
        Some("space-tools")
    );
    let system = req.system.unwrap();
    for tool in [
        "run_check",
        "start_app",
        "read_logs",
        "space_info",
        "ship",
        "get_space_diff",
        "get_review_comments",
        "add_review_comment",
        "set_base",
        "update_progress",
    ] {
        assert!(system.contains(tool), "{system}");
    }
    // The token goes with the turn.
    assert_eq!(e.f.tools.unregistered(), [req.mcp.unwrap().token]);
}

#[tokio::test]
async fn without_an_mcp_server_a_turn_has_no_tools() {
    let e = start_env(false).await;
    e.f.agents.push_script(done("ok"));
    let s = e.create("Fix it").await;
    e.until(&s.id, |s| s.turns == 1).await;
    let req = e.f.agents.last_request().unwrap();
    assert!(req.mcp.is_none());
    assert!(!req.system.unwrap().contains("space-tools"));
}

#[tokio::test]
async fn review_runs_get_the_space_reads_only_on_a_spaces_pr() {
    let e = start_env(true).await;
    let s = e.create("").await;
    let review = s.review_pr.clone().expect("the space's review");

    e.f.agents.push_script(done("fine"));
    e.core
        .run_skill(RunSkillRequest {
            pr: review.clone(),
            skill: "security".into(),
            agents: vec![AgentId::Claude],
        })
        .await
        .expect("run_skill");
    e.f.agents.push_script(done("an answer"));
    e.core
        .ask(AskRequest {
            pr: review.clone(),
            question: "does it build?".into(),
            about: None,
            group: None,
            agent: None,
            attachments: None,
        })
        .await
        .expect("ask");
    wait_until(|| e.f.tools.registered().len() == 2).await;
    for reg in e.f.tools.registered() {
        assert_eq!(reg.name, "review-tools");
        assert_eq!(reg.pr.as_ref(), Some(&review));
        assert_eq!(reg.space.as_deref(), Some(s.id.as_str()), "{reg:?}");
    }

    // Any other PR: plain review-tools.
    let other = theodore_domain::PrId("github:acme/widgets#7".into());
    let endpoint = e
        .core
        .pr_tools("r-other", &other, AgentId::Claude)
        .await
        .0
        .unwrap();
    assert_eq!(endpoint.name, "review-tools");
    let last = e.f.tools.registered().pop().unwrap();
    assert_eq!(last.space, None);
}
