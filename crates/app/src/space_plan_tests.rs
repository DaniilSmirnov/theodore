//! Plan mode and `ask_user` (docs/spaces.md §5) against `Core` + `testkit`
//! fakes: a Plan turn's plan lands on its turn, Approve builds it in the
//! space's build mode, Revise is another Plan turn; a question waits for
//! your answer, the turn's Stop, or its timeout.

use std::time::Duration;

use serde_json::json;
use theodore_contract::{
    AddProjectRequest, CreateSpaceRequest, Project, Space, SpaceAnswerItem, SpaceAnswerRequest,
    SpaceMode, SpaceSendRequest, SpaceStatus, models,
};
use theodore_domain::{Sha, Usage};
use theodore_ports::{AgentEvent, AgentMode, SpaceToolError, Store, UserQuestion};

use crate::testkit::{Fixtures, fixtures};
use crate::{AppError, Core, Receivers};

async fn wait_until(mut cond: impl FnMut() -> bool) {
    for _ in 0..400 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("condition not met in time");
}

async fn started(f: &Fixtures) -> (Core, Receivers, Project) {
    let (core, recv) = Core::start(f.deps());
    tokio::time::sleep(Duration::from_millis(30)).await;
    let project = core
        .add_project(AddProjectRequest {
            repo: None,
            path: Some("/tmp/my-folder".into()),
        })
        .await
        .expect("add_project");
    wait_until(|| {
        f.store
            .get(models::PROJECT, &project.id)
            .unwrap()
            .is_some_and(|r| r.data["status"] == json!("ready"))
    })
    .await;
    f.git.set_resolve("main", Sha::new("base000"));
    (core, recv, project)
}

fn space(f: &Fixtures, id: &str) -> Space {
    serde_json::from_value(f.store.get(models::SPACE, id).unwrap().unwrap().data).unwrap()
}

async fn idle(f: &Fixtures, id: &str) -> Space {
    wait_until(|| space(f, id).status != SpaceStatus::Running).await;
    space(f, id)
}

/// A space with no turn yet (empty goal).
async fn new_space(core: &Core, project: &Project) -> Space {
    core.create_space(CreateSpaceRequest {
        project: project.id.clone(),
        goal: String::new(),
        name: Some("fix-login".into()),
        branch: None,
        base: None,
        agent: None,
        model: None,
        start_from: None,
    })
    .await
    .unwrap()
}

fn send(space: &Space, text: &str, mode: Option<SpaceMode>) -> SpaceSendRequest {
    SpaceSendRequest {
        space: space.id.clone(),
        text: text.into(),
        agent: None,
        model: None,
        mode,
        attachments: None,
    }
}

fn done(text: &str) -> AgentEvent {
    AgentEvent::Done {
        final_text: text.into(),
        usage: Usage::default(),
    }
}

const PLAN: &str = "# Plan\n1. Guard the redirect in login.rs\n2. Test it";

#[tokio::test]
async fn a_plan_turn_ends_in_a_plan_and_approve_builds_it_in_the_build_mode() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let s = new_space(&core, &project).await;

    // Run once, so Run is the build mode Plan comes back to.
    f.agents.push_script(vec![done("ran")]);
    core.space_send(send(&s, "warm up", Some(SpaceMode::Run)))
        .await
        .unwrap();
    idle(&f, &s.id).await;

    // The plan the agent handed in wins over its final message.
    f.agents.push_script(vec![
        AgentEvent::Plan { text: PLAN.into() },
        done("I wrote the plan to a file."),
    ]);
    core.space_send(send(&s, "Fix the login redirect", Some(SpaceMode::Plan)))
        .await
        .unwrap();
    let after = idle(&f, &s.id).await;
    let req = f.agents.last_request().unwrap();
    assert_eq!(req.mode, AgentMode::Plan);
    assert!(req.system.as_deref().unwrap().contains("Plan mode"));
    assert_eq!(after.mode, SpaceMode::Plan, "sticky");
    assert_eq!(after.build_mode, Some(SpaceMode::Run));
    let turn = after.turn_log.last().unwrap();
    assert_eq!(turn.plan.as_deref(), Some(PLAN));

    // Approve: the next turn runs in Run with the plan, the transcript
    // shows the short message.
    f.agents.push_script(vec![done("Implemented.")]);
    core.approve_space_plan(&s.id).await.unwrap();
    let after = idle(&f, &s.id).await;
    let req = f.agents.last_request().unwrap();
    assert_eq!(req.mode, AgentMode::Run);
    assert!(req.prompt.contains("approved your plan"), "{}", req.prompt);
    assert!(req.prompt.contains("Guard the redirect"), "{}", req.prompt);
    assert!(!req.system.as_deref().unwrap().contains("Plan mode"));
    assert_eq!(after.mode, SpaceMode::Run);
    assert_eq!(after.turn_log.last().unwrap().plan, None);
    let rows = f
        .store
        .query(
            models::CHAT_MESSAGE,
            "pr",
            &crate::usecases::spaces::space_chat_key(&s.id).0,
        )
        .unwrap();
    assert!(
        rows.iter()
            .any(|r| r.data["text"] == json!("Plan approved, implement it."))
    );

    // Nothing left to approve: the last turn has no plan.
    let err = core.approve_space_plan(&s.id).await.unwrap_err();
    assert!(matches!(err, AppError::Invalid(_)), "{err:?}");
}

