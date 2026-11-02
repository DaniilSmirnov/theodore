//! Sending while a space's agent works (docs/spaces.md §8): the queue,
//! its drain, held queues after Stop / a failure / a restart, and "Send
//! now" steering — against `Core` + `testkit` fakes.

use std::time::Duration;

use serde_json::json;
use theodore_contract::{
    AddProjectRequest, ChatMessage, CreateSpaceRequest, Project, QueuedAction, Space, SpaceMode,
    SpaceQueuedRequest, SpaceSendRequest, SpaceStatus, models,
};
use theodore_domain::{AgentId, Author, Sha, Usage};
use theodore_ports::{AgentEvent, AgentMode, Store};

use crate::testkit::{Fixtures, fixtures};
use crate::usecases::spaces::space_chat_key;
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

fn message(f: &Fixtures, id: &str) -> Option<ChatMessage> {
    f.store
        .get(models::CHAT_MESSAGE, id)
        .unwrap()
        .map(|r| serde_json::from_value(r.data).unwrap())
}

fn transcript(f: &Fixtures, id: &str) -> Vec<ChatMessage> {
    f.store
        .query(models::CHAT_MESSAGE, "pr", &space_chat_key(id).0)
        .unwrap()
        .into_iter()
        .map(|r| serde_json::from_value(r.data).unwrap())
        .collect()
}

fn held(f: &Fixtures, id: &str) -> bool {
    message(f, id)
        .and_then(|m| m.queued)
        .is_some_and(|q| q.held)
}

fn request(project: &Project, goal: &str) -> CreateSpaceRequest {
    CreateSpaceRequest {
        project: project.id.clone(),
        goal: goal.into(),
        name: None,
        branch: None,
        base: None,
        agent: None,
        model: None,
    }
}

fn send(id: &str, text: &str) -> SpaceSendRequest {
    SpaceSendRequest {
        space: id.into(),
        text: text.into(),
        agent: None,
        model: None,
        mode: None,
        attachments: None,
    }
}

fn queued(id: &str, message: &str, action: QueuedAction) -> SpaceQueuedRequest {
    SpaceQueuedRequest {
        space: id.into(),
        message: message.into(),
        action,
    }
}

fn done(text: &str) -> Vec<AgentEvent> {
    vec![AgentEvent::Done {
        final_text: text.into(),
        usage: Usage::default(),
    }]
}

async fn idle(f: &Fixtures, id: &str) -> Space {
    wait_until(|| space(f, id).status != SpaceStatus::Running).await;
    space(f, id)
}

/// A space whose goal turn is running until the returned sender fires.
async fn running(f: &Fixtures, core: &Core, project: &Project) -> (Space, impl FnOnce()) {
    let go = f.agents.pause_next_run();
    let s = core
        .create_space(request(project, "Long job"))
        .await
        .unwrap();
    assert_eq!(space(f, &s.id).status, SpaceStatus::Running);
    (s, move || go.send(()).unwrap())
}

fn invalid(err: AppError) -> String {
    match err {
        AppError::Invalid(m) => m,
        other => panic!("expected Invalid, got {other:?}"),
    }
}

// ── the queue ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn sends_while_running_queue_and_run_in_order_after_the_turn() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let (s, release) = running(&f, &core, &project).await;

    let a = core.space_send(send(&s.id, "first after")).await.unwrap();
    let b = core.space_send(send(&s.id, "second after")).await.unwrap();
    assert_eq!(f.agents.requests().len(), 1, "both wait");
    let (qa, qb) = (
        message(&f, &a).unwrap().queued.unwrap(),
        message(&f, &b).unwrap().queued.unwrap(),
    );
    assert!(qa.seq < qb.seq && !qa.held && !qb.held);

    release();
    wait_until(|| f.agents.requests().len() == 3 && space(&f, &s.id).turns == 3).await;
    let s = idle(&f, &s.id).await;
    let prompts: Vec<String> = f.agents.requests().into_iter().map(|r| r.prompt).collect();
    assert!(prompts[1].ends_with("first after"), "{}", prompts[1]);
    assert!(prompts[2].ends_with("second after"), "{}", prompts[2]);
    assert!(
        !prompts[1].contains("second after"),
        "a queued message isn't history yet"
    );
    let order: Vec<&str> = s.turn_log.iter().map(|t| t.message.as_str()).collect();
    assert_eq!(&order[1..], [a.as_str(), b.as_str()]);
    assert!(message(&f, &a).unwrap().queued.is_none());
    assert!(message(&f, &b).unwrap().queued.is_none());
}

