//! Update from base (docs/spaces.md §6) against `Core` + `testkit` fakes:
//! a clean merge moves the space onto the new base; conflicts become an
//! agent turn whose end Theodore turns into the merge commit; Abort goes
//! back; shipping after an update builds on the new base.

use std::time::Duration;

use theodore_contract::{
    CreateSpaceRequest, Project, ProjectStatus, ShipAction, ShipSpaceRequest, Space, SpaceMode,
    SpaceStatus, models,
};
use theodore_domain::{RepoRef, Sha, Usage};
use theodore_ports::{
    AgentEvent, AgentMode, CommitLog, NewAction, SpaceMerge, Store, SyncActionCode,
};

use crate::testkit::{Fixtures, fixtures};
use crate::{Core, Receivers};

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

fn space(f: &Fixtures, id: &str) -> Space {
    serde_json::from_value(f.store.get(models::SPACE, id).unwrap().unwrap().data).unwrap()
}

async fn idle(f: &Fixtures, id: &str) -> Space {
    wait_until(|| space(f, id).status != SpaceStatus::Running).await;
    space(f, id)
}

fn done(text: &str) -> Vec<AgentEvent> {
    vec![AgentEvent::Done {
        final_text: text.into(),
        usage: Usage {
            input_tokens: 1,
            output_tokens: 1,
            usd: 0.0,
        },
    }]
}