#[tokio::test]
async fn revise_is_another_plan_turn_and_its_final_text_is_the_plan() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let s = new_space(&core, &project).await;

    f.agents.push_script(vec![done("1. First idea")]);
    core.space_send(send(&s, "Plan it", Some(SpaceMode::Plan)))
        .await
        .unwrap();
    let after = idle(&f, &s.id).await;
    assert_eq!(
        after.turn_log.last().unwrap().plan.as_deref(),
        Some("1. First idea")
    );

    // Revise: your feedback, the mode already Plan.
    f.agents.push_script(vec![done("1. Better idea")]);
    core.space_send(send(&s, "Use the session cookie instead", None))
        .await
        .unwrap();
    let after = idle(&f, &s.id).await;
    assert_eq!(f.agents.last_request().unwrap().mode, AgentMode::Plan);
    assert_eq!(
        after.turn_log.last().unwrap().plan.as_deref(),
        Some("1. Better idea")
    );
    // No Edit/Run yet: the plan is built in Edit.
    assert_eq!(after.build_mode, None);
    f.agents.push_script(vec![done("done")]);
    core.approve_space_plan(&s.id).await.unwrap();
    idle(&f, &s.id).await;
    let req = f.agents.last_request().unwrap();
    assert_eq!(req.mode, AgentMode::Edit);
    assert!(req.prompt.contains("1. Better idea"));
}

fn question() -> Vec<UserQuestion> {
    vec![UserQuestion {
        question: "Which store?".into(),
        options: vec!["SQLite".into(), "Postgres".into()],
        multi: false,
    }]
}

/// A turn that waits forever, and its run id.
async fn running(f: &Fixtures, core: &Core, project: &Project) -> (Space, String) {
    let s = new_space(core, project).await;
    f.agents.hang_next_run();
    core.space_send(send(&s, "Long job", None)).await.unwrap();
    let run = space(f, &s.id).run.expect("running");
    (s, run)
}

#[tokio::test]
async fn ask_user_waits_for_the_answer_and_returns_it() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let (s, run) = running(&f, &core, &project).await;

    let asking = tokio::spawn({
        let (core, id, run) = (core.clone(), s.id.clone(), run.clone());
        async move { core.space_tool_ask(&id, &run, question()).await }
    });
    wait_until(|| space(&f, &s.id).question.is_some()).await;
    let q = space(&f, &s.id).question.unwrap();
    assert_eq!(q.run, run);
    assert_eq!(q.questions[0].options, ["SQLite", "Postgres"]);
    assert!(core.asking(&run));

    // A second set while one waits is refused.
    let second = core.space_tool_ask(&s.id, &run, question()).await;
    assert!(matches!(second, Err(SpaceToolError::Unavailable(_))));

    // An answer that isn't one of the options is refused, the wait goes on.
    let bad = core
        .answer_space_question(SpaceAnswerRequest {
            space: s.id.clone(),
            question: q.id.clone(),
            answers: vec![SpaceAnswerItem {
                selected: vec!["MySQL".into()],
                other: None,
            }],
        })
        .await;
    assert!(matches!(bad, Err(AppError::Invalid(_))), "{bad:?}");

    core.answer_space_question(SpaceAnswerRequest {
        space: s.id.clone(),
        question: q.id.clone(),
        answers: vec![SpaceAnswerItem {
            selected: vec!["SQLite".into()],
            other: None,
        }],
    })
    .await
    .unwrap();
    let answers = asking.await.unwrap().unwrap();
    assert_eq!(answers[0].question, "Which store?");
    assert_eq!(answers[0].selected, ["SQLite"]);
    assert_eq!(space(&f, &s.id).question, None);
    assert!(!core.asking(&run));

    // Answered once: the card is gone.
    let again = core
        .answer_space_question(SpaceAnswerRequest {
            space: s.id.clone(),
            question: q.id,
            answers: vec![],
        })
        .await;
    assert!(matches!(again, Err(AppError::NotFound(_))));
    core.space_stop(&s.id).await.unwrap();
}

#[tokio::test]
async fn stop_releases_a_waiting_question() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let (s, run) = running(&f, &core, &project).await;
    let asking = tokio::spawn({
        let (core, id, run) = (core.clone(), s.id.clone(), run.clone());
        async move { core.space_tool_ask(&id, &run, question()).await }
    });
    wait_until(|| space(&f, &s.id).question.is_some()).await;
    core.space_stop(&s.id).await.unwrap();
    let err = asking.await.unwrap().unwrap_err();
    assert!(err.to_string().contains("stopped"), "{err}");
    let s = idle(&f, &s.id).await;
    assert_eq!(s.question, None);
}

#[tokio::test]
async fn an_unanswered_question_times_out_and_the_agent_goes_on() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let (s, run) = running(&f, &core, &project).await;
    let err = core
        .ask_within(&s.id, &run, question(), Duration::from_millis(50))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("best judgment"), "{err}");
    assert_eq!(space(&f, &s.id).question, None);
    core.space_stop(&s.id).await.unwrap();
}

#[tokio::test]
async fn only_the_running_turn_may_ask() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let s = new_space(&core, &project).await;
    let err = core
        .space_tool_ask(&s.id, "space:x:old", question())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("turn is over"), "{err}");
}

/// A restart while a question waits: the turn is interrupted as any other
/// and the card goes with it.
#[tokio::test]
async fn a_restart_clears_a_waiting_question() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let (s, run) = running(&f, &core, &project).await;
    let _asking = tokio::spawn({
        let (core, id, run) = (core.clone(), s.id.clone(), run.clone());
        async move { core.space_tool_ask(&id, &run, question()).await }
    });
    wait_until(|| space(&f, &s.id).question.is_some()).await;
    let patch =
        crate::usecases::space_hooks::interrupted_patch(&space(&f, &s.id), 0, false).unwrap();
    assert_eq!(patch["question"], serde_json::Value::Null);
    assert_eq!(patch["status"], json!("failed"));
    core.space_stop(&s.id).await.unwrap();
}
