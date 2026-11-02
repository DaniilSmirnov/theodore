//! The agent's change tools (docs/spaces.md "Tools the agent gets")
//! against `Core` and the testkit fakes: `ship` from the running turn,
//! the diff with uncommitted edits, review comments from both reviews, a
//! comment as a finding, a new base, and the progress tracker.

use serde_json::json;
use theodore_contract::{SpaceProgressState, SpaceSendRequest, models};
use theodore_domain::{
    AgentId, Author, Comment, CommentId, Draft, DraftId, Finding, FindingId, FindingState, PrId,
    Severity, Sha, SkillId, Thread, ThreadId,
};
use theodore_ports::{
    NewAction, NewReviewComment, ProgressStep, ProgressStepState, ReviewItemKind, ReviewItemState,
    ReviewSource, ShipToolAction, SpaceToolError, Store, SyncActionCode,
};

use crate::space_commands_tests::{Env, done, start_env};
use crate::testkit::{sample_anchor, sample_person, sample_pr_detail, sample_pr_ref};

async fn patch(e: &Env, id: &str, data: serde_json::Value) {
    e.core
        .apply(vec![NewAction {
            model: models::SPACE.into(),
            model_id: id.into(),
            action: SyncActionCode::C,
            data: Some(data),
        }])
        .await
        .unwrap();
}

/// The space as if turn `run` were in flight.
async fn running(e: &Env, id: &str, run: &str) {
    patch(e, id, json!({ "status": "running", "run": run })).await;
}

fn put(e: &Env, model: &str, id: &str, data: serde_json::Value) {
    e.f.store
        .apply(vec![NewAction {
            model: model.into(),
            model_id: id.into(),
            action: SyncActionCode::I,
            data: Some(data),
        }])
        .unwrap();
}

fn text(e: SpaceToolError) -> String {
    e.to_string()
}

#[tokio::test]
async fn ship_runs_from_the_agents_own_turn_only() {
    let e = start_env(false).await;
    let s = e.create("").await;
    e.f.git.ship.lock().unwrap().identity = Some(theodore_ports::Identity {
        name: "Ada".into(),
        email: "ada@example.com".into(),
    });
    e.f.git.set_working_state(theodore_ports::WorkingState {
        current_branch: Some("main".into()),
        dirty_files: 0,
    });
    e.f.git.push_checkpoint(Sha::new("turn111"));
    running(&e, &s.id, "space:t:1").await;

    // Another run's call: the turn holds the space.
    let err = e
        .core
        .space_tool_ship(&s.id, "space:t:other", ShipToolAction::MergeLocally, None)
        .await
        .unwrap_err();
    assert!(text(err).contains("still working"));
    // A host action in a local folder: refused, nothing pushed.
    let err = e
        .core
        .space_tool_ship(&s.id, "space:t:1", ShipToolAction::OpenPr, None)
        .await
        .unwrap_err();
    assert_eq!(
        text(err),
        "a local folder has no host to open a PR on; merge locally or keep it as a branch"
    );

    let out = e
        .core
        .space_tool_ship(
            &s.id,
            "space:t:1",
            ShipToolAction::MergeLocally,
            Some("Fix the login".into()),
        )
        .await
        .unwrap();
    assert_eq!(out.branch, "main");
    assert_eq!(out.sha, "squash1");
    assert_eq!(out.summary, "Merged into main in the local folder");
    assert_eq!(out.pr_number, None);
    {
        let ship = e.f.git.ship.lock().unwrap();
        assert_eq!(ship.merges.len(), 1);
        assert!(ship.commits[0].1.message.starts_with("Fix the login"));
        assert!(ship.pushes.is_empty(), "never a push from a folder");
    }

    let err = e
        .core
        .space_tool_ship(&s.id, "space:t:1", ShipToolAction::MergeLocally, None)
        .await
        .unwrap_err();
    assert!(text(err).contains("no new turns"), "nothing new to merge");
}

