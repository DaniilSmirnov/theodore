//! Spaces (docs/spaces.md) against `Core` + `testkit` fakes: a turn runs in
//! the space's worktree in Edit mode and ends in a checkpoint, the live diff
//! follows it ungrouped, Review groups it, and stop / failure /
//! archive / restart leave the space in a sane state.

use std::time::Duration;

use serde_json::json;
use theodore_contract::{
    AddProjectRequest, BootstrapScope, ChatMessage, CreateSpaceRequest, Project, Setting, Space,
    SpaceReviewRequest, SpaceSendRequest, SpaceStatus, models,
};
use theodore_domain::{AgentId, Author, PrSummary, Sha, Usage};
use theodore_ports::{AgentEvent, AgentMode, AgentRequest, NewAction, Store, SyncActionCode};

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

fn put_setting(f: &Fixtures, key: &str, value: serde_json::Value) {
    let setting = Setting {
        key: key.into(),
        scope: "user".into(),
        value,
    };
    f.store
        .apply(vec![NewAction {
            model: models::SETTING.into(),
            model_id: format!("user:{key}"),
            action: SyncActionCode::I,
            data: Some(serde_json::to_value(&setting).unwrap()),
        }])
        .unwrap();
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

fn done(text: &str, usd: f64) -> Vec<AgentEvent> {
    vec![
        AgentEvent::Text {
            text: "working…".into(),
        },
        AgentEvent::Done {
            final_text: text.into(),
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                usd,
            },
        },
    ]
}

async fn idle(f: &Fixtures, id: &str) -> Space {
    wait_until(|| space(f, id).status != SpaceStatus::Running).await;
    space(f, id)
}

#[tokio::test]
async fn the_goal_runs_as_an_edit_turn_and_ends_in_a_checkpoint() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.agents.push_script(done("Added a redirect guard.", 0.25));
    f.git.push_checkpoint(Sha::new("turn111"));

    let created = core
        .create_space(request(&project, "Fix the login redirect"))
        .await
        .expect("create_space");
    let s = idle(&f, &created.id).await;

    assert_eq!(s.name, "fix-the-login-redirect");
    assert_eq!(s.branch, "space/fix-the-login-redirect");
    assert_eq!(s.base, "main");
    assert_eq!(s.base_sha, Sha::new("base000"));
    assert_eq!(s.status, SpaceStatus::Idle);
    assert_eq!(s.turns, 1);
    assert_eq!(s.head_sha, Sha::new("turn111"));
    assert_eq!(s.run, None);

    let req = f.agents.last_request().expect("agent ran");
    assert_eq!(req.mode, AgentMode::Edit);
    assert_eq!(
        req.cwd.display().to_string(),
        format!("/tmp/theodore-fake-space-{}", s.id)
    );
    assert_eq!(req.prompt, "Fix the login redirect");
    // A space's agent gets the user's own MCP servers (on by default).
    assert!(req.user_mcp);
    assert!(req.system.unwrap().contains("never run git push"));

    let chat = transcript(&f, &s.id);
    assert_eq!(chat.len(), 2);
    assert_eq!(chat[0].author, Author::Me);
    assert_eq!(chat[1].author, Author::Agent(AgentId::Claude));
    assert_eq!(chat[1].text, "Added a redirect guard.");

    let checkpoints = f.git.checkpoints.lock().unwrap().clone();
    assert_eq!(
        checkpoints,
        vec![(s.id.clone(), "fix-the-login-redirect: turn 1".to_string())]
    );

    // The live diff follows the checkpoint, named after the space, and is
    // not grouped (that costs an agent run per turn).
    let review = s.review_pr.clone().expect("review pr");
    wait_until(|| {
        f.store
            .get(models::PR, &review.0)
            .unwrap()
            .is_some_and(|r| r.data["head_sha"] == json!("turn111"))
    })
    .await;
    let pr: PrSummary =
        serde_json::from_value(f.store.get(models::PR, &review.0).unwrap().unwrap().data).unwrap();
    assert_eq!(pr.title, "fix-the-login-redirect");
    assert_eq!(pr.head_ref, "space/fix-the-login-redirect");
    assert_eq!(pr.base_ref, "main");
    assert!(pr.url.is_empty());
    assert_eq!(f.grouper.last_agent(), None);
}

