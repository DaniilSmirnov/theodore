//! The second slice of spaces (docs/spaces.md): undo last turn, unarchive,
//! "Send to agent" from a review, names from non-Latin goals and the agent
//! session a space's turns continue — against
//! `Core` + `testkit` fakes.

use std::time::Duration;

use serde_json::json;
use theodore_contract::{
    AddProjectRequest, ChatMessage, CreateSpaceRequest, Project, SentFeedback, ShipAction,
    ShipSpaceRequest, Space, SpaceFeedback, SpaceFeedbackRequest, SpaceMode, SpaceSendRequest,
    SpaceStatus, TurnStep, models,
};
use theodore_domain::{AgentId, Author, PrId, Sha, Usage};
use theodore_ports::{AgentEvent, AgentRequest, NewAction, Store, SyncActionCode};

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

fn transcript(f: &Fixtures, id: &str) -> Vec<ChatMessage> {
    let mut out: Vec<ChatMessage> = f
        .store
        .query(models::CHAT_MESSAGE, "pr", &space_chat_key(id).0)
        .unwrap()
        .into_iter()
        .map(|r| serde_json::from_value(r.data).unwrap())
        .collect();
    out.sort_by_key(|m| (m.at, matches!(m.author, Author::Agent(_))));
    out
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
        start_from: None,
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

/// One more turn that moves the space to `head`.
async fn turn(f: &Fixtures, core: &Core, id: &str, text: &str, reply: &str, head: &str) -> Space {
    f.agents.push_script(done(reply));
    f.git.push_checkpoint(Sha::new(head));
    core.space_send(SpaceSendRequest {
        space: id.into(),
        text: text.into(),
        agent: None,
        model: None,
        mode: None,
        attachments: None,
    })
    .await
    .unwrap();
    idle(f, id).await
}

fn put(f: &Fixtures, model: &str, id: &str, data: serde_json::Value) {
    f.store
        .apply(vec![NewAction {
            model: model.into(),
            model_id: id.into(),
            action: SyncActionCode::I,
            data: Some(data),
        }])
        .unwrap();
}

fn patch_space(f: &Fixtures, id: &str, data: serde_json::Value) {
    f.store
        .apply(vec![NewAction {
            model: models::SPACE.into(),
            model_id: id.into(),
            action: SyncActionCode::C,
            data: Some(data),
        }])
        .unwrap();
}

fn invalid(err: AppError) -> String {
    match err {
        AppError::Invalid(m) => m,
        other => panic!("expected Invalid, got {other:?}"),
    }
}

// ── undo last turn ──────────────────────────────────────────────────────────

#[tokio::test]
async fn each_turn_records_its_checkpoints() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.agents.push_script(done("one"));
    f.git.push_checkpoint(Sha::new("turn1"));
    let s = core.create_space(request(&project, "First")).await.unwrap();
    let s = idle(&f, &s.id).await;
    let s = turn(&f, &core, &s.id, "second", "two", "turn2").await;

    let chat = transcript(&f, &s.id);
    assert_eq!(s.turn_log.len(), 2);
    assert_eq!(s.turn_log[0].before, Sha::new("base000"));
    assert_eq!(s.turn_log[0].after, Sha::new("turn1"));
    assert_eq!(s.turn_log[1].before, Sha::new("turn1"));
    assert_eq!(s.turn_log[1].after, Sha::new("turn2"));
    let id_of = |text: &str| chat.iter().find(|m| m.text == text).unwrap().id.clone();
    assert_eq!(s.turn_log[1].message, id_of("second"));
    assert_eq!(s.turn_log[1].reply, id_of("two"));
    assert!(s.turn_log.iter().all(|t| !t.undone));
}

#[tokio::test]
async fn undo_moves_back_one_checkpoint_at_a_time_and_keeps_the_transcript() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.agents.push_script(done("Added the guard."));
    f.git.push_checkpoint(Sha::new("turn1"));
    let s = core.create_space(request(&project, "Guard")).await.unwrap();
    idle(&f, &s.id).await;
    turn(&f, &core, &s.id, "Also log it", "Logged it.", "turn2").await;

    let s = core.undo_space_turn(&s.id).await.expect("undo turn 2");
    assert_eq!(s.head_sha, Sha::new("turn1"));
    assert_eq!(s.status, SpaceStatus::Idle);
    assert!(s.turn_log[1].undone && !s.turn_log[0].undone);
    assert_eq!(
        f.git.resets.lock().unwrap().clone(),
        vec![(s.id.clone(), Sha::new("turn1"))]
    );
    assert_eq!(transcript(&f, &s.id).len(), 4, "the turn stays in the chat");

    // The next turn doesn't hear about the undone one.
    let s = turn(&f, &core, &s.id, "Log differently", "ok", "turn3").await;
    let prompt = f.agents.last_request().unwrap().prompt;
    assert!(prompt.contains("Added the guard."));
    assert!(!prompt.contains("Also log it") && !prompt.contains("Logged it."));
    assert_eq!(s.turn_log[2].before, Sha::new("turn1"));

    // Undo walks back over undone turns to the last live one.
    let s = core.undo_space_turn(&s.id).await.unwrap();
    assert_eq!(s.head_sha, Sha::new("turn1"));
    let s = core.undo_space_turn(&s.id).await.unwrap();
    assert_eq!(s.head_sha, Sha::new("base000"));
    let err = core.undo_space_turn(&s.id).await.unwrap_err();
    assert!(invalid(err).contains("no turn"));
}

#[tokio::test]
async fn undo_clears_a_failed_turn() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.agents.push_script(vec![AgentEvent::Error {
        message: "boom".into(),
        stderr_tail: String::new(),
    }]);
    f.git.push_checkpoint(Sha::new("half"));
    let s = core.create_space(request(&project, "Try")).await.unwrap();
    let s = idle(&f, &s.id).await;
    assert_eq!(s.status, SpaceStatus::Failed);

    let s = core.undo_space_turn(&s.id).await.unwrap();
    assert_eq!(s.status, SpaceStatus::Idle);
    assert_eq!(s.error, None);
    assert_eq!(s.head_sha, Sha::new("base000"));
}

