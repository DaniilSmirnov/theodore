//! A space from its PR to merged and beyond (docs/spaces.md §3, §6, §7)
//! against `Core` + `testkit` fakes: the PR template and the link-back
//! footer on Open PR, Merge from the space, Continue after merge.

use std::time::Duration;

use serde_json::json;
use theodore_contract::{
    CreateSpaceRequest, MergeMethod as WireMergeMethod, MergeSpaceRequest, Project, ProjectStatus,
    ShipAction, ShipSpaceRequest, Space, SpaceSendRequest, SpaceStatus, models,
};
use theodore_domain::{PrRef, PrState, RepoRef, Sha, Usage};
use theodore_ports::{
    AgentEvent, MergeMethod, MergeState, MergeStatus, NewAction, SpaceMerge, Store, SyncActionCode,
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

fn pr_ref(number: u64) -> PrRef {
    PrRef {
        repo: widget(),
        number,
        host_id: None,
    }
}

async fn host_project(f: &Fixtures) -> (Core, Receivers, Project) {
    host_project_with(f, f.deps()).await
}

async fn host_project_with(f: &Fixtures, deps: crate::Deps) -> (Core, Receivers, Project) {
    let (core, recv) = Core::start(deps);
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
        usage: Usage::default(),
    }]
}

async fn one_turn(f: &Fixtures, core: &Core, project: &Project, goal: &str) -> Space {
    f.agents.push_script(done("done"));
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

async fn send(f: &Fixtures, core: &Core, s: &Space, text: &str, head: Option<&str>) -> Space {
    f.agents.push_script(done("ok"));
    if let Some(head) = head {
        f.git.push_checkpoint(Sha::new(head));
    }
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

fn open_pr(s: &Space) -> ShipSpaceRequest {
    ShipSpaceRequest {
        space: s.id.clone(),
        action: ShipAction::OpenPr,
        title: None,
    }
}

/// A space with one turn and its PR #100 open.
async fn in_review(f: &Fixtures, core: &Core, project: &Project, goal: &str) -> Space {
    let s = one_turn(f, core, project, goal).await;
    f.host.push_get_pr(Ok(theodore_ports::Fetched::Modified {
        etag: None,
        value: sample_pr_detail(&pr_ref(100), &Sha::new("base000"), &Sha::new("squash1")),
    }));
    core.ship_space(open_pr(&s)).await.unwrap();
    wait_until(|| space(f, &s.id).pr.is_some()).await;
    space(f, &s.id)
}

/// The host says PR #`number` merged; the poller's sweep notices.
async fn merge_on_host(f: &Fixtures, core: &Core, number: u64) {
    let mut merged = sample_pr_detail(&pr_ref(number), &Sha::new("base000"), &Sha::new("squash1"));
    merged.summary.state = PrState::Merged;
    f.host.push_get_pr(Ok(theodore_ports::Fetched::Modified {
        etag: None,
        value: merged,
    }));
    core.sweep_spaces().await;
}

/// The base moved on the host to `tip` (the merge landed there).
fn base_moved(f: &Fixtures, tip: &str) {
    f.git
        .ship
        .lock()
        .unwrap()
        .remote
        .insert("main".into(), Sha::new(tip));
    f.git.set_resolve(tip, Sha::new(tip));
}

/// The PR-description fill (a one-shot call), not a background naming or
/// handoff call that could take its script first.
/// `s` as if started from an existing PR, its tip at `fork`.
fn patch_adopted(f: &Fixtures, id: &str, fork: &str, pr: &str) {
    f.store
        .apply(vec![NewAction {
            model: models::SPACE.into(),
            model_id: id.to_string(),
            action: SyncActionCode::C,
            data: Some(json!({ "fork_sha": fork, "pr": pr })),
        }])
        .unwrap();
}

fn is_fill(r: &theodore_ports::AgentRequest) -> bool {
    r.system
        .as_deref()
        .is_some_and(|s| s.contains("pull request descriptions"))
}

fn body_of_the_opened_pr(f: &Fixtures) -> String {
    let calls = f.host.create_pr_calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    calls[0].1.body.clone()
}

fn status(state: MergeState, mergeable: Option<bool>) -> MergeStatus {
    MergeStatus {
        mergeable,
        state,
        allowed_methods: vec![MergeMethod::Merge, MergeMethod::Squash],
        auto_merge_enabled: false,
        in_queue: false,
        required_approvals: None,
        auto_merge_allowed: false,
        merge_queue: false,
        default_method: Some(MergeMethod::Squash),
    }
}

// ── PR template and the link back ─────────────────────────────────────────

const TEMPLATE: &str = "## Summary\n<!-- what and why -->\n\n## Testing\n- [ ] tests";

#[tokio::test]
async fn open_pr_fills_the_repos_template_and_links_back_to_the_space() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    f.git.put_file(
        &Sha::new("turn111"),
        ".github/pull_request_template.md",
        TEMPLATE.as_bytes(),
    );
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
    let s = idle(&f, &s.id).await;
    // The fill: the template with its headings kept.
    f.agents.push_tagged(
        is_fill,
        done("```markdown\n## Summary\nRetries 429s with backoff.\n\n## Testing\n- [x] tests\n```"),
    );
    core.ship_space(open_pr(&s)).await.unwrap();
    wait_until(|| space(&f, &s.id).pr.is_some()).await;

    let fill = f
        .agents
        .utility_requests()
        .into_iter()
        .find(is_fill)
        .expect("the template was filled in");
    assert_eq!(fill.mode, theodore_ports::AgentMode::ReadOnly);
    assert_eq!(fill.model.as_deref(), Some("haiku"), "the cheap model");
    assert!(fill.prompt.contains("## Testing"), "{}", fill.prompt);
    assert!(fill.prompt.contains("Retry on 429"));
    let body = body_of_the_opened_pr(&f);
    assert_eq!(
        body,
        format!(
            "## Summary\nRetries 429s with backoff.\n\n## Testing\n- [x] tests\n\n\
             <!-- theodore-space:{id} -->\n<sub>From a Theodore space: theodore://space/{id}</sub>",
            id = s.id
        )
    );
}

#[tokio::test]
async fn a_fill_that_drops_the_template_keeps_the_template_and_the_goal() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    core.set_space_link_scheme(Some("theodore-staging"));
    // Lower in the order: only used because none above it exists.
    f.git.put_file(
        &Sha::new("turn111"),
        "pull_request_template.md",
        b"Root template",
    );
    f.git.put_file(
        &Sha::new("turn111"),
        "docs/pull_request_template.md",
        TEMPLATE.as_bytes(),
    );
    let s = one_turn(&f, &core, &project, "Retry on 429").await;
    f.agents.push_tagged(is_fill, done("ok"));
    core.ship_space(open_pr(&s)).await.unwrap();
    wait_until(|| space(&f, &s.id).pr.is_some()).await;
    let body = body_of_the_opened_pr(&f);
    assert!(
        body.starts_with(&format!("Retry on 429\n\n{TEMPLATE}\n\n")),
        "{body}"
    );
    assert!(!body.contains("Root template"), "docs/ comes first");
    assert!(body.ends_with(&format!(
        "<sub>From a Theodore space: theodore-staging://space/{}</sub>",
        s.id
    )));
    assert_eq!(body.matches("<!-- theodore-space:").count(), 1);
}