fn put_skill(root: &std::path::Path, id: &str, body: &str) {
    std::fs::create_dir_all(root.join(id)).unwrap();
    std::fs::write(root.join(id).join("SKILL.md"), body).unwrap();
}

#[tokio::test]
async fn a_slash_skill_runs_as_the_turns_instructions_and_is_stored_as_typed() {
    let f = fixtures();
    let spaces = tempfile::tempdir().unwrap();
    f.git.set_space_root(spaces.path());
    let (core, _recv, project) = started(&f).await;
    f.agents.push_script(done("First pass.", 0.0));
    let s = core
        .create_space(request(&project, "Add CSV export"))
        .await
        .unwrap();
    idle(&f, &s.id).await;

    let repo_skills = std::path::Path::new(&s.worktree).join(".claude/skills");
    put_skill(
        f.claude_home.path(),
        "polish",
        "---\nname: Polish\ndescription: Tidy up\n---\nMine.",
    );
    put_skill(&repo_skills, "polish", "Repo's polish.");
    put_skill(&repo_skills, "release", "---\nname: Release\n---\nCut it.");

    let mut skills = core.space_skills(&s.id).await.unwrap();
    skills.sort_by(|a, b| (&a.id, &a.source).cmp(&(&b.id, &b.source)));
    let listed: Vec<(&str, &str, &str)> = skills
        .iter()
        .map(|k| (k.id.as_str(), k.title.as_str(), k.source.as_str()))
        .collect();
    assert_eq!(
        listed,
        vec![
            ("polish", "polish", "my-folder · .claude/skills"),
            ("polish", "Polish", "~/.claude/skills"),
            ("release", "Release", "my-folder · .claude/skills"),
        ],
        "no built-ins; both copies of a shared id are listed"
    );

    let send = |text: &str| SpaceSendRequest {
        space: s.id.clone(),
        text: text.into(),
        agent: None,
        model: None,
        mode: None,
        attachments: None,
    };
    f.agents.push_script(done("Polished.", 0.0));
    core.space_send(send("/polish the export  ")).await.unwrap();
    idle(&f, &s.id).await;
    let prompt = f.agents.last_request().unwrap().prompt;
    assert!(
        prompt.ends_with(
            "New message from the user:\nUse the `polish` skill. Its instructions \
             (SKILL.md):\n\n---\nname: Polish\ndescription: Tidy up\n---\nMine.\n\n\
             Task: the export"
        ),
        "{prompt}"
    );
    assert!(!prompt.contains("Repo's polish."), "yours wins");

    f.agents.push_script(done("Released.", 0.0));
    core.space_send(send("/release")).await.unwrap();
    idle(&f, &s.id).await;
    let prompt = f.agents.last_request().unwrap().prompt;
    assert!(
        prompt.ends_with("Cut it.\n\nApply it to the work in this space."),
        "{prompt}"
    );
    assert!(
        prompt.contains("[user]\n/polish the export\n"),
        "earlier turns replay as typed"
    );

    f.agents.push_script(done("Hm.", 0.0));
    core.space_send(send("/bugs please")).await.unwrap();
    idle(&f, &s.id).await;
    let prompt = f.agents.last_request().unwrap().prompt;
    assert!(
        prompt.ends_with("New message from the user:\n/bugs please"),
        "a review built-in isn't a space skill: {prompt}"
    );

    // The fake clock doesn't move, so compare as a set.
    let mut mine: Vec<String> = transcript(&f, &s.id)
        .into_iter()
        .filter(|m| m.author == Author::Me)
        .map(|m| m.text)
        .collect();
    mine.sort();
    assert_eq!(
        mine,
        vec![
            "/bugs please",
            "/polish the export",
            "/release",
            "Add CSV export"
        ]
    );
}