#[tokio::test]
async fn undo_is_refused_while_running_and_when_archived() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.agents.push_script(done("one"));
    f.git.push_checkpoint(Sha::new("turn1"));
    let s = core.create_space(request(&project, "First")).await.unwrap();
    idle(&f, &s.id).await;

    f.agents.hang_next_run();
    core.space_send(SpaceSendRequest {
        space: s.id.clone(),
        text: "long".into(),
        agent: None,
        model: None,
        mode: None,
        attachments: None,
    })
    .await
    .unwrap();
    let err = core.undo_space_turn(&s.id).await.unwrap_err();
    assert!(invalid(err).contains("still working"));
    core.space_stop(&s.id).await.unwrap();
    idle(&f, &s.id).await;

    core.archive_space(&s.id).await.unwrap();
    let err = core.undo_space_turn(&s.id).await.unwrap_err();
    assert!(invalid(err).contains("archived"));
    assert!(f.git.resets.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_shipped_turn_cannot_be_undone_but_later_ones_can() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.agents.push_script(done("one"));
    f.git.push_checkpoint(Sha::new("turn1"));
    let s = core
        .create_space(request(&project, "Ship me"))
        .await
        .unwrap();
    idle(&f, &s.id).await;
    core.ship_space(ShipSpaceRequest {
        space: s.id.clone(),
        action: ShipAction::KeepBranch,
        title: None,
    })
    .await
    .expect("keep branch");

    let err = core.undo_space_turn(&s.id).await.unwrap_err();
    assert!(invalid(err).contains("already written as a branch"));

    turn(&f, &core, &s.id, "more", "two", "turn2").await;
    let s = core
        .undo_space_turn(&s.id)
        .await
        .expect("an unshipped turn");
    assert_eq!(s.head_sha, Sha::new("turn1"));
    let err = core.undo_space_turn(&s.id).await.unwrap_err();
    assert!(invalid(err).contains("already"));
}

#[tokio::test]
async fn a_pushed_turn_cannot_be_undone() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.agents.push_script(done("one"));
    f.git.push_checkpoint(Sha::new("turn1"));
    let s = core
        .create_space(request(&project, "Push me"))
        .await
        .unwrap();
    idle(&f, &s.id).await;
    // As Open PR leaves it: the squashed commit came from turn 1.
    patch_space(
        &f,
        &s.id,
        json!({ "shipped": {
            "how": "pr", "branch": "space/push-me", "sha": "squash1",
            "head": "turn1", "pushes": 1, "at": 0
        } }),
    );
    let err = core.undo_space_turn(&s.id).await.unwrap_err();
    assert!(invalid(err).contains("already pushed"));
}

#[tokio::test]
async fn a_space_without_a_turn_log_has_nothing_to_undo() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let s = core.create_space(request(&project, "")).await.unwrap();
    let err = core.undo_space_turn(&s.id).await.unwrap_err();
    assert!(invalid(err).contains("no turn"));
}

// ── unarchive ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn unarchive_recreates_the_worktree_and_reopens_the_diff() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.agents.push_script(done("one"));
    f.git.push_checkpoint(Sha::new("turn1"));
    let s = core
        .create_space(request(&project, "Come back"))
        .await
        .unwrap();
    idle(&f, &s.id).await;
    patch_space(&f, &s.id, json!({ "done_at": 5 }));
    core.archive_space(&s.id).await.unwrap();
    assert_eq!(space(&f, &s.id).review_pr, None);

    f.clock.advance(60_000);
    let s = core.unarchive_space(&s.id).await.expect("unarchive");
    assert_eq!(s.status, SpaceStatus::Idle);
    assert_eq!(s.head_sha, Sha::new("turn1"));
    assert!(s.review_pr.is_some(), "the live diff is back");
    assert!(
        s.done_at.unwrap() > 5,
        "auto-archive counts from the unarchive, not the merge"
    );
    assert_eq!(s.error, None);

    // It takes turns again; a second unarchive is a no-op.
    turn(&f, &core, &s.id, "more", "two", "turn2").await;
    let again = core.unarchive_space(&s.id).await.unwrap();
    assert_eq!(again.head_sha, Sha::new("turn2"));
}

#[tokio::test]
async fn unarchive_without_its_checkpoint_says_it_restarted_from_base() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.agents.push_script(done("one"));
    f.git.push_checkpoint(Sha::new("turn1"));
    let s = core.create_space(request(&project, "Lost")).await.unwrap();
    idle(&f, &s.id).await;
    core.archive_space(&s.id).await.unwrap();
    // The ref went missing while archived; the fake now restarts at base.
    f.git.set_resolve(
        &format!("refs/theodore/spaces/{}", s.id),
        Sha::new("base000"),
    );

    let s = core.unarchive_space(&s.id).await.unwrap();
    assert_eq!(s.status, SpaceStatus::Failed);
    assert_eq!(s.head_sha, Sha::new("base000"));
    assert!(s.error.unwrap().contains("restarted from main"));
}

// ── send to agent ───────────────────────────────────────────────────────────

fn anchor(path: &str, line: u32) -> serde_json::Value {
    json!({ "rev": "turn1", "path": path, "side": "new", "line": line,
            "start_line": null, "start_side": null })
}

#[tokio::test]
async fn a_draft_on_the_space_review_becomes_the_next_turn() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.agents.push_script(done("one"));
    f.git.push_checkpoint(Sha::new("turn1"));
    let s = core
        .create_space(request(&project, "Redirect"))
        .await
        .unwrap();
    let s = idle(&f, &s.id).await;
    let review = s.review_pr.clone().unwrap();
    put(
        &f,
        models::DRAFT,
        "d1",
        json!({ "id": "d1", "pr": review, "anchor": anchor("src/redirect.ts", 12),
                "body": "also handle //evil.com", "source": { "kind": "me" },
                "outdated": false }),
    );

    f.agents.push_script(done("Handled it."));
    core.space_send_feedback(SpaceFeedbackRequest {
        space: s.id.clone(),
        items: vec![SpaceFeedback::Draft("d1".into())],
    })
    .await
    .expect("send to agent");
    let s = idle(&f, &s.id).await;
    assert_eq!(s.turns, 2);

    let prompt = f.agents.last_request().unwrap().prompt;
    assert!(
        prompt.contains("Review comment on src/redirect.ts:12:\n> also handle //evil.com\n"),
        "{prompt}"
    );
    assert!(
        transcript(&f, &s.id)
            .iter()
            .any(|m| m.author == Author::Me && m.text.starts_with("Address this review feedback"))
    );
}

