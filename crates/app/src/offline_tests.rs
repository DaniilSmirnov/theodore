//! Opening a PR while the host can't be reached: a PR opened in an earlier
//! session reopens from its stored detail and the commits already in the
//! clone; one never downloaded says so plainly. A host project works from
//! its clone the same way, and the review's baseline never moves the
//! worktree its head is read from.

use std::time::Duration;

use theodore_contract::{
    AddProjectRequest, CreateSpaceRequest, OpenPrRequest, Project, ProjectStatus, RevisionRequest,
    SpaceSendRequest, SpaceStatus, models,
};
use theodore_domain::{HostKind, PrId, PrRef, RepoRef, Revision, Sha, Usage};
use theodore_ports::{
    AgentEvent, BranchInfo, Fetched, HostError, NewAction, Store, SyncActionCode,
};

use crate::testkit::{self, Fixtures, fixtures};
use crate::{AppError, Core};

fn pr_ref() -> PrRef {
    PrRef {
        repo: RepoRef {
            host: HostKind::Github,
            owner: "acme".into(),
            name: "widgets".into(),
        },
        number: 7,
        host_id: Some("node-7".into()),
    }
}

fn base() -> Sha {
    Sha::new("base000")
}

fn head() -> Sha {
    Sha::new("head111")
}

fn offline() -> HostError {
    HostError::Network("error sending request: connection refused".into())
}

async fn wait_until(mut cond: impl FnMut() -> bool) {
    for _ in 0..400 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("condition not met in time");
}

/// Opens the PR online in one session, then ends it: the store keeps its
/// rows, the clone keeps its commits — what an app restart leaves.
async fn opened_in_an_earlier_session(f: &Fixtures) -> PrId {
    f.host.push_get_pr(Ok(Fetched::Modified {
        etag: Some("etag-1".into()),
        value: testkit::sample_pr_detail(&pr_ref(), &base(), &head()),
    }));
    let (core, _recv) = Core::start(f.deps());
    let resp = core
        .open_pr(OpenPrRequest {
            url: "https://github.com/acme/widgets/pull/7".into(),
        })
        .await
        .expect("online open");
    core.shutdown();
    // Let its poller wind down before the next session queues responses.
    tokio::time::sleep(Duration::from_millis(50)).await;
    resp.pr
}

/// A new session with the host unreachable for every read — the inbox
/// poller's `changed_since` included: left to its default `Ok`, it would
/// flip host health back online whenever it ran after the PR open.
fn start_offline(f: &Fixtures) -> Core {
    f.host.push_get_pr(Err(offline()));
    for _ in 0..8 {
        f.host.push_search(Err(offline()));
    }
    f.host.push_changed_since(Err(offline()));
    Core::start(f.deps()).0
}

/// Once offline the inbox poller stops asking, so the `changed_since`
/// failure `start_offline` queued may still be waiting: drop it, so the
/// next one is what the probe gets.
async fn settled_offline(f: &Fixtures, core: &Core) {
    wait_until(|| !core.host_online()).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    f.host.changed_since_responses.lock().unwrap().clear();
}

#[tokio::test]
async fn online_open_fetches_the_pr() {
    let f = fixtures();
    opened_in_an_earlier_session(&f).await;
    assert!(f.git.network_calls().contains(&"fetch_pr"));
}

#[tokio::test]
async fn a_pr_opened_before_reopens_offline_from_its_local_copy() {
    let f = fixtures();
    let pr = opened_in_an_earlier_session(&f).await;
    let fetched = f.git.network_calls().len();

    let core = start_offline(&f);
    let revision = core
        .get_revision(RevisionRequest {
            pr: pr.clone(),
            head: head(),
        })
        .await
        .expect("the diff opens offline");

    assert_eq!(revision.files.len(), 1);
    assert_eq!(revision.files[0].path, "src/lib.rs");
    assert!(f.host.get_pr_responses.lock().unwrap().is_empty());
    assert_eq!(f.git.network_calls().len(), fetched, "no clone, no fetch");
    assert_eq!(core.0.current_head.read().await.get(&pr), Some(&head()));
    assert!(!core.host_online());
}

#[tokio::test]
async fn offline_with_the_commits_missing_says_so_plainly() {
    let f = fixtures();
    let pr = opened_in_an_earlier_session(&f).await;
    f.git.set_missing_commit(&head());

    let core = start_offline(&f);
    let err = core
        .get_revision(RevisionRequest {
            pr: pr.clone(),
            head: head(),
        })
        .await
        .unwrap_err();

    assert!(matches!(err, AppError::OfflineNotCached), "{err}");
    assert_eq!(err.code(), "app.offline_not_cached");
    let text = err.to_string();
    assert_eq!(
        text,
        "You're offline and this PR's code hasn't been downloaded yet"
    );
    assert!(!core.0.current_head.read().await.contains_key(&pr));
}