#[tokio::test]
async fn space_skills_without_a_worktree_are_just_yours() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.agents.push_script(done("First pass.", 0.0));
    let s = core
        .create_space(request(&project, "Add CSV export"))
        .await
        .unwrap();
    idle(&f, &s.id).await;
    put_skill(f.claude_home.path(), "polish", "Mine.");
    let skills = core.space_skills(&s.id).await.unwrap();
    let ids: Vec<(&str, &str)> = skills
        .iter()
        .map(|k| (k.id.as_str(), k.source.as_str()))
        .collect();
    assert_eq!(ids, vec![("polish", "~/.claude/skills")]);
}

#[tokio::test]
async fn later_turns_replay_the_transcript_and_can_switch_agent() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.agents.push_script(done("First pass.", 0.0));
    let s = core
        .create_space(request(&project, "Add CSV export"))
        .await
        .unwrap();
    idle(&f, &s.id).await;

    f.agents.push_script(done("Quoted fields.", 0.0));
    core.space_send(SpaceSendRequest {
        space: s.id.clone(),
        text: "Quote fields with commas".into(),
        agent: Some(AgentId::Codex),
        model: None,
        mode: None,
        attachments: None,
    })
    .await
    .unwrap();
    let s = idle(&f, &s.id).await;

    let req = f.agents.last_request().unwrap();
    assert_eq!(req.agent, AgentId::Codex);
    assert!(req.prompt.contains("[user]\nAdd CSV export"));
    assert!(req.prompt.contains("[claude]\nFirst pass."));
    assert!(req.prompt.ends_with("Quote fields with commas"));
    assert_eq!(s.agent, AgentId::Codex);
    assert_eq!(s.turns, 2);
}

#[tokio::test]
async fn attachments_ride_on_the_message_and_reach_the_agent() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.agents.push_script(done("First pass.", 0.0));
    let s = core
        .create_space(request(&project, "Add CSV export"))
        .await
        .unwrap();
    idle(&f, &s.id).await;
    let att = core
        .save_attachment(theodore_contract::SaveAttachmentRequest {
            name: "shot.png".into(),
            mime: "image/png".into(),
            data: "iVBORw==".into(),
        })
        .await
        .unwrap();

    f.agents.push_script(done("Matched the mock.", 0.0));
    let sent = core
        .space_send(SpaceSendRequest {
            space: s.id.clone(),
            text: "Make it look like this".into(),
            agent: None,
            model: None,
            mode: None,
            attachments: Some(vec![att.id.clone()]),
        })
        .await
        .unwrap();
    idle(&f, &s.id).await;

    let mine = transcript(&f, &s.id)
        .into_iter()
        .find(|m| m.id == sent)
        .unwrap();
    assert_eq!(mine.text, "Make it look like this", "stored as typed");
    assert_eq!(mine.attachments, vec![att.clone()]);
    let req = f.agents.last_request().unwrap();
    assert!(
        req.prompt.ends_with(&format!(
            "Make it look like this\n\nAttached files (read them; images and PDFs too):\n\
             - {} (image/png, shot.png)",
            att.path
        )),
        "{}",
        req.prompt
    );
    assert_eq!(
        req.attachments,
        vec![theodore_ports::AgentAttachment {
            path: att.path.clone().into(),
            mime: "image/png".into(),
        }]
    );

    // The next turn names it but doesn't attach it again.
    f.agents.push_script(done("Ok.", 0.0));
    core.space_send(SpaceSendRequest {
        space: s.id.clone(),
        text: "Thanks".into(),
        agent: None,
        model: None,
        mode: None,
        attachments: None,
    })
    .await
    .unwrap();
    idle(&f, &s.id).await;
    let req = f.agents.last_request().unwrap();
    assert!(
        req.prompt.contains("(attached: shot.png)"),
        "{}",
        req.prompt
    );
    assert!(req.attachments.is_empty());
}