/// A space with one finished turn at `turn111`, on `base000`.
async fn one_turn(f: &Fixtures, core: &Core, project: &Project) -> Space {
    f.agents.push_script(done("done"));
    f.git.push_checkpoint(Sha::new("turn111"));
    let s = core
        .create_space(CreateSpaceRequest {
            project: project.id.clone(),
            goal: "Retry on 429".into(),
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

/// `main` moved on to `base222`, three commits ahead.
fn base_moves(f: &Fixtures) {
    f.git.set_resolve("main", Sha::new("base222"));
    // The live diff resolves the space's base by its sha.
    f.git.set_resolve("base222", Sha::new("base222"));
    f.git.set_commit_log(
        (0..3)
            .map(|i| CommitLog {
                sha: Sha::new(format!("c{i}")),
                title: format!("base {i}"),
                author_name: "bob".into(),
                author_email: "bob@example.com".into(),
                at: 0,
            })
            .collect(),
    );
}

fn ship(s: &Space, action: ShipAction) -> ShipSpaceRequest {
    ShipSpaceRequest {
        space: s.id.clone(),
        action,
        title: None,
    }
}

fn merge_calls(f: &Fixtures) -> Vec<(&'static str, Sha)> {
    f.git
        .merge_calls
        .lock()
        .unwrap()
        .iter()
        .map(|(call, _, sha)| (*call, sha.clone()))
        .collect()
}

#[tokio::test]
async fn a_clean_update_moves_the_space_onto_the_new_base() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let s = one_turn(&f, &core, &project).await;
    assert_eq!(core.space_base_status(&s.id).await.unwrap().behind, 0);
    base_moves(&f);
    let status = core.space_base_status(&s.id).await.unwrap();
    assert_eq!((status.behind, status.base_tip), (3, Sha::new("base222")));

    let s = core.update_space_from_base(&s.id).await.unwrap();
    assert_eq!(merge_calls(&f), vec![("merge", Sha::new("base222"))]);
    assert_eq!(s.base_sha, Sha::new("base222"));
    assert_eq!(s.head_sha, Sha::new("merged-base222"));
    assert_eq!(s.updating, None);
    assert_eq!(s.status, SpaceStatus::Idle);
    // Hand edits got their own checkpoint before the merge.
    let checkpoints = f.git.checkpoints.lock().unwrap().clone();
    assert!(
        checkpoints
            .last()
            .unwrap()
            .1
            .ends_with("edits before updating from main"),
        "{checkpoints:?}"
    );
    assert_eq!(core.space_base_status(&s.id).await.unwrap().behind, 0);

    let err = core.update_space_from_base(&s.id).await.unwrap_err();
    assert!(
        err.to_string().contains("already up to date with main"),
        "{err}"
    );
    // The merge isn't a turn: Undo stops at it.
    let err = core.undo_space_turn(&s.id).await.unwrap_err();
    assert!(
        err.to_string().contains("moved since its last turn"),
        "{err}"
    );
}

#[tokio::test]
async fn conflicts_go_to_the_agent_and_theodore_commits_the_merge() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let s = one_turn(&f, &core, &project).await;
    base_moves(&f);
    f.git
        .push_merge_result(SpaceMerge::Conflicts(vec!["src/retry.rs".into()]));
    f.agents
        .push_script(done("Kept both: jitter and the new limiter."));
    let checkpoints_before = f.git.checkpoints.lock().unwrap().len();

    let updating = core.update_space_from_base(&s.id).await.unwrap();
    let u = updating.updating.clone().expect("resolving");
    assert_eq!(u.base_tip, Sha::new("base222"));
    assert_eq!(u.from, Sha::new("turn111"));
    assert_eq!(u.conflicts, vec!["src/retry.rs".to_string()]);

    let s = idle(&f, &s.id).await;
    let req = f.agents.last_request().expect("the agent resolved");
    assert_eq!(req.mode, AgentMode::Edit);
    assert!(req.prompt.contains("- src/retry.rs\n"), "{}", req.prompt);
    assert!(req.prompt.contains("Keep both intents"), "{}", req.prompt);
    assert!(req.prompt.contains("Theodore commits the merge"));
    // The turn's end completed the merge instead of a normal checkpoint.
    assert_eq!(
        merge_calls(&f),
        vec![
            ("merge", Sha::new("base222")),
            ("finish", Sha::new("base222"))
        ]
    );
    assert_eq!(
        f.git.checkpoints.lock().unwrap().len(),
        checkpoints_before + 1,
        "only the one before the merge"
    );
    assert_eq!(s.updating, None);
    assert_eq!(s.status, SpaceStatus::Idle);
    assert_eq!(s.error, None);
    assert_eq!(s.base_sha, Sha::new("base222"));
    assert_eq!(s.head_sha, Sha::new("merged-base222"));
    let last = s.turn_log.last().unwrap();
    assert_eq!(last.before, Sha::new("turn111"));
    assert_eq!(last.after, Sha::new("merged-base222"));
    assert_eq!(last.base_before, Some(Sha::new("base000")));

    // Undo of that turn takes the merge back too, base included.
    let s = core.undo_space_turn(&s.id).await.unwrap();
    assert_eq!(s.head_sha, Sha::new("turn111"));
    assert_eq!(s.base_sha, Sha::new("base000"));
}

#[tokio::test]
async fn conflicts_left_keep_it_resolving_until_completed_or_aborted() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let s = one_turn(&f, &core, &project).await;
    base_moves(&f);
    f.git
        .push_merge_result(SpaceMerge::Conflicts(vec!["a.txt".into(), "b.txt".into()]));
    f.git
        .push_merge_result(SpaceMerge::Conflicts(vec!["b.txt".into()]));
    f.agents.push_script(done("Fixed a.txt."));
    core.update_space_from_base(&s.id).await.unwrap();

    let s = idle(&f, &s.id).await;
    assert_eq!(s.status, SpaceStatus::Failed);
    assert!(
        s.error
            .as_deref()
            .unwrap()
            .starts_with("conflicts left in b.txt;"),
        "{:?}",
        s.error
    );
    assert_eq!(s.updating.as_ref().unwrap().conflicts, vec!["b.txt"]);
    assert_eq!(s.head_sha, Sha::new("turn111"), "nothing half-merged");
    assert_eq!(s.base_sha, Sha::new("base000"));

    // Nothing half-merged ships, and Undo waits too.
    let err = core
        .ship_space(ship(&s, ShipAction::OpenPr))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("update from base"), "{err}");
    let err = core.undo_space_turn(&s.id).await.unwrap_err();
    assert!(err.to_string().contains("update from base"), "{err}");

    // "Complete" with markers still there says where.
    f.git
        .push_merge_result(SpaceMerge::Conflicts(vec!["b.txt".into()]));
    let err = core.update_space_from_base(&s.id).await.unwrap_err();
    assert!(err.to_string().contains("conflicts left in b.txt"), "{err}");

    let s = core.abort_space_update(&s.id).await.unwrap();
    assert_eq!(
        merge_calls(&f).last().unwrap(),
        &("abort", Sha::new("turn111"))
    );
    assert_eq!(s.updating, None);
    assert_eq!(s.head_sha, Sha::new("turn111"));
    assert_eq!(s.base_sha, Sha::new("base000"));
    assert_eq!(s.status, SpaceStatus::Idle);
    assert_eq!(s.error, None);
    assert_eq!(s.agent_session, None);
    let err = core.abort_space_update(&s.id).await.unwrap_err();
    assert!(err.to_string().contains("no update from base"), "{err}");
}