#[tokio::test]
async fn a_thread_and_a_finding_on_the_space_pr_go_as_one_turn() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let s = core.create_space(request(&project, "")).await.unwrap();
    let pr = PrId("github:acme/widget#7".into());
    patch_space(&f, &s.id, json!({ "pr": pr }));
    put(
        &f,
        models::THREAD,
        "t1",
        json!({ "pr": pr, "id": "t1", "anchor": anchor("src/limiter.ts", 41),
                "resolved": false, "outdated": false,
                "comments": [{ "id": "c1", "author": { "login": "anna", "name": null, "avatar_url": null },
                               "body": "Per-token, not global?", "created_at": 0, "edited": false }] }),
    );
    put(
        &f,
        models::FINDING,
        "f1",
        json!({ "id": "f1", "pr": pr, "skill": "security", "agents": ["claude"],
                "anchor": anchor("src/db.ts", 3), "severity": "high",
                "title": "SQL injection", "body": "The query concatenates input.",
                "state": "open", "fix": null, "outdated": false }),
    );

    f.agents.push_script(done("Fixed both."));
    core.space_send_feedback(SpaceFeedbackRequest {
        space: s.id.clone(),
        items: vec![
            SpaceFeedback::Thread("t1".into()),
            SpaceFeedback::Finding("f1".into()),
        ],
    })
    .await
    .unwrap();
    idle(&f, &s.id).await;
    let prompt = f.agents.last_request().unwrap().prompt;
    assert!(prompt.contains(
        "1. Review thread on src/limiter.ts:41:\nanna wrote:\n> Per-token, not global?\n"
    ));
    assert!(prompt.contains(
        "2. Finding (high) on src/db.ts:3: SQL injection:\n> The query concatenates input.\n"
    ));
}

#[tokio::test]
async fn sent_feedback_is_marked_with_the_turn_it_rides_and_not_sent_twice() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.agents.push_script(done("one"));
    f.git.push_checkpoint(Sha::new("turn1"));
    let s = core
        .create_space(request(&project, "Redirect"))
        .await
        .unwrap();
    let s = idle(&f, &s.id).await;
    assert_eq!(s.turns, 1);
    let review = s.review_pr.clone().unwrap();
    put(
        &f,
        models::DRAFT,
        "d1",
        json!({ "id": "d1", "pr": review, "anchor": anchor("src/redirect.ts", 12),
                "body": "also handle //evil.com", "source": { "kind": "me" },
                "outdated": false }),
    );

    f.agents.push_script(done("Handled it."));
    core.space_send_feedback(SpaceFeedbackRequest {
        space: s.id.clone(),
        items: vec![SpaceFeedback::Draft("d1".into())],
    })
    .await
    .expect("send to agent");

    // Recorded at once, before the turn it starts has even finished.
    let mid = space(&f, &s.id);
    assert_eq!(
        mid.sent_feedback,
        vec![SentFeedback {
            item: SpaceFeedback::Draft("d1".into()),
            turn: 2,
        }]
    );

    let s = idle(&f, &s.id).await;
    assert_eq!(s.turns, 2);
    // The turn finishing doesn't touch the marking; it's keyed by turn
    // number, not by a flag the turn flips.
    assert_eq!(s.sent_feedback, mid.sent_feedback);

    // Sending it again (e.g. the UI composed a second batch before seeing
    // the first was recorded) adds a second entry at the next turn — the
    // core doesn't refuse a resend, the footer's own filtering does.
    f.agents.push_script(done("Handled it again."));
    core.space_send_feedback(SpaceFeedbackRequest {
        space: s.id.clone(),
        items: vec![SpaceFeedback::Draft("d1".into())],
    })
    .await
    .expect("send to agent again");
    let s = idle(&f, &s.id).await;
    assert_eq!(
        s.sent_feedback,
        vec![
            SentFeedback {
                item: SpaceFeedback::Draft("d1".into()),
                turn: 2,
            },
            SentFeedback {
                item: SpaceFeedback::Draft("d1".into()),
                turn: 3,
            },
        ]
    );
}

#[tokio::test]
async fn feedback_from_another_pr_is_refused() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let s = core.create_space(request(&project, "")).await.unwrap();
    put(
        &f,
        models::DRAFT,
        "d9",
        json!({ "id": "d9", "pr": "github:acme/other#1", "anchor": anchor("a.ts", 1),
                "body": "x", "source": { "kind": "me" }, "outdated": false }),
    );
    let err = core
        .space_send_feedback(SpaceFeedbackRequest {
            space: s.id.clone(),
            items: vec![SpaceFeedback::Draft("d9".into())],
        })
        .await
        .unwrap_err();
    assert!(invalid(err).contains("not on this space"));
    let err = core
        .space_send_feedback(SpaceFeedbackRequest {
            space: s.id.clone(),
            items: vec![SpaceFeedback::Thread("nope".into())],
        })
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
    let err = core
        .space_send_feedback(SpaceFeedbackRequest {
            space: s.id.clone(),
            items: vec![],
        })
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Invalid(_)));
    assert_eq!(space(&f, &s.id).turns, 0);
}

// ── names ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_cyrillic_goal_names_the_space_and_repeats_get_a_number() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let a = core.create_space(request(&project, "")).await.unwrap();
    assert_eq!(a.name, "space");
    let mut req = request(&project, "");
    req.goal = String::new();
    let b = core.create_space(req).await.unwrap();
    assert_eq!(b.name, "space-2");
    assert_eq!(b.branch, "space/space-2");

    f.agents.push_script(done("ok"));
    let c = core
        .create_space(request(&project, "исправить логин"))
        .await
        .unwrap();
    assert_eq!(c.name, "ispravit-login");
    assert_eq!(c.branch, "space/ispravit-login");
    idle(&f, &c.id).await;
    f.agents.push_script(done("ok"));
    let d = core
        .create_space(request(&project, "Исправить логин!"))
        .await
        .unwrap();
    assert_eq!(d.name, "ispravit-login-2");
}

// ── agent sessions ──────────────────────────────────────────────────────────

/// A run that reports session `id`, then answers `reply`.
fn in_session(id: &str, reply: &str) -> Vec<AgentEvent> {
    let mut events = vec![AgentEvent::Session { id: id.into() }];
    events.extend(done(reply));
    events
}