#[tokio::test]
async fn an_unknown_attachment_fails_the_send_before_anything_is_stored() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.agents.push_script(done("First pass.", 0.0));
    let s = core
        .create_space(request(&project, "Add CSV export"))
        .await
        .unwrap();
    idle(&f, &s.id).await;
    let before = transcript(&f, &s.id).len();
    let runs = f.agents.requests().len();
    let err = core
        .space_send(SpaceSendRequest {
            space: s.id.clone(),
            text: "See this".into(),
            agent: None,
            model: None,
            mode: None,
            attachments: Some(vec!["0b6e8a52-3c1f-4d7e-9a1b-2f3c4d5e6f70".into()]),
        })
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Invalid(_)), "{err:?}");
    assert_eq!(transcript(&f, &s.id).len(), before);
    assert_eq!(f.agents.requests().len(), runs);
    assert_eq!(space(&f, &s.id).status, SpaceStatus::Idle);
}

#[tokio::test]
async fn a_running_space_queues_a_second_message_and_stops_cleanly() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.agents.hang_next_run();
    let s = core
        .create_space(request(&project, "Long job"))
        .await
        .unwrap();
    assert_eq!(space(&f, &s.id).status, SpaceStatus::Running);

    let queued_id = core
        .space_send(SpaceSendRequest {
            space: s.id.clone(),
            text: "and another".into(),
            agent: None,
            model: None,
            mode: None,
            attachments: None,
        })
        .await
        .unwrap();
    assert_eq!(f.agents.requests().len(), 1, "queued, not run");

    core.space_stop(&s.id).await.unwrap();
    let s = idle(&f, &s.id).await;
    assert_eq!(s.status, SpaceStatus::Idle);
    assert_eq!(s.error, None);
    let chat = transcript(&f, &s.id);
    let reply = chat.iter().find(|m| m.author != Author::Me).unwrap();
    assert_eq!(reply.failed.as_deref(), Some("stopped"));
    // Stop holds the queue: it waits for you, nothing runs on its own.
    let queued = chat.iter().find(|m| m.id == queued_id).unwrap();
    assert!(queued.queued.as_ref().is_some_and(|q| q.held));
    assert_eq!(f.agents.requests().len(), 1);
    // Edits made before the stop are still checkpointed.
    assert!(
        f.git.checkpoints.lock().unwrap()[0]
            .1
            .ends_with("turn 1 (stopped)")
    );
}

#[tokio::test]
async fn a_failed_turn_marks_the_space_failed_and_can_be_retried() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.agents.push_script(vec![AgentEvent::Error {
        message: "boom".into(),
        stderr_tail: String::new(),
    }]);
    let s = core.create_space(request(&project, "Try")).await.unwrap();
    let s = idle(&f, &s.id).await;
    assert_eq!(s.status, SpaceStatus::Failed);
    assert_eq!(s.error.as_deref(), Some("boom"));
    assert_eq!(
        transcript(&f, &s.id).last().unwrap().failed.as_deref(),
        Some("boom")
    );

    f.agents.push_script(done("ok now", 0.0));
    core.space_send(SpaceSendRequest {
        space: s.id.clone(),
        text: "retry".into(),
        agent: None,
        model: None,
        mode: None,
        attachments: None,
    })
    .await
    .unwrap();
    let s = idle(&f, &s.id).await;
    assert_eq!(s.status, SpaceStatus::Idle);
    assert_eq!(s.error, None);
    // The failed placeholder isn't replayed as conversation.
    assert!(!f.agents.last_request().unwrap().prompt.contains("boom"));
}

/// A rebase the agent left mid-way: the answer is kept, no checkpoint is
/// recorded, and the space says why; archive waits for it too.
#[tokio::test]
async fn a_busy_worktree_skips_the_checkpoint_and_says_why() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let root = tempfile::TempDir::new().unwrap();
    f.git.set_space_root(root.path());
    f.agents.push_script(done("Rebased onto main.", 0.0));
    f.git.push_checkpoint_error(theodore_ports::GitError::Busy(
        theodore_ports::WorktreeBusy::Rebase,
    ));
    let s = core
        .create_space(request(&project, "Rebase"))
        .await
        .unwrap();
    let s = idle(&f, &s.id).await;
    assert_eq!(s.status, SpaceStatus::Failed);
    assert_eq!(
        s.error.as_deref(),
        Some("checkpoint skipped: rebase in progress")
    );
    assert_eq!(s.head_sha, Sha::new("base000"));
    assert!(s.turn_log.is_empty());
    let reply = transcript(&f, &s.id).pop().unwrap();
    assert_eq!(reply.text, "Rebased onto main.");
    assert_eq!(reply.failed, None);

    f.git.push_checkpoint_error(theodore_ports::GitError::Busy(
        theodore_ports::WorktreeBusy::Rebase,
    ));
    let err = core.archive_space(&s.id).await.unwrap_err();
    assert!(err.to_string().contains("rebase in progress"), "{err}");
    assert!(err.is_expected());
    assert_ne!(space(&f, &s.id).status, SpaceStatus::Archived);
}

