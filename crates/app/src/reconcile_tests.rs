//! Stored PR rows that say "open" after the host stopped saying so
//! (`usecases/reconcile.rs`), against `Core` + `testkit` fakes.

use std::sync::atomic::Ordering;
use std::time::Duration;

use serde_json::json;
use theodore_contract::{
    AddProjectRequest, CreateSpaceRequest, InboxState, Project, ProjectStatus, models,
};
use theodore_domain::{HostKind, PrDetail, PrId, PrRef, PrState, RepoRef, Sha};
use theodore_ports::{Fetched, HostError, NewAction, Store, SyncActionCode};

use crate::testkit::{self, Fixtures, fixtures};
use crate::{Core, Receivers};

async fn wait_until(mut cond: impl FnMut() -> bool) {
    for _ in 0..2000 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("condition not met in time");
}

/// With the startup inbox refresh already drained, and searches complete
/// from here on.
async fn started(f: &Fixtures) -> (Core, Receivers) {
    let (core, recv) = Core::start(f.deps());
    tokio::time::sleep(Duration::from_millis(30)).await;
    f.host.search_complete.store(true, Ordering::SeqCst);
    (core, recv)
}

fn repo(owner: &str, name: &str) -> RepoRef {
    RepoRef {
        host: HostKind::Github,
        owner: owner.into(),
        name: name.into(),
    }
}

fn pr_ref(r: &RepoRef, number: u64) -> PrRef {
    PrRef {
        repo: r.clone(),
        number,
        host_id: None,
    }
}

fn put(f: &Fixtures, model: &str, id: &str, data: serde_json::Value) {
    f.store
        .apply(vec![NewAction {
            model: model.into(),
            model_id: id.into(),
            action: SyncActionCode::U,
            data: Some(data),
        }])
        .unwrap();
}

/// A stored row claiming `state`.
fn seed_row(f: &Fixtures, pr: &PrRef, state: PrState) -> PrId {
    let mut summary = testkit::sample_pr_summary(pr, &Sha::new("h"));
    summary.state = state;
    let id = pr.id();
    put(
        f,
        models::PR,
        &id.0,
        serde_json::to_value(&summary).unwrap(),
    );
    id
}

async fn seed_project(core: &Core, r: &RepoRef) -> String {
    let id = format!("github:{}", r.slug());
    let project = Project {
        id: id.clone(),
        repo: r.clone(),
        path: None,
        status: ProjectStatus::Ready,
        error: None,
        default_branch: Some("main".into()),
        added_at: 0,
        description: None,
        private: false,
        pushed_at: None,
        last_fetched_at: None,
    };
    core.apply(vec![NewAction {
        model: models::PROJECT.into(),
        model_id: id.clone(),
        action: SyncActionCode::I,
        data: Some(serde_json::to_value(&project).unwrap()),
    }])
    .await
    .unwrap();
    id
}

fn state_of(f: &Fixtures, id: &PrId) -> Option<serde_json::Value> {
    f.store
        .get(models::PR, &id.0)
        .unwrap()
        .map(|r| r.data["state"].clone())
}

fn detail_in(pr: &PrRef, state: PrState) -> PrDetail {
    let mut detail = testkit::sample_pr_detail(pr, &Sha::new("b"), &Sha::new("h"));
    detail.summary.state = state;
    detail
}

fn answer(f: &Fixtures, pr: &PrRef, state: PrState) {
    f.host.push_get_pr(Ok(Fetched::Modified {
        etag: None,
        value: detail_in(pr, state),
    }));
}

fn get_pr_calls(f: &Fixtures) -> usize {
    f.host.get_pr_calls.load(Ordering::SeqCst)
}

/// What a project refresh searches for: open, merged, closed.
fn search_project(f: &Fixtures, open: Vec<&PrRef>) {
    let open = open
        .into_iter()
        .map(|p| testkit::sample_pr_summary(p, &Sha::new("h")))
        .collect();
    f.host.push_search(Ok(open));
    f.host.push_search(Ok(vec![]));
    f.host.push_search(Ok(vec![]));
}

/// The inbox's four sections, all empty.
fn search_inbox_empty(f: &Fixtures) {
    for _ in 0..4 {
        f.host.push_search(Ok(vec![]));
    }
}

// ── a project's repo ────────────────────────────────────────────────────────

