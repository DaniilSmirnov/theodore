//! Shipping a space (docs/spaces.md §1, §12) against `Core` + `testkit`
//! fakes: Open PR squashes, pushes `<login>/<slug>` and opens exactly one
//! PR through the outbox; Push adds a commit on top; a local folder merges
//! or keeps a branch; auto-archive.

use std::time::Duration;

use serde_json::json;
use theodore_contract::{
    AddProjectRequest, CreateSpaceRequest, Project, ProjectStatus, Setting, ShipAction, ShipKind,
    ShipSpaceRequest, Space, SpaceMode, SpaceSendRequest, SpaceStatus, models,
};
use theodore_domain::{PrState, RepoRef, Sha, Usage};
use theodore_ports::{
    AgentEvent, AgentMode, GitError, HostError, NewAction, Store, SyncActionCode,
};

use crate::testkit::{Fixtures, fixtures, sample_pr_detail};
use crate::{AppError, Core, Receivers};

const DAY: i64 = 86_400_000;

async fn wait_until(mut cond: impl FnMut() -> bool) {
    for _ in 0..400 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("condition not met in time");
}

fn widget() -> RepoRef {
    RepoRef {
        host: theodore_domain::HostKind::Github,
        owner: "acme".into(),
        name: "widget".into(),
    }
}

/// A ready GitHub project, written straight to the store.
async fn host_project(f: &Fixtures) -> (Core, Receivers, Project) {
    let (core, recv) = Core::start(f.deps());
    tokio::time::sleep(Duration::from_millis(30)).await;
    let project = Project {
        id: "github:acme/widget".into(),
        repo: widget(),
        path: None,
        status: ProjectStatus::Ready,
        error: None,
        default_branch: Some("main".into()),
        added_at: 0,
        description: None,
        private: false,
        pushed_at: None,
        last_fetched_at: Some(0),
    };
    core.apply(vec![NewAction {
        model: models::PROJECT.into(),
        model_id: project.id.clone(),
        action: SyncActionCode::I,
        data: Some(serde_json::to_value(&project).unwrap()),
    }])
    .await
    .unwrap();
    f.git.set_resolve("main", Sha::new("base000"));
    (core, recv, project)
}