/// Budgets were dropped (owner, 2026-10-22): an old
/// `budget.per_space_usd` row, already exceeded, never blocks a turn.
#[tokio::test]
async fn a_leftover_space_budget_no_longer_stops_turns() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    put_setting(&f, "budget.per_space_usd", json!(0.2));
    f.agents.push_script(done("spent", 0.25));
    let s = core.create_space(request(&project, "Spend")).await.unwrap();
    idle(&f, &s.id).await;

    f.agents.push_script(done("more", 0.25));
    core.space_send(SpaceSendRequest {
        space: s.id.clone(),
        text: "more".into(),
        agent: None,
        model: None,
        mode: None,
        attachments: None,
    })
    .await
    .expect("over an old budget, the turn still starts");
    assert_eq!(idle(&f, &s.id).await.turns, 2);
}

#[tokio::test]
async fn review_groups_the_space_diff() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let s = core.create_space(request(&project, "")).await.unwrap();
    assert_eq!(space(&f, &s.id).turns, 0, "no goal, no turn");

    let opened = core
        .space_review(SpaceReviewRequest {
            space: s.id.clone(),
            group: true,
        })
        .await
        .unwrap();
    assert_eq!(Some(opened.pr), space(&f, &s.id).review_pr);
    wait_until(|| f.grouper.last_agent().is_some()).await;
}

#[tokio::test]
async fn archive_removes_the_worktree_and_review_but_keeps_the_transcript() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.agents.push_script(done("done", 0.0));
    let s = core
        .create_space(request(&project, "Archive me"))
        .await
        .unwrap();
    let s = idle(&f, &s.id).await;
    let review = s.review_pr.clone().unwrap();

    core.archive_space(&s.id).await.unwrap();

    let s = space(&f, &s.id);
    assert_eq!(s.status, SpaceStatus::Archived);
    assert_eq!(s.review_pr, None);
    assert_eq!(f.git.removed_spaces(), vec![s.id.clone()]);
    assert!(f.store.get(models::PR, &review.0).unwrap().is_none());
    assert_eq!(transcript(&f, &s.id).len(), 2);
    assert!(
        core.space_send(SpaceSendRequest {
            space: s.id.clone(),
            text: "x".into(),
            agent: None,
            model: None,
            mode: None,
            attachments: None,
        })
        .await
        .is_err()
    );
    assert_eq!(
        core.list_spaces(Some(project.id.clone()))
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        core.list_spaces(Some("other".into()))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_turn_left_running_by_a_crash_is_failed_on_start() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    let s = core.create_space(request(&project, "")).await.unwrap();
    core.shutdown();
    f.store
        .apply(vec![NewAction {
            model: models::SPACE.into(),
            model_id: s.id.clone(),
            action: SyncActionCode::C,
            data: Some(json!({ "status": "running", "run": "space:x:y" })),
        }])
        .unwrap();

    let (_core, _recv) = Core::start(f.deps());
    wait_until(|| space(&f, &s.id).status == SpaceStatus::Failed).await;
    let s = space(&f, &s.id);
    assert_eq!(s.run, None);
    assert!(s.error.unwrap().starts_with("interrupted"));
}

#[tokio::test]
async fn bootstrap_returns_spaces_and_a_space_transcript_without_opening_anything() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.agents.push_script(done("hi", 0.0));
    let s = core.create_space(request(&project, "Hello")).await.unwrap();
    idle(&f, &s.id).await;

    let global = core.bootstrap(BootstrapScope::Global).await.unwrap();
    assert!(
        global
            .rows
            .iter()
            .any(|r| r.model == models::SPACE && r.id == s.id)
    );
    let scoped = core
        .bootstrap(BootstrapScope::Pr(space_chat_key(&s.id)))
        .await
        .unwrap();
    assert_eq!(scoped.rows.len(), 2);
    assert!(scoped.rows.iter().all(|r| r.model == models::CHAT_MESSAGE));
}