#[tokio::test]
async fn a_pr_merged_while_away_gets_its_real_state() {
    let f = fixtures();
    let (core, _recv) = started(&f).await;
    let r = repo("acme", "widgets");
    let project = seed_project(&core, &r).await;
    let gone = pr_ref(&r, 1);
    let id = seed_row(&f, &gone, PrState::Open);

    search_project(&f, vec![]);
    answer(&f, &gone, PrState::Merged);
    core.refresh_project(&project).await.unwrap();

    assert_eq!(state_of(&f, &id), Some(json!("merged")));
    assert_eq!(f.host.get_pr_refs.lock().unwrap().as_slice(), [gone]);
}

#[tokio::test]
async fn a_draft_claims_open_too() {
    let f = fixtures();
    let (core, _recv) = started(&f).await;
    let r = repo("acme", "widgets");
    let project = seed_project(&core, &r).await;
    let gone = pr_ref(&r, 1);
    let id = seed_row(&f, &gone, PrState::Draft);

    search_project(&f, vec![]);
    answer(&f, &gone, PrState::Closed);
    core.refresh_project(&project).await.unwrap();

    assert_eq!(state_of(&f, &id), Some(json!("closed")));
}

/// The owner's number: a row GitHub can't resolve kept counting as open.
/// Rows hanging off the PR stay: it comes back if the repo does.
#[tokio::test]
async fn a_pr_the_host_cant_find_is_removed_and_keeps_your_work() {
    let f = fixtures();
    let (core, _recv) = started(&f).await;
    let r = repo("acme", "widgets");
    let project = seed_project(&core, &r).await;
    let gone = pr_ref(&r, 2681023619);
    let id = seed_row(&f, &gone, PrState::Open);
    let detail = detail_in(&gone, PrState::Open);
    put(
        &f,
        models::PR_DETAIL,
        &id.0,
        serde_json::to_value(&detail).unwrap(),
    );
    let state = InboxState {
        pr: id.clone(),
        done: true,
        snooze: None,
    };
    put(
        &f,
        models::INBOX_STATE,
        &id.0,
        serde_json::to_value(&state).unwrap(),
    );

    search_project(&f, vec![]);
    f.host.push_get_pr(Err(HostError::NotFound("gone".into())));
    core.refresh_project(&project).await.unwrap();

    assert!(f.store.get(models::PR, &id.0).unwrap().is_none());
    assert!(f.store.get(models::PR_DETAIL, &id.0).unwrap().is_none());
    assert!(f.store.get(models::INBOX_STATE, &id.0).unwrap().is_some());
}

#[tokio::test]
async fn a_pr_open_in_a_tab_keeps_its_row_when_the_host_cant_find_it() {
    let f = fixtures();
    let (core, _recv) = started(&f).await;
    let r = repo("acme", "widgets");
    let project = seed_project(&core, &r).await;
    let gone = pr_ref(&r, 1);
    let id = seed_row(&f, &gone, PrState::Open);
    core.0
        .current_head
        .write()
        .await
        .insert(id.clone(), Sha::new("h"));

    search_project(&f, vec![]);
    f.host.push_get_pr(Err(HostError::NotFound("gone".into())));
    core.refresh_project(&project).await.unwrap();

    assert_eq!(state_of(&f, &id), Some(json!("open")));
}

#[tokio::test]
async fn a_failed_open_search_changes_nothing() {
    let f = fixtures();
    let (core, _recv) = started(&f).await;
    let r = repo("acme", "widgets");
    let project = seed_project(&core, &r).await;
    let id = seed_row(&f, &pr_ref(&r, 1), PrState::Open);

    f.host
        .push_search(Err(HostError::Protocol("search broke".into())));
    f.host.push_search(Ok(vec![]));
    f.host.push_search(Ok(vec![]));
    core.refresh_project(&project).await.unwrap();

    assert_eq!(get_pr_calls(&f), 0);
    assert_eq!(state_of(&f, &id), Some(json!("open")));
}

#[tokio::test]
async fn a_search_cut_short_changes_nothing() {
    let f = fixtures();
    let (core, _recv) = started(&f).await;
    let r = repo("acme", "widgets");
    let project = seed_project(&core, &r).await;
    let id = seed_row(&f, &pr_ref(&r, 1), PrState::Open);

    f.host.search_complete.store(false, Ordering::SeqCst);
    search_project(&f, vec![]);
    core.refresh_project(&project).await.unwrap();

    assert_eq!(get_pr_calls(&f), 0);
    assert_eq!(state_of(&f, &id), Some(json!("open")));
}

#[tokio::test]
async fn a_pr_the_search_returned_is_not_fetched() {
    let f = fixtures();
    let (core, _recv) = started(&f).await;
    let r = repo("acme", "widgets");
    let project = seed_project(&core, &r).await;
    let open = pr_ref(&r, 1);
    seed_row(&f, &open, PrState::Open);

    search_project(&f, vec![&open]);
    core.refresh_project(&project).await.unwrap();

    assert_eq!(get_pr_calls(&f), 0);
}