async fn folder_project(f: &Fixtures) -> (Core, Receivers, Project) {
    let (core, recv) = Core::start(f.deps());
    tokio::time::sleep(Duration::from_millis(30)).await;
    let project = core
        .add_project(AddProjectRequest {
            repo: None,
            path: Some("/tmp/my-folder".into()),
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
    f.git.set_resolve("main", Sha::new("base000"));
    f.git.set_working_state(theodore_ports::WorkingState {
        current_branch: Some("main".into()),
        dirty_files: 0,
    });
    (core, recv, project)
}

fn space(f: &Fixtures, id: &str) -> Space {
    serde_json::from_value(f.store.get(models::SPACE, id).unwrap().unwrap().data).unwrap()
}

async fn idle(f: &Fixtures, id: &str) -> Space {
    wait_until(|| space(f, id).status != SpaceStatus::Running).await;
    space(f, id)
}

fn done(text: &str, usd: f64) -> Vec<AgentEvent> {
    vec![AgentEvent::Done {
        final_text: text.into(),
        usage: Usage {
            input_tokens: 1,
            output_tokens: 1,
            usd,
        },
    }]
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

/// A space with one finished turn at checkpoint `turn111`.
async fn one_turn(f: &Fixtures, core: &Core, project: &Project, goal: &str) -> Space {
    f.agents.push_script(done("done", 0.0));
    f.git.push_checkpoint(Sha::new("turn111"));
    let s = core
        .create_space(CreateSpaceRequest {
            project: project.id.clone(),
            goal: goal.into(),
            name: None,
            branch: None,
            base: None,
            agent: None,
            model: None,
            start_from: None,
        })
        .await
        .unwrap();
    idle(f, &s.id).await
}

async fn turn(f: &Fixtures, core: &Core, s: &Space, text: &str, head: &str) -> Space {
    f.agents.push_script(done("ok", 0.0));
    f.git.push_checkpoint(Sha::new(head));
    core.space_send(SpaceSendRequest {
        space: s.id.clone(),
        text: text.into(),
        agent: None,
        model: None,
        mode: None,
        attachments: None,
    })
    .await
    .unwrap();
    idle(f, &s.id).await
}

fn ship(s: &Space, action: ShipAction) -> ShipSpaceRequest {
    ShipSpaceRequest {
        space: s.id.clone(),
        action,
        title: None,
    }
}

async fn with_pr(f: &Fixtures, id: &str) -> Space {
    wait_until(|| space(f, id).pr.is_some()).await;
    space(f, id)
}

/// The default `copy` (`.env*`) has nothing to copy on a host project:
/// that's no news, so no setup note says so.
#[tokio::test]
async fn a_host_project_gets_no_note_about_the_default_copy() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let s = one_turn(&f, &core, &project, "Retry on 429").await;
    let notes = s.setup.map(|s| s.notes).unwrap_or_default();
    assert!(!notes.iter().any(|n| n.starts_with("copy:")), "{notes:?}");
}

// ── Open PR ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn open_pr_squashes_pushes_the_login_branch_and_opens_one_pr() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let s = one_turn(&f, &core, &project, "Fix the login redirect").await;
    assert_eq!(
        s.branch, "me/fix-the-login-redirect",
        "named after the viewer"
    );
    let pr_ref = theodore_domain::PrRef {
        repo: widget(),
        number: 100,
        host_id: None,
    };
    f.host.push_get_pr(Ok(theodore_ports::Fetched::Modified {
        etag: None,
        value: sample_pr_detail(&pr_ref, &Sha::new("base000"), &Sha::new("squash1")),
    }));

    let shipped = core.ship_space(ship(&s, ShipAction::OpenPr)).await.unwrap();

    let info = shipped.shipped.clone().expect("shipped");
    assert_eq!(info.how, ShipKind::Pr);
    assert_eq!(info.sha, Sha::new("squash1"));
    assert_eq!(info.head, Sha::new("turn111"));
    assert_eq!(info.pushes, 1);
    {
        let ship = f.git.ship.lock().unwrap();
        assert_eq!(ship.commits.len(), 1);
        let (id, c) = &ship.commits[0];
        assert_eq!(id, &s.id);
        assert_eq!(c.parent, Sha::new("base000"), "one commit on the base");
        assert_eq!(c.head, Sha::new("turn111"));
        assert_eq!(c.replay_from, None);
        assert_eq!(c.message, "Fix the login redirect");
        assert_eq!(c.author.email, "ada@example.com");
        assert_eq!(
            ship.pushes,
            vec![("me/fix-the-login-redirect".to_string(), Sha::new("squash1"))]
        );
    }

    let s = with_pr(&f, &s.id).await;
    assert_eq!(s.pr.as_ref().unwrap().0, "github:acme/widget#100");
    let calls = f.host.create_pr_calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, widget());
    assert_eq!(calls[0].1.head, "me/fix-the-login-redirect");
    assert_eq!(calls[0].1.base, "main");
    assert_eq!(calls[0].1.title, "Fix the login redirect");
    assert!(!calls[0].1.draft);
    // The PR is an ordinary PR row: Pull requests and the Inbox list it.
    wait_until(|| {
        f.store
            .get(models::PR, "github:acme/widget#100")
            .unwrap()
            .is_some()
    })
    .await;

    let err = core
        .ship_space(ship(&s, ShipAction::OpenPr))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("already has a PR"), "{err}");
}