#[tokio::test]
async fn a_queued_message_keeps_its_agent_mode_and_attachments() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let (s, release) = running(&f, &core, &project).await;
    let att = core
        .save_attachment(theodore_contract::SaveAttachmentRequest {
            name: "shot.png".into(),
            mime: "image/png".into(),
            data: "iVBORw==".into(),
        })
        .await
        .unwrap();
    let id = core
        .space_send(SpaceSendRequest {
            agent: Some(AgentId::Codex),
            mode: Some(SpaceMode::Ask),
            attachments: Some(vec![att.id.clone()]),
            ..send(&s.id, "look at this")
        })
        .await
        .unwrap();
    assert_eq!(message(&f, &id).unwrap().attachments, vec![att.clone()]);

    release();
    wait_until(|| f.agents.requests().len() == 2).await;
    let s = idle(&f, &s.id).await;
    let req = f.agents.last_request().unwrap();
    assert_eq!(req.agent, AgentId::Codex);
    assert_eq!(req.mode, AgentMode::ReadOnly);
    assert_eq!(req.attachments.len(), 1);
    assert!(req.prompt.contains("shot.png"));
    assert_eq!((s.agent, s.mode), (AgentId::Codex, SpaceMode::Ask));
}

#[tokio::test]
async fn a_queued_message_can_be_edited_or_cancelled_before_it_runs() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let (s, release) = running(&f, &core, &project).await;
    let a = core.space_send(send(&s.id, "typo")).await.unwrap();
    let b = core.space_send(send(&s.id, "never mind")).await.unwrap();

    core.space_queued(queued(
        &s.id,
        &a,
        QueuedAction::Edit {
            text: " fixed ".into(),
        },
    ))
    .await
    .unwrap();
    let err = core
        .space_queued(queued(&s.id, &a, QueuedAction::Edit { text: " ".into() }))
        .await
        .unwrap_err();
    assert!(invalid(err).contains("empty"));
    core.space_queued(queued(&s.id, &b, QueuedAction::Cancel))
        .await
        .unwrap();
    assert!(message(&f, &b).is_none(), "gone from the transcript");

    release();
    wait_until(|| f.agents.requests().len() == 2).await;
    idle(&f, &s.id).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(f.agents.requests().len(), 2, "the cancelled one never ran");
    assert!(f.agents.last_request().unwrap().prompt.ends_with("fixed"));
    // Only queued messages: one that ran is history.
    let err = core
        .space_queued(queued(&s.id, &a, QueuedAction::Cancel))
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)), "{err:?}");
}

#[tokio::test]
async fn stop_holds_the_queue_until_you_send_it() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.agents.hang_next_run();
    let s = core.create_space(request(&project, "Long")).await.unwrap();
    let a = core.space_send(send(&s.id, "next")).await.unwrap();
    let b = core.space_send(send(&s.id, "after that")).await.unwrap();

    core.space_stop(&s.id).await.unwrap();
    idle(&f, &s.id).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(f.agents.requests().len(), 1, "nothing ran on its own");
    assert!(held(&f, &a) && held(&f, &b));

    // "Send" on a held one runs it now; the other one stays held.
    f.agents.push_script(done("did next"));
    core.space_queued(queued(&s.id, &a, QueuedAction::Send))
        .await
        .unwrap();
    assert!(message(&f, &a).unwrap().queued.is_none());
    idle(&f, &s.id).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(f.agents.requests().len(), 2);
    assert!(held(&f, &b), "held ones wait even after a good turn");

    // A new message while one is held runs at once.
    core.space_send(send(&s.id, "fresh")).await.unwrap();
    wait_until(|| f.agents.requests().len() == 3).await;
    idle(&f, &s.id).await;
    assert!(held(&f, &b));
}

#[tokio::test]
async fn send_on_a_held_message_while_running_puts_it_back_in_line() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.agents.hang_next_run();
    let s = core.create_space(request(&project, "Long")).await.unwrap();
    let a = core.space_send(send(&s.id, "next")).await.unwrap();
    core.space_stop(&s.id).await.unwrap();
    idle(&f, &s.id).await;
    assert!(held(&f, &a));

    let go = f.agents.pause_next_run();
    core.space_send(send(&s.id, "first")).await.unwrap();
    core.space_queued(queued(&s.id, &a, QueuedAction::Send))
        .await
        .unwrap();
    assert!(!held(&f, &a), "released, still queued");
    go.send(()).unwrap();
    wait_until(|| f.agents.requests().len() == 3).await;
    assert!(f.agents.last_request().unwrap().prompt.ends_with("next"));
}

#[tokio::test]
async fn a_failed_turn_holds_the_queue() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.agents.push_script(vec![AgentEvent::Error {
        message: "boom".into(),
        stderr_tail: String::new(),
    }]);
    let (s, release) = running(&f, &core, &project).await;
    let a = core.space_send(send(&s.id, "next")).await.unwrap();
    release();
    let s = idle(&f, &s.id).await;
    assert_eq!(s.status, SpaceStatus::Failed);
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(f.agents.requests().len(), 1);
    assert!(held(&f, &a));
}

#[tokio::test]
async fn queued_messages_survive_a_restart_held_with_send() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let (s, _release) = running(&f, &core, &project).await;
    let a = core.space_send(send(&s.id, "after restart")).await.unwrap();
    core.shutdown();

    let (core, _recv) = Core::start(f.deps());
    wait_until(|| space(&f, &s.id).status == SpaceStatus::Failed).await;
    wait_until(|| held(&f, &a)).await;
    let runs = f.agents.requests().len();
    assert_eq!(message(&f, &a).unwrap().text, "after restart");

    f.agents.push_script(done("done"));
    core.space_queued(queued(&s.id, &a, QueuedAction::Send))
        .await
        .unwrap();
    wait_until(|| f.agents.requests().len() == runs + 1).await;
    assert!(
        f.agents
            .last_request()
            .unwrap()
            .prompt
            .contains("after restart")
    );
}