#[tokio::test]
async fn offline_a_pr_never_opened_says_so_plainly() {
    let f = fixtures();
    let core = start_offline(&f);
    let err = core
        .ensure_open(&PrId("github:acme/widgets#7".into()))
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::OfflineNotCached), "{err}");
}

#[tokio::test]
async fn a_rejected_token_with_nothing_cached_keeps_the_host_error() {
    let f = fixtures();
    f.host.push_get_pr(Err(HostError::Auth));
    let core = Core::start(f.deps()).0;
    let err = core
        .ensure_open(&PrId("github:acme/widgets#7".into()))
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Host(HostError::Auth)), "{err}");
}

#[tokio::test]
async fn offline_an_earlier_head_with_its_commits_opens_without_fetching() {
    let f = fixtures();
    let pr = opened_in_an_earlier_session(&f).await;
    let core = start_offline(&f);
    core.ensure_open(&pr).await.unwrap();
    let fetched = f.git.network_calls().len();

    let earlier = Sha::new("head000");
    core.get_revision(RevisionRequest {
        pr: pr.clone(),
        head: earlier.clone(),
    })
    .await
    .expect("an earlier head opens from the clone");
    assert_eq!(f.git.network_calls().len(), fetched);

    let gone = Sha::new("head-gone");
    f.git.set_missing_commit(&gone);
    let err = core
        .get_revision(RevisionRequest { pr, head: gone })
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::OfflineNotCached), "{err}");
}