#[tokio::test]
async fn without_a_template_the_body_is_the_goal_and_the_footer() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let s = in_review(&f, &core, &project, "Add CSV export\nQuote commas too.").await;
    let body = body_of_the_opened_pr(&f);
    assert_eq!(
        body,
        format!(
            "Add CSV export\nQuote commas too.\n\n<!-- theodore-space:{id} -->\n\
             <sub>From a Theodore space: theodore://space/{id}</sub>",
            id = s.id
        )
    );
    // No template: the agent ran the turn only.
    assert_eq!(f.agents.requests().len(), 1);
}

// ── Merge from the space ──────────────────────────────────────────────────

#[tokio::test]
async fn merge_from_the_space_uses_the_hosts_default_method() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let s = in_review(&f, &core, &project, "Retry on 429").await;
    f.host
        .push_merge_status(Ok(status(MergeState::Clean, Some(true))));
    let r = core
        .merge_space(MergeSpaceRequest {
            space: s.id.clone(),
            method: None,
        })
        .await
        .unwrap();
    assert_eq!(r.method, WireMergeMethod::Squash);
    assert!(!r.auto && !r.queue);
    wait_until(|| !f.host.merge_calls().is_empty()).await;
    assert_eq!(f.host.merge_calls()[0].0, MergeMethod::Squash);
}