#[tokio::test]
async fn open_as_draft_and_an_edited_title() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let s = one_turn(&f, &core, &project, "Add CSV export\nQuote commas too.").await;
    core.ship_space(ShipSpaceRequest {
        title: Some("  Export orders as CSV ".into()),
        ..ship(&s, ShipAction::OpenDraftPr)
    })
    .await
    .unwrap();
    with_pr(&f, &s.id).await;
    let calls = f.host.create_pr_calls.lock().unwrap().clone();
    assert!(calls[0].1.draft);
    assert_eq!(calls[0].1.title, "Export orders as CSV");
    assert!(
        calls[0]
            .1
            .body
            .starts_with("Add CSV export\nQuote commas too.\n\n<!-- theodore-space:"),
        "the goal, then the link back"
    );
    assert_eq!(
        f.git.ship.lock().unwrap().commits[0].1.message,
        "Export orders as CSV\n\nAdd CSV export\nQuote commas too."
    );
}

#[tokio::test]
async fn a_create_that_timed_out_after_landing_is_not_opened_twice() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let s = one_turn(&f, &core, &project, "Retry on 429").await;
    *f.host.create_pr_lands.lock().unwrap() = true;
    f.host
        .create_pr_errors
        .lock()
        .unwrap()
        .push_back(HostError::Network("timed out".into()));

    core.ship_space(ship(&s, ShipAction::OpenPr)).await.unwrap();
    // The outbox retries after its backoff; the retry finds the PR.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(space(&f, &s.id).pr.is_none());
    f.clock.advance(60_000);
    let s = with_pr(&f, &s.id).await;
    assert_eq!(s.pr.unwrap().0, "github:acme/widget#100");
    assert_eq!(f.host.create_pr_calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn open_pr_again_after_a_rejected_create_pushes_nothing_new() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let s = one_turn(&f, &core, &project, "Retry on 429").await;
    f.host
        .create_pr_errors
        .lock()
        .unwrap()
        .push_back(HostError::Validation("base branch is protected".into()));
    core.ship_space(ship(&s, ShipAction::OpenPr)).await.unwrap();
    wait_until(|| {
        f.store
            .outbox_snapshot()
            .iter()
            .all(|i| i.status == theodore_ports::OutboxStatus::Failed)
    })
    .await;
    assert!(space(&f, &s.id).pr.is_none());

    core.ship_space(ship(&s, ShipAction::OpenPr)).await.unwrap();
    with_pr(&f, &s.id).await;
    let ship = f.git.ship.lock().unwrap();
    assert_eq!(ship.commits.len(), 1, "the same squashed commit");
    assert_eq!(ship.pushes.len(), 1, "already on the host");
}

#[tokio::test]
async fn a_branch_name_taken_on_the_host_is_refused() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let s = one_turn(&f, &core, &project, "Retry on 429").await;
    f.git
        .ship
        .lock()
        .unwrap()
        .remote
        .insert(s.branch.clone(), Sha::new("someone"));
    let err = core
        .ship_space(ship(&s, ShipAction::OpenPr))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, AppError::Invalid(m) if m.contains("already exists")),
        "{err}"
    );
    assert!(f.git.ship.lock().unwrap().pushes.is_empty());
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

/// A space adopted from an existing branch or PR (`Space::fork_sha`,
/// docs/spaces.md §3 "New space from an issue/branch/PR") ships onto that
/// branch's own tip, not `base_sha` — and isn't refused as "already
/// exists" the way a fresh `<login>/<slug>` space would be.
#[tokio::test]
async fn an_adopted_branch_ships_onto_its_own_fork_point_not_base() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let s = one_turn(&f, &core, &project, "Fix the redirect").await;
    let fork_sha = Sha::new("fork000");
    patch_space(&f, &s.id, json!({ "fork_sha": fork_sha.0 }));
    f.git
        .ship
        .lock()
        .unwrap()
        .remote
        .insert(s.branch.clone(), fork_sha.clone());

    core.ship_space(ship(&s, ShipAction::OpenPr)).await.unwrap();
    with_pr(&f, &s.id).await;

    let ship = f.git.ship.lock().unwrap();
    assert_eq!(ship.commits.len(), 1);
    assert_eq!(
        ship.commits[0].1.parent, fork_sha,
        "squashed onto the fork point"
    );
    assert_eq!(ship.pushes.len(), 1);
}