#[tokio::test]
async fn undo_and_archive_keep_a_held_queue() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.agents.push_script(done("one"));
    f.git.push_checkpoint(Sha::new("turn1"));
    let s = core.create_space(request(&project, "First")).await.unwrap();
    idle(&f, &s.id).await;
    f.agents.hang_next_run();
    core.space_send(send(&s.id, "long")).await.unwrap();
    let a = core.space_send(send(&s.id, "queued")).await.unwrap();
    core.space_stop(&s.id).await.unwrap();
    idle(&f, &s.id).await;
    assert!(held(&f, &a));

    // Undo walks the checkpoints back; the queue is untouched.
    core.undo_space_turn(&s.id).await.unwrap();
    assert!(held(&f, &a));

    // Archive keeps it too, but an archived space sends nothing.
    core.archive_space(&s.id).await.unwrap();
    assert!(held(&f, &a));
    let err = core
        .space_queued(queued(&s.id, &a, QueuedAction::Send))
        .await
        .unwrap_err();
    assert!(invalid(err).contains("archived"));

    core.unarchive_space(&s.id).await.unwrap();
    let runs = f.agents.requests().len();
    core.space_queued(queued(&s.id, &a, QueuedAction::Send))
        .await
        .unwrap();
    wait_until(|| f.agents.requests().len() == runs + 1).await;
    assert!(f.agents.last_request().unwrap().prompt.contains("queued"));
}

#[tokio::test]
async fn the_running_turns_reply_still_comes_before_a_queued_message() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.agents.push_script(done("goal done"));
    let (s, release) = running(&f, &core, &project).await;
    let a = core.space_send(send(&s.id, "next")).await.unwrap();
    release();
    wait_until(|| f.agents.requests().len() == 2).await;
    idle(&f, &s.id).await;
    let chat = transcript(&f, &s.id);
    let reply = chat
        .iter()
        .find(|m| m.author == Author::Agent(AgentId::Claude) && m.text == "goal done")
        .unwrap();
    let sent = chat.iter().find(|m| m.id == a).unwrap();
    assert!(sent.at >= reply.at, "sent after the turn it waited for");
}

// ── send now (steering) ────────────────────────────────────────────────────

#[tokio::test]
async fn send_now_goes_into_a_running_turn_that_takes_it() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.agents.take_steering();
    f.git.push_checkpoint(Sha::new("turn1"));
    let (s, release) = running(&f, &core, &project).await;
    // The agent is up: its run takes messages from here on.
    wait_until(|| f.agents.requests().len() == 1).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let id = core
        .space_send_now(send(&s.id, "also handle 404s"))
        .await
        .unwrap();
    assert_eq!(f.agents.steered(), ["also handle 404s"]);
    let m = message(&f, &id).unwrap();
    assert!(m.queued.is_none(), "in the turn, not queued");
    assert_eq!(f.agents.requests().len(), 1, "no new run");

    release();
    let s = idle(&f, &s.id).await;
    assert_eq!(s.turn_log[0].steered, std::slice::from_ref(&id));

    // Undone with its turn: the next turn's replay leaves it out.
    core.undo_space_turn(&s.id).await.unwrap();
    f.agents.push_script(done("ok"));
    core.space_send(send(&s.id, "again")).await.unwrap();
    wait_until(|| f.agents.requests().len() == 2).await;
    let prompt = f.agents.last_request().unwrap().prompt;
    assert!(!prompt.contains("also handle 404s"), "{prompt}");
}

#[tokio::test]
async fn send_now_queues_when_the_run_cant_take_it() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    // An agent without steering (Codex, Gemini): queued.
    let (s, release) = running(&f, &core, &project).await;
    let a = core.space_send_now(send(&s.id, "plain")).await.unwrap();
    assert!(message(&f, &a).unwrap().queued.is_some());
    release();
    idle(&f, &s.id).await;

    // A steerable run, but the message switches the mode: it waits too.
    f.agents.take_steering();
    let (s, _release) = running(&f, &core, &project).await;
    let b = core
        .space_send_now(SpaceSendRequest {
            mode: Some(SpaceMode::Run),
            ..send(&s.id, "with Run")
        })
        .await
        .unwrap();
    assert!(message(&f, &b).unwrap().queued.is_some());
    assert!(f.agents.steered().is_empty());

    // Idle: "Send now" is just a send.
    let f2 = fixtures();
    let (core2, _r, project2) = started(&f2).await;
    let s2 = core2.create_space(request(&project2, "")).await.unwrap();
    let c = core2.space_send_now(send(&s2.id, "go")).await.unwrap();
    assert!(message(&f2, &c).unwrap().queued.is_none());
    wait_until(|| f2.agents.requests().len() == 1).await;
}