#[tokio::test]
async fn a_merged_pr_the_merged_search_returned_is_not_fetched() {
    let f = fixtures();
    let (core, _recv) = started(&f).await;
    let r = repo("acme", "widgets");
    let project = seed_project(&core, &r).await;
    let merged = pr_ref(&r, 1);
    let id = seed_row(&f, &merged, PrState::Open);

    let mut summary = testkit::sample_pr_summary(&merged, &Sha::new("h"));
    summary.state = PrState::Merged;
    // `recent` keeps what's within 14 days of the clock.
    summary.updated_at = testkit::sample_pr_summary(&merged, &Sha::new("h")).updated_at;
    f.clock.advance(1);
    f.host.push_search(Ok(vec![]));
    f.host.push_search(Ok(vec![summary]));
    f.host.push_search(Ok(vec![]));
    core.refresh_project(&project).await.unwrap();

    assert_eq!(get_pr_calls(&f), 0);
    assert_eq!(state_of(&f, &id), Some(json!("merged")));
}

/// Other repos' rows, closed rows and rows with no host PR behind them.
#[tokio::test]
async fn only_host_prs_of_this_repo_that_claim_open_are_fetched() {
    let f = fixtures();
    let (core, _recv) = started(&f).await;
    let r = repo("acme", "widgets");
    let project = seed_project(&core, &r).await;

    let other = seed_row(&f, &pr_ref(&repo("acme", "other"), 1), PrState::Open);
    let closed = seed_row(&f, &pr_ref(&r, 2), PrState::Closed);

    // A branch review of a GitHub project: its number is the branch's hash,
    // its author a commit's email, and it has no host URL.
    let branch_ref = PrRef {
        repo: r.clone(),
        number: 2681023619,
        host_id: Some("feature".into()),
    };
    let mut branch = testkit::sample_pr_summary(&branch_ref, &Sha::new("h"));
    branch.url = String::new();
    branch.author = testkit::sample_person("someone@example.com");
    let branch_id = branch_ref.id();
    put(
        &f,
        models::PR,
        &branch_id.0,
        serde_json::to_value(&branch).unwrap(),
    );

    let space_ref = PrRef {
        repo: r.clone(),
        number: 3,
        host_id: Some(theodore_ports::space_ref("abc")),
    };
    let mut space = testkit::sample_pr_summary(&space_ref, &Sha::new("h"));
    space.url = String::new();
    let space_id = space_ref.id();
    put(
        &f,
        models::PR,
        &space_id.0,
        serde_json::to_value(&space).unwrap(),
    );

    search_project(&f, vec![]);
    core.refresh_project(&project).await.unwrap();

    assert_eq!(get_pr_calls(&f), 0);
    assert_eq!(state_of(&f, &other), Some(json!("open")));
    assert_eq!(state_of(&f, &closed), Some(json!("closed")));
    assert_eq!(state_of(&f, &branch_id), Some(json!("open")));
    assert_eq!(state_of(&f, &space_id), Some(json!("open")));
}

/// The space's own sweep keeps its PR fresh, and that sweep is what marks
/// the space done. Once archived, nothing else would.
#[tokio::test]
async fn a_live_spaces_pr_is_left_to_its_sweep() {
    let f = fixtures();
    let (core, _recv) = started(&f).await;
    let folder = core
        .add_project(AddProjectRequest {
            repo: None,
            path: Some("/tmp/my-folder".into()),
        })
        .await
        .unwrap();
    wait_until(|| {
        f.store
            .get(models::PROJECT, &folder.id)
            .unwrap()
            .is_some_and(|r| r.data["status"] == json!("ready"))
    })
    .await;
    f.git.set_resolve("main", Sha::new("base000"));
    let s = core
        .create_space(CreateSpaceRequest {
            project: folder.id.clone(),
            goal: String::new(),
            name: None,
            branch: None,
            base: None,
            agent: None,
            model: None,
            start_from: None,
        })
        .await
        .unwrap();

    let r = repo("acme", "widgets");
    let project = seed_project(&core, &r).await;
    let shipped = pr_ref(&r, 7);
    let id = seed_row(&f, &shipped, PrState::Open);
    core.apply(vec![NewAction {
        model: models::SPACE.into(),
        model_id: s.id.clone(),
        action: SyncActionCode::C,
        data: Some(json!({ "pr": id })),
    }])
    .await
    .unwrap();

    search_project(&f, vec![]);
    core.refresh_project(&project).await.unwrap();
    assert_eq!(get_pr_calls(&f), 0);

    core.apply(vec![NewAction {
        model: models::SPACE.into(),
        model_id: s.id.clone(),
        action: SyncActionCode::C,
        data: Some(json!({ "status": "archived" })),
    }])
    .await
    .unwrap();
    search_project(&f, vec![]);
    answer(&f, &shipped, PrState::Merged);
    core.refresh_project(&project).await.unwrap();
    assert_eq!(state_of(&f, &id), Some(json!("merged")));
}

