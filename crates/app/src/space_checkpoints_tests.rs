//! Snapshot checkpoints in a space (docs/spaces.md §3, §6): the worktree's
//! own `HEAD` recorded per turn, rewinding several turns at once, reverting
//! one file, and the live diff during a turn — against `Core` + `testkit`.

use std::time::Duration;

use serde_json::json;
use theodore_contract::{
    AddProjectRequest, ChatMessage, CreateSpaceRequest, Project, RevertSpaceFileRequest,
    RewindSpaceRequest, Space, SpaceRevertFrom, SpaceSendRequest, SpaceStatus, models,
};
use theodore_domain::{Author, Sha, Usage};
use theodore_ports::{AgentEvent, NewAction, Store, SyncActionCode, space_ref};

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

/// One more turn that moves the space to the checkpoint `head`, with the
/// worktree's own `HEAD` at `wt_head`.
async fn turn(f: &Fixtures, core: &Core, id: &str, text: &str, head: &str, wt_head: &str) -> Space {
    f.agents.push_script(done(&format!("did {text}")));
    f.git.push_checkpoint(Sha::new(head));
    f.git
        .space_heads
        .lock()
        .unwrap()
        .insert(id.to_string(), Sha::new(wt_head));
    core.space_send(send(id, text)).await.unwrap();
    idle(f, id).await
}

/// A space with no turns yet.
async fn empty_space(f: &Fixtures, core: &Core, project: &Project) -> Space {
    let s = core
        .create_space(CreateSpaceRequest {
            project: project.id.clone(),
            goal: String::new(),
            name: Some("work".into()),
            branch: None,
            base: None,
            agent: None,
            model: None,
            start_from: None,
        })
        .await
        .unwrap();
    space(f, &s.id)
}

fn invalid(err: AppError) -> String {
    match err {
        AppError::Invalid(m) => m,
        other => panic!("expected Invalid, got {other:?}"),
    }
}

fn reply_of(s: &Space, n: usize) -> String {
    s.turn_log[n].reply.clone()
}

// ── heads ───────────────────────────────────────────────────────────────────

/// A checkpoint is a snapshot on the worktree's `HEAD`: both are recorded,
/// and Undo puts `HEAD` back where it was, not on the snapshot.
#[tokio::test]
async fn each_turn_records_the_worktrees_head_and_undo_restores_it() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let s = empty_space(&f, &core, &project).await;
    let s = turn(&f, &core, &s.id, "one", "snap1", "base000").await;
    // The agent committed in turn 2: its commit is the worktree's HEAD.
    let s = turn(&f, &core, &s.id, "two", "snap2", "agent1").await;

    assert_eq!(s.turn_log[0].before_head, None, "a new space: HEAD = base");
    assert_eq!(s.turn_log[0].after_head, Some(Sha::new("base000")));
    assert_eq!(s.turn_log[1].before_head, Some(Sha::new("base000")));
    assert_eq!(s.turn_log[1].after_head, Some(Sha::new("agent1")));
    assert_eq!(s.worktree_head, Some(Sha::new("agent1")));

    let s = core.undo_space_turn(&s.id).await.unwrap();
    assert_eq!(s.head_sha, Sha::new("snap1"));
    assert_eq!(s.worktree_head, Some(Sha::new("base000")));
    assert_eq!(
        f.git.reset_heads.lock().unwrap().clone(),
        vec![Sha::new("base000")]
    );
}

/// A space from before snapshots: no heads in its log, its checkpoints
/// were `HEAD` itself — Undo resets `HEAD` to the checkpoint.
#[tokio::test]
async fn an_old_turn_log_without_heads_still_undoes() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let s = empty_space(&f, &core, &project).await;
    let s = turn(&f, &core, &s.id, "one", "old1", "old1").await;
    let s = turn(&f, &core, &s.id, "two", "old2", "old2").await;
    let mut log = serde_json::to_value(&s.turn_log).unwrap();
    for t in log.as_array_mut().unwrap() {
        let t = t.as_object_mut().unwrap();
        t.remove("before_head");
        t.remove("after_head");
        t.remove("reverted");
    }
    f.store
        .apply(vec![NewAction {
            model: models::SPACE.into(),
            model_id: s.id.clone(),
            action: SyncActionCode::C,
            data: Some(json!({ "turn_log": log, "worktree_head": null })),
        }])
        .unwrap();

    let s = core.undo_space_turn(&s.id).await.unwrap();
    assert_eq!(s.head_sha, Sha::new("old1"));
    assert_eq!(
        f.git.reset_heads.lock().unwrap().clone(),
        vec![Sha::new("old1")]
    );
}