async fn send(f: &Fixtures, core: &Core, id: &str, text: &str, agent: Option<AgentId>) -> Space {
    core.space_send(SpaceSendRequest {
        space: id.into(),
        text: text.into(),
        agent,
        model: None,
        mode: None,
        attachments: None,
    })
    .await
    .unwrap();
    idle(f, id).await
}

/// A space whose first turn ran in Claude Code session `s1`.
async fn space_in_session(f: &Fixtures, core: &Core, project: &Project) -> Space {
    f.agents.push_script(in_session("s1", "Added the guard."));
    let s = core
        .create_space(request(project, "Add a guard"))
        .await
        .unwrap();
    idle(f, &s.id).await
}

#[tokio::test]
async fn the_next_turn_continues_the_session_with_only_the_new_message() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let s = space_in_session(&f, &core, &project).await;
    let session = s.agent_session.clone().expect("the first turn's session");
    assert_eq!(session.agent, AgentId::Claude);
    assert_eq!(session.id, "s1");
    assert_eq!(session.cwd, s.worktree);
    assert_eq!(f.agents.last_request().unwrap().resume, None);

    f.agents.push_script(in_session("s1", "Logged it."));
    let s = send(&f, &core, &s.id, "Also log it", None).await;
    let req = f.agents.last_request().unwrap();
    assert_eq!(req.resume.as_deref(), Some("s1"));
    assert_eq!(req.prompt, "Also log it");
    assert!(req.system.is_some(), "the system prompt goes on every turn");
    assert_eq!(s.agent_session.unwrap().id, "s1");
    assert!(transcript(&f, &s.id).iter().any(|m| m.text == "Logged it."));
}

/// A turn's answer streams piece by piece, shows once, and the stored
/// reply is the whole message.
#[tokio::test]
async fn a_turn_streams_its_answer_once_and_stores_it_whole() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let s = space_in_session(&f, &core, &project).await;
    let mut events = core.0.ephemeral_tx.subscribe();

    f.agents.push_script(vec![
        AgentEvent::TextDelta { text: "Log".into() },
        AgentEvent::TextDelta {
            text: "ged.".into(),
        },
        AgentEvent::Text {
            text: "Logged.".into(),
        },
        AgentEvent::Done {
            final_text: "Logged.".into(),
            usage: Usage::default(),
        },
    ]);
    let s = send(&f, &core, &s.id, "Log it", None).await;
    assert!(f.agents.last_request().unwrap().stream_text);

    let mut shown = String::new();
    while let Ok(e) = events.try_recv() {
        if let theodore_contract::Ephemeral::AgentDelta { text, .. } = e {
            shown.push_str(&text);
        }
    }
    assert_eq!(shown, "Logged.");
    let replies: Vec<_> = transcript(&f, &s.id)
        .into_iter()
        .filter(|m| m.text.starts_with("Logged"))
        .map(|m| m.text)
        .collect();
    assert_eq!(replies, vec!["Logged.".to_string()]);
}

#[tokio::test]
async fn another_agent_starts_over_from_the_transcript() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let s = space_in_session(&f, &core, &project).await;

    // Gemini reports no session: none is left to continue afterwards.
    f.agents.push_script(done("Reviewed."));
    let s = send(&f, &core, &s.id, "Have a look", Some(AgentId::Gemini)).await;
    let req = f.agents.last_request().unwrap();
    assert_eq!(req.resume, None);
    assert!(
        req.prompt.contains("Conversation so far:"),
        "{}",
        req.prompt
    );
    assert!(req.prompt.contains("Added the guard."));
    assert_eq!(s.agent_session, None);

    // Back on Claude: a new session, since s1 never heard Gemini's turn.
    f.agents.push_script(in_session("s2", "ok"));
    let s = send(&f, &core, &s.id, "Go on", Some(AgentId::Claude)).await;
    let req = f.agents.last_request().unwrap();
    assert_eq!(req.resume, None);
    assert!(req.prompt.contains("Reviewed."));
    assert_eq!(s.agent_session.unwrap().id, "s2");
}

#[tokio::test]
async fn undo_drops_the_session_that_heard_the_undone_turn() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.git.push_checkpoint(Sha::new("turn1"));
    let s = space_in_session(&f, &core, &project).await;
    f.agents.push_script(in_session("s1", "Logged it."));
    f.git.push_checkpoint(Sha::new("turn2"));
    send(&f, &core, &s.id, "Also log it", None).await;

    let s = core.undo_space_turn(&s.id).await.unwrap();
    assert_eq!(s.agent_session, None);

    f.agents.push_script(in_session("s2", "ok"));
    let s = send(&f, &core, &s.id, "Log differently", None).await;
    let req = f.agents.last_request().unwrap();
    assert_eq!(req.resume, None);
    assert!(req.prompt.contains("Added the guard."));
    assert!(!req.prompt.contains("Also log it"), "{}", req.prompt);
    assert_eq!(s.agent_session.unwrap().id, "s2");
}

#[tokio::test]
async fn a_gone_session_is_retried_once_from_the_transcript() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let s = space_in_session(&f, &core, &project).await;
    let before = f.agents.requests().len();

    // As `claude -p … --resume s1` fails once the session is deleted.
    f.agents.push_script(vec![AgentEvent::Error {
        message: "No conversation found with session ID: s1".into(),
        stderr_tail: String::new(),
    }]);
    f.agents.push_script(in_session("s2", "Logged it."));
    let s = send(&f, &core, &s.id, "Also log it", None).await;

    let reqs = f.agents.requests();
    assert_eq!(reqs.len(), before + 2, "one resume, one retry");
    let (resumed, retried) = (&reqs[before], &reqs[before + 1]);
    assert_eq!(resumed.resume.as_deref(), Some("s1"));
    assert_eq!(resumed.prompt, "Also log it");
    assert_eq!(retried.resume, None);
    assert!(retried.prompt.contains("Conversation so far:"));
    assert!(
        retried
            .prompt
            .ends_with("New message from the user:\nAlso log it")
    );

    assert_eq!(s.status, SpaceStatus::Idle);
    assert_eq!(s.error, None);
    assert_eq!(s.agent_session.unwrap().id, "s2");
    assert!(transcript(&f, &s.id).iter().any(|m| m.text == "Logged it."));
}