#[tokio::test]
async fn a_blocked_pr_auto_merges_when_the_repo_allows_it() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let s = in_review(&f, &core, &project, "Retry on 429").await;
    let mut blocked = status(MergeState::Blocked, Some(true));
    blocked.auto_merge_allowed = true;
    f.host.push_merge_status(Ok(blocked));
    let r = core
        .merge_space(MergeSpaceRequest {
            space: s.id.clone(),
            method: Some(WireMergeMethod::Merge),
        })
        .await
        .unwrap();
    assert_eq!(r.method, WireMergeMethod::Merge);
    assert!(r.auto);
    let item = f
        .store
        .outbox_snapshot()
        .into_iter()
        .find(|i| i.op == "merge_pr")
        .expect("a merge_pr item");
    assert_eq!(item.payload["auto"], json!(true));

    // Without auto-merge it is refused, and nothing is queued.
    f.host
        .push_merge_status(Ok(status(MergeState::Blocked, Some(true))));
    let err = core
        .merge_space(MergeSpaceRequest {
            space: s.id.clone(),
            method: None,
        })
        .await
        .unwrap_err();
    assert!(
        matches!(&err, AppError::Invalid(m) if m.contains("auto-merge")),
        "{err}"
    );
}

#[tokio::test]
async fn merge_is_refused_with_turns_not_pushed_or_without_a_pr() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let s = one_turn(&f, &core, &project, "Retry on 429").await;
    let err = core
        .merge_space(MergeSpaceRequest {
            space: s.id.clone(),
            method: None,
        })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no PR"), "{err}");

    core.ship_space(open_pr(&s)).await.unwrap();
    wait_until(|| space(&f, &s.id).pr.is_some()).await;
    let s = send(&f, &core, &space(&f, &s.id), "more", Some("turn222")).await;
    let err = core
        .merge_space(MergeSpaceRequest {
            space: s.id.clone(),
            method: None,
        })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("aren't pushed"), "{err}");
    assert!(f.host.merge_calls().is_empty());
}

#[tokio::test]
async fn an_adopted_prs_new_turns_must_be_pushed_before_it_merges() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let s = one_turn(&f, &core, &project, "Retry on 429").await;
    patch_adopted(&f, &s.id, "fork000", "github:acme/widget#1");
    let err = core
        .merge_space(MergeSpaceRequest {
            space: s.id.clone(),
            method: None,
        })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("aren't pushed"), "{err}");
    assert!(f.host.merge_calls().is_empty());
}

#[tokio::test]
async fn continue_keeps_an_adopted_prs_unpushed_turns_and_leaves_its_branch() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let s = one_turn(&f, &core, &project, "Retry on 429").await;
    patch_adopted(&f, &s.id, "fork000", "github:acme/widget#1");
    merge_on_host(&f, &core, 1).await;
    base_moved(&f, "tip999");

    let s = core.continue_space(&s.id).await.unwrap();
    assert_eq!(s.fork_sha, None, "the new branch isn't the adopted one");
    assert_ne!(
        s.head_sha.0, "tip999",
        "the turn nobody pushed is replayed, not dropped"
    );
}