#[tokio::test]
async fn the_diff_includes_uncommitted_edits_and_filters_by_file() {
    let e = start_env(false).await;
    let s = e.create("").await;
    let mut rev = crate::testkit::two_hunk_revision(&s.base_sha, &Sha::new("x"), "src/a.rs");
    rev.files
        .extend(crate::testkit::two_hunk_revision(&s.base_sha, &Sha::new("x"), "src/b.rs").files);
    e.f.git.set_revision(rev);

    // Clean worktree: the checkpoint itself.
    let clean = e.core.space_tool_diff(&s.id, None).await.unwrap();
    assert!(!clean.uncommitted);
    assert_eq!(clean.head, s.head_sha);
    assert_eq!(clean.files.len(), 2);

    e.f.git.ship.lock().unwrap().snapshot = Some(Sha::new("snap1"));
    let live = e
        .core
        .space_tool_diff(&s.id, Some("src/b.rs"))
        .await
        .unwrap();
    assert!(live.uncommitted);
    assert_eq!(live.base, "main");
    assert_eq!(live.head, Sha::new("snap1"));
    let paths: Vec<&str> = live.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, ["src/b.rs"]);
    let calls = e.f.git.ship.lock().unwrap().diff_calls.clone();
    assert_eq!(
        calls.last().unwrap(),
        &(s.base_sha.clone(), Sha::new("snap1")),
        "against the fork point"
    );
}