#[tokio::test]
async fn files_resolved_by_hand_are_completed_by_update() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let s = one_turn(&f, &core, &project).await;
    base_moves(&f);
    f.git
        .push_merge_result(SpaceMerge::Conflicts(vec!["a.txt".into()]));
    f.git
        .push_merge_result(SpaceMerge::Conflicts(vec!["a.txt".into()]));
    f.agents.push_script(done("I couldn't decide."));
    core.update_space_from_base(&s.id).await.unwrap();
    idle(&f, &s.id).await;

    let s = core.update_space_from_base(&s.id).await.unwrap();
    assert_eq!(s.updating, None);
    assert_eq!(s.base_sha, Sha::new("base222"));
    assert_eq!(s.head_sha, Sha::new("merged-base222"));
    assert_eq!(s.status, SpaceStatus::Idle);
}

#[tokio::test]
async fn an_ask_space_resolves_in_edit_mode() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let s = one_turn(&f, &core, &project).await;
    core.apply(vec![NewAction {
        model: models::SPACE.into(),
        model_id: s.id.clone(),
        action: SyncActionCode::C,
        data: Some(serde_json::json!({ "mode": SpaceMode::Ask })),
    }])
    .await
    .unwrap();
    base_moves(&f);
    f.git
        .push_merge_result(SpaceMerge::Conflicts(vec!["a.txt".into()]));
    f.agents.push_script(done("ok"));
    core.update_space_from_base(&s.id).await.unwrap();
    idle(&f, &s.id).await;
    assert_eq!(f.agents.last_request().unwrap().mode, AgentMode::Edit);
}

// ── Shipping after an update ──────────────────────────────────────────────

#[tokio::test]
async fn open_pr_after_an_update_is_one_commit_on_the_new_base() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let s = one_turn(&f, &core, &project).await;
    base_moves(&f);
    let s = core.update_space_from_base(&s.id).await.unwrap();

    core.ship_space(ship(&s, ShipAction::OpenPr)).await.unwrap();
    let ship = f.git.ship.lock().unwrap();
    let c = &ship.commits[0].1;
    assert_eq!(c.parent, Sha::new("base222"));
    assert_eq!(c.merge_parent, None);
    assert_eq!(c.head, Sha::new("merged-base222"));
}

#[tokio::test]
async fn push_after_an_update_merges_the_base_into_the_pr_branch() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let s = one_turn(&f, &core, &project).await;
    core.ship_space(ship(&s, ShipAction::OpenPr)).await.unwrap();
    wait_until(|| space(&f, &s.id).pr.is_some()).await;
    base_moves(&f);
    let s = core.update_space_from_base(&s.id).await.unwrap();

    let s = core.ship_space(ship(&s, ShipAction::Push)).await.unwrap();
    assert_eq!(s.shipped.as_ref().unwrap().pushes, 2);
    let ship = f.git.ship.lock().unwrap();
    let c = &ship.commits[1].1;
    assert_eq!(c.parent, Sha::new("squash1"), "on top, fast-forward");
    assert_eq!(c.merge_parent, Some(Sha::new("base222")));
    assert_eq!(c.head, Sha::new("merged-base222"), "the merged tree");
    assert_eq!(ship.pushes[1], (s.branch.clone(), Sha::new("squash2")));
}