// ── Checks while the page is open ─────────────────────────────────────────

/// The space page asks for its PR's checks (`refresh_space_pr`): a
/// conditional fetch that lands in `pr_detail`, so a failure shows without
/// opening the PR's review.
#[tokio::test]
async fn refresh_space_pr_brings_the_hosts_checks() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let s = in_review(&f, &core, &project, "Retry on 429").await;
    let pr = s.pr.clone().unwrap();
    let mut failed = sample_pr_detail(&pr_ref(100), &Sha::new("base000"), &Sha::new("squash1"));
    failed.checks = vec![theodore_domain::Check {
        id: "c1".into(),
        name: "test".into(),
        status: theodore_domain::CheckStatus::Fail,
        url: None,
        duration_ms: None,
    }];
    f.host.push_get_pr(Ok(theodore_ports::Fetched::Modified {
        etag: Some("e2".into()),
        value: failed,
    }));
    core.refresh_space_pr(&s.id).await.unwrap();
    let detail = f.store.get(models::PR_DETAIL, &pr.0).unwrap().unwrap().data;
    assert_eq!(detail["checks"][0]["status"], json!("fail"));

    // Nothing to ask about without a PR.
    let other = one_turn(&f, &core, &project, "Something else").await;
    core.refresh_space_pr(&other.id).await.unwrap();
}

// ── Continue after merge ──────────────────────────────────────────────────

async fn send_reply(
    f: &Fixtures,
    core: &Core,
    s: &Space,
    text: &str,
    reply: &str,
    head: &str,
) -> Space {
    f.agents.push_script(done(reply));
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

/// The round from the report: a workflow edit made and undone within it,
/// a note added. Only the note is in the net diff.
async fn a_round_with_an_undone_edit(f: &Fixtures, core: &Core, project: &Project) -> Space {
    let s = in_review(f, core, project, "Initialize a tiny static tamagotchi").await;
    merge_on_host(f, core, 100).await;
    base_moved(f, "main222");
    let mut c = core.continue_space(&s.id).await.unwrap();
    for (reply, head) in [
        (
            "Added # qa3 note as the first line of .github/workflows/test.yml",
            "turn222",
        ),
        ("I created notes/t3.md with the round's notes.", "turn333"),
        (
            ".github/workflows/test.yml now matches its original content, with the # qa3 \
             line removed so the workflow runs exactly as it did before.",
            "turn444",
        ),
    ] {
        f.clock.advance(1_000);
        c = send_reply(f, core, &c, "Yes, do that", reply, head).await;
    }
    let (base, head) = (Sha::new("main222"), Sha::new("turn444"));
    f.git.set_revision(crate::testkit::two_hunk_revision(
        &base,
        &head,
        "notes/t3.md",
    ));
    f.host.push_get_pr(Ok(theodore_ports::Fetched::Modified {
        etag: None,
        value: sample_pr_detail(&pr_ref(101), &base, &Sha::new("squash2")),
    }));
    c
}

fn is_ship_title(r: &theodore_ports::AgentRequest) -> bool {
    r.system.as_deref() == Some(crate::usecases::ship_title::SHIP_TITLE_SYSTEM)
}

/// After a Continue, the next PR is titled by its net change — never by a
/// sentence copied from the chat, never by an edit the round undid. With
/// no usable proposal (the chat agent's "ok"): the changed path.
#[tokio::test]
async fn the_pr_after_a_continue_is_titled_by_its_net_change() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let c = a_round_with_an_undone_edit(&f, &core, &project).await;
    core.ship_space(open_pr(&c)).await.unwrap();
    wait_until(|| f.host.create_pr_calls.lock().unwrap().len() == 2).await;
    let calls = f.host.create_pr_calls.lock().unwrap().clone();
    let (title, body) = (&calls[1].1.title, &calls[1].1.body);
    assert_eq!(title, "Update notes/t3.md");
    for chat in ["Added #", "I created", "workflows", "tamagotchi"] {
        assert!(!body.contains(chat), "{chat}: {body}");
    }
    let ship = f.git.ship.lock().unwrap();
    assert_eq!(ship.commits.last().unwrap().1.message, "Update notes/t3.md");
}