#[tokio::test]
async fn nothing_to_ship_without_changes() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    f.agents.push_script(done("nothing", 0.0));
    let s = core
        .create_space(CreateSpaceRequest {
            project: project.id.clone(),
            goal: "Look around".into(),
            name: None,
            branch: None,
            base: None,
            agent: None,
            model: None,
            start_from: None,
        })
        .await
        .unwrap();
    let s = idle(&f, &s.id).await;
    let err = core
        .ship_space(ship(&s, ShipAction::OpenPr))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no changes"), "{err}");
}

// ── Push ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn push_adds_one_commit_on_top_of_the_pr_branch() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let s = one_turn(&f, &core, &project, "Retry on 429").await;
    core.ship_space(ship(&s, ShipAction::OpenPr)).await.unwrap();
    let s = with_pr(&f, &s.id).await;

    f.clock.advance(1_000);
    let s = turn(
        &f,
        &core,
        &s,
        "Use jitter in the backoff\nplease",
        "turn222",
    )
    .await;
    let s = core.ship_space(ship(&s, ShipAction::Push)).await.unwrap();

    let info = s.shipped.clone().unwrap();
    assert_eq!(info.pushes, 2);
    assert_eq!(info.head, Sha::new("turn222"));
    {
        let ship = f.git.ship.lock().unwrap();
        let c = &ship.commits[1].1;
        assert_eq!(c.parent, Sha::new("squash1"), "on top, not a rewrite");
        assert_eq!(c.replay_from, None);
        // No proposal (the chat agent's "ok" doesn't parse), and your
        // message never goes in: the path the push changes.
        assert_eq!(c.message, "Update src/lib.rs");
        assert_eq!(ship.pushes[1], (s.branch.clone(), Sha::new("squash2")));
    }
    let err = core
        .ship_space(ship(&s, ShipAction::Push))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("nothing new"), "{err}");
}

/// Push's commit says what the turns since the last push changed: the chat
/// agent's title from the diff stat and the agent's replies, never a
/// sentence copied from them, a chat message, a feedback or a Fix CI prompt.
#[tokio::test]
async fn a_push_commit_says_what_changed_not_what_was_said() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let s = one_turn(&f, &core, &project, "Retry on 429").await;
    core.ship_space(ship(&s, ShipAction::OpenPr)).await.unwrap();
    let s = with_pr(&f, &s.id).await;

    f.clock.advance(1_000);
    let say = |reply: &str, head: &str| {
        f.agents.push_script(done(reply, 0.0));
        f.git.push_checkpoint(Sha::new(head));
    };
    let send = |text: &str| {
        core.space_send(SpaceSendRequest {
            space: s.id.clone(),
            text: text.into(),
            agent: None,
            model: None,
            mode: None,
            attachments: None,
        })
    };
    say(
        "Done. The dead-state message now reads \"Your pet has passed on\".",
        "turn222",
    );
    send("Yes, change the dead-state message to something gentler")
        .await
        .unwrap();
    idle(&f, &s.id).await;
    f.clock.advance(1_000);
    say(
        "Fixed the failing test: the hunger timer now starts at zero.",
        "turn333",
    );
    send("This pull request's check is failing:\n\n- test (exit 1)")
        .await
        .unwrap();
    let s = idle(&f, &s.id).await;
    let is_ship_title = |r: &theodore_ports::AgentRequest| {
        r.system.as_deref() == Some(crate::usecases::ship_title::SHIP_TITLE_SYSTEM)
    };
    f.agents.push_tagged(
        is_ship_title,
        done(
            "TITLE: Soften the dead-state message\nSUMMARY:\n\
             - Start the hunger timer at zero.",
            0.0,
        ),
    );
    core.ship_space(ship(&s, ShipAction::Push)).await.unwrap();

    let ask = f
        .agents
        .utility_requests()
        .into_iter()
        .find(|r| is_ship_title(r))
        .expect("asked for a title");
    assert!(
        ask.prompt.contains("Your pet has passed on"),
        "{}",
        ask.prompt
    );
    assert!(ask.prompt.contains("hunger timer"), "{}", ask.prompt);
    assert!(!ask.prompt.contains("check is failing"), "{}", ask.prompt);
    let ship = f.git.ship.lock().unwrap();
    assert_eq!(
        ship.commits[1].1.message,
        "Soften the dead-state message\n\n- Start the hunger timer at zero."
    );
}