#[tokio::test]
async fn codex_reports_a_gone_thread_on_stderr() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.agents.push_script(in_session("t1", "one"));
    let mut req = request(&project, "Codex it");
    req.agent = Some(AgentId::Codex);
    let s = core.create_space(req).await.unwrap();
    let s = idle(&f, &s.id).await;
    assert_eq!(s.agent_session.as_ref().unwrap().agent, AgentId::Codex);

    f.agents.push_script(vec![AgentEvent::Error {
        message: "codex exited with a non-zero status".into(),
        stderr_tail:
            "Error: thread/resume: thread/resume failed: no rollout found for thread id t1".into(),
    }]);
    f.agents.push_script(in_session("t2", "two"));
    let s = send(&f, &core, &s.id, "more", None).await;
    assert_eq!(f.agents.last_request().unwrap().resume, None);
    assert_eq!(s.status, SpaceStatus::Idle);
    assert_eq!(s.agent_session.unwrap().id, "t2");
}

#[tokio::test]
async fn a_resumed_turn_that_fails_otherwise_keeps_the_session() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let s = space_in_session(&f, &core, &project).await;
    let before = f.agents.requests().len();

    f.agents.push_script(vec![AgentEvent::Error {
        message: "rate limited".into(),
        stderr_tail: String::new(),
    }]);
    let s = send(&f, &core, &s.id, "Also log it", None).await;
    assert_eq!(f.agents.requests().len(), before + 1, "no retry");
    assert_eq!(s.status, SpaceStatus::Failed);
    assert_eq!(s.agent_session.as_ref().unwrap().id, "s1");

    f.agents.push_script(in_session("s1", "ok"));
    send(&f, &core, &s.id, "Try again", None).await;
    assert_eq!(
        f.agents.last_request().unwrap().resume.as_deref(),
        Some("s1")
    );
}

#[tokio::test]
async fn a_session_from_another_worktree_is_not_resumed() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let s = space_in_session(&f, &core, &project).await;
    patch_space(
        &f,
        &s.id,
        json!({ "agent_session": { "agent": "claude", "id": "s1", "cwd": "/elsewhere" } }),
    );
    f.agents.push_script(in_session("s2", "ok"));
    let s = send(&f, &core, &s.id, "Go on", None).await;
    assert_eq!(f.agents.last_request().unwrap().resume, None);
    assert_eq!(s.agent_session.unwrap().id, "s2");
}

#[tokio::test]
async fn unarchive_keeps_the_session_unless_the_checkpoints_are_gone() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.git.push_checkpoint(Sha::new("turn1"));
    let s = space_in_session(&f, &core, &project).await;
    core.archive_space(&s.id).await.unwrap();
    let s = core.unarchive_space(&s.id).await.unwrap();
    assert_eq!(
        s.agent_session.as_ref().unwrap().id,
        "s1",
        "same worktree path"
    );

    core.archive_space(&s.id).await.unwrap();
    f.git.set_resolve(
        &format!("refs/theodore/spaces/{}", s.id),
        Sha::new("base000"),
    );
    let s = core.unarchive_space(&s.id).await.unwrap();
    assert_eq!(s.agent_session, None);
}

// ── handoff summaries ────────────────────────────────────────────────────────

fn is_summary_call(req: &AgentRequest) -> bool {
    req.system.as_deref() == Some(crate::usecases::space_handoff::SUMMARY_SYSTEM)
}

fn summary_calls(f: &Fixtures) -> usize {
    f.agents
        .utility_requests()
        .iter()
        .filter(|r| is_summary_call(r))
        .count()
}

/// `space_in_session`, then `extra` more turns resuming the same session —
/// enough of them pushes the history past `HISTORY_MESSAGES` without ever
/// needing a summary yet.
async fn long_space_in_session(f: &Fixtures, core: &Core, project: &Project, extra: u32) -> Space {
    let mut s = space_in_session(f, core, project).await;
    for i in 0..extra {
        f.agents.push_script(in_session("s1", &format!("did {i}")));
        s = send(f, core, &s.id, &format!("do {i}"), None).await;
    }
    s
}

#[tokio::test]
async fn a_long_space_gets_a_cached_summary_when_it_cant_resume() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    // Turn 1 plus 6 more: 14 messages, past the 12-message replay window.
    let s = long_space_in_session(&f, &core, &project, 6).await;
    assert_eq!(
        f.agents.last_request().unwrap().resume.as_deref(),
        Some("s1")
    );

    // Switching to Gemini can't resume: the summarizer runs once.
    f.agents.push_tagged(
        is_summary_call,
        vec![AgentEvent::Done {
            final_text: "Added a guard; six follow-ups logged things.".into(),
            usage: Usage::default(),
        }],
    );
    f.agents.push_script(vec![AgentEvent::Done {
        final_text: "Reviewed.".into(),
        usage: Usage::default(),
    }]);
    let s = send(&f, &core, &s.id, "Have a look", Some(AgentId::Gemini)).await;

    let req = f.agents.last_request().unwrap();
    assert!(
        req.prompt.starts_with(
            "Summary of the conversation before this point:\n\
             Added a guard; six follow-ups logged things.\n\n\
             Conversation so far:"
        ),
        "{}",
        req.prompt
    );
    assert_eq!(summary_calls(&f), 1);
    assert_eq!(
        space(&f, &s.id).handoff_summary.as_deref(),
        Some("Added a guard; six follow-ups logged things.")
    );

    // Another switch reuses the cached summary: no second call.
    f.agents.push_script(in_session("s2", "ok"));
    send(&f, &core, &s.id, "Go on", Some(AgentId::Claude)).await;
    assert_eq!(summary_calls(&f), 1, "cached, not asked again");
}

#[tokio::test]
async fn a_failed_summarizer_falls_back_to_the_plain_replay() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let s = long_space_in_session(&f, &core, &project, 6).await;

    f.agents.push_tagged(
        is_summary_call,
        vec![AgentEvent::Error {
            message: "rate limited".into(),
            stderr_tail: String::new(),
        }],
    );
    f.agents.push_script(done("Reviewed."));
    let s = send(&f, &core, &s.id, "Have a look", Some(AgentId::Gemini)).await;

    let req = f.agents.last_request().unwrap();
    assert!(
        !req.prompt
            .contains("Summary of the conversation before this point"),
        "{}",
        req.prompt
    );
    assert!(req.prompt.contains("Conversation so far:"));
    assert_eq!(summary_calls(&f), 1);
    assert_eq!(space(&f, &s.id).handoff_summary, None);
}