// ── rewind ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn back_to_here_rewinds_several_turns_at_once() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let s = empty_space(&f, &core, &project).await;
    let mut s = s;
    for n in 1..=4 {
        s = turn(
            &f,
            &core,
            &s.id,
            &format!("t{n}"),
            &format!("snap{n}"),
            "base000",
        )
        .await;
    }

    let s = core
        .rewind_space(RewindSpaceRequest {
            space: s.id.clone(),
            reply: reply_of(&s, 1),
        })
        .await
        .expect("back to turn 2");
    assert_eq!(s.head_sha, Sha::new("snap2"));
    let undone: Vec<bool> = s.turn_log.iter().map(|t| t.undone).collect();
    assert_eq!(undone, [false, false, true, true]);
    assert_eq!(
        f.git.resets.lock().unwrap().clone(),
        vec![(s.id.clone(), Sha::new("snap2"))],
        "one reset, straight to turn 2"
    );

    // The next turn's transcript leaves turns 3 and 4 out.
    let s = turn(&f, &core, &s.id, "t5", "snap5", "base000").await;
    let prompt = f.agents.last_request().unwrap().prompt;
    assert!(prompt.contains("did t2") && !prompt.contains("did t3"));
    assert_eq!(s.turn_log[4].before, Sha::new("snap2"));

    // Its own last turn, or an undone one, can't be rewound to.
    let err = core
        .rewind_space(RewindSpaceRequest {
            space: s.id.clone(),
            reply: reply_of(&s, 4),
        })
        .await
        .unwrap_err();
    assert!(invalid(err).contains("already the space's last turn"));
    let err = core
        .rewind_space(RewindSpaceRequest {
            space: s.id.clone(),
            reply: reply_of(&s, 2),
        })
        .await
        .unwrap_err();
    assert!(invalid(err).contains("undone"));
}

#[tokio::test]
async fn rewind_is_refused_over_a_shipped_turn_running_or_archived() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let s = empty_space(&f, &core, &project).await;
    let mut s = s;
    for n in 1..=3 {
        s = turn(
            &f,
            &core,
            &s.id,
            &format!("t{n}"),
            &format!("snap{n}"),
            "base000",
        )
        .await;
    }
    // Shipped after turn 2: back to turn 2 is fine, back to turn 1 isn't.
    f.store
        .apply(vec![NewAction {
            model: models::SPACE.into(),
            model_id: s.id.clone(),
            action: SyncActionCode::C,
            data: Some(json!({ "shipped": {
                "how": "pr", "branch": "space/work", "sha": "squash1",
                "head": "snap2", "pushes": 1, "at": 0
            } })),
        }])
        .unwrap();
    let err = core
        .rewind_space(RewindSpaceRequest {
            space: s.id.clone(),
            reply: reply_of(&s, 0),
        })
        .await
        .unwrap_err();
    assert!(invalid(err).contains("already pushed"));
    let s = core
        .rewind_space(RewindSpaceRequest {
            space: s.id.clone(),
            reply: reply_of(&s, 1),
        })
        .await
        .expect("turn 3 never shipped");
    assert_eq!(s.head_sha, Sha::new("snap2"));

    // While a turn runs.
    f.agents.hang_next_run();
    core.space_send(send(&s.id, "long")).await.unwrap();
    let err = core
        .rewind_space(RewindSpaceRequest {
            space: s.id.clone(),
            reply: reply_of(&s, 0),
        })
        .await
        .unwrap_err();
    assert!(invalid(err).contains("still working"));
    core.space_stop(&s.id).await.unwrap();
    idle(&f, &s.id).await;

    // A rebase left in the worktree.
    let s = space(&f, &s.id);
    f.git.push_checkpoint_error(theodore_ports::GitError::Busy(
        theodore_ports::WorktreeBusy::Rebase,
    ));
    let err = core.undo_space_turn(&s.id).await.unwrap_err();
    assert!(invalid(err).contains("can't undo: rebase in progress"));

    core.archive_space(&s.id).await.unwrap();
    let err = core
        .rewind_space(RewindSpaceRequest {
            space: s.id.clone(),
            reply: reply_of(&s, 0),
        })
        .await
        .unwrap_err();
    assert!(invalid(err).contains("archived"));
}

// ── revert one file ─────────────────────────────────────────────────────────