/// The chat agent's proposal from the round's diff stat and replies: its
/// title, and its whole sentences about files in the net diff only.
#[tokio::test]
async fn the_pr_after_a_continue_takes_the_chat_agents_proposed_title() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let c = a_round_with_an_undone_edit(&f, &core, &project).await;
    f.agents.push_tagged(
        is_ship_title,
        done(
            "TITLE: Add the round's t3 notes\nSUMMARY:\n\
             - Add notes/t3.md with what the round found.\n\
             - Restore .github/workflows/test.yml to its original content.",
        ),
    );
    core.ship_space(open_pr(&c)).await.unwrap();
    wait_until(|| f.host.create_pr_calls.lock().unwrap().len() == 2).await;
    let calls = f.host.create_pr_calls.lock().unwrap().clone();
    let (title, body) = (&calls[1].1.title, &calls[1].1.body);
    assert_eq!(title, "Add the round's t3 notes");
    assert!(
        body.contains("- Add notes/t3.md with what the round found."),
        "{body}"
    );
    assert!(!body.contains("workflows"), "{body}");
    let ask = f
        .agents
        .utility_requests()
        .into_iter()
        .find(is_ship_title)
        .expect("asked for a title");
    assert!(ask.prompt.contains("- notes/t3.md (+"), "{}", ask.prompt);
    assert!(
        ask.prompt.contains("I created notes/t3.md"),
        "{}",
        ask.prompt
    );
    assert!(!ask.prompt.contains("Yes, do that"), "{}", ask.prompt);
    let ship = f.git.ship.lock().unwrap();
    assert_eq!(
        ship.commits.last().unwrap().1.message,
        "Add the round's t3 notes\n\n- Add notes/t3.md with what the round found."
    );
}

/// `s` as if started from host issue `n` (`Space::issue`).
fn patch_issue(f: &Fixtures, id: &str, n: u64) {
    f.store
        .apply(vec![NewAction {
            model: models::SPACE.into(),
            model_id: id.to_string(),
            action: SyncActionCode::C,
            data: Some(json!({ "issue": n })),
        }])
        .unwrap();
}

fn pr_bodies(f: &Fixtures) -> Vec<String> {
    let calls = f.host.create_pr_calls.lock().unwrap();
    calls.iter().map(|c| c.1.body.clone()).collect()
}

/// The first round's PR closed issue #12 when it merged: the next round's
/// doesn't say "Closes #12" again (the host can't tell its state here).
#[tokio::test]
async fn closes_is_not_repeated_after_a_continue() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let s = one_turn(&f, &core, &project, "Fix the login loop").await;
    patch_issue(&f, &s.id, 12);
    f.host.push_get_pr(Ok(theodore_ports::Fetched::Modified {
        etag: None,
        value: sample_pr_detail(&pr_ref(100), &Sha::new("base000"), &Sha::new("squash1")),
    }));
    core.ship_space(open_pr(&space(&f, &s.id))).await.unwrap();
    wait_until(|| space(&f, &s.id).pr.is_some()).await;
    assert!(
        pr_bodies(&f)[0].contains("Closes #12"),
        "{:?}",
        pr_bodies(&f)
    );

    merge_on_host(&f, &core, 100).await;
    base_moved(&f, "main222");
    let c = core.continue_space(&s.id).await.unwrap();
    f.clock.advance(1_000);
    let c = send(&f, &core, &c, "One more thing", Some("turn222")).await;
    f.host.push_get_pr(Ok(theodore_ports::Fetched::Modified {
        etag: None,
        value: sample_pr_detail(&pr_ref(101), &Sha::new("main222"), &Sha::new("squash2")),
    }));
    core.ship_space(open_pr(&c)).await.unwrap();
    wait_until(|| f.host.create_pr_calls.lock().unwrap().len() == 2).await;
    assert!(
        !pr_bodies(&f)[1].contains("Closes #12"),
        "{:?}",
        pr_bodies(&f)
    );
}