/// A space whose history never outgrows the replay window needs no
/// summarizer call at all, even across an agent switch.
#[tokio::test]
async fn a_short_space_switch_needs_no_summary() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let s = space_in_session(&f, &core, &project).await;

    f.agents.push_script(done("Reviewed."));
    let s = send(&f, &core, &s.id, "Have a look", Some(AgentId::Gemini)).await;
    assert_eq!(summary_calls(&f), 0);
    assert_eq!(space(&f, &s.id).handoff_summary, None);
}

// ── a turn's activity ───────────────────────────────────────────────────────

fn said(text: &str) -> AgentEvent {
    AgentEvent::Text { text: text.into() }
}

fn tool(id: &str, name: &str, input: serde_json::Value) -> AgentEvent {
    AgentEvent::ToolCall {
        id: id.into(),
        name: name.into(),
        input,
    }
}

fn result(id: &str, ok: bool) -> AgentEvent {
    AgentEvent::ToolResult {
        id: id.into(),
        ok,
        output: String::new(),
    }
}

fn finished(text: &str) -> AgentEvent {
    AgentEvent::Done {
        final_text: text.into(),
        usage: Usage::default(),
    }
}

fn text_step(text: &str) -> TurnStep {
    TurnStep::Text {
        text: text.into(),
        parent: None,
    }
}

fn tool_step(name: &str, target: Option<&str>, ok: Option<bool>) -> TurnStep {
    TurnStep::Tool {
        name: name.into(),
        target: target.map(Into::into),
        ok,
        id: None,
        parent: None,
        detail: None,
        summary: None,
        added: None,
        removed: None,
        background: false,
    }
}

/// The steps as these tests compare them: what a call is, not its id or what
/// its input and result add (`turn_steps` tests those).
fn plain(steps: &[TurnStep]) -> Vec<TurnStep> {
    steps
        .iter()
        .map(|s| match s {
            TurnStep::Tool {
                name, target, ok, ..
            } => tool_step(name, target.as_deref(), *ok),
            other => other.clone(),
        })
        .collect()
}

fn only_reply(f: &Fixtures, id: &str) -> ChatMessage {
    transcript(f, id)
        .into_iter()
        .find(|m| m.author != Author::Me)
        .expect("a reply")
}

/// What the agent said on the way stays, in order with its tools; what it
/// ended on is the reply, and only the reply.
#[tokio::test]
async fn a_turn_keeps_its_steps_in_order_without_repeating_the_reply() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.agents.push_script(vec![
        AgentEvent::Thinking {
            text: "Where is it?".into(),
        },
        AgentEvent::TextDelta {
            text: "Reading the ".into(),
        },
        said("Reading the config."),
        tool(
            "t1",
            "Read",
            json!({ "file_path": "/repo/src/config/load.rs" }),
        ),
        result("t1", true),
        said("Now running it."),
        tool("t2", "Bash", json!({ "command": "cargo test\nmore" })),
        result("t2", false),
        tool("t3", "mcp__space-tools__ask_user", json!({})),
        said("All done."),
        finished("All done."),
    ]);
    let s = core
        .create_space(request(&project, "Run it"))
        .await
        .unwrap();
    let s = idle(&f, &s.id).await;

    let reply = only_reply(&f, &s.id);
    assert_eq!(reply.text, "All done.");
    assert_eq!(
        plain(&reply.activity),
        vec![
            TurnStep::Thinking {
                text: "Where is it?".into(),
                parent: None,
            },
            text_step("Reading the config."),
            tool_step("Read", Some("config/load.rs"), Some(true)),
            text_step("Now running it."),
            tool_step("Bash", Some("cargo test"), Some(false)),
            // Never answered, and by what it does rather than its raw id.
            tool_step("ask you", None, None),
        ]
    );
    assert!(reply.duration_ms.is_some());

    // Only the reply holds them: your own message has none.
    let question = transcript(&f, &s.id)
        .into_iter()
        .find(|m| m.author == Author::Me)
        .unwrap();
    assert!(question.activity.is_empty());
    assert_eq!(question.duration_ms, None);
}

/// A result that joins several with blank lines repeats each of them.
#[tokio::test]
async fn a_final_text_joining_results_leaves_none_of_them_in_the_steps() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.agents.push_script(vec![
        said("Looking."),
        tool("t1", "Read", json!({ "file_path": "a.rs" })),
        said(" First result. "),
        tool("t2", "Read", json!({ "file_path": "b.rs" })),
        said("Second result."),
        finished("First result.\n\nSecond result."),
    ]);
    let s = core.create_space(request(&project, "Join")).await.unwrap();
    let s = idle(&f, &s.id).await;

    let reply = only_reply(&f, &s.id);
    assert_eq!(reply.text, "First result.\n\nSecond result.");
    assert!(!reply.text_in_steps);
    assert_eq!(
        plain(&reply.activity),
        vec![
            text_step("Looking."),
            tool_step("Read", Some("a.rs"), None),
            tool_step("Read", Some("b.rs"), None),
        ]
    );
}

/// No final text: the reply holds every block, and the steps keep them where
/// they came, the block cut off last.
#[tokio::test]
async fn a_stopped_turn_keeps_what_it_wrote_in_the_order_it_came() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.agents.push_script_then_hang(vec![
        said("I found the cause."),
        tool("t1", "Edit", json!({ "file_path": "src/a.rs" })),
        AgentEvent::TextDelta {
            text: "Now fixing".into(),
        },
    ]);
    let s = core
        .create_space(request(&project, "Long job"))
        .await
        .unwrap();
    wait_until(|| f.agents.requests().iter().any(|r| r.stream_text)).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    core.space_stop(&s.id).await.unwrap();
    let s = idle(&f, &s.id).await;

    let reply = only_reply(&f, &s.id);
    assert_eq!(reply.failed.as_deref(), Some("stopped"));
    assert_eq!(reply.text, "I found the cause.\n\nNow fixing");
    assert!(reply.text_in_steps);
    assert_eq!(
        plain(&reply.activity),
        vec![
            text_step("I found the cause."),
            tool_step("Edit", Some("src/a.rs"), None),
            text_step("Now fixing"),
        ]
    );
}