#[tokio::test]
async fn fetches_per_run_are_bounded_and_still_open_rows_dont_starve_the_rest() {
    let f = fixtures();
    let (core, _recv) = started(&f).await;
    let r = repo("acme", "widgets");
    let project = seed_project(&core, &r).await;
    let prs: Vec<PrRef> = (1..=15).map(|n| pr_ref(&r, n)).collect();
    for pr in &prs {
        seed_row(&f, pr, PrState::Open);
    }

    // Each asked PR answers "still open": it's real, the search just missed it.
    let (first, rest) = {
        search_project(&f, vec![]);
        for _ in 0..10 {
            answer(&f, &prs[0], PrState::Open);
        }
        core.refresh_project(&project).await.unwrap();
        let asked: Vec<PrRef> = f.host.get_pr_refs.lock().unwrap().drain(..).collect();
        assert_eq!(asked.len(), 10, "bounded per run");

        search_project(&f, vec![]);
        for _ in 0..10 {
            answer(&f, &prs[0], PrState::Open);
        }
        core.refresh_project(&project).await.unwrap();
        let more: Vec<PrRef> = f.host.get_pr_refs.lock().unwrap().drain(..).collect();
        (asked, more)
    };
    assert_eq!(rest.len(), 5, "the first ten aren't asked again soon");
    assert!(rest.iter().all(|p| !first.contains(p)));

    // An hour on, they're due again. The poller would refresh the project
    // too, and share the fake's queues.
    core.shutdown();
    f.clock.advance(60 * 60_000);
    search_project(&f, vec![]);
    for _ in 0..10 {
        answer(&f, &prs[0], PrState::Open);
    }
    core.refresh_project(&project).await.unwrap();
    assert_eq!(f.host.get_pr_refs.lock().unwrap().len(), 10);
}

/// A lost connection says nothing about the PR: ask again next run.
#[tokio::test]
async fn a_connection_error_stops_the_run_and_is_retried() {
    let f = fixtures();
    let (core, _recv) = started(&f).await;
    let r = repo("acme", "widgets");
    let project = seed_project(&core, &r).await;
    let one = pr_ref(&r, 1);
    let two = pr_ref(&r, 2);
    let one_id = seed_row(&f, &one, PrState::Open);
    let two_id = seed_row(&f, &two, PrState::Open);

    search_project(&f, vec![]);
    f.host
        .push_get_pr(Err(HostError::RateLimited { reset_at: 1 }));
    core.refresh_project(&project).await.unwrap();
    assert_eq!(get_pr_calls(&f), 1);
    assert_eq!(state_of(&f, &one_id), Some(json!("open")));
    assert_eq!(state_of(&f, &two_id), Some(json!("open")));

    search_project(&f, vec![]);
    answer(&f, &one, PrState::Merged);
    answer(&f, &two, PrState::Closed);
    core.refresh_project(&project).await.unwrap();
    assert_eq!(state_of(&f, &one_id), Some(json!("merged")));
    assert_eq!(state_of(&f, &two_id), Some(json!("closed")));
}

/// Another kind of failure on one PR doesn't hold the others up, or keep
/// getting asked ahead of them.
#[tokio::test]
async fn another_error_leaves_the_row_and_moves_on() {
    let f = fixtures();
    let (core, _recv) = started(&f).await;
    let r = repo("acme", "widgets");
    let project = seed_project(&core, &r).await;
    let one = pr_ref(&r, 1);
    let two = pr_ref(&r, 2);
    let one_id = seed_row(&f, &one, PrState::Open);
    let two_id = seed_row(&f, &two, PrState::Open);

    search_project(&f, vec![]);
    f.host
        .push_get_pr(Err(HostError::Protocol("odd answer".into())));
    answer(&f, &two, PrState::Merged);
    core.refresh_project(&project).await.unwrap();

    assert_eq!(state_of(&f, &one_id), Some(json!("open")));
    assert_eq!(state_of(&f, &two_id), Some(json!("merged")));
}

// ── inbox rows of repos that aren't projects ────────────────────────────────