#[tokio::test]
async fn push_replays_new_turns_onto_a_branch_someone_else_moved() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let s = one_turn(&f, &core, &project, "Retry on 429").await;
    core.ship_space(ship(&s, ShipAction::OpenPr)).await.unwrap();
    let s = with_pr(&f, &s.id).await;
    f.git
        .ship
        .lock()
        .unwrap()
        .remote
        .insert(s.branch.clone(), Sha::new("reviewer9"));

    let s = turn(&f, &core, &s, "more", "turn222").await;
    core.ship_space(ship(&s, ShipAction::Push)).await.unwrap();
    let ship = f.git.ship.lock().unwrap();
    let c = &ship.commits[1].1;
    assert_eq!(c.parent, Sha::new("reviewer9"));
    assert_eq!(c.replay_from, Some(Sha::new("turn111")));
}

#[tokio::test]
async fn push_needs_a_pr_and_reports_a_rejected_push() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let s = one_turn(&f, &core, &project, "Retry on 429").await;
    let err = core
        .ship_space(ship(&s, ShipAction::Push))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("open a PR"), "{err}");

    f.git
        .ship
        .lock()
        .unwrap()
        .push_errors
        .push_back(GitError::Refused("me/x changed on the host".into()));
    let err = core
        .ship_space(ship(&s, ShipAction::OpenPr))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, AppError::Invalid(m) if m.contains("changed")),
        "{err}"
    );
    assert!(space(&f, &s.id).shipped.is_none());
}

// ── local folder ─────────────────────────────────────────────────────────

#[tokio::test]
async fn merge_locally_merges_and_marks_the_space_done() {
    let f = fixtures();
    let (core, _recv, project) = folder_project(&f).await;
    let s = one_turn(&f, &core, &project, "Add export").await;
    assert_eq!(s.branch, "space/add-export");

    let s = core
        .ship_space(ship(&s, ShipAction::MergeLocally))
        .await
        .unwrap();
    let info = s.shipped.clone().unwrap();
    assert_eq!(info.how, ShipKind::MergedLocally);
    assert_eq!(info.branch, "main");
    assert!(s.done_at.is_some());
    let ship = f.git.ship.lock().unwrap();
    assert_eq!(ship.commits[0].1.parent, Sha::new("base000"));
    assert_eq!(
        ship.merges,
        vec![(
            std::path::PathBuf::from("/tmp/my-folder"),
            s.id.clone(),
            "Merge space add-export".to_string()
        )]
    );
    assert!(ship.pushes.is_empty(), "nothing leaves the machine");
}

#[tokio::test]
async fn a_refused_merge_is_a_clear_error_and_changes_nothing() {
    let f = fixtures();
    let (core, _recv, project) = folder_project(&f).await;
    let s = one_turn(&f, &core, &project, "Add export").await;
    f.git
        .ship
        .lock()
        .unwrap()
        .merge_errors
        .push_back(GitError::Refused(
            "the folder has 2 uncommitted changes; commit or stash them first".into(),
        ));
    let err = core
        .ship_space(ship(&s, ShipAction::MergeLocally))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, AppError::Invalid(m) if m.contains("uncommitted")),
        "{err}"
    );
    let s = space(&f, &s.id);
    assert!(s.shipped.is_none());
    assert!(s.done_at.is_none());
}

#[tokio::test]
async fn keep_as_branch_writes_the_space_branch() {
    let f = fixtures();
    let (core, _recv, project) = folder_project(&f).await;
    let s = one_turn(&f, &core, &project, "Add export").await;
    let s = core
        .ship_space(ship(&s, ShipAction::KeepBranch))
        .await
        .unwrap();
    assert_eq!(s.shipped.as_ref().unwrap().how, ShipKind::Branch);
    assert!(s.done_at.is_some());
    assert_eq!(
        f.git.ship.lock().unwrap().branches,
        vec![(
            std::path::PathBuf::from("/tmp/my-folder"),
            s.id.clone(),
            "space/add-export".to_string()
        )]
    );
    assert!(f.git.ship.lock().unwrap().merges.is_empty());
}