/// An issue the host says is closed already gets no "Closes #n".
#[tokio::test]
async fn closes_is_left_out_for_an_issue_the_host_says_is_closed() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let s = one_turn(&f, &core, &project, "Fix the login loop").await;
    patch_issue(&f, &s.id, 12);
    f.host.push_get_issue(Ok(theodore_ports::TrackedIssue {
        number: 12,
        title: "Login loops".into(),
        body: String::new(),
        url: "https://example.test/12".into(),
        closed: true,
    }));
    core.ship_space(open_pr(&space(&f, &s.id))).await.unwrap();
    wait_until(|| !f.host.create_pr_calls.lock().unwrap().is_empty()).await;
    assert!(
        !pr_bodies(&f)[0].contains("Closes #12"),
        "{:?}",
        pr_bodies(&f)
    );
}

#[tokio::test]
async fn continue_moves_the_space_to_a_new_branch_on_the_fresh_base() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let s = in_review(&f, &core, &project, "Retry on 429").await;
    let err = core.continue_space(&s.id).await.unwrap_err();
    assert!(err.to_string().contains("hasn't merged"), "{err}");

    merge_on_host(&f, &core, 100).await;
    assert!(space(&f, &s.id).done_at.is_some());
    base_moved(&f, "main222");

    let c = core.continue_space(&s.id).await.unwrap();
    assert_eq!(c.branch, "me/retry-on-429-2");
    assert_eq!(c.base, "main");
    assert_eq!(c.base_sha, Sha::new("main222"));
    assert_eq!(c.head_sha, Sha::new("main222"), "the merged work is in it");
    assert_eq!(
        f.git.resets.lock().unwrap().last(),
        Some(&(s.id.clone(), Sha::new("main222")))
    );
    assert!(c.pr.is_none() && c.shipped.is_none() && c.done_at.is_none());
    assert_eq!(c.status, SpaceStatus::Idle);
    assert_eq!(c.turns, 1, "turns stay");
    assert_eq!(c.rounds.len(), 1);
    assert_eq!(c.rounds[0].pr.0, "github:acme/widget#100");
    assert_eq!(c.rounds[0].branch, "me/retry-on-429");
    assert_eq!(c.rounds[0].head_sha, Sha::new("turn111"));
    // Only the replay commit would be new; nothing was replayed.
    assert_eq!(f.git.ship.lock().unwrap().commits.len(), 1);

    // The next turn hears it once.
    let c = send(&f, &core, &c, "Now the 503s", None).await;
    let prompt = f.agents.last_request().unwrap().prompt;
    assert!(
        prompt.starts_with(
            "Note: the previous pull request (#100, branch me/retry-on-429) was merged into main."
        ),
        "{prompt}"
    );
    assert!(prompt.contains("me/retry-on-429-2"));
    assert!(c.continue_note.is_none());
    send(&f, &core, &c, "And 502s", None).await;
    let prompt = f.agents.last_request().unwrap().prompt;
    assert!(
        !prompt.contains("Note: the previous pull request"),
        "{prompt}"
    );
    assert!(
        f.agents
            .last_request()
            .unwrap()
            .system
            .unwrap()
            .contains("me/retry-on-429-2"),
        "the system prompt names the new branch"
    );
}