#[tokio::test]
async fn after_reconnecting_a_moved_head_is_picked_up() {
    let f = fixtures();
    let pr = opened_in_an_earlier_session(&f).await;
    let core = start_offline(&f);
    core.ensure_open(&pr).await.unwrap();
    settled_offline(&f, &core).await;
    let fetched = f.git.network_calls().len();

    // The host is back, and the author pushed while it was away.
    let new_head = Sha::new("head222");
    f.git.set_head(new_head.clone());
    f.git
        .set_revision(testkit::two_hunk_revision(&base(), &new_head, "src/lib.rs"));
    f.host.push_get_pr(Ok(Fetched::Modified {
        etag: Some("etag-2".into()),
        value: testkit::sample_pr_detail(&pr_ref(), &base(), &new_head),
    }));
    // The first probe, 30 s after going offline, finds the host back.
    f.clock.advance(crate::poller::PROBE_FIRST_MS);

    let mut moved = false;
    for _ in 0..400 {
        if core.0.current_head.read().await.get(&pr) == Some(&new_head) {
            moved = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(moved, "the poller moves the PR to the pushed head");
    // The push pipeline fetched the new head into the clone.
    wait_until(|| f.git.network_calls()[fetched..].contains(&"fetch_pr")).await;

    // Reopening now is the cached new head, not the offline copy.
    core.ensure_open(&pr).await.unwrap();
    let revision: Revision = core
        .get_revision(RevisionRequest {
            pr: pr.clone(),
            head: new_head.clone(),
        })
        .await
        .unwrap();
    assert_eq!(revision.head, new_head);
}

// ── projects and spaces without a network (a host project's clone) ────────

/// A ready host project written straight to the store (no clone spawned),
/// fetched once at `last_fetched_at`.
async fn seed_host_project(core: &Core, last_fetched_at: i64) -> Project {
    let project = Project {
        id: "github:acme/widgets".into(),
        repo: pr_ref().repo,
        path: None,
        status: ProjectStatus::Ready,
        error: None,
        default_branch: Some("main".into()),
        added_at: 0,
        description: None,
        private: false,
        pushed_at: None,
        last_fetched_at: Some(last_fetched_at),
    };
    core.apply(vec![NewAction {
        model: models::PROJECT.into(),
        model_id: project.id.clone(),
        action: SyncActionCode::I,
        data: Some(serde_json::to_value(&project).unwrap()),
    }])
    .await
    .expect("seed project");
    project
}

fn project_field(f: &Fixtures, id: &str, field: &str) -> serde_json::Value {
    f.store.get(models::PROJECT, id).unwrap().unwrap().data[field].clone()
}

fn branch(name: &str) -> BranchInfo {
    BranchInfo {
        name: name.into(),
        head: Sha::new("feat111"),
        subject: "wip".into(),
        author: "Ada".into(),
        ahead: 1,
        behind: 0,
        updated_at: 0,
    }
}

#[tokio::test]
async fn offline_branches_come_from_the_clone_on_this_mac() {
    let f = fixtures();
    let core = Core::start(f.deps()).0;
    let project = seed_host_project(&core, 0).await;
    f.git.set_branches(vec![branch("feature")]);
    f.git.set_offline(true);

    let branches = core.list_branches(&project.id).await.expect("offline list");

    assert_eq!(branches.len(), 1);
    assert_eq!(branches[0].name, "feature");
    assert!(
        f.git.network_calls().contains(&"ensure_repo"),
        "tried first"
    );
}

#[tokio::test]
async fn offline_with_no_clone_says_so_plainly() {
    let f = fixtures();
    let core = Core::start(f.deps()).0;
    let project = seed_host_project(&core, 0).await;
    f.git.set_offline(true);
    f.git.set_cached(false);

    let err = core.list_branches(&project.id).await.unwrap_err();

    assert!(matches!(err, AppError::Offline), "{err:?}");
    assert_eq!(err.code(), "app.offline");
    assert!(err.is_expected() && err.is_offline());
    assert_eq!(
        err.to_string(),
        "You're offline and this project hasn't been downloaded to this Mac yet"
    );
}

#[tokio::test]
async fn known_offline_skips_the_fetch_entirely() {
    let f = fixtures();
    // Every host read fails, so no poller flips health back online.
    let core = start_offline(&f);
    let project = seed_host_project(&core, 0).await;
    core.note_host::<()>(&Err(offline()));
    assert!(!core.host_online());

    core.list_branches(&project.id).await.expect("offline list");

    assert!(f.git.network_calls().is_empty(), "no fetch to time out");
}

#[tokio::test]
async fn an_offline_refresh_keeps_the_project_ready_and_its_fetch_time() {
    let f = fixtures();
    let core = Core::start(f.deps()).0;
    let project = seed_host_project(&core, 42).await;
    f.git.set_offline(true);

    core.refresh_project(&project.id)
        .await
        .expect("an offline refresh is not an error");

    assert_eq!(project_field(&f, &project.id, "status"), "ready");
    assert_eq!(
        project_field(&f, &project.id, "error"),
        serde_json::Value::Null
    );
    assert_eq!(project_field(&f, &project.id, "last_fetched_at"), 42);
}

#[tokio::test]
async fn adding_a_project_offline_waits_instead_of_failing() {
    let f = fixtures();
    let core = Core::start(f.deps()).0;
    f.git.set_offline(true);
    f.git.set_cached(false);

    let project = core
        .add_project(AddProjectRequest {
            repo: Some(pr_ref().repo),
            path: None,
        })
        .await
        .expect("add_project");
    wait_until(|| project_field(&f, &project.id, "error").is_string()).await;

    assert_eq!(project_field(&f, &project.id, "status"), "cloning");
    assert_eq!(
        project_field(&f, &project.id, "error"),
        "You're offline and this project hasn't been downloaded to this Mac yet"
    );
    core.shutdown();
}

#[tokio::test]
async fn a_clone_left_cloning_resumes_offline_from_the_copy_on_disk() {
    let f = fixtures();
    let core = Core::start(f.deps()).0;
    let project = seed_host_project(&core, 0).await;
    core.apply(vec![NewAction {
        model: models::PROJECT.into(),
        model_id: project.id.clone(),
        action: SyncActionCode::C,
        data: Some(serde_json::json!({ "status": ProjectStatus::Cloning })),
    }])
    .await
    .unwrap();
    f.git.set_offline(true);

    core.resume_cloning_projects().await.unwrap();

    wait_until(|| project_field(&f, &project.id, "status") == "ready").await;
}

fn space_status(f: &Fixtures, id: &str) -> SpaceStatus {
    serde_json::from_value(f.store.get(models::SPACE, id).unwrap().unwrap().data["status"].clone())
        .unwrap()
}

fn send(space: &str, text: &str) -> SpaceSendRequest {
    SpaceSendRequest {
        space: space.into(),
        text: text.into(),
        agent: None,
        model: None,
        mode: None,
        attachments: None,
    }
}

fn agent_done() -> Vec<AgentEvent> {
    vec![AgentEvent::Done {
        final_text: "ok".into(),
        usage: Usage::default(),
    }]
}

#[tokio::test]
async fn a_space_starts_and_takes_messages_offline() {
    let f = fixtures();
    let core = Core::start(f.deps()).0;
    let project = seed_host_project(&core, 0).await;
    f.git.set_resolve("main", base());
    f.git.set_offline(true);

    f.agents.push_script(agent_done());
    let space = core
        .create_space(CreateSpaceRequest {
            project: project.id.clone(),
            goal: "Fix the redirect".into(),
            name: None,
            branch: None,
            base: None,
            agent: None,
            model: None,
            start_from: None,
        })
        .await
        .expect("create_space offline");
    wait_until(|| space_status(&f, &space.id) == SpaceStatus::Idle).await;

    f.agents.push_script(agent_done());
    core.space_send(send(&space.id, "and the tests"))
        .await
        .expect("space_send offline");
    wait_until(|| space_status(&f, &space.id) == SpaceStatus::Idle).await;

    // Without a clone there is nothing to work in.
    f.git.set_cached(false);
    let err = core
        .space_send(send(&space.id, "one more"))
        .await
        .unwrap_err();
    assert!(err.is_offline(), "{err:?}");
}

// ── the review's head and baseline share one worktree ─────────────────────

#[tokio::test]
async fn loading_the_baseline_never_moves_the_head_worktree() {
    let f = fixtures();
    f.host.push_get_pr(Ok(Fetched::Modified {
        etag: Some("etag-1".into()),
        value: testkit::sample_pr_detail(&pr_ref(), &base(), &head()),
    }));
    let core = Core::start(f.deps()).0;
    let pr = core
        .open_pr(OpenPrRequest {
            url: "https://github.com/acme/widgets/pull/7".into(),
        })
        .await
        .expect("open")
        .pr;
    // Not cached, as after a restart: both revisions take the full path,
    // and with the delay their checkouts would overlap.
    core.0.revisions.write().await.clear();
    f.git.set_git_delay(Duration::from_millis(20));

    let baseline = Sha::new("seen000");
    let (h, b) = tokio::join!(
        core.get_revision(RevisionRequest {
            pr: pr.clone(),
            head: head(),
        }),
        core.get_revision(RevisionRequest {
            pr: pr.clone(),
            head: baseline.clone(),
        }),
    );
    h.expect("head");
    b.expect("baseline");

    let calls = f.git.worktree_calls();
    assert!(!calls.contains(&baseline), "{calls:?}");
    let revisions = core.0.revisions.read().await;
    let at_head = revisions[&(pr.clone(), head())].worktree_path.clone();
    assert_eq!(revisions[&(pr.clone(), baseline)].worktree_path, at_head);
    assert_eq!(at_head, f.git.worktree_path(&head()));
}

fn calls(n: &std::sync::atomic::AtomicUsize) -> usize {
    n.load(std::sync::atomic::Ordering::SeqCst)
}

/// Gives the pollers a few of their real-time ticks to act on a clock jump.
async fn let_pollers_run() {
    tokio::time::sleep(Duration::from_millis(150)).await;
}

#[tokio::test]
async fn offline_the_pollers_wait_for_a_backed_off_probe() {
    let f = fixtures();
    let pr = opened_in_an_earlier_session(&f).await;
    let core = start_offline(&f);
    core.ensure_open(&pr).await.unwrap();
    settled_offline(&f, &core).await;
    // The startup searches' failures left over would greet the authored-PRs
    // poll once the host is back and take it offline again.
    f.host.search_responses.lock().unwrap().clear();
    let (prs, inbox) = (
        calls(&f.host.get_pr_calls),
        calls(&f.host.changed_since_calls),
    );

    // Past the PR poll and the inbox retry: neither asks the host.
    f.host.push_changed_since(Err(offline()));
    f.clock.advance(15_000);
    let_pollers_run().await;
    assert_eq!(calls(&f.host.get_pr_calls), prs, "no PR polls offline");
    assert_eq!(calls(&f.host.changed_since_calls), inbox, "no inbox polls");

    // 30 s in: one probe, still offline, so the next one is 60 s out.
    f.clock.advance(15_000);
    wait_until(|| calls(&f.host.changed_since_calls) == inbox + 1).await;
    let_pollers_run().await;
    assert_eq!(core.0.host_probe.lock().unwrap().delay_ms(), 60_000);
    f.clock.advance(30_000);
    let_pollers_run().await;
    assert_eq!(calls(&f.host.changed_since_calls), inbox + 1, "backed off");
    assert_eq!(calls(&f.host.get_pr_calls), prs);
    assert!(!core.host_online());

    // 60 s after the first probe the host answers: polling resumes at once.
    f.clock.advance(30_000);
    wait_until(|| core.host_online()).await;
    wait_until(|| calls(&f.host.get_pr_calls) > prs).await;
}

#[test]
fn the_probe_backs_off_to_five_minutes_and_resets_on_recovery() {
    use crate::poller::{HostProbe, PROBE_FIRST_MS, PROBE_MAX_MS};
    let mut probe = HostProbe::default();
    probe.arm(0);
    assert_eq!(probe.delay_ms(), PROBE_FIRST_MS);
    let mut delays = vec![];
    for _ in 0..6 {
        probe.failed(0);
        delays.push(probe.delay_ms() / 1000);
    }
    assert_eq!(delays, vec![60, 120, 240, 300, 300, 300]);
    assert_eq!(PROBE_MAX_MS, 300_000);
    probe.reset();
    assert_eq!(probe.delay_ms(), 0);
}