#[tokio::test]
async fn host_and_folder_actions_dont_mix() {
    let f = fixtures();
    let (core, _recv, project) = folder_project(&f).await;
    let s = one_turn(&f, &core, &project, "Add export").await;
    let err = core
        .ship_space(ship(&s, ShipAction::OpenPr))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("local folder"), "{err}");
}

// ── archive, run mode ─────────────────────────────────────────────────────

#[tokio::test]
async fn done_spaces_auto_archive_after_the_setting() {
    let f = fixtures();
    let (core, _recv, project) = folder_project(&f).await;
    let a = one_turn(&f, &core, &project, "Add export").await;
    core.ship_space(ship(&a, ShipAction::KeepBranch))
        .await
        .unwrap();
    let b = one_turn(&f, &core, &project, "Still drafting").await;

    f.clock.advance(6 * DAY);
    core.sweep_spaces().await;
    assert_eq!(space(&f, &a.id).status, SpaceStatus::Idle);

    f.clock.advance(DAY);
    core.sweep_spaces().await;
    assert_eq!(space(&f, &a.id).status, SpaceStatus::Archived);
    assert_eq!(
        space(&f, &b.id).status,
        SpaceStatus::Idle,
        "not done, never auto-archived"
    );
}

#[tokio::test]
async fn zero_days_never_auto_archives() {
    let f = fixtures();
    let (core, _recv, project) = folder_project(&f).await;
    put_setting(&f, "spaces.auto_archive_days", json!(0));
    let a = one_turn(&f, &core, &project, "Add export").await;
    core.ship_space(ship(&a, ShipAction::MergeLocally))
        .await
        .unwrap();
    f.clock.advance(365 * DAY);
    core.sweep_spaces().await;
    assert_eq!(space(&f, &a.id).status, SpaceStatus::Idle);
}

#[tokio::test]
async fn a_merged_pr_marks_the_space_done_then_archives_it() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    put_setting(&f, "spaces.auto_archive_days", json!(2));
    let s = one_turn(&f, &core, &project, "Retry on 429").await;
    core.ship_space(ship(&s, ShipAction::OpenPr)).await.unwrap();
    let s = with_pr(&f, &s.id).await;

    let pr_ref = theodore_domain::PrRef {
        repo: widget(),
        number: 100,
        host_id: None,
    };
    let mut merged = sample_pr_detail(&pr_ref, &Sha::new("base000"), &Sha::new("squash1"));
    merged.summary.state = PrState::Merged;
    f.host.push_get_pr(Ok(theodore_ports::Fetched::Modified {
        etag: None,
        value: merged,
    }));
    core.sweep_spaces().await;
    let done_at = space(&f, &s.id).done_at.expect("done when merged");
    assert_eq!(space(&f, &s.id).status, SpaceStatus::Idle);

    f.clock.advance(2 * DAY);
    core.sweep_spaces().await;
    let s = space(&f, &s.id);
    assert_eq!(s.status, SpaceStatus::Archived);
    assert_eq!(s.done_at, Some(done_at));
}

#[tokio::test]
async fn run_mode_reaches_the_agent() {
    let f = fixtures();
    let (core, _recv, project) = folder_project(&f).await;
    let s = one_turn(&f, &core, &project, "Add export").await;
    f.agents.push_script(done("ran tests", 0.0));
    core.space_send(SpaceSendRequest {
        space: s.id.clone(),
        text: "run the tests".into(),
        agent: None,
        model: None,
        mode: Some(SpaceMode::Run),
        attachments: None,
    })
    .await
    .unwrap();
    let s = idle(&f, &s.id).await;
    assert_eq!(f.agents.last_request().unwrap().mode, AgentMode::Run);
    assert_eq!(s.mode, SpaceMode::Run, "sticky");
}