// ── chat-proposed name and title ─────────────────────────────────────────────

/// A tagged response for the identity one-shot call (`space_naming`),
/// distinct from the turn's own (`stream_text: true`) request.
fn identity_answer(name: &str, title: &str) -> Vec<AgentEvent> {
    vec![AgentEvent::Done {
        final_text: format!("NAME: {name}\nTITLE: {title}"),
        usage: Usage::default(),
    }]
}

fn is_identity_call(req: &AgentRequest) -> bool {
    req.system.as_deref() == Some(crate::usecases::space_naming::IDENTITY_SYSTEM)
}

#[tokio::test]
async fn the_chat_agent_renames_the_space_and_sets_its_title() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.agents.push_script(done("ok", 0.0));
    f.agents.push_tagged(
        is_identity_call,
        identity_answer("retry-export-backoff", "Retry export on 429s"),
    );
    let created = core
        .create_space(request(&project, "Retry the export job on 429s"))
        .await
        .unwrap();
    idle(&f, &created.id).await;

    wait_until(|| space(&f, &created.id).title.is_some()).await;
    let s = space(&f, &created.id);
    assert_eq!(s.name, "retry-export-backoff");
    assert_eq!(s.branch, "space/retry-export-backoff");
    assert_eq!(s.title.as_deref(), Some("Retry export on 429s"));

    let utility = f.agents.last_utility_request().expect("one-shot ran");
    assert!(!utility.stream_text);
    assert_eq!(utility.mode, AgentMode::ReadOnly);
}

#[tokio::test]
async fn the_proposed_name_is_deduped_against_its_siblings() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    // Each creation's own (untagged) identity call runs in the background
    // too; waiting for its count after every one keeps it from racing the
    // tag meant for the third space below.
    f.agents.push_script(done("ok", 0.0));
    core.create_space(request(&project, "Retry the export job"))
        .await
        .unwrap();
    wait_until(|| !f.agents.utility_requests().is_empty()).await;

    // The first space already took this name.
    let mut req = request(&project, "Retry the export job");
    req.name = Some("retry-export-backoff".into());
    f.agents.push_script(done("ok", 0.0));
    core.create_space(req).await.unwrap();
    wait_until(|| f.agents.utility_requests().len() >= 2).await;

    f.agents.push_script(done("ok", 0.0));
    f.agents.push_tagged(
        is_identity_call,
        identity_answer("retry-export-backoff", "Retry export"),
    );
    let third = core
        .create_space(request(&project, "Retry the export once more"))
        .await
        .unwrap();
    idle(&f, &third.id).await;

    wait_until(|| space(&f, &third.id).title.is_some()).await;
    assert_eq!(space(&f, &third.id).name, "retry-export-backoff-2");
}

#[tokio::test]
async fn a_user_typed_name_and_branch_are_kept_but_the_title_still_lands() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.agents.push_script(done("ok", 0.0));
    f.agents.push_tagged(
        is_identity_call,
        identity_answer("retry-export-backoff", "Retry export on 429s"),
    );
    let mut req = request(&project, "Retry the export job on 429s");
    req.name = Some("my-own-name".into());
    req.branch = Some("me/my-own-branch".into());
    let created = core.create_space(req).await.unwrap();
    idle(&f, &created.id).await;

    wait_until(|| space(&f, &created.id).title.is_some()).await;
    let s = space(&f, &created.id);
    assert_eq!(s.name, "my-own-name");
    assert_eq!(s.branch, "me/my-own-branch");
    assert_eq!(s.title.as_deref(), Some("Retry export on 429s"));
}