#[tokio::test]
async fn review_comments_cover_both_reviews_and_a_comment_becomes_a_finding() {
    let e = start_env(false).await;
    let s = e.create("").await;
    let review = s.review_pr.clone().expect("branch review");
    let pr_ref = sample_pr_ref("acme", "widget", 12);
    let pr: PrId = pr_ref.id();
    patch(&e, &s.id, json!({ "pr": pr })).await;

    let draft = Draft {
        id: DraftId::new("d1"),
        pr: review.clone(),
        anchor: sample_anchor("src/a.rs", 3),
        body: "rename this".into(),
        source: Author::Me,
        outdated: false,
    };
    put(
        &e,
        models::DRAFT,
        "d1",
        serde_json::to_value(&draft).unwrap(),
    );
    let ignored = Finding {
        id: FindingId::new("f-ignored"),
        pr: review.clone(),
        skill: SkillId("security".into()),
        agents: vec![AgentId::Codex],
        anchor: sample_anchor("src/a.rs", 9),
        severity: Severity::Info,
        title: "nit".into(),
        body: "nit".into(),
        state: FindingState::Ignored,
        fix: None,
        outdated: false,
    };
    put(
        &e,
        models::FINDING,
        "f-ignored",
        serde_json::to_value(&ignored).unwrap(),
    );
    let mut detail = sample_pr_detail(&pr_ref, &s.base_sha, &s.head_sha);
    detail.threads = vec![Thread {
        id: ThreadId::new("t1"),
        anchor: Some(sample_anchor("src/b.rs", 7)),
        resolved: true,
        outdated: false,
        comments: vec![Comment {
            id: CommentId::new("c1"),
            author: sample_person("anna"),
            body: "Per-token?".into(),
            created_at: 0,
            edited: false,
            reactions: vec![],
        }],
    }];
    put(
        &e,
        models::PR_DETAIL,
        &pr.0,
        serde_json::to_value(&detail).unwrap(),
    );

    let id = e
        .core
        .space_tool_comment(
            &s.id,
            AgentId::Claude,
            NewReviewComment {
                file: "./src/a.rs".into(),
                line: 5,
                body: "This drops the error\n\nReturn it instead.".into(),
                severity: Severity::Warn,
            },
        )
        .await
        .unwrap();
    let stored: Finding = serde_json::from_value(
        e.f.store
            .get(models::FINDING, &id.0)
            .unwrap()
            .expect("stored")
            .data,
    )
    .unwrap();
    assert_eq!(stored.pr, review, "on the space's own review");
    assert_eq!(stored.anchor.path, "src/a.rs");
    assert_eq!(stored.anchor.line, 5);
    assert_eq!(stored.anchor.rev, s.head_sha);
    assert_eq!(stored.title, "This drops the error");
    assert_eq!(stored.severity, Severity::Warn);

    let items = e.core.space_tool_comments(&s.id, None).await.unwrap();
    let seen: Vec<(ReviewItemKind, ReviewSource, &str, ReviewItemState, &str)> = items
        .iter()
        .map(|i| {
            (
                i.kind,
                i.on,
                i.location.as_str(),
                i.state,
                i.comments[0].author.as_str(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            (
                ReviewItemKind::Draft,
                ReviewSource::Space,
                "src/a.rs:3",
                ReviewItemState::Draft,
                "you"
            ),
            (
                ReviewItemKind::Finding,
                ReviewSource::Space,
                "src/a.rs:5",
                ReviewItemState::Open,
                "claude"
            ),
            (
                ReviewItemKind::Thread,
                ReviewSource::Pr,
                "src/b.rs:7",
                ReviewItemState::Resolved,
                "anna"
            ),
        ]
    );
    let only_b = e
        .core
        .space_tool_comments(&s.id, Some("src/b.rs"))
        .await
        .unwrap();
    assert_eq!(only_b.len(), 1);

    let err = e
        .core
        .space_tool_comment(
            &s.id,
            AgentId::Claude,
            NewReviewComment {
                file: "src/a.rs".into(),
                line: 1,
                body: "  ".into(),
                severity: Severity::Info,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(text(err), "the comment is empty");
}

#[tokio::test]
async fn set_base_moves_the_fork_point_and_refuses_what_it_cant() {
    let e = start_env(false).await;
    let s = e.create("").await;
    e.f.git
        .set_resolve("refs/heads/release", Sha::new("rel000"));
    e.f.git.set_resolve("rel000", Sha::new("rel000"));
    e.f.git.set_resolve("refs/heads/main", Sha::new("base000"));
    running(&e, &s.id, "space:t:1").await;

    let err = e
        .core
        .space_tool_set_base(&s.id, "space:t:1", "nope")
        .await
        .unwrap_err();
    assert_eq!(text(err), "there is no branch nope in the folder");
    let err = e
        .core
        .space_tool_set_base(&s.id, "space:t:1", &s.branch)
        .await
        .unwrap_err();
    assert!(text(err).contains("own branch"));
    let err = e
        .core
        .space_tool_set_base(&s.id, "space:t:2", "release")
        .await
        .unwrap_err();
    assert!(text(err).contains("another turn"));

    let change = e
        .core
        .space_tool_set_base(&s.id, "space:t:1", "release")
        .await
        .unwrap();
    assert_eq!(change.base, "release");
    assert_eq!(change.previous, "main");
    assert_eq!(change.base_sha, "rel000");
    assert_eq!(change.note, None);
    let row = e.space(&s.id);
    assert_eq!(row.base, "release");
    assert_eq!(row.base_sha, Sha::new("rel000"));

    // With a PR open: changed here, and said that the PR isn't.
    patch(&e, &s.id, json!({ "pr": "github:acme/widget#12" })).await;
    let change = e
        .core
        .space_tool_set_base(&s.id, "space:t:1", "main")
        .await
        .unwrap();
    assert_eq!(change.base, "main");
    let note = change.note.unwrap();
    assert!(note.contains("PR #12 still targets release"), "{note}");
}

#[tokio::test]
async fn progress_is_replaced_cleared_and_gone_after_the_turn() {
    let e = start_env(false).await;
    let s = e.create("").await;
    running(&e, &s.id, "space:t:1").await;
    let step = |label: &str, state| ProgressStep {
        label: label.into(),
        state,
    };
    e.core
        .space_tool_progress(
            &s.id,
            "space:t:1",
            vec![
                step("Read the code", ProgressStepState::Done),
                step("Fix it", ProgressStepState::Active),
                step(" ", ProgressStepState::Pending),
                step("Run the check", ProgressStepState::Pending),
            ],
            Some("halfway".into()),
        )
        .await
        .unwrap();
    let p = e.space(&s.id).progress.expect("progress");
    let labels: Vec<(&str, SpaceProgressState)> = p
        .steps
        .iter()
        .map(|s| (s.label.as_str(), s.state))
        .collect();
    assert_eq!(
        labels,
        [
            ("Read the code", SpaceProgressState::Done),
            ("Fix it", SpaceProgressState::Active),
            ("Run the check", SpaceProgressState::Pending),
        ]
    );
    assert_eq!(p.note.as_deref(), Some("halfway"));

    e.core
        .space_tool_progress(&s.id, "space:t:1", vec![], None)
        .await
        .unwrap();
    assert!(e.space(&s.id).progress.is_none(), "no steps clears it");

    let err = e
        .core
        .space_tool_progress(
            &s.id,
            "space:t:old",
            vec![step("x", ProgressStepState::Active)],
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(text(err), "this turn is over");

    // A turn's end clears what it left.
    e.core
        .space_tool_progress(
            &s.id,
            "space:t:1",
            vec![step("Fix it", ProgressStepState::Active)],
            None,
        )
        .await
        .unwrap();
    patch(&e, &s.id, json!({ "status": "idle", "run": null })).await;
    assert!(e.space(&s.id).progress.is_some());
    e.f.agents.push_script(done("ok"));
    e.core
        .space_send(SpaceSendRequest {
            space: s.id.clone(),
            text: "go".into(),
            agent: None,
            model: None,
            mode: None,
            attachments: None,
        })
        .await
        .unwrap();
    let after = e.until(&s.id, |s| s.turns == 1 && s.run.is_none()).await;
    assert!(after.progress.is_none());
}