/// A CLI that only sends `Text`: the blocks are the reply.
#[tokio::test]
async fn a_run_without_a_final_text_is_its_blocks_joined() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.agents
        .push_script(vec![said("One."), said("Two."), finished("")]);
    let s = core
        .create_space(request(&project, "Blocks"))
        .await
        .unwrap();
    let s = idle(&f, &s.id).await;

    let reply = only_reply(&f, &s.id);
    assert_eq!(reply.text, "One.\n\nTwo.");
    assert!(reply.text_in_steps);
    assert_eq!(reply.activity, vec![text_step("One."), text_step("Two.")]);
}

/// The reply stores what the turn changed, from git between its checkpoints,
/// and the model it asked for.
#[tokio::test]
async fn a_reply_stores_what_its_turn_changed() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let s = space_in_session(&f, &core, &project).await;
    let before = space(&f, &s.id).head_sha;
    f.git.set_numstat(vec![theodore_ports::FileStat {
        path: "src/a.rs".into(),
        additions: 3,
        deletions: 1,
    }]);
    f.git.push_checkpoint(Sha("after-the-turn".into()));
    f.agents.push_script(vec![
        tool("t1", "Edit", json!({ "file_path": "src/a.rs" })),
        finished("Changed it."),
    ]);
    core.space_send(SpaceSendRequest {
        space: s.id.clone(),
        text: "Change it".into(),
        agent: None,
        model: Some("opus".into()),
        mode: None,
        attachments: None,
    })
    .await
    .unwrap();
    let s = idle(&f, &s.id).await;

    let reply = transcript(&f, &s.id)
        .into_iter()
        .find(|m| m.text == "Changed it.")
        .unwrap();
    assert_eq!(
        reply.changed_files,
        vec![theodore_contract::TurnFile {
            path: "src/a.rs".into(),
            additions: 3,
            deletions: 1,
        }]
    );
    assert_eq!(reply.model.as_deref(), Some("opus"));
    assert_eq!(
        f.git.numstat_calls.lock().unwrap().last().unwrap(),
        &(before, Sha("after-the-turn".into()))
    );
}

/// Git failing leaves it empty and the turn alone; a turn that changed
/// nothing doesn't ask.
#[tokio::test]
async fn changed_files_never_fail_a_turn() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let s = space_in_session(&f, &core, &project).await;
    f.git.push_checkpoint(Sha("after-the-turn".into()));
    f.agents.push_script(vec![finished("Changed it.")]);
    let s = send(&f, &core, &s.id, "Change it", None).await;
    let reply = transcript(&f, &s.id)
        .into_iter()
        .find(|m| m.text == "Changed it.")
        .unwrap();
    assert!(reply.failed.is_none());
    assert!(reply.changed_files.is_empty());
    assert!(!f.git.numstat_calls.lock().unwrap().is_empty());

    f.git.numstat_calls.lock().unwrap().clear();
    f.agents.push_script(vec![finished("Nothing to do.")]);
    let s = send(&f, &core, &s.id, "Again", None).await;
    assert!(s.error.is_none());
    assert!(f.git.numstat_calls.lock().unwrap().is_empty());
}

/// Parallel calls and a subagent, the way the runner reports them: each
/// result marks its own call, nothing the subagent did is the turn's own, and
/// its text never reaches the reply.
#[tokio::test]
async fn a_subagent_is_stored_under_its_call_and_results_mark_their_own() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let nested = |event| AgentEvent::Nested {
        parent: "ag".into(),
        event: Box::new(event),
    };
    f.agents.push_script(vec![
        tool("t1", "Bash", json!({ "command": "one" })),
        tool(
            "ag",
            "Agent",
            json!({ "description": "Look around", "prompt": "…" }),
        ),
        nested(tool("r1", "Read", json!({ "file_path": "a.rs" }))),
        nested(result("r1", true)),
        nested(said("It says so.")),
        result("ag", true),
        result("t1", false),
        finished(""),
    ]);
    let s = core.create_space(request(&project, "Look")).await.unwrap();
    let s = idle(&f, &s.id).await;

    let reply = only_reply(&f, &s.id);
    assert_eq!(reply.text, "", "a subagent's text isn't the reply");
    let parents_oks: Vec<_> = reply
        .activity
        .iter()
        .map(|s| match s {
            TurnStep::Tool {
                name, parent, ok, ..
            } => (name.as_str(), parent.as_deref(), *ok),
            TurnStep::Text { parent, .. } => ("text", parent.as_deref(), None),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        parents_oks,
        [
            ("Bash", None, Some(false)),
            ("Agent", None, Some(true)),
            ("Read", Some("ag"), Some(true)),
            ("text", Some("ag"), None),
        ]
    );
}

/// Rows stored before the new step fields, and before changed files, load.
#[test]
fn an_older_step_loads() {
    let row: ChatMessage = serde_json::from_value(json!({
        "id": "m1",
        "pr": "space:s1",
        "author": { "kind": "agent", "agent": "claude" },
        "text": "Added it.",
        "refs": [],
        "at": 1,
        "activity": [
            { "kind": "text", "text": "Hm." },
            { "kind": "thinking", "text": "Where?" },
            { "kind": "tool", "name": "Read", "target": "a.rs", "ok": true },
        ],
        "duration_ms": 12,
    }))
    .unwrap();
    assert_eq!(
        row.activity,
        vec![
            text_step("Hm."),
            TurnStep::Thinking {
                text: "Where?".into(),
                parent: None,
            },
            tool_step("Read", Some("a.rs"), Some(true)),
        ]
    );
    assert!(row.changed_files.is_empty());
    assert_eq!(row.model, None);
    assert!(!row.text_in_steps);
}

/// A final text takes over from deltas no whole block ever confirmed.
#[tokio::test]
async fn a_final_text_replaces_the_deltas_before_it() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.agents.push_script(vec![
        AgentEvent::TextDelta {
            text: "All ".into(),
        },
        AgentEvent::TextDelta {
            text: "done.".into(),
        },
        finished("All done."),
    ]);
    let s = core
        .create_space(request(&project, "Deltas"))
        .await
        .unwrap();
    let s = idle(&f, &s.id).await;
    assert_eq!(only_reply(&f, &s.id).text, "All done.");
}