/// Continue moves the worktree onto the new base: the files that moved
/// are reindexed.
#[tokio::test]
async fn continue_reindexes_the_files_it_moved() {
    use crate::code_intel_tests::{diffs_touch, reindexed};
    let f = fixtures();
    let (core, _recv, project) = host_project_with(&f, f.deps_with_code()).await;
    let s = in_review(&f, &core, &project, "Retry on 429").await;
    let root = s.worktree.clone();
    wait_until(|| {
        f.code_intel
            .calls()
            .iter()
            .any(|c| c.starts_with(&format!("open {root}")))
    })
    .await;
    merge_on_host(&f, &core, 100).await;
    base_moved(&f, "main222");
    // Whatever the turn reindexed has settled.
    tokio::time::sleep(Duration::from_millis(50)).await;
    diffs_touch(&f, "src/continued.ts");
    core.continue_space(&s.id).await.unwrap();
    wait_until(|| reindexed(&f, &root, "src/continued.ts") == 1).await;
}

#[tokio::test]
async fn a_continued_space_never_auto_archives_for_its_old_pr() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let s = in_review(&f, &core, &project, "Retry on 429").await;
    merge_on_host(&f, &core, 100).await;
    base_moved(&f, "main222");
    core.continue_space(&s.id).await.unwrap();

    f.clock.advance(30 * DAY);
    core.sweep_spaces().await;
    let s = space(&f, &s.id);
    assert_eq!(s.status, SpaceStatus::Idle);
    assert!(s.done_at.is_none());
}

#[tokio::test]
async fn turns_after_the_last_push_come_along_onto_the_new_base() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let s = in_review(&f, &core, &project, "Retry on 429").await;
    let s = send(&f, &core, &s, "more", Some("turn222")).await;
    merge_on_host(&f, &core, 100).await;
    base_moved(&f, "main222");

    let c = core.continue_space(&s.id).await.unwrap();
    let replay = f.git.ship.lock().unwrap().commits[1].1.clone();
    assert_eq!(replay.parent, Sha::new("main222"));
    assert_eq!(replay.replay_from, Some(Sha::new("turn111")));
    assert_eq!(replay.head, Sha::new("turn222"));
    assert_eq!(c.head_sha, Sha::new("squash2"));
    assert_eq!(
        f.git.resets.lock().unwrap().last(),
        Some(&(s.id.clone(), Sha::new("squash2")))
    );
}

#[tokio::test]
async fn a_second_round_counts_on_and_skips_taken_names() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let s = in_review(&f, &core, &project, "Retry on 429").await;
    merge_on_host(&f, &core, 100).await;
    base_moved(&f, "main222");
    let c = core.continue_space(&s.id).await.unwrap();
    assert_eq!(c.branch, "me/retry-on-429-2");

    // Round two: a turn, a PR, merged; `-3` is taken on the host.
    let c = send(&f, &core, &c, "503s", Some("turn333")).await;
    f.host.push_get_pr(Ok(theodore_ports::Fetched::Modified {
        etag: None,
        value: sample_pr_detail(&pr_ref(101), &Sha::new("main222"), &Sha::new("squash2")),
    }));
    core.ship_space(open_pr(&c)).await.unwrap();
    wait_until(|| space(&f, &c.id).pr.is_some()).await;
    merge_on_host(&f, &core, 101).await;
    {
        let mut ship = f.git.ship.lock().unwrap();
        ship.remote
            .insert("me/retry-on-429-3".into(), Sha::new("someone"));
    }
    base_moved(&f, "main333");
    let c = core.continue_space(&c.id).await.unwrap();
    assert_eq!(c.branch, "me/retry-on-429-4");
    assert_eq!(c.rounds.len(), 2);
    assert_eq!(c.rounds[1].branch, "me/retry-on-429-2");
}

#[tokio::test]
async fn an_archived_space_must_be_unarchived_before_it_continues() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let s = in_review(&f, &core, &project, "Retry on 429").await;
    merge_on_host(&f, &core, 100).await;
    core.archive_space(&s.id).await.unwrap();
    let err = core.continue_space(&s.id).await.unwrap_err();
    assert!(err.to_string().contains("unarchive"), "{err}");
}