#[tokio::test]
async fn a_bad_answer_keeps_the_slug_name_and_no_title() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    f.agents.push_script(done("ok", 0.0));
    f.agents.push_tagged(
        is_identity_call,
        vec![AgentEvent::Error {
            message: "rate limited".into(),
            stderr_tail: String::new(),
        }],
    );
    let created = core
        .create_space(request(&project, "Retry the export job on 429s"))
        .await
        .unwrap();
    let s = idle(&f, &created.id).await;
    assert_eq!(s.name, "retry-the-export-job-on");

    // No successful answer ever lands: nothing to wait for but time.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let s = space(&f, &created.id);
    assert_eq!(s.name, "retry-the-export-job-on");
    assert_eq!(s.title, None);
}

#[tokio::test]
async fn a_shipped_space_is_not_renamed_even_if_the_answer_lands_late() {
    let f = fixtures();
    let (core, _recv, project) = started(&f).await;
    // Turn 1's own call: no tagged answer, so it isn't mistaken for the
    // identity one below.
    f.agents.push_script(done("ok", 0.0));
    f.git.push_checkpoint(Sha::new("kept1"));
    let created = core
        .create_space(request(&project, "Retry the export job on 429s"))
        .await
        .unwrap();
    let s = idle(&f, &created.id).await;
    // Let the creation's own (untagged) identity call run its course
    // first, so it can't race the tagged one pushed below.
    wait_until(|| !f.agents.utility_requests().is_empty()).await;
    core.ship_space(theodore_contract::ShipSpaceRequest {
        space: s.id.clone(),
        action: theodore_contract::ShipAction::KeepBranch,
        title: None,
    })
    .await
    .unwrap();

    // A valid answer, landing only now — after the space already shipped.
    f.agents.push_tagged(
        is_identity_call,
        identity_answer("retry-export-backoff", "Retry export on 429s"),
    );
    crate::usecases::space_naming::apply(
        &core,
        s.id.clone(),
        AgentId::Claude,
        "Retry the export job on 429s".into(),
        std::path::PathBuf::from(&s.worktree),
        false,
        false,
    )
    .await;
    let after = space(&f, &s.id);
    assert_eq!(after.name, s.name, "shipped: the name stays");
    assert_eq!(
        after.title.as_deref(),
        Some("Retry export on 429s"),
        "the title isn't tied to shipping"
    );
}

// ── real git ────────────────────────────────────────────────────────────────

mod real_git {
    use std::path::Path;
    use std::process::Command;
    use std::sync::Arc;

    use theodore_contract::RevisionRequest;
    use theodore_domain::RepoRef;
    use theodore_ports::CredentialSource;

    use super::*;

    struct NoAuth;
    impl CredentialSource for NoAuth {
        fn token_for(&self, _repo: &RepoRef) -> Option<String> {
            None
        }
    }