/// A run that finds its session gone starts over: only the retry's steps
/// count, and replaying the transcript carries replies, never their steps.
#[tokio::test]
async fn a_retried_run_starts_its_steps_over_and_prompts_never_hear_them() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.agents.push_script(vec![
        AgentEvent::Session { id: "s1".into() },
        said("A note on the way."),
        tool("t0", "Read", json!({ "file_path": "first.rs" })),
        said("Added the guard."),
        finished("Added the guard."),
    ]);
    let s = core.create_space(request(&project, "Guard")).await.unwrap();
    let s = idle(&f, &s.id).await;
    assert_eq!(only_reply(&f, &s.id).activity.len(), 2);

    f.agents.push_script(vec![
        tool("t1", "Read", json!({ "file_path": "lost.rs" })),
        AgentEvent::Error {
            message: "No conversation found with session ID: s1".into(),
            stderr_tail: String::new(),
        },
    ]);
    f.agents.push_script(vec![
        tool("t2", "Read", json!({ "file_path": "kept.rs" })),
        said("Logged it."),
        finished("Logged it."),
    ]);
    let s = send(&f, &core, &s.id, "Also log it", None).await;

    let reply = transcript(&f, &s.id)
        .into_iter()
        .find(|m| m.text == "Logged it.")
        .unwrap();
    assert_eq!(
        plain(&reply.activity),
        vec![tool_step("Read", Some("kept.rs"), None)]
    );
    let retried = f.agents.last_request().unwrap();
    assert!(retried.prompt.contains("Added the guard."));
    assert!(!retried.prompt.contains("A note on the way."));
    assert!(!retried.prompt.contains("first.rs"));
}

/// A plan handed in is the reply; what the run wrote isn't repeated by it.
#[tokio::test]
async fn a_handed_in_plan_leaves_the_written_text_in_the_steps() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let s = space_in_session(&f, &core, &project).await;
    f.agents.push_script(vec![
        said("Writing the plan."),
        AgentEvent::Plan {
            text: "1. Do it".into(),
        },
        finished(""),
    ]);
    core.space_send(SpaceSendRequest {
        space: s.id.clone(),
        text: "Plan it".into(),
        agent: None,
        model: None,
        mode: Some(SpaceMode::Plan),
        attachments: None,
    })
    .await
    .unwrap();
    let s = idle(&f, &s.id).await;

    let reply = transcript(&f, &s.id)
        .into_iter()
        .find(|m| m.text == "1. Do it")
        .unwrap();
    assert_eq!(reply.activity, vec![text_step("Writing the plan.")]);
}

/// A plan made of the text it wrote is the reply, not steps that may fold.
#[tokio::test]
async fn a_plan_in_its_text_blocks_stays_the_reply() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let s = space_in_session(&f, &core, &project).await;
    f.agents.push_script(vec![said("1. Do it"), finished("")]);
    core.space_send(SpaceSendRequest {
        space: s.id.clone(),
        text: "Plan it".into(),
        agent: None,
        model: None,
        mode: Some(SpaceMode::Plan),
        attachments: None,
    })
    .await
    .unwrap();
    let s = idle(&f, &s.id).await;

    let reply = transcript(&f, &s.id)
        .into_iter()
        .find(|m| m.text == "1. Do it" && m.author != Author::Me)
        .unwrap();
    assert!(!reply.text_in_steps);
    assert!(reply.activity.is_empty());
}

/// Rows stored before steps existed still load.
#[test]
fn an_older_chat_row_has_no_steps() {
    let row: ChatMessage = serde_json::from_value(json!({
        "id": "m1",
        "pr": "space:s1",
        "author": { "kind": "agent", "agent": "claude" },
        "text": "Added it.",
        "refs": [],
        "at": 1,
    }))
    .unwrap();
    assert!(row.activity.is_empty());
    assert_eq!(row.duration_ms, None);
}

#[tokio::test]
async fn a_steps_file_is_named_from_the_worktree_not_by_its_directory() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let s = space_in_session(&f, &core, &project).await;
    let wt = space(&f, &s.id).worktree;
    f.agents.push_script(vec![
        AgentEvent::ToolCall {
            id: "1".into(),
            name: "Read".into(),
            input: serde_json::json!({ "file_path": format!("{wt}/package.json") }),
        },
        AgentEvent::Done {
            final_text: "Read it.".into(),
            usage: Usage::default(),
        },
    ]);
    let s = send(&f, &core, &s.id, "Read it", None).await;
    let reply = transcript(&f, &s.id)
        .into_iter()
        .rfind(|m| m.text == "Read it." && m.author != theodore_domain::Author::Me)
        .unwrap();
    assert!(
        matches!(
            reply.activity.as_slice(),
            [theodore_contract::TurnStep::Tool { target: Some(t), .. }] if t == "package.json"
        ),
        "{:?}",
        reply.activity
    );
}

/// The ● (docs/spaces.md §3): only a turn's end is news, not every write
/// that bumps `updated_at`.
#[tokio::test]
async fn only_a_turn_ending_stamps_attention() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let s = core.create_space(request(&project, "")).await.unwrap();
    assert_eq!(s.attention_at, None);

    f.clock.advance(1_000);
    turn(&f, &core, &s.id, "go", "done", "head1").await;
    wait_until(|| space(&f, &s.id).attention_at.is_some()).await;
    let ended = space(&f, &s.id);
    assert_eq!(ended.attention_at, Some(ended.updated_at));

    f.clock.advance(1_000);
    core.set_space_pinned(&s.id, true).await.unwrap();
    core.space_pr_opened(&s.id, &crate::testkit::sample_pr_ref("o", "r", 7))
        .await;
    let later = space(&f, &s.id);
    assert_eq!(later.attention_at, ended.attention_at);
    assert!(later.updated_at > ended.updated_at, "the write did bump it");

    f.clock.advance(1_000);
    f.agents.push_script(vec![AgentEvent::Error {
        message: "boom".into(),
        stderr_tail: String::new(),
    }]);
    core.space_send(SpaceSendRequest {
        space: s.id.clone(),
        text: "again".into(),
        agent: None,
        model: None,
        mode: None,
        attachments: None,
    })
    .await
    .unwrap();
    wait_until(|| space(&f, &s.id).status == SpaceStatus::Failed).await;
    wait_until(|| space(&f, &s.id).attention_at > ended.attention_at).await;
}