#[tokio::test]
async fn an_inbox_row_outside_any_project_gets_its_real_state() {
    let f = fixtures();
    let (core, _recv) = started(&f).await;
    let stray = pr_ref(&repo("else", "where"), 1);
    let id = seed_row(&f, &stray, PrState::Open);

    search_inbox_empty(&f);
    answer(&f, &stray, PrState::Closed);
    core.refresh_inbox().await.unwrap();

    assert_eq!(state_of(&f, &id), Some(json!("closed")));
}

#[tokio::test]
async fn an_inbox_row_the_host_cant_find_is_removed() {
    let f = fixtures();
    let (core, _recv) = started(&f).await;
    let id = seed_row(&f, &pr_ref(&repo("else", "where"), 1), PrState::Open);

    search_inbox_empty(&f);
    f.host.push_get_pr(Err(HostError::NotFound("gone".into())));
    core.refresh_inbox().await.unwrap();

    assert_eq!(state_of(&f, &id), None);
}

#[tokio::test]
async fn an_inbox_row_the_search_returned_is_not_fetched() {
    let f = fixtures();
    let (core, _recv) = started(&f).await;
    let stray = pr_ref(&repo("else", "where"), 1);
    seed_row(&f, &stray, PrState::Open);

    f.host
        .push_search(Ok(vec![testkit::sample_pr_summary(&stray, &Sha::new("h"))]));
    f.host.push_search(Ok(vec![]));
    f.host.push_search(Ok(vec![]));
    f.host.push_search(Ok(vec![]));
    core.refresh_inbox().await.unwrap();

    assert_eq!(get_pr_calls(&f), 0);
}

/// A project's rows include other people's PRs the inbox never returns.
#[tokio::test]
async fn the_inbox_leaves_a_projects_rows_to_its_refresh() {
    let f = fixtures();
    let (core, _recv) = started(&f).await;
    let r = repo("acme", "widgets");
    seed_project(&core, &r).await;
    let id = seed_row(&f, &pr_ref(&r, 1), PrState::Open);
    // Same repo as far as the host goes, spelled differently.
    let cased = seed_row(&f, &pr_ref(&repo("Acme", "Widgets"), 2), PrState::Open);

    search_inbox_empty(&f);
    core.refresh_inbox().await.unwrap();

    assert_eq!(get_pr_calls(&f), 0);
    assert_eq!(state_of(&f, &id), Some(json!("open")));
    assert_eq!(state_of(&f, &cased), Some(json!("open")));
}

/// The inbox only searches the primary host: it can't speak for another's.
#[tokio::test]
async fn the_inbox_leaves_another_hosts_rows_alone() {
    let f = fixtures();
    let (core, _recv) = started(&f).await;
    let gitlab = RepoRef {
        host: HostKind::Gitlab,
        owner: "else".into(),
        name: "where".into(),
    };
    let id = seed_row(&f, &pr_ref(&gitlab, 1), PrState::Open);

    search_inbox_empty(&f);
    core.refresh_inbox().await.unwrap();

    assert_eq!(get_pr_calls(&f), 0);
    assert_eq!(state_of(&f, &id), Some(json!("open")));
}

#[tokio::test]
async fn a_failed_inbox_section_changes_nothing() {
    let f = fixtures();
    let (core, _recv) = started(&f).await;
    let id = seed_row(&f, &pr_ref(&repo("else", "where"), 1), PrState::Open);

    f.host.push_search(Ok(vec![]));
    f.host
        .push_search(Err(HostError::Protocol("section broke".into())));
    f.host.push_search(Ok(vec![]));
    f.host.push_search(Ok(vec![]));
    core.refresh_inbox().await.unwrap();

    assert_eq!(get_pr_calls(&f), 0);
    assert_eq!(state_of(&f, &id), Some(json!("open")));
}

/// Cut short, "not returned" means nothing: not even a reason to clear.
#[tokio::test]
async fn an_inbox_search_cut_short_changes_nothing() {
    let f = fixtures();
    let (core, _recv) = started(&f).await;
    let stray = pr_ref(&repo("else", "where"), 1);
    let mut summary = testkit::sample_pr_summary(&stray, &Sha::new("h"));
    summary.reasons = vec![theodore_domain::InboxReason::Authored];
    let id = stray.id();
    put(
        &f,
        models::PR,
        &id.0,
        serde_json::to_value(&summary).unwrap(),
    );

    f.host.search_complete.store(false, Ordering::SeqCst);
    search_inbox_empty(&f);
    core.refresh_inbox().await.unwrap();

    assert_eq!(get_pr_calls(&f), 0);
    let row = f.store.get(models::PR, &id.0).unwrap().unwrap();
    assert_eq!(row.data["state"], json!("open"));
    assert_eq!(row.data["reasons"], json!(["authored"]));
}