    fn run(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .expect("git installed");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A local folder project on real git: the space's worktree and
    /// checkpoints land in Theodore's cache, the folder is never touched, a
    /// project refresh still works with a space checked out, and archive
    /// keeps the checkpoint ref.
    #[tokio::test]
    async fn a_space_lives_in_the_cache_and_never_touches_the_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("app");
        std::fs::create_dir_all(&folder).unwrap();
        run(&folder, &["init", "-q", "-b", "main"]);
        run(&folder, &["config", "user.email", "a@b.com"]);
        run(&folder, &["config", "user.name", "test"]);
        std::fs::write(folder.join("readme.md"), "hello\n").unwrap();
        run(&folder, &["add", "-A"]);
        run(
            &folder,
            &[
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "-q",
                "-m",
                "init",
            ],
        );
        let folder_head = run(&folder, &["rev-parse", "HEAD"]);

        let root = tmp.path().join("theodore");
        std::fs::create_dir_all(&root).unwrap();
        let f = fixtures();
        let deps = crate::Deps {
            git: Arc::new(theodore_git::GitCli::new(root.clone(), Arc::new(NoAuth))),
            ..f.deps()
        };
        let (core, _recv) = Core::start(deps);
        let project = core
            .add_project(AddProjectRequest {
                repo: None,
                path: Some(folder.display().to_string()),
            })
            .await
            .unwrap();
        wait_until(|| {
            f.store
                .get(models::PROJECT, &project.id)
                .unwrap()
                .is_some_and(|r| r.data["status"] == json!("ready"))
        })
        .await;

        let s = core
            .create_space(request(&project, ""))
            .await
            .expect("create_space");
        let wt = Path::new(&s.worktree).to_path_buf();
        assert!(wt.starts_with(root.join("spaces")));
        assert_eq!(s.head_sha.as_str(), folder_head);

        // What the agent would have written during its turn.
        std::fs::write(wt.join("export.csv"), "a,b\n").unwrap();
        f.agents.push_script(done("Added export.csv", 0.0));
        core.space_send(SpaceSendRequest {
            space: s.id.clone(),
            text: "add an export".into(),
            agent: None,
            model: None,
            mode: None,
            attachments: None,
        })
        .await
        .unwrap();
        let s = idle(&f, &s.id).await;
        assert_eq!(s.status, SpaceStatus::Idle, "{:?}", s.error);
        assert_ne!(s.head_sha.as_str(), folder_head);

        let mirror = root.join("repos/local/local/app.git");
        let refname = format!("refs/theodore/spaces/{}", s.id);
        assert_eq!(run(&mirror, &["rev-parse", &refname]), s.head_sha.as_str());
        assert_eq!(
            run(&mirror, &["show", &format!("{}:export.csv", s.head_sha)]),
            "a,b"
        );

        // The live diff is base...checkpoint.
        let review = s.review_pr.clone().unwrap();
        wait_until(|| {
            f.store
                .get(models::PR, &review.0)
                .unwrap()
                .is_some_and(|r| r.data["head_sha"] == json!(s.head_sha))
        })
        .await;
        let rev = core
            .get_revision(RevisionRequest {
                pr: review,
                head: s.head_sha.clone(),
            })
            .await
            .unwrap();
        assert_eq!(
            rev.files
                .iter()
                .map(|f| f.path.as_str())
                .collect::<Vec<_>>(),
            vec!["export.csv"]
        );

        // The cache still refreshes with a space checked out.
        core.refresh_project(&project.id).await.expect("refresh");

        // The user's folder: same HEAD, same branches, clean.
        assert_eq!(run(&folder, &["rev-parse", "HEAD"]), folder_head);
        assert_eq!(
            run(&folder, &["branch", "--format=%(refname:short)"]),
            "main"
        );
        assert_eq!(run(&folder, &["status", "--porcelain"]), "");

        // Merge locally is the one action that writes into the folder: a
        // dirty tree is refused untouched, a clean one gets the squashed
        // commit on its checked-out branch.
        std::fs::write(folder.join("scratch.txt"), "wip\n").unwrap();
        let err = core
            .ship_space(theodore_contract::ShipSpaceRequest {
                space: s.id.clone(),
                action: theodore_contract::ShipAction::MergeLocally,
                title: None,
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("uncommitted"), "{err}");
        assert_eq!(run(&folder, &["rev-parse", "HEAD"]), folder_head);
        std::fs::remove_file(folder.join("scratch.txt")).unwrap();

        let shipped = core
            .ship_space(theodore_contract::ShipSpaceRequest {
                space: s.id.clone(),
                action: theodore_contract::ShipAction::MergeLocally,
                title: Some("Add an export".into()),
            })
            .await
            .expect("merge locally");
        assert_eq!(shipped.shipped.as_ref().unwrap().branch, "main");
        assert_eq!(
            run(&folder, &["log", "-1", "--format=%s|%an"]),
            "Add an export|test"
        );
        assert_eq!(
            run(&folder, &["rev-parse", "HEAD~1"]),
            folder_head,
            "one commit on top"
        );
        assert_eq!(
            std::fs::read_to_string(folder.join("export.csv")).unwrap(),
            "a,b\n"
        );
        assert_eq!(run(&folder, &["status", "--porcelain"]), "");

        core.archive_space(&s.id).await.unwrap();
        assert!(!wt.exists());
        assert_eq!(run(&mirror, &["rev-parse", &refname]), s.head_sha.as_str());
        assert_eq!(space(&f, &s.id).status, SpaceStatus::Archived);
    }
}