#[tokio::test]
async fn reverting_a_file_checkpoints_it_and_tells_the_next_turn() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let s = empty_space(&f, &core, &project).await;
    let s = turn(&f, &core, &s.id, "one", "snap1", "base000").await;
    let s = turn(&f, &core, &s.id, "two", "snap2", "base000").await;

    f.git.push_checkpoint(Sha::new("rev1"));
    let s = core
        .revert_space_file(RevertSpaceFileRequest {
            space: s.id.clone(),
            paths: vec!["src/a.ts".into()],
            from: SpaceRevertFrom::Base,
        })
        .await
        .expect("revert");
    assert_eq!(
        f.git.restored.lock().unwrap().clone(),
        vec![(vec!["src/a.ts".to_string()], Sha::new("base000"))]
    );
    assert_eq!(s.head_sha, Sha::new("rev1"));
    let entry = s.turn_log.last().unwrap();
    assert_eq!(entry.reverted, ["src/a.ts"]);
    assert_eq!(entry.before, Sha::new("snap2"));
    assert_eq!(entry.reply, entry.message, "not an agent turn");
    let said = transcript(&f, &s.id)
        .into_iter()
        .find(|m| m.id == entry.message)
        .unwrap();
    assert_eq!(said.text, "Reverted `src/a.ts` to the base.");
    assert_eq!(said.author, Author::Me);

    // The last turn's change to another file: from turn 2's `before`.
    f.git.push_checkpoint(Sha::new("rev2"));
    core.revert_space_file(RevertSpaceFileRequest {
        space: s.id.clone(),
        paths: vec!["b.ts".into()],
        from: SpaceRevertFrom::LastTurn,
    })
    .await
    .unwrap();
    assert_eq!(
        f.git.restored.lock().unwrap()[1],
        (vec!["b.ts".to_string()], Sha::new("snap1"))
    );

    // The next prompt says so, once.
    let s = turn(&f, &core, &s.id, "three", "snap3", "base000").await;
    let prompt = f.agents.last_request().unwrap().prompt;
    assert!(
        prompt.contains(
            "The user reverted `src/a.ts` since your last turn; don't redo that change unless asked."
        ),
        "{prompt}"
    );
    assert!(prompt.contains("The user reverted `b.ts`"));
    turn(&f, &core, &s.id, "four", "snap4", "base000").await;
    let prompt = f.agents.last_request().unwrap().prompt;
    assert!(!prompt.contains("The user reverted"), "{prompt}");
}

#[tokio::test]
async fn a_revert_is_undone_like_a_turn_and_refuses_a_no_op() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let s = empty_space(&f, &core, &project).await;
    let s = turn(&f, &core, &s.id, "one", "snap1", "base000").await;

    // Nothing changed: the checkpoint stays where it was.
    let err = core
        .revert_space_file(RevertSpaceFileRequest {
            space: s.id.clone(),
            paths: vec!["same.ts".into()],
            from: SpaceRevertFrom::Base,
        })
        .await
        .unwrap_err();
    assert!(invalid(err).contains("already as it was the base"));

    f.git.push_checkpoint(Sha::new("rev1"));
    let s = core
        .revert_space_file(RevertSpaceFileRequest {
            space: s.id.clone(),
            paths: vec!["a.ts".into()],
            from: SpaceRevertFrom::Base,
        })
        .await
        .unwrap();
    let s = core.undo_space_turn(&s.id).await.expect("undo the revert");
    assert_eq!(s.head_sha, Sha::new("snap1"));
    assert!(s.turn_log[1].undone && !s.turn_log[0].undone);
    // An undone revert isn't mentioned to the agent.
    turn(&f, &core, &s.id, "two", "snap2", "base000").await;
    let prompt = f.agents.last_request().unwrap().prompt;
    assert!(!prompt.contains("The user reverted"));
}

// ── live diff ───────────────────────────────────────────────────────────────

/// While a turn runs, a changed worktree shows as a snapshot in the
/// space's review — no checkpoint, no ref — and the turn's end replaces
/// it with the real checkpoint.
#[tokio::test]
async fn the_diff_follows_the_agent_during_a_turn_without_checkpoints() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let s = empty_space(&f, &core, &project).await;
    let review = s.review_pr.clone().expect("a review from the start");
    let head_of = |core: &Core| {
        let core = core.clone();
        let review = review.clone();
        async move { core.0.current_head.read().await.get(&review).cloned() }
    };

    f.agents.hang_next_run();
    core.space_send(send(&s.id, "long")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(
        head_of(&core).await,
        Some(Sha::new("base000")),
        "nothing changed yet"
    );

    // The agent edits: the fingerprint moves, a snapshot shows.
    f.git.ship.lock().unwrap().snapshot = Some(Sha::new("live1"));
    *f.git.fingerprint.lock().unwrap() = "edited".into();
    for _ in 0..200 {
        if head_of(&core).await == Some(Sha::new("live1")) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(head_of(&core).await, Some(Sha::new("live1")));
    assert!(
        f.git.checkpoints.lock().unwrap().is_empty(),
        "no checkpoint"
    );
    assert_eq!(
        f.git.resolve_ref_of(&space_ref(&s.id)),
        Some(Sha::new("base000")),
        "the space ref stays"
    );

    // The turn ends: the real checkpoint replaces the live snapshot.
    f.git.push_checkpoint(Sha::new("snap1"));
    core.space_stop(&s.id).await.unwrap();
    let s = idle(&f, &s.id).await;
    assert_eq!(s.head_sha, Sha::new("snap1"));
    // The review follows right after the space goes idle.
    for _ in 0..400 {
        if head_of(&core).await == Some(Sha::new("snap1")) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(head_of(&core).await, Some(Sha::new("snap1")));
}