#[tokio::test]
async fn an_update_from_base_still_merging_must_finish_before_it_continues() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let s = in_review(&f, &core, &project, "Retry on 429").await;
    // An update started while in review, still resolving when the PR
    // merges on the host.
    base_moved(&f, "main222");
    f.git.set_resolve("main", Sha::new("main222"));
    f.git
        .push_merge_result(SpaceMerge::Conflicts(vec!["a.txt".into()]));
    f.git
        .push_merge_result(SpaceMerge::Conflicts(vec!["a.txt".into()]));
    f.agents.push_script(done("Tried."));
    core.update_space_from_base(&s.id).await.unwrap();
    let s = idle(&f, &s.id).await;
    assert!(s.updating.is_some(), "still resolving");
    merge_on_host(&f, &core, 100).await;

    let err = core.continue_space(&s.id).await.unwrap_err();
    assert!(err.to_string().contains("update from the base"), "{err}");
    assert_eq!(space(&f, &s.id).branch, s.branch, "nothing moved");
}

#[tokio::test]
async fn a_merged_space_continues_rather_than_updating_from_base() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let s = in_review(&f, &core, &project, "Retry on 429").await;
    merge_on_host(&f, &core, 100).await;
    let err = core.update_space_from_base(&s.id).await.unwrap_err();
    assert!(err.to_string().contains("Continue"), "{err}");
}

/// Continue moves the worktree onto the base's tip without a turn: the same
/// stop as a clean update.
#[tokio::test]
async fn back_to_here_stops_at_continue() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    let s = in_review(&f, &core, &project, "Retry on 429").await;
    let send = |text: &str| theodore_contract::SpaceSendRequest {
        space: s.id.clone(),
        text: text.into(),
        agent: None,
        model: None,
        mode: None,
        attachments: None,
    };
    f.agents.push_script(done("done"));
    f.git.push_checkpoint(Sha::new("turn222"));
    core.space_send(send("not pushed")).await.unwrap();
    idle(&f, &s.id).await;
    merge_on_host(&f, &core, 100).await;
    base_moved(&f, "main222");
    core.continue_space(&s.id).await.unwrap();
    f.agents.push_script(done("done"));
    f.git.push_checkpoint(Sha::new("turn333"));
    core.space_send(send("next round")).await.unwrap();
    idle(&f, &s.id).await;
    let s = space(&f, &s.id);

    // Right after the first turn: across Continue.
    let err = core
        .rewind_space(theodore_contract::RewindSpaceRequest {
            space: s.id.clone(),
            reply: s.turn_log[0].reply.clone(),
        })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("can't go back past"), "{err}");
    assert_eq!(space(&f, &s.id).head_sha, Sha::new("turn333"));
}

// ── Open PR interrupted ───────────────────────────────────────────────────

/// Theodore quits while the agent fills the PR template: the branch is on
/// the host, the outbox item isn't written yet.
#[tokio::test]
async fn open_pr_interrupted_during_the_fill_can_be_retried() {
    let f = fixtures();
    let (core, _recv, project) = host_project(&f).await;
    f.git.put_file(
        &Sha::new("turn111"),
        ".github/pull_request_template.md",
        TEMPLATE.as_bytes(),
    );
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
    let s = idle(&f, &s.id).await;

    f.agents.hang_next_utility();
    let interrupted = {
        let (core, req) = (core.clone(), open_pr(&s));
        tokio::spawn(async move { core.ship_space(req).await })
    };
    wait_until(|| !f.git.ship.lock().unwrap().pushes.is_empty()).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    interrupted.abort();
    let _ = interrupted.await;
    assert!(space(&f, &s.id).pr.is_none());

    core.ship_space(open_pr(&s))
        .await
        .expect("a retry after an interrupted Open PR is safe");
    wait_until(|| space(&f, &s.id).pr.is_some()).await;
    let ship = f.git.ship.lock().unwrap();
    assert_eq!(ship.commits.len(), 1, "the same squashed commit");
    assert_eq!(ship.pushes.len(), 1, "already on the host");
    assert_eq!(f.host.create_pr_calls.lock().unwrap().len(), 1);
}
