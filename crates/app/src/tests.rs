//! Integration-style tests against `Core` + `testkit` fakes, per the
//! coverage list in the agent brief for this slice.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use theodore_contract::{
    AddProjectRequest, BlobResponse, BootstrapScope, CommitSuggestionsRequest,
    ConflictKind as WireConflictKind, DelegateRequest, DeviceLogin, DiffSide, DraftStateRequest,
    EditCommentRequest, Ephemeral, FileViewedRow, GetBlobRequest, HostAuthStatus, HunkRead,
    InboxState, LoginState, MergeMethod as WireMergeMethod, MergeRequest, MutationOp,
    OpenBranchRequest, OpenPrRequest, ProjectStatus, ReactRequest, ReplyRequest, RerunCheckRequest,
    ResolveRequest, ReviewersRequest, RevisionRequest, RunSkillRequest, SaveSkillRequest,
    SetTokenRequest, Setting, SubmitReviewRequest, TaskDecision, ToastLevel, TokenSource,
    Transaction, TransactionResult, TriageAction, TriageRequest, models,
};
use theodore_domain::{
    AgentId, Anchor, Assignee, Check, CheckStatus, Comment, CommentId, FileDiff, FileStatus, Fix,
    FixStatus, HostKind, HostRepo, HunkId, Patch, PrId, PrRef, PrSummary, RepoRef, ReviewEvent,
    Revision, Sha, Side, Task, TaskId, TaskStatus, Thread, ThreadId, Usage, ViewedState,
};
use theodore_ports::{
    AgentEvent, BranchInfo, Clock, CommitLog, ConflictEntry, ConflictKind as PortConflictKind,
    DevicePoll, Fetched, GitError, HostError, MergeMethod as PortMergeMethod, MergeState,
    MergeStatus, NewAction, OutboxStatus, Secrets, Store, SyncActionCode,
};

use crate::testkit::{self, Fixtures, fixtures};
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

/// Waits for an `Ephemeral` matching `pred`, skipping any that don't.
async fn expect_ephemeral(
    recv: &mut Receivers,
    mut pred: impl FnMut(&Ephemeral) -> bool,
) -> Ephemeral {
    for _ in 0..200 {
        match tokio::time::timeout(Duration::from_millis(50), recv.ephemeral.recv()).await {
            Ok(Ok(event)) if pred(&event) => return event,
            _ => continue,
        }
    }
    panic!("expected ephemeral event not observed in time");
}

fn fixture_pr_ref() -> PrRef {
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

/// Opens the fixture PR and returns everything a test typically needs.
async fn open_fixture_pr(f: &Fixtures) -> (Core, Receivers, PrId, Sha) {
    let pr_ref = fixture_pr_ref();
    let head = Sha::new("head111");
    let base = Sha::new("base000");
    let detail = testkit::sample_pr_detail(&pr_ref, &base, &head);
    f.host.push_get_pr(Ok(Fetched::Modified {
        etag: Some("etag-1".into()),
        value: detail,
    }));

    let (core, recv) = Core::start(f.deps());
    let resp = core
        .open_pr(OpenPrRequest {
            url: "https://github.com/acme/widgets/pull/7".into(),
        })
        .await
        .expect("open_pr");
    (core, recv, resp.pr, resp.head)
}

/// Lets the grouping `open_pr` started finish: a second run for the same PR
/// is refused while one is in flight.
async fn wait_grouping_idle(f: &Fixtures, core: &Core, pr: &PrId) {
    wait_until(|| {
        f.store
            .get(models::GROUPING, &pr.0)
            .ok()
            .flatten()
            .is_some()
            && core.0.grouping_inflight.lock().unwrap().is_empty()
    })
    .await;
}

fn hunk_read_tx(id: &str, pr: &PrId, hunk: &HunkId) -> Transaction {
    Transaction {
        id: id.into(),
        ops: vec![MutationOp::Create {
            model: models::HUNK_READ.into(),
            id: format!("{}:{}", pr.0, hunk),
            data: serde_json::to_value(HunkRead {
                pr: pr.clone(),
                hunk: hunk.clone(),
            })
            .unwrap(),
        }],
    }
}

fn file_viewed_row(f: &Fixtures, pr: &PrId, path: &str) -> Option<FileViewedRow> {
    f.store
        .get(models::FILE_VIEWED, &format!("{}:{path}", pr.0))
        .ok()
        .flatten()
        .and_then(|r| serde_json::from_value(r.data).ok())
}

fn sample_task(
    id: &str,
    pr: &PrId,
    anchor: Option<theodore_domain::Anchor>,
    thread: Option<ThreadId>,
    now: i64,
) -> Task {
    Task {
        id: TaskId::new(id),
        pr: pr.clone(),
        title: "Fix the thing".into(),
        body: "Do the fix, please.".into(),
        anchor,
        thread,
        status: TaskStatus::Todo,
        assignee: Assignee::Human("author".into()),
        fix: None,
        created_at: now,
        outdated: false,
    }
}

fn put_task(f: &Fixtures, task: &Task) {
    f.store
        .apply(vec![NewAction {
            model: models::TASK.into(),
            model_id: task.id.0.clone(),
            action: SyncActionCode::I,
            data: Some(serde_json::to_value(task).unwrap()),
        }])
        .unwrap();
}

fn task_row(f: &Fixtures, id: &TaskId) -> Task {
    let row = f.store.get(models::TASK, &id.0).unwrap().unwrap();
    serde_json::from_value(row.data).unwrap()
}

// ── transact / sync frame / de-dup / rejection ──────────────────────────────

#[tokio::test]
async fn transact_produces_a_contiguous_sync_frame() {
    let f = fixtures();
    let (core, mut recv) = Core::start(f.deps());

    let tx = Transaction {
        id: "tx-1".into(),
        ops: vec![MutationOp::Create {
            model: models::SETTING.into(),
            id: "custom.key".into(),
            data: json!({"key": "custom.key", "scope": "user", "value": 1}),
        }],
    };
    let result = core.transact(tx).await;
    let TransactionResult::Ok { last_sync_id } = result else {
        panic!("expected Ok, got {result:?}")
    };
    assert!(last_sync_id >= 1);

    let frame = recv.sync.try_recv().expect("a sync frame was broadcast");
    assert_eq!(frame.last_sync_id, last_sync_id);
    assert_eq!(frame.actions.len(), 1);
    assert_eq!(frame.actions[0].model, models::SETTING);
}

#[tokio::test]
async fn transact_is_deduped_by_transaction_id() {
    let f = fixtures();
    let (core, _recv) = Core::start(f.deps());

    let tx = Transaction {
        id: "same-tx".into(),
        ops: vec![MutationOp::Create {
            model: models::SETTING.into(),
            id: "custom.key".into(),
            data: json!({"key": "custom.key", "scope": "user", "value": 1}),
        }],
    };
    let first = core.transact(tx.clone()).await;
    let second = core.transact(tx).await;
    assert_eq!(first, second);

    let frame = core.catch_up(0).await.expect("catch_up");
    assert_eq!(
        frame.actions.len(),
        1,
        "a retried transaction must not double-apply"
    );
}

#[tokio::test]
async fn transact_rejects_a_hunk_read_outside_the_current_revision() {
    let f = fixtures();
    let (core, _recv) = Core::start(f.deps());

    // No PR is open, so any hunk_read must be rejected.
    let tx = hunk_read_tx(
        "tx-reject",
        &PrId::new("github:acme/widgets#7"),
        &HunkId::new("deadbeef"),
    );
    let result = core.transact(tx).await;
    assert!(
        matches!(result, TransactionResult::Rejected { .. }),
        "expected Rejected, got {result:?}"
    );
}

fn single_op_tx(id: &str, op: MutationOp) -> Transaction {
    Transaction {
        id: id.into(),
        ops: vec![op],
    }
}

fn put_model_row(f: &Fixtures, model: &str, id: &str, data: serde_json::Value) {
    f.store
        .apply(vec![NewAction {
            model: model.into(),
            model_id: id.into(),
            action: SyncActionCode::I,
            data: Some(data),
        }])
        .unwrap();
}

#[tokio::test]
async fn transact_rejects_client_writes_to_core_owned_models() {
    let f = fixtures();
    let (core, _recv) = Core::start(f.deps());

    for model in [
        models::PR,
        models::PR_DETAIL,
        models::GROUPING,
        models::PROJECT,
        models::CHAT_MESSAGE,
    ] {
        let tx = single_op_tx(
            &format!("tx-core-owned-{model}"),
            MutationOp::Create {
                model: model.into(),
                id: "x".into(),
                data: json!({}),
            },
        );
        let result = core.transact(tx).await;
        assert!(
            matches!(result, TransactionResult::Rejected { .. }),
            "{model} create must be rejected, got {result:?}"
        );
    }
}

#[tokio::test]
async fn transact_allows_only_the_state_field_on_finding_updates() {
    let f = fixtures();
    let (core, _recv) = Core::start(f.deps());
    put_model_row(
        &f,
        models::FINDING,
        "f1",
        json!({
            "id": "f1", "pr": "github:acme/widgets#7", "skill": "s1",
            "agents": [], "anchor": {"path": "src/lib.rs", "side": "right", "line": 2},
            "severity": "warn", "title": "orig", "body": "orig",
            "state": "open", "fix": null,
        }),
    );

    let bad = core
        .transact(single_op_tx(
            "tx-finding-bad",
            MutationOp::Update {
                model: models::FINDING.into(),
                id: "f1".into(),
                data: json!({"title": "hacked"}),
            },
        ))
        .await;
    assert!(
        matches!(bad, TransactionResult::Rejected { .. }),
        "finding title must not be client-writable, got {bad:?}"
    );

    let good = core
        .transact(single_op_tx(
            "tx-finding-good",
            MutationOp::Update {
                model: models::FINDING.into(),
                id: "f1".into(),
                data: json!({"state": "ignored"}),
            },
        ))
        .await;
    assert!(
        matches!(good, TransactionResult::Ok { .. }),
        "finding state must stay client-writable, got {good:?}"
    );
}

#[tokio::test]
async fn transact_restricts_task_updates_to_status_and_assignee() {
    let f = fixtures();
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;
    let task = sample_task("t-allowlist", &pr, None, None, f.clock.now());
    put_task(&f, &task);

    let result = core
        .transact(single_op_tx(
            "tx-task-bad",
            MutationOp::Update {
                model: models::TASK.into(),
                id: task.id.0.clone(),
                data: json!({"title": "retitled"}),
            },
        ))
        .await;
    assert!(
        matches!(result, TransactionResult::Rejected { .. }),
        "task title must not be client-writable, got {result:?}"
    );
}

#[tokio::test]
async fn transact_rejects_a_new_task_that_does_not_start_in_todo() {
    let f = fixtures();
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;
    let mut task = sample_task("t-skip-todo", &pr, None, None, f.clock.now());
    task.status = TaskStatus::Done;

    let result = core
        .transact(single_op_tx(
            "tx-task-create-bad",
            MutationOp::Create {
                model: models::TASK.into(),
                id: task.id.0.clone(),
                data: serde_json::to_value(&task).unwrap(),
            },
        ))
        .await;
    assert!(
        matches!(result, TransactionResult::Rejected { .. }),
        "a client-created task must start in todo, got {result:?}"
    );
}

#[tokio::test]
async fn transact_allows_only_archiving_a_skill_run() {
    let f = fixtures();
    let (core, _recv) = Core::start(f.deps());
    put_model_row(
        &f,
        models::SKILL_RUN,
        "r1",
        json!({"id": "r1", "pr": "github:acme/widgets#7"}),
    );

    let update = core
        .transact(single_op_tx(
            "tx-skillrun-update",
            MutationOp::Update {
                model: models::SKILL_RUN.into(),
                id: "r1".into(),
                data: json!({"id": "r1"}),
            },
        ))
        .await;
    assert!(
        matches!(update, TransactionResult::Rejected { .. }),
        "skill_run must only be archivable by the client, got {update:?}"
    );

    let archive = core
        .transact(single_op_tx(
            "tx-skillrun-archive",
            MutationOp::Archive {
                model: models::SKILL_RUN.into(),
                id: "r1".into(),
            },
        ))
        .await;
    assert!(
        matches!(archive, TransactionResult::Ok { .. }),
        "archiving a skill_run must stay client-writable, got {archive:?}"
    );
}

#[tokio::test]
async fn transact_validates_draft_update_anchors_like_create() {
    let f = fixtures();
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;
    // Planted directly (bypassing the validated `create` path) to simulate a
    // draft whose anchor the diff has since outrun.
    put_model_row(
        &f,
        models::DRAFT,
        "d1",
        json!({
            "id": "d1", "pr": pr.0, "body": "orig",
            "anchor": {"path": "no/such/file.rs", "side": "right", "line": 1},
            "source": {"kind": "me"},
        }),
    );

    let result = core
        .transact(single_op_tx(
            "tx-draft-update-stale",
            MutationOp::Update {
                model: models::DRAFT.into(),
                id: "d1".into(),
                data: json!({"body": "edited"}),
            },
        ))
        .await;
    assert!(
        matches!(result, TransactionResult::Rejected { .. }),
        "a draft update must re-validate the anchor like create, got {result:?}"
    );
}

/// The UI's undo queue (`ui/src/sync/UndoQueue.ts`) never rolls back
/// locally: it resends the inverse of a committed op as a brand new
/// transaction (plan "sync" §UndoQueue doc comment) — same model and id,
/// same changed keys for an `update`, the old value in place of the new
/// one. The allowlist must accept that inverse exactly as readily as the
/// forward op, for every op shape `ui/src/actions.ts` actually sends.
#[tokio::test]
async fn transact_accepts_the_undo_inverse_of_every_client_op() {
    let f = fixtures();
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;

    // task: assignTask's `{assignee, status: "progress"}`, then undo's
    // `{assignee: <old>, status: "todo"}` — not a legal forward lifecycle
    // transition, but a legal undo.
    let mut task = sample_task("t-undo", &pr, None, None, f.clock.now());
    task.assignee = Assignee::Human("someone".into());
    put_task(&f, &task);

    let forward = core
        .transact(single_op_tx(
            "tx-assign-forward",
            MutationOp::Update {
                model: models::TASK.into(),
                id: task.id.0.clone(),
                data: json!({
                    "assignee": Assignee::Agent(AgentId::Claude),
                    "status": "progress",
                }),
            },
        ))
        .await;
    assert!(
        matches!(forward, TransactionResult::Ok { .. }),
        "assignTask's forward update must be accepted, got {forward:?}"
    );

    let inverse = core
        .transact(single_op_tx(
            "tx-assign-undo",
            MutationOp::Update {
                model: models::TASK.into(),
                id: task.id.0.clone(),
                data: json!({
                    "assignee": Assignee::Human("someone".to_string()),
                    "status": "todo",
                }),
            },
        ))
        .await;
    assert!(
        matches!(inverse, TransactionResult::Ok { .. }),
        "undoing assignTask must be accepted even though it isn't a forward transition, got {inverse:?}"
    );

    // inbox_state: markDone's `create`, then undo's `delete`.
    let create_inbox = core
        .transact(single_op_tx(
            "tx-inbox-create",
            MutationOp::Create {
                model: models::INBOX_STATE.into(),
                id: pr.0.clone(),
                data: json!({"pr": pr.0, "done": true, "snooze": null}),
            },
        ))
        .await;
    assert!(matches!(create_inbox, TransactionResult::Ok { .. }));
    let delete_inbox = core
        .transact(single_op_tx(
            "tx-inbox-undo",
            MutationOp::Delete {
                model: models::INBOX_STATE.into(),
                id: pr.0.clone(),
            },
        ))
        .await;
    assert!(
        matches!(delete_inbox, TransactionResult::Ok { .. }),
        "undoing markDone's create must be accepted, got {delete_inbox:?}"
    );

    // inbox_state: unsnooze's `update` with the old value restored.
    put_model_row(
        &f,
        models::INBOX_STATE,
        &pr.0,
        json!({"pr": pr.0, "done": false, "snooze": {"kind": "next_push"}}),
    );
    let snooze_forward = core
        .transact(single_op_tx(
            "tx-unsnooze-forward",
            MutationOp::Update {
                model: models::INBOX_STATE.into(),
                id: pr.0.clone(),
                data: json!({"snooze": null}),
            },
        ))
        .await;
    assert!(matches!(snooze_forward, TransactionResult::Ok { .. }));
    let snooze_undo = core
        .transact(single_op_tx(
            "tx-unsnooze-undo",
            MutationOp::Update {
                model: models::INBOX_STATE.into(),
                id: pr.0.clone(),
                data: json!({"snooze": {"kind": "next_push"}}),
            },
        ))
        .await;
    assert!(
        matches!(snooze_undo, TransactionResult::Ok { .. }),
        "undoing unsnooze must restore the old snooze value, got {snooze_undo:?}"
    );

    // draft: addDraft's `create`, then undo's `delete`.
    let anchor = testkit::sample_anchor("src/lib.rs", 2);
    let create_draft = core
        .transact(single_op_tx(
            "tx-draft-create",
            MutationOp::Create {
                model: models::DRAFT.into(),
                id: "d-undo".into(),
                data: json!({
                    "id": "d-undo", "pr": pr.0, "anchor": anchor, "body": "x",
                    "source": {"kind": "me"},
                }),
            },
        ))
        .await;
    assert!(matches!(create_draft, TransactionResult::Ok { .. }));
    let delete_draft = core
        .transact(single_op_tx(
            "tx-draft-create-undo",
            MutationOp::Delete {
                model: models::DRAFT.into(),
                id: "d-undo".into(),
            },
        ))
        .await;
    assert!(
        matches!(delete_draft, TransactionResult::Ok { .. }),
        "undoing addDraft's create must be accepted, got {delete_draft:?}"
    );

    // draft: editDraft's `update` body, then undo's `update` with the old body.
    put_model_row(
        &f,
        models::DRAFT,
        "d-edit",
        json!({
            "id": "d-edit", "pr": pr.0, "anchor": anchor, "body": "before",
            "source": {"kind": "me"}, "outdated": false,
        }),
    );
    let edit_forward = core
        .transact(single_op_tx(
            "tx-draft-edit-forward",
            MutationOp::Update {
                model: models::DRAFT.into(),
                id: "d-edit".into(),
                data: json!({"body": "after"}),
            },
        ))
        .await;
    assert!(matches!(edit_forward, TransactionResult::Ok { .. }));
    let edit_undo = core
        .transact(single_op_tx(
            "tx-draft-edit-undo",
            MutationOp::Update {
                model: models::DRAFT.into(),
                id: "d-edit".into(),
                data: json!({"body": "before"}),
            },
        ))
        .await;
    assert!(
        matches!(edit_undo, TransactionResult::Ok { .. }),
        "undoing editDraft must restore the old body, got {edit_undo:?}"
    );
}

#[tokio::test]
async fn catch_up_replays_the_requested_gap() {
    let f = fixtures();
    let (core, _recv) = Core::start(f.deps());

    for i in 0..3 {
        let tx = Transaction {
            id: format!("tx-{i}"),
            ops: vec![MutationOp::Create {
                model: models::SETTING.into(),
                id: format!("k{i}"),
                data: json!({"key": format!("k{i}"), "scope": "user", "value": i}),
            }],
        };
        core.transact(tx).await;
    }

    let frame = core.catch_up(0).await.expect("catch_up");
    assert_eq!(frame.from_sync_id, 0);
    assert_eq!(frame.actions.len(), 3);
    assert_eq!(frame.last_sync_id, frame.actions.last().unwrap().id);

    let mid = frame.actions[0].id;
    let tail = core.catch_up(mid).await.expect("catch_up");
    assert_eq!(tail.actions.len(), 2);
}

// ── open_pr ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn open_pr_happy_path_stores_rows_and_groups_in_background() {
    let f = fixtures();
    let (core, _recv, pr, head) = open_fixture_pr(&f).await;
    assert_eq!(head, Sha::new("head111"));

    let bootstrap = core
        .bootstrap(BootstrapScope::Pr(pr.clone()))
        .await
        .expect("bootstrap");
    assert!(bootstrap.rows.iter().any(|r| r.model == models::PR_DETAIL));

    wait_until(|| {
        f.store
            .get(models::GROUPING, &pr.0)
            .ok()
            .flatten()
            .is_some()
    })
    .await;
}

#[tokio::test]
async fn opening_a_pr_links_it_to_an_already_stored_child_in_the_same_repo() {
    let f = fixtures();
    // Pre-seed a PR row based on the fixture PR's `head_ref` ("feature"),
    // as if an earlier inbox refresh had already stored it.
    let child_ref = PrRef {
        repo: fixture_pr_ref().repo,
        number: 8,
        host_id: None,
    };
    let mut child = testkit::sample_pr_summary(&child_ref, &Sha::new("h8"));
    child.base_ref = "feature".into();
    child.head_ref = "feat/child".into();
    f.store
        .apply(vec![NewAction {
            model: models::PR.into(),
            model_id: child_ref.id().0.clone(),
            action: SyncActionCode::I,
            data: Some(serde_json::to_value(&child).unwrap()),
        }])
        .unwrap();

    // The fixture PR (`#7`, base "main", head "feature") is `#8`'s parent.
    let (_core, _recv, pr, _head) = open_fixture_pr(&f).await;

    let parent_row = f.store.get(models::PR, &pr.0).unwrap().unwrap();
    let parent_summary: PrSummary = serde_json::from_value(parent_row.data).unwrap();
    assert_eq!(parent_summary.stack.expect("has a child").children, vec![8]);

    let child_row = f.store.get(models::PR, &child_ref.id().0).unwrap().unwrap();
    let child_summary: PrSummary = serde_json::from_value(child_row.data).unwrap();
    assert_eq!(child_summary.stack.expect("has a parent").base_pr, Some(7));
}

// ── read -> viewed -> outbox ─────────────────────────────────────────────

#[tokio::test]
async fn reading_every_hunk_of_a_file_marks_it_viewed_and_enqueues_set_viewed() {
    let f = fixtures();
    let (core, _recv, pr, head) = open_fixture_pr(&f).await;
    let revision = core
        .get_revision(RevisionRequest {
            pr: pr.clone(),
            head,
        })
        .await
        .unwrap();
    let hunks = testkit::hunk_ids(&revision);
    assert_eq!(hunks.len(), 2);

    let r1 = core.transact(hunk_read_tx("read-1", &pr, &hunks[0])).await;
    assert!(matches!(r1, TransactionResult::Ok { .. }));
    // Only one of two hunks read: not viewed yet, nothing queued.
    assert!(f.store.outbox_snapshot().is_empty());

    let r2 = core.transact(hunk_read_tx("read-2", &pr, &hunks[1])).await;
    assert!(matches!(r2, TransactionResult::Ok { .. }));

    wait_until(|| !f.store.outbox_snapshot().is_empty()).await;
    assert_eq!(f.store.outbox_snapshot()[0].op, "set_viewed");

    wait_until(|| {
        f.store
            .outbox_snapshot()
            .iter()
            .all(|i| i.status == OutboxStatus::Done)
    })
    .await;
    let fv = file_viewed_row(&f, &pr, "src/lib.rs").expect("file_viewed row");
    assert_eq!(fv.state, ViewedState::Viewed);
}

// ── outbox retry / backoff / auth pause / compensation ──────────────────

#[tokio::test]
async fn outbox_backs_off_on_network_errors_then_succeeds() {
    let f = fixtures();
    let (core, _recv, pr, head) = open_fixture_pr(&f).await;
    let revision = core
        .get_revision(RevisionRequest {
            pr: pr.clone(),
            head,
        })
        .await
        .unwrap();
    let hunks = testkit::hunk_ids(&revision);

    f.host
        .push_set_viewed(Err(HostError::Network("timeout".into())));
    f.host.push_set_viewed(Ok(()));

    for (i, h) in hunks.iter().enumerate() {
        core.transact(hunk_read_tx(&format!("read-{i}"), &pr, h))
            .await;
    }

    wait_until(|| {
        f.store
            .outbox_snapshot()
            .first()
            .is_some_and(|i| i.attempts >= 1)
    })
    .await;
    assert_eq!(f.store.outbox_snapshot()[0].attempts, 1);
    assert_eq!(f.store.outbox_snapshot()[0].status, OutboxStatus::Pending);

    // Advance past the backoff window so the retry becomes due and succeeds.
    f.clock.advance(10_000);
    wait_until(|| f.store.outbox_snapshot()[0].status == OutboxStatus::Done).await;
}

#[tokio::test]
async fn outbox_pauses_on_auth_error_and_reports_host_status() {
    let f = fixtures();
    let (core, mut recv, pr, head) = open_fixture_pr(&f).await;
    let revision = core
        .get_revision(RevisionRequest {
            pr: pr.clone(),
            head,
        })
        .await
        .unwrap();
    let hunks = testkit::hunk_ids(&revision);

    f.host.push_set_viewed(Err(HostError::Auth));
    f.host.push_set_viewed(Ok(()));

    for (i, h) in hunks.iter().enumerate() {
        core.transact(hunk_read_tx(&format!("read-{i}"), &pr, h))
            .await;
    }

    let event = expect_ephemeral(&mut recv, |e| matches!(e, Ephemeral::HostStatus { .. })).await;
    assert!(matches!(
        event,
        Ephemeral::HostStatus { auth_ok: false, .. }
    ));

    f.clock.advance(60_000);
    wait_until(|| f.store.outbox_snapshot()[0].status == OutboxStatus::Done).await;
}

#[tokio::test]
async fn outbox_reports_a_rate_limit_with_its_reset_time() {
    let f = fixtures();
    let (core, mut recv, pr, head) = open_fixture_pr(&f).await;
    let revision = core
        .get_revision(RevisionRequest {
            pr: pr.clone(),
            head,
        })
        .await
        .unwrap();
    let hunks = testkit::hunk_ids(&revision);

    let reset_at = f.clock.now() + 30_000;
    f.host
        .push_set_viewed(Err(HostError::RateLimited { reset_at }));
    f.host.push_set_viewed(Ok(()));
    for (i, h) in hunks.iter().enumerate() {
        core.transact(hunk_read_tx(&format!("read-{i}"), &pr, h))
            .await;
    }

    let event = expect_ephemeral(&mut recv, |e| matches!(e, Ephemeral::HostStatus { .. })).await;
    assert!(matches!(
        event,
        Ephemeral::HostStatus { rate_limited_until: Some(t), auth_ok: true, .. } if t == reset_at
    ));
    f.clock.advance(31_000);
    wait_until(|| f.store.outbox_snapshot()[0].status == OutboxStatus::Done).await;
}

#[tokio::test]
async fn outbox_compensates_on_a_permanent_failure() {
    let f = fixtures();
    let (core, mut recv, pr, head) = open_fixture_pr(&f).await;
    let revision = core
        .get_revision(RevisionRequest {
            pr: pr.clone(),
            head,
        })
        .await
        .unwrap();
    let hunks = testkit::hunk_ids(&revision);

    f.host
        .push_set_viewed(Err(HostError::Validation("file too large".into())));

    for (i, h) in hunks.iter().enumerate() {
        core.transact(hunk_read_tx(&format!("read-{i}"), &pr, h))
            .await;
    }

    let event = expect_ephemeral(&mut recv, |e| {
        matches!(
            e,
            Ephemeral::Toast {
                level: ToastLevel::Error,
                ..
            }
        )
    })
    .await;
    assert!(matches!(
        event,
        Ephemeral::Toast {
            level: ToastLevel::Error,
            ..
        }
    ));

    wait_until(|| {
        f.store
            .outbox_snapshot()
            .first()
            .is_some_and(|i| i.status == OutboxStatus::Failed)
    })
    .await;
    let fv = file_viewed_row(&f, &pr, "src/lib.rs").expect("file_viewed row");
    assert_eq!(
        fv.state,
        ViewedState::Unviewed,
        "a permanent failure must revert the optimistic Viewed state"
    );
}

/// Reads every hunk of the fixture PR, which queues one `set_viewed`.
async fn queue_one_set_viewed(core: &Core, pr: &PrId, head: Sha) {
    let revision = core
        .get_revision(RevisionRequest {
            pr: pr.clone(),
            head,
        })
        .await
        .unwrap();
    for (i, h) in testkit::hunk_ids(&revision).iter().enumerate() {
        core.transact(hunk_read_tx(&format!("read-{i}"), pr, h))
            .await;
    }
}

#[tokio::test]
async fn outbox_fails_a_protocol_error_at_once_and_keeps_the_hosts_message() {
    let f = fixtures();
    let (core, mut recv, pr, head) = open_fixture_pr(&f).await;

    f.host.push_set_viewed(Err(HostError::Protocol(
        "markFileAsViewed returned nothing".into(),
    )));
    queue_one_set_viewed(&core, &pr, head).await;

    let event = expect_ephemeral(&mut recv, |e| matches!(e, Ephemeral::Toast { .. })).await;
    assert!(matches!(
        event,
        Ephemeral::Toast { level: ToastLevel::Error, ref text } if text.contains("markFileAsViewed returned nothing")
    ));
    wait_until(|| {
        f.store
            .outbox_snapshot()
            .first()
            .is_some_and(|i| i.status == OutboxStatus::Failed)
    })
    .await;
    let item = f.store.outbox_snapshot()[0].clone();
    assert_eq!(item.attempts, 0, "no retry for a permanent error");
    assert!(
        item.last_error
            .as_deref()
            .is_some_and(|e| e.contains("markFileAsViewed returned nothing")),
        "{:?}",
        item.last_error
    );
    assert!(
        core.is_idle().await,
        "a failed item must not hold off a restart"
    );
}

#[tokio::test]
async fn outbox_fails_a_refused_review_with_githubs_message() {
    let f = fixtures();
    let (core, mut recv, pr, _head) = open_fixture_pr(&f).await;

    f.host.push_submit_review(Err(HostError::Validation(
        "Can not approve your own pull request".into(),
    )));
    core.submit_review(SubmitReviewRequest {
        pr: pr.clone(),
        event: ReviewEvent::Approve,
        body: String::new(),
    })
    .await
    .expect("queued");

    let event = expect_ephemeral(&mut recv, |e| matches!(e, Ephemeral::Toast { .. })).await;
    assert!(matches!(
        event,
        Ephemeral::Toast { ref text, .. } if text == "Can not approve your own pull request"
    ));
    wait_until(|| {
        f.store
            .outbox_snapshot()
            .iter()
            .any(|i| i.op == "submit_review" && i.status == OutboxStatus::Failed)
    })
    .await;
    let item = f
        .store
        .outbox_snapshot()
        .into_iter()
        .find(|i| i.op == "submit_review")
        .unwrap();
    assert_eq!(
        item.last_error.as_deref(),
        Some("Can not approve your own pull request")
    );
}

#[tokio::test]
async fn outbox_gives_up_on_a_transient_error_after_max_attempts() {
    let f = fixtures();
    let (core, mut recv, pr, head) = open_fixture_pr(&f).await;

    for _ in 0..crate::outbox::MAX_ATTEMPTS {
        f.host
            .push_set_viewed(Err(HostError::Network("connection reset".into())));
    }
    queue_one_set_viewed(&core, &pr, head).await;

    for n in 1..crate::outbox::MAX_ATTEMPTS {
        wait_until(|| {
            f.store
                .outbox_snapshot()
                .first()
                .is_some_and(|i| i.attempts >= n)
        })
        .await;
        let item = f.store.outbox_snapshot()[0].clone();
        assert_eq!(item.status, OutboxStatus::Pending, "attempt {n}");
        assert!(
            item.last_error
                .as_deref()
                .is_some_and(|e| e.contains("connection reset"))
        );
        // Past the longest backoff (3 h).
        f.clock.advance(3 * 60 * 60_000 + 1_000);
    }

    wait_until(|| f.store.outbox_snapshot()[0].status == OutboxStatus::Failed).await;
    let event = expect_ephemeral(&mut recv, |e| matches!(e, Ephemeral::Toast { .. })).await;
    assert!(matches!(
        event,
        Ephemeral::Toast { ref text, .. } if text.contains("gave up after") && text.contains("connection reset")
    ));
    assert!(core.is_idle().await);
}

#[tokio::test]
async fn open_refetches_once_when_a_fresh_fetch_comes_back_not_modified() {
    let f = fixtures();
    let pr_ref = fixture_pr_ref();
    let detail = testkit::sample_pr_detail(&pr_ref, &Sha::new("base000"), &Sha::new("head111"));
    f.host.push_get_pr(Ok(Fetched::NotModified));
    f.host.push_get_pr(Ok(Fetched::Modified {
        etag: Some("etag-2".into()),
        value: detail,
    }));

    let (core, _recv) = Core::start(f.deps());
    let resp = core
        .open_pr(OpenPrRequest {
            url: "https://github.com/acme/widgets/pull/7".into(),
        })
        .await
        .expect("the refetch must open the PR");
    assert_eq!(resp.head, Sha::new("head111"));
}

#[tokio::test]
async fn open_refuses_when_the_refetch_is_not_modified_too() {
    let f = fixtures();
    f.host.push_get_pr(Ok(Fetched::NotModified));
    f.host.push_get_pr(Ok(Fetched::NotModified));

    let (core, _recv) = Core::start(f.deps());
    let err = core
        .open_pr(OpenPrRequest {
            url: "https://github.com/acme/widgets/pull/7".into(),
        })
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Invalid(_)), "{err}");
}

// ── push handling ─────────────────────────────────────────────────────────

#[tokio::test]
async fn a_push_clears_reads_of_changed_hunks_and_dismisses_their_file() {
    let f = fixtures();
    let (core, _recv, pr, head) = open_fixture_pr(&f).await;
    let revision = core
        .get_revision(RevisionRequest {
            pr: pr.clone(),
            head: head.clone(),
        })
        .await
        .unwrap();
    let hunks = testkit::hunk_ids(&revision);

    for (i, h) in hunks.iter().enumerate() {
        core.transact(hunk_read_tx(&format!("read-{i}"), &pr, h))
            .await;
    }
    wait_until(|| file_viewed_row(&f, &pr, "src/lib.rs").is_some()).await;

    // The author pushes: hunk 1's content changes (new id), hunk 2 is untouched.
    let new_head = Sha::new("head222");
    let changed = testkit::make_hunk(
        "src/lib.rs",
        1,
        1,
        1,
        2,
        vec![
            testkit::line_ctx(1, 1, "a"),
            testkit::line_add(2, "b1-changed"),
        ],
    );
    let unchanged = revision.files[0].hunks[1].clone();
    let new_revision = Revision {
        base: revision.base.clone(),
        head: new_head.clone(),
        files: vec![FileDiff {
            hunks: vec![changed, unchanged],
            ..revision.files[0].clone()
        }],
    };
    f.git.set_head(new_head.clone());
    f.git.set_revision(new_revision);

    let detail = testkit::sample_pr_detail(&fixture_pr_ref(), &revision.base, &new_head);
    f.host.push_get_pr(Ok(Fetched::Modified {
        etag: Some("etag-2".into()),
        value: detail,
    }));

    f.clock.advance(15_000);

    wait_until(|| {
        file_viewed_row(&f, &pr, "src/lib.rs")
            .map(|fv| fv.state == ViewedState::Dismissed)
            .unwrap_or(false)
    })
    .await;

    let remaining_reads = f.store.query(models::HUNK_READ, "pr", &pr.0).unwrap();
    assert_eq!(
        remaining_reads.len(),
        1,
        "only the unchanged hunk's read should survive the push"
    );
}

#[tokio::test]
async fn a_push_clears_reads_and_dismisses_a_file_whose_hunk_vanished_with_no_replacement() {
    let f = fixtures();
    let (core, _recv, pr, head) = open_fixture_pr(&f).await;
    let revision = core
        .get_revision(RevisionRequest {
            pr: pr.clone(),
            head: head.clone(),
        })
        .await
        .unwrap();
    let hunks = testkit::hunk_ids(&revision);

    for (i, h) in hunks.iter().enumerate() {
        core.transact(hunk_read_tx(&format!("read-{i}"), &pr, h))
            .await;
    }
    wait_until(|| file_viewed_row(&f, &pr, "src/lib.rs").is_some()).await;

    // The author pushes: hunk 1 is reverted outright — dropped, with
    // nothing landing in its place — while hunk 2 survives untouched.
    // `interdiff::classify` only walks the *new* revision's hunks, so it
    // never sees hunk 1 at all; it must still be caught as vanished.
    let new_head = Sha::new("head222");
    let unchanged = revision.files[0].hunks[1].clone();
    let new_revision = Revision {
        base: revision.base.clone(),
        head: new_head.clone(),
        files: vec![FileDiff {
            hunks: vec![unchanged],
            ..revision.files[0].clone()
        }],
    };
    f.git.set_head(new_head.clone());
    f.git.set_revision(new_revision);

    let detail = testkit::sample_pr_detail(&fixture_pr_ref(), &revision.base, &new_head);
    f.host.push_get_pr(Ok(Fetched::Modified {
        etag: Some("etag-vanish".into()),
        value: detail,
    }));

    f.clock.advance(15_000);

    wait_until(|| {
        file_viewed_row(&f, &pr, "src/lib.rs")
            .map(|fv| fv.state == ViewedState::Dismissed)
            .unwrap_or(false)
    })
    .await;

    let remaining_reads = f.store.query(models::HUNK_READ, "pr", &pr.0).unwrap();
    assert_eq!(
        remaining_reads.len(),
        1,
        "the vanished hunk's read must be cleared, the survivor's must not"
    );
    let remaining: HunkRead = serde_json::from_value(remaining_reads[0].data.clone()).unwrap();
    assert_eq!(
        remaining.hunk, hunks[1],
        "the surviving hunk's read must stay"
    );
}

#[tokio::test]
async fn poller_applies_pr_detail_updates_even_when_the_head_is_unchanged() {
    let f = fixtures();
    let (_core, _recv, pr, head) = open_fixture_pr(&f).await;

    // Same head — not a push — but the host reports a change: a label was
    // added. There's no new revision to diff, just an updated pr_detail
    // that must not be dropped just because the head didn't move.
    let mut detail = testkit::sample_pr_detail(&fixture_pr_ref(), &Sha::new("base000"), &head);
    detail.labels = vec!["needs-review".into()];
    f.host.push_get_pr(Ok(Fetched::Modified {
        etag: Some("etag-labels".into()),
        value: detail,
    }));

    f.clock.advance(15_000);

    wait_until(|| {
        f.store
            .get(models::PR_DETAIL, &pr.0)
            .unwrap()
            .is_some_and(|r| r.data["labels"] == json!(["needs-review"]))
    })
    .await;
}

#[tokio::test]
async fn a_push_remaps_moved_anchors_and_marks_rewritten_ones_outdated() {
    let f = fixtures();
    let (_core, _recv, pr, head) = open_fixture_pr(&f).await;

    // Anchored well after the push's insertion point: must only shift.
    let moving_anchor = Anchor {
        rev: head.clone(),
        path: "src/lib.rs".into(),
        side: Side::New,
        line: 20,
        start_line: None,
        start_side: None,
    };
    put_model_row(
        &f,
        models::DRAFT,
        "d-move",
        json!({
            "id": "d-move", "pr": pr.0, "anchor": moving_anchor, "body": "x",
            "source": {"kind": "me"}, "outdated": false,
        }),
    );

    // Anchored exactly on the line the push rewrites: must come back
    // outdated, not moved.
    let doomed_anchor = Anchor {
        rev: head.clone(),
        path: "src/lib.rs".into(),
        side: Side::New,
        line: 50,
        start_line: None,
        start_side: None,
    };
    put_model_row(
        &f,
        models::FINDING,
        "f-outdated",
        json!({
            "id": "f-outdated", "pr": pr.0, "skill": "s1", "agents": [],
            "anchor": doomed_anchor, "severity": "warn", "title": "t", "body": "b",
            "state": "open", "fix": null, "outdated": false,
        }),
    );

    // The push, described as the diff *between the two heads* (old_head ->
    // new_head): 3 lines inserted at the top shift everything after them by
    // +3; the line at old line 50 is rewritten in place.
    let new_head = Sha::new("head222");
    let insert = testkit::make_hunk(
        "src/lib.rs",
        1,
        0,
        1,
        3,
        vec![
            testkit::line_add(1, "x"),
            testkit::line_add(2, "y"),
            testkit::line_add(3, "z"),
        ],
    );
    let rewrite = testkit::make_hunk(
        "src/lib.rs",
        50,
        1,
        50,
        1,
        vec![testkit::line_add(50, "rewritten")],
    );
    let between = Revision {
        base: Sha::new("base000"),
        head: new_head.clone(),
        files: vec![FileDiff {
            path: "src/lib.rs".into(),
            old_path: None,
            status: FileStatus::Modified,
            similarity: None,
            old_mode: None,
            new_mode: None,
            flags: vec![],
            additions: 4,
            deletions: 1,
            hunks: vec![insert, rewrite],
        }],
    };
    f.git.set_head(new_head.clone());
    f.git.set_revision(between);

    let detail = testkit::sample_pr_detail(&fixture_pr_ref(), &Sha::new("base000"), &new_head);
    f.host.push_get_pr(Ok(Fetched::Modified {
        etag: Some("etag-anchor-remap".into()),
        value: detail,
    }));
    f.clock.advance(15_000);

    wait_until(|| {
        f.store
            .get(models::DRAFT, "d-move")
            .unwrap()
            .is_some_and(|r| r.data["anchor"]["line"] == json!(23))
    })
    .await;
    let moved = f.store.get(models::DRAFT, "d-move").unwrap().unwrap();
    assert_eq!(moved.data["anchor"]["rev"], json!(new_head.0));
    assert_eq!(moved.data["outdated"], json!(false));

    wait_until(|| {
        f.store
            .get(models::FINDING, "f-outdated")
            .unwrap()
            .is_some_and(|r| r.data["outdated"] == json!(true))
    })
    .await;
}

// ── run cancel ────────────────────────────────────────────────────────────

#[tokio::test]
async fn cancel_run_stops_a_skill_run_before_it_finishes() {
    let f = fixtures();
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;
    f.agents.hang_next_run();

    let run_id = core
        .run_skill(RunSkillRequest {
            pr: pr.clone(),
            skill: "security".into(),
            agents: vec![AgentId::Claude],
        })
        .await
        .expect("run_skill");
    core.cancel_run(run_id.clone()).await.expect("cancel_run");

    wait_until(|| {
        f.store
            .get(models::SKILL_RUN, &run_id)
            .ok()
            .flatten()
            .and_then(|r| r.data.get("status").cloned())
            .map(|s| s == json!("cancelled"))
            .unwrap_or(false)
    })
    .await;
}

#[tokio::test]
async fn a_skill_run_without_cancellation_completes() {
    let f = fixtures();
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;
    f.agents.push_script(vec![
        AgentEvent::Text {
            text: "looking...".into(),
        },
        AgentEvent::Done {
            final_text: "done".into(),
            usage: Usage::default(),
        },
    ]);

    let run_id = core
        .run_skill(RunSkillRequest {
            pr: pr.clone(),
            skill: "security".into(),
            agents: vec![AgentId::Claude],
        })
        .await
        .expect("run_skill");

    wait_until(|| {
        f.store
            .get(models::SKILL_RUN, &run_id)
            .ok()
            .flatten()
            .and_then(|r| r.data.get("status").cloned())
            .map(|s| s == json!("done"))
            .unwrap_or(false)
    })
    .await;
}

// ── refresh_inbox ───────────────────────────────────────────────────────────

#[tokio::test]
async fn refresh_inbox_writes_pr_rows_and_seeds_inbox_state_once() {
    let f = fixtures();
    let (core, _recv) = Core::start(f.deps());
    // Let the one-shot startup refresh (empty fixtures) drain first.
    tokio::time::sleep(Duration::from_millis(30)).await;

    let pr_ref = fixture_pr_ref();
    let head = Sha::new("head111");
    let summary = testkit::sample_pr_summary(&pr_ref, &head);
    let pr_id = pr_ref.id();
    for _ in 0..4 {
        f.host.push_search(Ok(vec![summary.clone()]));
    }

    core.refresh_inbox().await.expect("refresh_inbox");

    let pr_row = f.store.get(models::PR, &pr_id.0).unwrap();
    assert!(pr_row.is_some(), "pr row written");
    let state_row = f.store.get(models::INBOX_STATE, &pr_id.0).unwrap();
    assert!(state_row.is_some(), "inbox_state row created");

    // Mark it done, then refresh again: a second pass must not clobber it.
    f.store
        .apply(vec![NewAction {
            model: models::INBOX_STATE.into(),
            model_id: pr_id.0.clone(),
            action: SyncActionCode::U,
            data: Some(json!({"pr": pr_id, "done": true, "snooze": null})),
        }])
        .unwrap();
    for _ in 0..4 {
        f.host.push_search(Ok(vec![summary.clone()]));
    }
    core.refresh_inbox().await.expect("refresh_inbox again");
    let state_row = f.store.get(models::INBOX_STATE, &pr_id.0).unwrap().unwrap();
    assert_eq!(state_row.data["done"], json!(true));
}

#[tokio::test]
async fn refresh_inbox_links_stacked_prs_in_the_same_repo() {
    let f = fixtures();
    let (core, _recv) = Core::start(f.deps());
    tokio::time::sleep(Duration::from_millis(30)).await;

    let repo = fixture_pr_ref().repo;
    let parent_ref = PrRef {
        repo: repo.clone(),
        number: 1,
        host_id: None,
    };
    let child_ref = PrRef {
        repo: repo.clone(),
        number: 2,
        host_id: None,
    };

    let mut parent = testkit::sample_pr_summary(&parent_ref, &Sha::new("h1"));
    parent.base_ref = "main".into();
    parent.head_ref = "feat/a".into();
    let mut child = testkit::sample_pr_summary(&child_ref, &Sha::new("h2"));
    child.base_ref = "feat/a".into();
    child.head_ref = "feat/b".into();

    for _ in 0..4 {
        f.host.push_search(Ok(vec![parent.clone(), child.clone()]));
    }
    core.refresh_inbox().await.expect("refresh_inbox");

    let parent_row = f
        .store
        .get(models::PR, &parent_ref.id().0)
        .unwrap()
        .unwrap();
    let parent_summary: PrSummary = serde_json::from_value(parent_row.data).unwrap();
    assert_eq!(
        parent_summary.stack.expect("parent has a child").children,
        vec![2]
    );

    let child_row = f.store.get(models::PR, &child_ref.id().0).unwrap().unwrap();
    let child_summary: PrSummary = serde_json::from_value(child_row.data).unwrap();
    assert_eq!(
        child_summary.stack.expect("child has a parent").base_pr,
        Some(1)
    );
}

// ── viewer / host_status / set_host_token ───────────────────────────────────

#[tokio::test]
async fn viewer_returns_the_authenticated_person() {
    let f = fixtures();
    let (core, _recv) = Core::start(f.deps());
    f.host.push_viewer(Ok(testkit::sample_person("octocat")));

    let me = core.viewer().await.expect("viewer");
    assert_eq!(me.login, "octocat");
}

#[tokio::test]
async fn host_status_reports_source_and_connection() {
    let f = fixtures();
    let (core, _recv) = Core::start(f.deps());

    f.host.push_viewer(Ok(testkit::sample_person("me")));
    let status = core.host_status().await.expect("host_status");
    assert!(status.connected);
    assert_eq!(status.login, Some("me".into()));
    assert_eq!(status.source, TokenSource::Keychain);

    f.token_status.set(TokenSource::None);
    let status = core.host_status().await.expect("host_status");
    assert!(!status.connected);
    assert_eq!(status.source, TokenSource::None);
}

#[tokio::test]
async fn set_host_token_validates_before_storing_and_rolls_back_on_failure() {
    let f = fixtures();
    let (core, _recv) = Core::start(f.deps());
    f.secrets
        .set(theodore_secrets::GITHUB_TOKEN, "old-token")
        .unwrap();

    f.host.push_viewer(Ok(testkit::sample_person("me")));
    let status = core
        .set_host_token(SetTokenRequest {
            host: HostKind::Github,
            token: Some("new-token".into()),
        })
        .await
        .expect("set_host_token");
    assert!(status.connected);
    assert_eq!(
        f.secrets.get(theodore_secrets::GITHUB_TOKEN).unwrap(),
        Some("new-token".into())
    );
    assert!(f.token_status.invalidations() >= 1);

    f.host.push_viewer(Err(HostError::Auth));
    let err = core
        .set_host_token(SetTokenRequest {
            host: HostKind::Github,
            token: Some("bad-token".into()),
        })
        .await;
    assert!(err.is_err(), "a rejected token must not be stored");
    assert_eq!(
        f.secrets.get(theodore_secrets::GITHUB_TOKEN).unwrap(),
        Some("new-token".into()),
        "rolled back to the last good token"
    );

    core.set_host_token(SetTokenRequest {
        host: HostKind::Github,
        token: None,
    })
    .await
    .expect("remove token");
    assert_eq!(f.secrets.get(theodore_secrets::GITHUB_TOKEN).unwrap(), None);
}

#[tokio::test]
async fn set_host_token_keys_by_host_and_leaves_other_hosts_token_alone() {
    let f = fixtures();
    let (core, _recv) = Core::start(f.deps());
    f.secrets
        .set(theodore_secrets::GITHUB_TOKEN, "old-gh")
        .unwrap();
    f.secrets
        .set(theodore_secrets::GITLAB_TOKEN, "old-gl")
        .unwrap();

    f.host.push_viewer(Ok(testkit::sample_person("me")));
    let status = core
        .set_host_token(SetTokenRequest {
            host: HostKind::Github,
            token: Some("new-gh".into()),
        })
        .await
        .expect("set_host_token");
    assert!(status.connected);
    assert_eq!(
        f.secrets.get(theodore_secrets::GITHUB_TOKEN).unwrap(),
        Some("new-gh".into())
    );
    assert_eq!(
        f.secrets.get(theodore_secrets::GITLAB_TOKEN).unwrap(),
        Some("old-gl".into()),
        "setting github's token must not touch gitlab's"
    );
}

#[tokio::test]
async fn set_host_token_for_a_non_primary_host_never_touches_the_primarys_token() {
    // `FakeHost::kind()` is `Github`; setting a `Gitlab` token used to be
    // rejected outright before it ever reached the secrets vault (a flat
    // `req.host != deps.host.kind()` gate). It must now actually attempt
    // that host's own validation (surfacing as `AppError::Host`, not the
    // old `AppError::Invalid("... is not configured")`), and never read,
    // write or delete another host's token while doing so.
    let f = fixtures();
    let (core, _recv) = Core::start(f.deps());
    f.secrets
        .set(theodore_secrets::GITHUB_TOKEN, "keep-me")
        .unwrap();

    let err = core
        .set_host_token(SetTokenRequest {
            host: HostKind::Gitlab,
            token: Some("gl-new".into()),
        })
        .await
        .unwrap_err();
    assert!(
        matches!(err, AppError::Host(_)),
        "expected the gitlab-specific validation to run and fail, got {err:?}"
    );
    assert_eq!(
        f.secrets.get(theodore_secrets::GITHUB_TOKEN).unwrap(),
        Some("keep-me".into()),
        "github's token must survive a gitlab token attempt"
    );
    assert_eq!(f.secrets.get(theodore_secrets::GITLAB_TOKEN).unwrap(), None);
}

// ── login (GitHub device flow) ──────────────────────────────────────────

async fn set_client_id(core: &Core, client_id: &str) {
    let setting = Setting {
        key: "host.github.client_id".into(),
        scope: "user".into(),
        value: json!(client_id),
    };
    let tx = Transaction {
        id: format!("client-id-{client_id}"),
        ops: vec![MutationOp::Create {
            model: models::SETTING.into(),
            id: setting.key.clone(),
            data: serde_json::to_value(&setting).unwrap(),
        }],
    };
    let result = core.transact(tx).await;
    assert!(
        matches!(result, TransactionResult::Ok { .. }),
        "expected the client id setting to apply, got {result:?}"
    );
}

#[tokio::test]
async fn host_status_reports_device_login_available_only_with_a_client_id() {
    let f = fixtures();
    let (core, _recv) = Core::start(f.deps());
    f.token_status.set(TokenSource::None);

    let status = core.host_status().await.expect("host_status");
    assert!(!status.device_login_available);

    set_client_id(&core, "cid-123").await;
    let status = core.host_status().await.expect("host_status");
    assert!(status.device_login_available);
}

#[tokio::test]
async fn login_start_fails_with_no_client_id_configured() {
    let f = fixtures();
    let (core, _recv) = Core::start(f.deps());

    let err = core.login_start().await.unwrap_err();
    assert!(matches!(err, AppError::Invalid(_)));
    assert!(f.device_login.calls().is_empty());
}

#[tokio::test]
async fn login_start_returns_the_device_code_and_login_poll_reports_pending() {
    let f = fixtures();
    let (core, _recv) = Core::start(f.deps());
    set_client_id(&core, "cid-123").await;

    f.device_login.set_start(theodore_ports::DeviceCodeStart {
        device_code: "dc-1".into(),
        user_code: "WXYZ-9876".into(),
        verification_uri: "https://github.com/login/device".into(),
        expires_in: 900,
        interval: 5,
    });
    let device = core.login_start().await.expect("login_start");
    assert_eq!(
        device,
        DeviceLogin {
            user_code: "WXYZ-9876".into(),
            verification_uri: "https://github.com/login/device".into(),
            expires_in: 900,
            interval: 5,
        }
    );

    f.device_login.push_poll(DevicePoll::Pending);
    let state = core.login_poll().await.expect("login_poll");
    assert_eq!(state, LoginState::Pending);
}

#[tokio::test]
async fn login_poll_without_a_prior_start_is_rejected() {
    let f = fixtures();
    let (core, _recv) = Core::start(f.deps());
    set_client_id(&core, "cid-123").await;

    let err = core.login_poll().await.unwrap_err();
    assert!(matches!(err, AppError::Invalid(_)));
}

#[tokio::test]
async fn login_poll_slow_down_denied_and_expired() {
    let f = fixtures();
    let (core, _recv) = Core::start(f.deps());
    set_client_id(&core, "cid-123").await;

    core.login_start().await.expect("login_start");
    f.device_login
        .push_poll(DevicePoll::SlowDown { interval: 10 });
    let state = core.login_poll().await.expect("login_poll");
    assert_eq!(state, LoginState::SlowDown { interval: 10 });

    f.device_login.push_poll(DevicePoll::AccessDenied);
    let state = core.login_poll().await.expect("login_poll");
    assert_eq!(state, LoginState::Denied);
    // Denied clears the session: the next poll has nothing to poll for.
    let err = core.login_poll().await.unwrap_err();
    assert!(matches!(err, AppError::Invalid(_)));

    core.login_start().await.expect("login_start again");
    f.device_login.push_poll(DevicePoll::ExpiredToken);
    let state = core.login_poll().await.expect("login_poll");
    assert_eq!(state, LoginState::Expired);
}

#[tokio::test]
async fn login_poll_success_stores_the_token_like_set_host_token_does() {
    let f = fixtures();
    let (core, _recv) = Core::start(f.deps());
    set_client_id(&core, "cid-123").await;

    core.login_start().await.expect("login_start");
    f.device_login.push_poll(DevicePoll::Success {
        access_token: "ghu_abc123".into(),
    });
    f.host.push_viewer(Ok(testkit::sample_person("octocat")));

    let state = core.login_poll().await.expect("login_poll");
    match state {
        LoginState::Done { status } => {
            assert!(status.connected);
            assert_eq!(status.login, Some("octocat".into()));
            assert_eq!(status.source, TokenSource::Keychain);
        }
        other => panic!("expected Done, got {other:?}"),
    }
    assert_eq!(
        f.secrets.get(theodore_secrets::GITHUB_TOKEN).unwrap(),
        Some("ghu_abc123".into())
    );
    assert!(f.token_status.invalidations() >= 1);
}

// ── submit_review ────────────────────────────────────────────────────────

#[tokio::test]
async fn submit_review_skips_outdated_drafts_with_a_toast() {
    let f = fixtures();
    let (core, mut recv, pr, _head) = open_fixture_pr(&f).await;

    put_model_row(
        &f,
        models::DRAFT,
        "d-ok",
        json!({
            "id": "d-ok", "pr": pr.0, "anchor": testkit::sample_anchor("src/lib.rs", 2),
            "body": "fine", "source": {"kind": "me"}, "outdated": false,
        }),
    );
    put_model_row(
        &f,
        models::DRAFT,
        "d-stale",
        json!({
            "id": "d-stale", "pr": pr.0, "anchor": testkit::sample_anchor("src/lib.rs", 2),
            "body": "stale", "source": {"kind": "me"}, "outdated": true,
        }),
    );

    core.submit_review(SubmitReviewRequest {
        pr: pr.clone(),
        event: ReviewEvent::Comment,
        body: "lgtm".into(),
    })
    .await
    .expect("submit_review");

    expect_ephemeral(
        &mut recv,
        |e| matches!(e, Ephemeral::Toast { text, .. } if text.contains("skipped")),
    )
    .await;

    wait_until(|| {
        f.store
            .outbox_snapshot()
            .iter()
            .any(|i| i.op == "submit_review")
    })
    .await;
    let item = f
        .store
        .outbox_snapshot()
        .into_iter()
        .find(|i| i.op == "submit_review")
        .unwrap();
    let comments = item.payload["comments"].as_array().unwrap();
    assert_eq!(
        comments.len(),
        1,
        "the outdated draft must not reach the host write"
    );
    assert_eq!(comments[0]["draft_id"], json!("d-ok"));
}

// ── reply_thread / resolve_thread ────────────────────────────────────────────

#[tokio::test]
async fn reply_thread_enqueues_outbox_and_refreshes_pr_detail_on_success() {
    let f = fixtures();
    let (core, _recv, pr, head) = open_fixture_pr(&f).await;
    let detail = testkit::sample_pr_detail(&fixture_pr_ref(), &Sha::new("base000"), &head);
    f.host.push_get_pr(Ok(Fetched::Modified {
        etag: Some("etag-reply".into()),
        value: detail,
    }));

    core.reply_thread(ReplyRequest {
        pr: pr.clone(),
        thread: ThreadId::new("thread-1"),
        body: "thanks, fixed".into(),
    })
    .await
    .expect("reply_thread");

    wait_until(|| {
        f.store
            .outbox_snapshot()
            .iter()
            .any(|i| i.op == "reply_thread" && i.status == OutboxStatus::Done)
    })
    .await;
}

#[tokio::test]
async fn reply_thread_does_not_repost_after_a_timeout_retry() {
    // The first `reply` call "succeeds" host-side but the client sees a
    // network error (as if the response was lost after the write landed).
    // The retry must find the marker and skip reposting, not create a
    // second reply.
    let f = fixtures();
    let (core, _recv, pr, head) = open_fixture_pr(&f).await;
    let detail = testkit::sample_pr_detail(&fixture_pr_ref(), &Sha::new("base000"), &head);
    f.host.push_get_pr(Ok(Fetched::Modified {
        etag: Some("etag-reply-retry".into()),
        value: detail,
    }));

    f.host.push_reply(Err(HostError::Network("timeout".into())));
    f.host
        .push_find_marker(Ok(Some(theodore_domain::CommentId::new("already-posted"))));

    core.reply_thread(ReplyRequest {
        pr: pr.clone(),
        thread: ThreadId::new("thread-1"),
        body: "thanks, fixed".into(),
    })
    .await
    .expect("reply_thread");

    wait_until(|| {
        f.store
            .outbox_snapshot()
            .first()
            .is_some_and(|i| i.attempts >= 1)
    })
    .await;

    f.clock.advance(10_000);
    wait_until(|| {
        f.store
            .outbox_snapshot()
            .iter()
            .any(|i| i.op == "reply_thread" && i.status == OutboxStatus::Done)
    })
    .await;

    assert_eq!(
        f.host.reply_calls().len(),
        1,
        "the retry must not post a second reply once the marker is found"
    );
}

#[tokio::test]
async fn resolve_thread_enqueues_outbox() {
    let f = fixtures();
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;

    core.resolve_thread(ResolveRequest {
        pr,
        thread: ThreadId::new("thread-1"),
        resolved: true,
    })
    .await
    .expect("resolve_thread");

    wait_until(|| {
        f.store
            .outbox_snapshot()
            .iter()
            .any(|i| i.op == "resolve_thread" && i.status == OutboxStatus::Done)
    })
    .await;
}

// ── list_skills / run_skill content ─────────────────────────────────────────

#[tokio::test]
async fn list_skills_includes_builtins_and_the_prs_own_claude_skills() {
    let f = fixtures();
    let (core, _recv, pr, head) = open_fixture_pr(&f).await;

    let skill_dir = f.git.worktree_path(&head).join(".claude/skills/demo");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: demo\ndescription: a demo skill\n---\n\nBody.\n",
    )
    .unwrap();

    let skills = core.list_skills(Some(pr)).await.expect("list_skills");
    assert!(
        skills
            .iter()
            .any(|s| s.id == "bugs" && s.source == "built-in")
    );
    assert!(skills.iter().any(|s| s.id == "security"));
    assert!(skills.iter().any(|s| s.id == "tests"));
    assert!(
        skills
            .iter()
            .any(|s| s.id == "demo" && s.title == "demo" && s.description == "a demo skill")
    );
}

#[tokio::test]
async fn run_skill_inlines_skill_text_for_non_claude_agents_only() {
    let f = fixtures();
    let (core, _recv, pr, head) = open_fixture_pr(&f).await;
    let skill_dir = f.git.worktree_path(&head).join(".claude/skills/demo");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: demo\ndescription: a demo skill\n---\n\nUnique skill body marker.\n",
    )
    .unwrap();

    f.agents.push_script(vec![AgentEvent::Done {
        final_text: "ok".into(),
        usage: Usage::default(),
    }]);
    core.run_skill(RunSkillRequest {
        pr: pr.clone(),
        skill: "demo".into(),
        agents: vec![AgentId::Codex],
    })
    .await
    .expect("run_skill codex");
    wait_until(|| f.agents.last_request().is_some()).await;
    let codex_prompt = f.agents.last_request().unwrap().prompt;
    assert!(
        codex_prompt.contains("Unique skill body marker."),
        "non-Claude agents get the SKILL.md text inlined"
    );

    f.agents.push_script(vec![AgentEvent::Done {
        final_text: "ok".into(),
        usage: Usage::default(),
    }]);
    core.run_skill(RunSkillRequest {
        pr,
        skill: "demo".into(),
        agents: vec![AgentId::Claude],
    })
    .await
    .expect("run_skill claude");
    wait_until(|| {
        f.agents
            .last_request()
            .is_some_and(|r| r.agent == AgentId::Claude)
    })
    .await;
    let claude_prompt = f.agents.last_request().unwrap().prompt;
    assert!(
        !claude_prompt.contains("Unique skill body marker."),
        "Claude references the skill by name instead of inlining it"
    );
    assert!(claude_prompt.contains("`demo` skill"));
}

// ── delegate_task / decide_task ──────────────────────────────────────────────

#[tokio::test]
async fn delegate_task_runs_the_agent_in_edit_mode_and_captures_a_patch() {
    let f = fixtures();
    let (core, _recv, pr, head) = open_fixture_pr(&f).await;
    f.git.put_file(&head, "src/lib.rs", b"before\n");
    let dir = f.git.worktree_path(&head);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/lib.rs"), b"after\n").unwrap();

    let task = sample_task(
        "t1",
        &pr,
        Some(testkit::sample_anchor("src/lib.rs", 2)),
        Some(ThreadId::new("thread-1")),
        f.clock.now(),
    );
    put_task(&f, &task);
    f.agents.push_script(vec![AgentEvent::Done {
        final_text: "ok".into(),
        usage: Usage::default(),
    }]);

    let run_id = core
        .delegate_task(DelegateRequest {
            task: task.id.clone(),
            agent: Some(AgentId::Codex),
        })
        .await
        .expect("delegate_task");
    assert!(run_id.starts_with("fix:"));

    wait_until(|| task_row(&f, &task.id).status == TaskStatus::Review).await;
    let updated = task_row(&f, &task.id);
    let patch = updated.fix.expect("fix").patch.expect("patch");
    assert_eq!(patch.path, "src/lib.rs");
    assert_eq!(patch.after, vec!["after".to_string()]);

    let req = f.agents.last_request().expect("agent was run");
    assert_eq!(req.mode, theodore_ports::AgentMode::Edit);
    assert!(req.prompt.contains("Fix the thing"));
}

/// What each run showed live: its `AgentDelta` text joined, and its
/// `RunLog` lines.
fn shown_live(
    events: &mut tokio::sync::broadcast::Receiver<Ephemeral>,
    run: &str,
) -> (String, Vec<String>) {
    let mut text = String::new();
    let mut log = Vec::new();
    while let Ok(e) = events.try_recv() {
        match e {
            Ephemeral::AgentDelta {
                run: r, text: t, ..
            } if r == run => text.push_str(&t),
            Ephemeral::RunLog { run: r, line, .. } if r == run => log.push(line),
            _ => {}
        }
    }
    (text, log)
}

/// A fix shows what the agent says and does while it works, under the run
/// id `delegate_task` returned.
#[tokio::test]
async fn a_fix_run_streams_its_text_and_tool_calls_live() {
    let f = fixtures();
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;
    let task = sample_task("t-live", &pr, None, None, f.clock.now());
    put_task(&f, &task);
    let mut events = core.0.ephemeral_tx.subscribe();
    f.agents.push_script(vec![
        AgentEvent::TextDelta {
            text: "Fixing ".into(),
        },
        AgentEvent::TextDelta { text: "it.".into() },
        AgentEvent::Text {
            text: "Fixing it.".into(),
        },
        AgentEvent::ToolCall {
            id: "c1".into(),
            name: "Edit".into(),
            input: json!({ "file_path": "src/lib.rs" }),
        },
        AgentEvent::ToolResult {
            id: "c1".into(),
            ok: true,
            output: String::new(),
        },
        AgentEvent::Done {
            final_text: "Fixing it.".into(),
            usage: Usage::default(),
        },
    ]);

    let run_id = core
        .delegate_task(DelegateRequest {
            task: task.id.clone(),
            agent: Some(AgentId::Claude),
        })
        .await
        .expect("delegate_task");
    wait_until(|| task_row(&f, &task.id).status == TaskStatus::Review).await;

    assert!(f.agents.last_request().unwrap().stream_text);
    let (text, log) = shown_live(&mut events, &run_id);
    assert_eq!(text, "Fixing it.");
    assert_eq!(log, vec!["tool: Edit · src/lib.rs", "tool result ok=true"]);
}

/// A chat answer streams piece by piece, shows once, and the stored reply
/// is the whole answer.
#[tokio::test]
async fn ask_streams_its_answer_once_and_stores_it_whole() {
    let f = fixtures();
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;
    let mut events = core.0.ephemeral_tx.subscribe();
    f.agents.push_script(vec![
        AgentEvent::TextDelta { text: "Be".into() },
        AgentEvent::TextDelta {
            text: "cause.".into(),
        },
        AgentEvent::Text {
            text: "Because.".into(),
        },
        AgentEvent::Done {
            final_text: "Because.".into(),
            usage: Usage::default(),
        },
    ]);
    let message = core
        .ask(theodore_contract::AskRequest {
            pr: pr.clone(),
            question: "why?".into(),
            about: None,
            group: None,
            agent: Some(AgentId::Claude),
            attachments: None,
        })
        .await
        .expect("ask");
    wait_until(|| {
        chat_messages(&f, &pr)
            .iter()
            .any(|m| matches!(m.author, theodore_domain::Author::Agent(_)))
    })
    .await;

    assert!(f.agents.last_request().unwrap().stream_text);
    let (text, _) = shown_live(&mut events, &format!("ask:{message}"));
    assert_eq!(text, "Because.");
    let replies: Vec<_> = chat_messages(&f, &pr)
        .into_iter()
        .filter(|m| matches!(m.author, theodore_domain::Author::Agent(_)))
        .map(|m| m.text)
        .collect();
    assert_eq!(replies, vec!["Because.".to_string()]);
}

#[tokio::test]
async fn a_fix_run_carries_the_repo_instructions() {
    let f = fixtures();
    std::fs::write(
        f.data_dir.path().join("config.yaml"),
        "review:\n  instructions: Unique fix instruction marker.\n",
    )
    .unwrap();
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;
    let task = sample_task("t-instr", &pr, None, None, f.clock.now());
    put_task(&f, &task);
    f.agents.push_script(vec![AgentEvent::Done {
        final_text: "ok".into(),
        usage: Usage::default(),
    }]);
    core.delegate_task(DelegateRequest {
        task: task.id.clone(),
        agent: Some(AgentId::Codex),
    })
    .await
    .expect("delegate_task");
    wait_until(|| f.agents.last_request().is_some()).await;
    let system = f.agents.last_request().unwrap().system.expect("system");
    assert!(system.contains("Unique fix instruction marker."));
}

#[tokio::test]
async fn delegate_task_opens_its_pr_after_a_restart() {
    // Tasks lists tasks of PRs this session hasn't opened yet; delegating
    // one used to fail with "pr is not open".
    let f = fixtures();
    let (_core, _recv, pr, _head) = open_fixture_pr(&f).await;
    let task = sample_task("t-restart", &pr, None, None, f.clock.now());
    put_task(&f, &task);
    f.agents.push_script(vec![AgentEvent::Done {
        final_text: "ok".into(),
        usage: Usage::default(),
    }]);

    // The restarted core fetches the PR again.
    f.host.push_get_pr(Ok(Fetched::Modified {
        etag: Some("etag-2".into()),
        value: testkit::sample_pr_detail(
            &fixture_pr_ref(),
            &Sha::new("base000"),
            &Sha::new("head111"),
        ),
    }));
    let (core2, _recv2) = started_core(&f).await;
    let run_id = core2
        .delegate_task(DelegateRequest {
            task: task.id.clone(),
            agent: Some(AgentId::Codex),
        })
        .await
        .expect("delegate after a restart");
    assert!(run_id.starts_with("fix:"));
}

#[tokio::test]
async fn decide_task_accept_on_own_pr_commits_pushes_and_resolves_thread() {
    let f = fixtures();
    let (core, _recv, pr, head) = open_fixture_pr(&f).await;
    // `sample_pr_detail`'s author is "author" (plan §4: own PR == viewer == author).
    f.host.push_viewer(Ok(testkit::sample_person("author")));

    let mut task = sample_task(
        "t-own",
        &pr,
        Some(testkit::sample_anchor("src/lib.rs", 2)),
        Some(ThreadId::new("thread-1")),
        f.clock.now(),
    );
    task.status = TaskStatus::Review;
    task.fix = Some(Fix {
        status: FixStatus::Ready,
        patch: Some(Patch {
            path: "src/lib.rs".into(),
            before: vec!["before".into()],
            after: vec!["after".into()],
            note: "agent fix".into(),
            by: AgentId::Codex,
        }),
        commit: None,
    });
    put_task(&f, &task);
    // The worktree as the fix found it (an app restart lost the agent's
    // edit): accepting re-applies the patch before committing.
    let file = f.git.worktree_path(&head).join("src/lib.rs");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "before\n").unwrap();

    core.decide_task(TaskDecision {
        task: task.id.clone(),
        accept: true,
        reason: None,
    })
    .await
    .expect("decide_task accept");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "after\n");

    let (branch, message) = f.git.last_push.lock().unwrap().clone().expect("pushed");
    assert_eq!(branch, "feature");
    assert!(message.contains("Fix the thing"));

    wait_until(|| {
        f.store
            .outbox_snapshot()
            .iter()
            .any(|i| i.op == "resolve_thread" && i.status == OutboxStatus::Done)
    })
    .await;

    let updated = task_row(&f, &task.id);
    assert_eq!(updated.status, TaskStatus::Done);
    let fix = updated.fix.expect("fix");
    assert_eq!(fix.status, FixStatus::Applied);
    assert!(fix.commit.is_some());
}

#[tokio::test]
async fn decide_task_accept_on_someone_elses_pr_drafts_a_suggestion() {
    let f = fixtures();
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;
    // Author is "author"; a different viewer makes this not-mine.
    f.host.push_viewer(Ok(testkit::sample_person("reviewer")));

    let mut task = sample_task(
        "t-other",
        &pr,
        Some(testkit::sample_anchor("src/lib.rs", 2)),
        Some(ThreadId::new("thread-1")),
        f.clock.now(),
    );
    task.status = TaskStatus::Review;
    task.fix = Some(Fix {
        status: FixStatus::Ready,
        patch: Some(Patch {
            path: "src/lib.rs".into(),
            before: vec!["before".into()],
            after: vec!["after".into()],
            note: "agent fix".into(),
            by: AgentId::Codex,
        }),
        commit: None,
    });
    put_task(&f, &task);

    core.decide_task(TaskDecision {
        task: task.id.clone(),
        accept: true,
        reason: None,
    })
    .await
    .expect("decide_task accept (suggestion)");

    assert!(
        f.git.last_push.lock().unwrap().is_none(),
        "someone else's PR must not be pushed to"
    );
    let drafts = f.store.query(models::DRAFT, "pr", &pr.0).unwrap();
    assert_eq!(drafts.len(), 1);
    assert!(
        drafts[0].data["body"]
            .as_str()
            .unwrap()
            .contains("```suggestion")
    );

    let updated = task_row(&f, &task.id);
    assert_eq!(updated.status, TaskStatus::Done);
    assert_eq!(updated.fix.unwrap().status, FixStatus::Suggested);
}

#[tokio::test]
async fn decide_task_reject_resets_to_todo_with_a_reason() {
    let f = fixtures();
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;

    let mut task = sample_task("t-reject", &pr, None, None, f.clock.now());
    task.status = TaskStatus::Review;
    task.fix = Some(Fix {
        status: FixStatus::Ready,
        patch: None,
        commit: None,
    });
    put_task(&f, &task);

    core.decide_task(TaskDecision {
        task: task.id.clone(),
        accept: false,
        reason: Some("doesn't fix the root cause".into()),
    })
    .await
    .expect("decide_task reject");

    let updated = task_row(&f, &task.id);
    assert_eq!(updated.status, TaskStatus::Todo);
    assert!(updated.fix.is_none());

    let reason = f
        .store
        .kv_get(&format!("task:{}:reject_reason", task.id))
        .unwrap();
    assert_eq!(reason, Some(json!("doesn't fix the root cause")));
}

/// Wraps a `Store` whose `apply` fails for any action on the `task` model —
/// standing in for a crash or a disk error partway through accepting a
/// task, so the test can see whether the *other* half of the accept
/// (enqueuing `resolve_thread`) landed anyway.
struct FailingTaskApplyStore(Arc<testkit::FakeStore>);

impl Store for FailingTaskApplyStore {
    fn apply(
        &self,
        actions: Vec<NewAction>,
    ) -> theodore_ports::StoreResult<Vec<theodore_ports::SyncAction>> {
        if actions.iter().any(|a| a.model == models::TASK) {
            return Err(theodore_ports::StoreError::Db("simulated failure".into()));
        }
        self.0.apply(actions)
    }
    fn last_sync_id(&self) -> theodore_ports::StoreResult<u64> {
        self.0.last_sync_id()
    }
    fn since(
        &self,
        sync_id: u64,
        limit: u32,
    ) -> theodore_ports::StoreResult<Vec<theodore_ports::SyncAction>> {
        self.0.since(sync_id, limit)
    }
    fn get(
        &self,
        model: &str,
        id: &str,
    ) -> theodore_ports::StoreResult<Option<theodore_ports::ModelRow>> {
        self.0.get(model, id)
    }
    fn query(
        &self,
        model: &str,
        field: &str,
        value: &str,
    ) -> theodore_ports::StoreResult<Vec<theodore_ports::ModelRow>> {
        self.0.query(model, field, value)
    }
    fn all(&self, model: &str) -> theodore_ports::StoreResult<Vec<theodore_ports::ModelRow>> {
        self.0.all(model)
    }
    fn outbox_push(
        &self,
        op: &str,
        payload: serde_json::Value,
    ) -> theodore_ports::StoreResult<u64> {
        self.0.outbox_push(op, payload)
    }
    fn outbox_take(
        &self,
        now: i64,
    ) -> theodore_ports::StoreResult<Option<theodore_ports::OutboxItem>> {
        self.0.outbox_take(now)
    }
    fn outbox_done(&self, id: u64) -> theodore_ports::StoreResult<()> {
        self.0.outbox_done(id)
    }
    fn outbox_retry(
        &self,
        id: u64,
        not_before: i64,
        error: &str,
    ) -> theodore_ports::StoreResult<()> {
        self.0.outbox_retry(id, not_before, error)
    }
    fn outbox_fail(&self, id: u64, error: &str) -> theodore_ports::StoreResult<()> {
        self.0.outbox_fail(id, error)
    }
    fn kv_get(&self, key: &str) -> theodore_ports::StoreResult<Option<serde_json::Value>> {
        self.0.kv_get(key)
    }
    fn kv_set(&self, key: &str, value: &serde_json::Value) -> theodore_ports::StoreResult<()> {
        self.0.kv_set(key, value)
    }
}

#[tokio::test]
async fn accept_task_does_not_leave_an_orphaned_resolve_thread_outbox_item() {
    let f = fixtures();
    let pr_ref = fixture_pr_ref();
    let head = Sha::new("head111");
    let base = Sha::new("base000");
    let detail = testkit::sample_pr_detail(&pr_ref, &base, &head);
    f.host.push_get_pr(Ok(Fetched::Modified {
        etag: Some("etag-1".into()),
        value: detail,
    }));
    let mut deps = f.deps();
    deps.store = Arc::new(FailingTaskApplyStore(f.store.clone()));
    let (core, _recv) = Core::start(deps);
    let resp = core
        .open_pr(OpenPrRequest {
            url: "https://github.com/acme/widgets/pull/7".into(),
        })
        .await
        .expect("open_pr");
    let pr = resp.pr;

    let mut task = sample_task(
        "t-atomic",
        &pr,
        None,
        Some(ThreadId::new("thread-1")),
        f.clock.now(),
    );
    task.status = TaskStatus::Review;
    task.fix = Some(Fix {
        status: FixStatus::Ready,
        patch: None,
        commit: None,
    });
    put_task(&f, &task);

    let result = core
        .decide_task(TaskDecision {
            task: task.id.clone(),
            accept: true,
            reason: None,
        })
        .await;
    assert!(
        result.is_err(),
        "expected the simulated task write failure to surface, got {result:?}"
    );

    assert!(
        f.store
            .outbox_snapshot()
            .iter()
            .all(|i| i.op != "resolve_thread"),
        "a failed task write must not leave a resolve_thread outbox item behind"
    );
}

// ── agents.grouping setting ──────────────────────────────────────────────────

#[tokio::test]
async fn regroup_honours_the_agents_grouping_setting() {
    let f = fixtures();
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;

    let tx = Transaction {
        id: "set-grouping-agent".into(),
        ops: vec![MutationOp::Create {
            model: models::SETTING.into(),
            id: "agents.grouping".into(),
            data: json!({"key": "agents.grouping", "scope": "user", "value": "codex"}),
        }],
    };
    assert!(matches!(
        core.transact(tx).await,
        TransactionResult::Ok { .. }
    ));

    wait_grouping_idle(&f, &core, &pr).await;
    core.regroup(pr).await.expect("regroup");
    assert_eq!(f.grouper.last_agent(), Some(AgentId::Codex));
}

// ── grouping context (relatedness graph inputs) ──────────────────────────

/// The grouper gets what the app can read beyond the diff: the head file
/// contents of the changed files (from the worktree) and the co-change
/// history from git, restricted to the PR's paths with bulk commits gone.
#[tokio::test]
async fn grouping_context_reads_head_files_and_co_change_history() {
    let f = fixtures();
    let many: Vec<String> = (0..20).map(|i| format!("gen/{i}.rs")).collect();
    f.git.set_history(vec![
        vec!["src/lib.rs".into(), "src/other.rs".into()],
        many,
        vec!["README.md".into()],
    ]);
    let (core, _recv, _pr, head) = open_fixture_pr(&f).await;
    let wt = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(wt.path().join("src")).unwrap();
    std::fs::write(wt.path().join("src/lib.rs"), "mod other;\n").unwrap();
    let rev = testkit::two_hunk_revision(&Sha::new("base000"), &head, "src/lib.rs");

    let ctx = core.grouping_context(wt.path(), &rev).await;
    assert_eq!(
        ctx.head_files.get("src/lib.rs").map(String::as_str),
        Some("mod other;\n")
    );
    assert_eq!(ctx.co_change_commits, vec![vec!["src/lib.rs".to_string()]]);
}

#[tokio::test]
async fn regroup_hands_the_grouping_context_to_the_grouper() {
    let f = fixtures();
    f.git
        .set_history(vec![vec!["src/lib.rs".into(), "docs/x.md".into()]]);
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;
    wait_grouping_idle(&f, &core, &pr).await;
    core.regroup(pr).await.expect("regroup");
    let ctx = f.grouper.last_ctx().expect("grouper called");
    assert_eq!(ctx.co_change_commits, vec![vec!["src/lib.rs".to_string()]]);
}

// ── is_idle ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn is_idle_is_false_while_the_outbox_has_pending_items() {
    let f = fixtures();
    let (core, _recv, pr, head) = open_fixture_pr(&f).await;
    let revision = core
        .get_revision(RevisionRequest {
            pr: pr.clone(),
            head,
        })
        .await
        .unwrap();
    let hunks = testkit::hunk_ids(&revision);

    // Block the outbox on a network error so the item stays pending.
    f.host
        .push_set_viewed(Err(HostError::Network("down".into())));
    for (i, h) in hunks.iter().enumerate() {
        core.transact(hunk_read_tx(&format!("read-{i}"), &pr, h))
            .await;
    }
    // Wait for the failed first attempt to land (back to `Pending`, armed
    // with a backoff `not_before`) before advancing the clock — advancing
    // too early would compute the backoff from the already-advanced time.
    wait_until(|| {
        f.store
            .outbox_snapshot()
            .first()
            .is_some_and(|i| i.attempts >= 1)
    })
    .await;

    assert!(
        !core.is_idle().await,
        "a pending outbox item must not be idle"
    );

    f.host.push_set_viewed(Ok(()));
    f.clock.advance(10_000);
    wait_until(|| f.store.outbox_snapshot()[0].status == OutboxStatus::Done).await;
    assert!(core.is_idle().await);
}

// Hand-written host authentication status smoke check: `HostAuthStatus`'s
// shape, not behavior, to catch an accidental contract drift at the type
// level (the rest is covered by `host_status_reports_source_and_connection`).
#[test]
fn host_auth_status_is_constructible() {
    let _ = HostAuthStatus {
        host: HostKind::Github,
        connected: true,
        login: Some("me".into()),
        source: TokenSource::Gh,
        device_login_available: false,
    };
}

// ── projects ──────────────────────────────────────────────────────────────

async fn started_core(f: &Fixtures) -> (Core, Receivers) {
    let (core, recv) = Core::start(f.deps());
    // Let the one-shot startup inbox refresh (empty fixtures) drain first —
    // it shares `FakeHost`'s search queue with a project's own refresh.
    tokio::time::sleep(Duration::from_millis(30)).await;
    (core, recv)
}

async fn add_ready_local_project(
    core: &Core,
    f: &Fixtures,
    path: &str,
) -> theodore_contract::Project {
    let project = core
        .add_project(AddProjectRequest {
            repo: None,
            path: Some(path.into()),
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
    project
}

fn feature_branch_commit() -> CommitLog {
    CommitLog {
        sha: Sha::new("feat-head"),
        title: "feature work".into(),
        author_name: "Ada".into(),
        author_email: "ada@example.com".into(),
        at: 42_000,
    }
}

#[tokio::test]
async fn add_project_clones_a_host_repo_then_becomes_ready_and_refreshes_its_prs() {
    let f = fixtures();
    let (core, _recv) = started_core(&f).await;

    let repo = RepoRef {
        host: HostKind::Github,
        owner: "acme".into(),
        name: "widgets".into(),
    };
    let pr_ref = PrRef {
        repo: repo.clone(),
        number: 9,
        host_id: Some("node-9".into()),
    };
    f.host.push_search(Ok(vec![testkit::sample_pr_summary(
        &pr_ref,
        &Sha::new("head9"),
    )]));

    let project = core
        .add_project(AddProjectRequest {
            repo: Some(repo.clone()),
            path: None,
        })
        .await
        .expect("add_project");
    assert_eq!(project.id, "github:acme/widgets");
    assert_eq!(project.status, ProjectStatus::Cloning);

    wait_until(|| {
        f.store
            .get(models::PROJECT, "github:acme/widgets")
            .unwrap()
            .is_some_and(|r| r.data["status"] == json!("ready"))
    })
    .await;
    let row = f
        .store
        .get(models::PROJECT, "github:acme/widgets")
        .unwrap()
        .unwrap();
    assert_eq!(row.data["default_branch"], json!("main"));

    // The initial refresh after the clone wrote the repo's open PRs.
    wait_until(|| f.store.get(models::PR, &pr_ref.id().0).unwrap().is_some()).await;
}

#[tokio::test]
async fn add_project_rejects_a_folder_that_is_not_a_git_repository() {
    let f = fixtures();
    let (core, _recv) = started_core(&f).await;
    f.git.set_is_repo(false);

    let err = core
        .add_project(AddProjectRequest {
            repo: None,
            path: Some("/tmp/not-a-repo".into()),
        })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not a git repository"));
}

#[tokio::test]
async fn add_project_local_folder_dedupes_a_colliding_name() {
    let f = fixtures();
    let (core, _recv) = started_core(&f).await;

    let first = core
        .add_project(AddProjectRequest {
            repo: None,
            path: Some("/tmp/widgets".into()),
        })
        .await
        .expect("first add_project");
    assert_eq!(first.id, "local:widgets");
    assert_eq!(first.repo.owner, "local");
    assert_eq!(first.repo.name, "widgets");
    assert_eq!(first.path.as_deref(), Some("/tmp/widgets"));

    // A different folder that happens to share a basename gets a
    // de-duplicated id instead of colliding with the first.
    let second = core
        .add_project(AddProjectRequest {
            repo: None,
            path: Some("/elsewhere/widgets".into()),
        })
        .await
        .expect("second add_project");
    assert_eq!(second.id, "local:widgets-2");

    wait_until(|| {
        f.store
            .get(models::PROJECT, "local:widgets")
            .unwrap()
            .is_some_and(|r| r.data["status"] == json!("ready"))
    })
    .await;
    wait_until(|| {
        f.store
            .get(models::PROJECT, "local:widgets-2")
            .unwrap()
            .is_some_and(|r| r.data["status"] == json!("ready"))
    })
    .await;
}

#[tokio::test]
async fn remove_project_deletes_the_cache_but_never_the_local_folder() {
    let f = fixtures();
    let (core, _recv) = started_core(&f).await;
    let project = add_ready_local_project(&core, &f, "/tmp/my-folder").await;

    core.remove_project(&project.id)
        .await
        .expect("remove_project");

    assert!(f.store.get(models::PROJECT, &project.id).unwrap().is_none());
    assert_eq!(f.git.removed_repos(), vec![project.repo.clone()]);
}

#[tokio::test]
async fn list_branches_reads_the_mirror_for_a_local_project() {
    let f = fixtures();
    let (core, _recv) = started_core(&f).await;
    let project = add_ready_local_project(&core, &f, "/tmp/my-folder").await;

    f.git.set_branches(vec![
        BranchInfo {
            name: "main".into(),
            head: Sha::new("m1"),
            subject: "base".into(),
            author: "Ada".into(),
            ahead: 0,
            behind: 0,
            updated_at: 1,
        },
        BranchInfo {
            name: "feature".into(),
            head: Sha::new("f1"),
            subject: "feature work".into(),
            author: "Grace".into(),
            ahead: 2,
            behind: 1,
            updated_at: 2,
        },
    ]);

    let branches = core
        .list_branches(&project.id)
        .await
        .expect("list_branches");
    assert_eq!(branches.len(), 2);
    let feature = branches.iter().find(|b| b.name == "feature").unwrap();
    assert_eq!((feature.ahead, feature.behind), (2, 1));
    assert_eq!(feature.subject, "feature work");
    assert_eq!(feature.author, "Grace");
}

#[tokio::test]
async fn refresh_project_fetches_merged_and_closed_prs_and_fills_host_metadata() {
    let f = fixtures();
    let (core, _recv) = started_core(&f).await;

    let repo = RepoRef {
        host: HostKind::Github,
        owner: "acme".into(),
        name: "widgets".into(),
    };
    // Drain the clone's own first refresh with empty results so the
    // assertions below only see the explicit `refresh_project` call.
    f.host.push_search(Ok(vec![]));
    f.host.push_search(Ok(vec![]));
    f.host.push_search(Ok(vec![]));
    let project = core
        .add_project(AddProjectRequest {
            repo: Some(repo.clone()),
            path: None,
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

    let open_ref = PrRef {
        repo: repo.clone(),
        number: 1,
        host_id: Some("n1".into()),
    };
    let merged_ref = PrRef {
        repo: repo.clone(),
        number: 2,
        host_id: Some("n2".into()),
    };
    let closed_ref = PrRef {
        repo: repo.clone(),
        number: 3,
        host_id: Some("n3".into()),
    };
    f.host.push_search(Ok(vec![testkit::sample_pr_summary(
        &open_ref,
        &Sha::new("h1"),
    )]));
    f.host.push_search(Ok(vec![testkit::sample_pr_summary(
        &merged_ref,
        &Sha::new("h2"),
    )]));
    f.host.push_search(Ok(vec![testkit::sample_pr_summary(
        &closed_ref,
        &Sha::new("h3"),
    )]));
    f.host.push_repo_info(Ok(HostRepo {
        repo: repo.clone(),
        description: Some("Widgets".into()),
        private: true,
        pushed_at: Some(123),
    }));

    core.refresh_project(&project.id)
        .await
        .expect("refresh_project");

    wait_until(|| f.store.get(models::PR, &open_ref.id().0).unwrap().is_some()).await;
    assert!(
        f.store
            .get(models::PR, &merged_ref.id().0)
            .unwrap()
            .is_some()
    );
    assert!(
        f.store
            .get(models::PR, &closed_ref.id().0)
            .unwrap()
            .is_some()
    );

    let row = f.store.get(models::PROJECT, &project.id).unwrap().unwrap();
    assert_eq!(row.data["description"], json!("Widgets"));
    assert_eq!(row.data["private"], json!(true));
    assert_eq!(row.data["pushed_at"], json!(123));
    assert!(row.data["last_fetched_at"].as_i64().unwrap() > 0);
}

#[tokio::test]
async fn refresh_project_without_repo_info_support_keeps_the_rest_of_the_refresh() {
    let f = fixtures();
    let (core, _recv) = started_core(&f).await;
    let project = add_ready_local_project(&core, &f, "/tmp/my-folder").await;

    // A local project never calls the host at all; refresh still stamps
    // `last_fetched_at` and succeeds.
    core.refresh_project(&project.id)
        .await
        .expect("refresh_project");
    let row = f.store.get(models::PROJECT, &project.id).unwrap().unwrap();
    assert!(row.data["last_fetched_at"].as_i64().unwrap() > 0);
    assert_eq!(row.data["description"], json!(null));
}

#[tokio::test]
async fn open_branch_opens_a_review_with_no_host_pr() {
    let f = fixtures();
    let (core, _recv) = started_core(&f).await;
    let project = add_ready_local_project(&core, &f, "/tmp/my-folder").await;

    f.git.set_resolve("feature", Sha::new("feat-head"));
    f.git.set_resolve("main", Sha::new("main-base"));
    f.git
        .set_commit(Sha::new("feat-head"), feature_branch_commit());
    f.git.set_commit_log(vec![feature_branch_commit()]);

    let resp = core
        .open_branch(OpenBranchRequest {
            project: project.id.clone(),
            branch: "feature".into(),
            base: None,
        })
        .await
        .expect("open_branch");
    assert_eq!(resp.head.as_str(), "feat-head");

    let pr_row = f
        .store
        .get(models::PR, &resp.pr.0)
        .unwrap()
        .expect("pr row");
    assert_eq!(pr_row.data["head_ref"], json!("feature"));
    assert_eq!(pr_row.data["base_ref"], json!("main"));
    assert_eq!(pr_row.data["state"], json!("open"));
    assert_eq!(pr_row.data["url"], json!(""));

    let detail_row = f
        .store
        .get(models::PR_DETAIL, &resp.pr.0)
        .unwrap()
        .expect("pr_detail row");
    assert_eq!(detail_row.data["body"], json!(""));
    assert_eq!(detail_row.data["commits"].as_array().unwrap().len(), 1);

    // Reopening the same branch is idempotent — keyed by the stable hash
    // of its name, it returns the same PR without re-running the flow.
    let again = core
        .open_branch(OpenBranchRequest {
            project: project.id.clone(),
            branch: "feature".into(),
            base: None,
        })
        .await
        .expect("open_branch again");
    assert_eq!(again.pr, resp.pr);
}

#[tokio::test]
async fn ensure_open_recovers_a_local_pr_after_a_restart() {
    let f = fixtures();
    let (core, _recv) = started_core(&f).await;
    let project = add_ready_local_project(&core, &f, "/tmp/my-folder").await;

    f.git.set_resolve("feature", Sha::new("feat-head"));
    f.git.set_resolve("main", Sha::new("main-base"));
    f.git
        .set_commit(Sha::new("feat-head"), feature_branch_commit());
    f.git.set_commit_log(vec![feature_branch_commit()]);

    let resp = core
        .open_branch(OpenBranchRequest {
            project: project.id.clone(),
            branch: "feature".into(),
            base: None,
        })
        .await
        .expect("open_branch");

    // A fresh `Core` over the same store/git has nothing cached in memory;
    // `ensure_open` must recover the branch from the stored `pr` row.
    let before = f.git.mirror_calls().len();
    let (core2, _recv2) = started_core(&f).await;
    core2.ensure_open(&resp.pr).await.expect("ensure_open");
    assert!(f.git.mirror_calls().len() > before);

    let revision = core2
        .get_revision(RevisionRequest {
            pr: resp.pr.clone(),
            head: resp.head.clone(),
        })
        .await
        .expect("get_revision after recovery");
    assert!(!revision.files.is_empty());
}

// ── merge / reviewers / draft / update_branch / react / edit_comment ───────

#[tokio::test]
async fn merge_pr_merges_now_by_default() {
    let f = fixtures();
    let (core, _recv, pr, head) = open_fixture_pr(&f).await;
    f.host.push_get_pr(Ok(Fetched::Modified {
        etag: Some("e".into()),
        value: testkit::sample_pr_detail(&fixture_pr_ref(), &Sha::new("base000"), &head),
    }));

    core.merge_pr(MergeRequest {
        pr: pr.clone(),
        method: WireMergeMethod::Squash,
        title: Some("t".into()),
        body: None,
        auto: false,
        queue: false,
    })
    .await
    .expect("merge_pr");

    wait_until(|| {
        f.store
            .outbox_snapshot()
            .iter()
            .any(|i| i.op == "merge_pr" && i.status == OutboxStatus::Done)
    })
    .await;
    let calls = f.host.merge_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, PortMergeMethod::Squash);
    assert_eq!(calls[0].1, Some("t".to_string()));
}

#[tokio::test]
async fn merge_pr_auto_enables_auto_merge_instead_of_merging() {
    let f = fixtures();
    let (core, _recv, pr, head) = open_fixture_pr(&f).await;
    f.host.push_get_pr(Ok(Fetched::Modified {
        etag: Some("e".into()),
        value: testkit::sample_pr_detail(&fixture_pr_ref(), &Sha::new("base000"), &head),
    }));

    core.merge_pr(MergeRequest {
        pr,
        method: WireMergeMethod::Merge,
        title: None,
        body: None,
        auto: true,
        queue: false,
    })
    .await
    .expect("merge_pr");

    wait_until(|| {
        f.store
            .outbox_snapshot()
            .iter()
            .any(|i| i.op == "merge_pr" && i.status == OutboxStatus::Done)
    })
    .await;
    assert!(
        f.host.merge_calls().is_empty(),
        "must not call merge directly"
    );
}

#[tokio::test]
async fn merge_pr_on_a_local_pr_rejects_synchronously() {
    let f = fixtures();
    let (core, _recv) = started_core(&f).await;
    let project = add_ready_local_project(&core, &f, "/tmp/my-folder").await;
    f.git.set_resolve("feature", Sha::new("feat-head"));
    f.git.set_resolve("main", Sha::new("main-base"));
    f.git
        .set_commit(Sha::new("feat-head"), feature_branch_commit());
    f.git.set_commit_log(vec![feature_branch_commit()]);
    let resp = core
        .open_branch(OpenBranchRequest {
            project: project.id.clone(),
            branch: "feature".into(),
            base: None,
        })
        .await
        .expect("open_branch");

    let err = core
        .merge_pr(MergeRequest {
            pr: resp.pr,
            method: WireMergeMethod::Merge,
            title: None,
            body: None,
            auto: false,
            queue: false,
        })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("local branch review"));
    assert!(f.store.outbox_snapshot().is_empty());
}

#[tokio::test]
async fn merge_info_maps_the_hosts_status() {
    let f = fixtures();
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;
    f.host.push_merge_status(Ok(MergeStatus {
        mergeable: Some(true),
        state: MergeState::Clean,
        allowed_methods: vec![PortMergeMethod::Squash],
        auto_merge_enabled: false,
        in_queue: false,
        required_approvals: Some(2),
        auto_merge_allowed: true,
        merge_queue: false,
    }));

    let info = core.merge_info(pr).await.expect("merge_info");
    assert_eq!(info.mergeable, Some(true));
    assert_eq!(info.required_approvals, Some(2));
    assert_eq!(info.allowed_methods, vec![WireMergeMethod::Squash]);
}

#[tokio::test]
async fn assignable_users_reads_through_to_the_host() {
    let f = fixtures();
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;
    f.host
        .push_assignable_users(Ok(vec![testkit::sample_person("bob")]));

    let users = core.assignable_users(pr).await.expect("assignable_users");
    assert_eq!(users.len(), 1);
    assert_eq!(users[0].login, "bob");
}

#[tokio::test]
async fn set_reviewers_enqueues_adds_and_removes_in_one_outbox_item() {
    let f = fixtures();
    let (core, _recv, pr, head) = open_fixture_pr(&f).await;
    f.host.push_get_pr(Ok(Fetched::Modified {
        etag: Some("e".into()),
        value: testkit::sample_pr_detail(&fixture_pr_ref(), &Sha::new("base000"), &head),
    }));

    core.set_reviewers(ReviewersRequest {
        pr,
        add: vec!["carol".into()],
        add_teams: vec!["core".into()],
        remove: vec!["dave".into()],
    })
    .await
    .expect("set_reviewers");

    wait_until(|| {
        f.store
            .outbox_snapshot()
            .iter()
            .any(|i| i.op == "set_reviewers" && i.status == OutboxStatus::Done)
    })
    .await;
    assert_eq!(
        f.host.reviewer_calls(),
        vec![(vec!["carol".to_string()], vec!["core".to_string()])]
    );
    assert_eq!(f.host.remove_reviewer_calls(), vec!["dave".to_string()]);
}

// ── rerun_check / triage ────────────────────────────────────────────────

#[tokio::test]
async fn rerun_check_resolves_the_check_from_pr_detail_and_calls_the_host() {
    let f = fixtures();
    let head = Sha::new("head111");
    let mut detail = testkit::sample_pr_detail(&fixture_pr_ref(), &Sha::new("base000"), &head);
    detail.checks = vec![Check {
        id: "900001".into(),
        name: "build".into(),
        status: CheckStatus::Fail,
        url: None,
        duration_ms: None,
    }];
    f.host.push_get_pr(Ok(Fetched::Modified {
        etag: Some("etag-1".into()),
        value: detail,
    }));
    let (core, _recv) = Core::start(f.deps());
    let resp = core
        .open_pr(OpenPrRequest {
            url: "https://github.com/acme/widgets/pull/7".into(),
        })
        .await
        .expect("open_pr");
    let pr = resp.pr;
    // `rerun_check` itself refreshes `pr_detail` on success too.
    f.host.push_get_pr(Ok(Fetched::NotModified));

    core.rerun_check(RerunCheckRequest {
        pr: pr.clone(),
        check: "900001".into(),
    })
    .await
    .expect("rerun_check");

    wait_until(|| {
        f.store
            .outbox_snapshot()
            .iter()
            .any(|i| i.op == "rerun_check" && i.status == OutboxStatus::Done)
    })
    .await;
    assert_eq!(f.host.rerun_check_calls(), vec!["900001".to_string()]);
}

#[tokio::test]
async fn rerun_check_fails_when_the_check_id_is_unknown() {
    let f = fixtures();
    let (core, _recv, pr, head) = open_fixture_pr(&f).await;
    f.host.push_get_pr(Ok(Fetched::Modified {
        etag: Some("e".into()),
        value: testkit::sample_pr_detail(&fixture_pr_ref(), &Sha::new("base000"), &head),
    }));

    core.rerun_check(RerunCheckRequest {
        pr: pr.clone(),
        check: "missing".into(),
    })
    .await
    .expect("enqueues even though the check doesn't resolve until it runs");

    wait_until(|| {
        f.store
            .outbox_snapshot()
            .iter()
            .any(|i| i.op == "rerun_check" && i.status == OutboxStatus::Failed)
    })
    .await;
    assert!(f.host.rerun_check_calls().is_empty());
}

#[tokio::test]
async fn triage_take_requests_review_from_the_viewer_and_drops_team_requests() {
    let f = fixtures();
    let head = Sha::new("head111");
    let mut detail = testkit::sample_pr_detail(&fixture_pr_ref(), &Sha::new("base000"), &head);
    detail.reviewers = vec![theodore_domain::Reviewer {
        person: testkit::sample_person("core-team"),
        state: theodore_domain::ReviewerState::Requested,
        team: Some("core-team".into()),
    }];
    f.host.push_get_pr(Ok(Fetched::Modified {
        etag: Some("etag-1".into()),
        value: detail,
    }));
    let (core, _recv) = Core::start(f.deps());
    let resp = core
        .open_pr(OpenPrRequest {
            url: "https://github.com/acme/widgets/pull/7".into(),
        })
        .await
        .expect("open_pr");
    let pr = resp.pr;
    f.host.push_viewer(Ok(testkit::sample_person("me")));
    f.host.push_get_pr(Ok(Fetched::NotModified));

    core.triage(TriageRequest {
        pr: pr.clone(),
        action: TriageAction::Take,
    })
    .await
    .expect("triage take");

    wait_until(|| {
        f.store
            .outbox_snapshot()
            .iter()
            .any(|i| i.op == "triage" && i.status == OutboxStatus::Done)
    })
    .await;
    assert_eq!(
        f.host.reviewer_calls(),
        vec![(vec!["me".to_string()], vec![])]
    );
    assert_eq!(
        f.host.remove_reviewer_calls(),
        vec!["core-team".to_string()]
    );
}

#[tokio::test]
async fn triage_decline_removes_the_viewer_and_marks_the_inbox_row_done() {
    let f = fixtures();
    let (core, _recv, pr, head) = open_fixture_pr(&f).await;
    f.host.push_get_pr(Ok(Fetched::Modified {
        etag: Some("e".into()),
        value: testkit::sample_pr_detail(&fixture_pr_ref(), &Sha::new("base000"), &head),
    }));
    f.host.push_viewer(Ok(testkit::sample_person("me")));
    f.host.push_get_pr(Ok(Fetched::NotModified));
    core.apply(vec![NewAction {
        model: models::INBOX_STATE.into(),
        model_id: pr.0.clone(),
        action: SyncActionCode::I,
        data: Some(
            serde_json::to_value(InboxState {
                pr: pr.clone(),
                done: false,
                snooze: None,
            })
            .unwrap(),
        ),
    }])
    .await
    .expect("seed inbox_state");

    core.triage(TriageRequest {
        pr: pr.clone(),
        action: TriageAction::Decline,
    })
    .await
    .expect("triage decline");

    wait_until(|| {
        f.store
            .outbox_snapshot()
            .iter()
            .any(|i| i.op == "triage" && i.status == OutboxStatus::Done)
    })
    .await;
    assert_eq!(f.host.remove_reviewer_calls(), vec!["me".to_string()]);
    let row = f
        .store
        .get(models::INBOX_STATE, &pr.0)
        .unwrap()
        .expect("inbox_state row");
    assert_eq!(row.data.get("done"), Some(&json!(true)));
}

#[tokio::test]
async fn triage_reassign_requests_the_given_users_and_drops_the_viewer() {
    let f = fixtures();
    let (core, _recv, pr, head) = open_fixture_pr(&f).await;
    f.host.push_get_pr(Ok(Fetched::Modified {
        etag: Some("e".into()),
        value: testkit::sample_pr_detail(&fixture_pr_ref(), &Sha::new("base000"), &head),
    }));
    f.host.push_viewer(Ok(testkit::sample_person("me")));
    f.host.push_get_pr(Ok(Fetched::NotModified));

    core.triage(TriageRequest {
        pr: pr.clone(),
        action: TriageAction::Reassign {
            to: vec!["carol".into()],
        },
    })
    .await
    .expect("triage reassign");

    wait_until(|| {
        f.store
            .outbox_snapshot()
            .iter()
            .any(|i| i.op == "triage" && i.status == OutboxStatus::Done)
    })
    .await;
    assert_eq!(
        f.host.reviewer_calls(),
        vec![(vec!["carol".to_string()], vec![])]
    );
    assert_eq!(f.host.remove_reviewer_calls(), vec!["me".to_string()]);
}

#[tokio::test]
async fn triage_on_a_local_pr_rejects_synchronously() {
    let f = fixtures();
    let (core, _recv) = started_core(&f).await;
    let project = add_ready_local_project(&core, &f, "/tmp/my-folder").await;
    f.git.set_resolve("feature", Sha::new("feat-head"));
    f.git.set_resolve("main", Sha::new("main-base"));
    f.git
        .set_commit(Sha::new("feat-head"), feature_branch_commit());
    f.git.set_commit_log(vec![feature_branch_commit()]);
    let resp = core
        .open_branch(OpenBranchRequest {
            project: project.id.clone(),
            branch: "feature".into(),
            base: None,
        })
        .await
        .expect("open_branch");

    let err = core
        .triage(TriageRequest {
            pr: resp.pr,
            action: TriageAction::Decline,
        })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("local branch review"));
    assert!(f.store.outbox_snapshot().is_empty());
}

#[tokio::test]
async fn set_draft_converts_to_draft_and_back_to_ready() {
    let f = fixtures();
    let (core, _recv, pr, head) = open_fixture_pr(&f).await;
    for _ in 0..2 {
        f.host.push_get_pr(Ok(Fetched::Modified {
            etag: Some("e".into()),
            value: testkit::sample_pr_detail(&fixture_pr_ref(), &Sha::new("base000"), &head),
        }));
    }

    core.set_draft(DraftStateRequest {
        pr: pr.clone(),
        draft: true,
    })
    .await
    .expect("set_draft true");
    wait_until(|| {
        f.store
            .outbox_snapshot()
            .iter()
            .filter(|i| i.op == "set_draft" && i.status == OutboxStatus::Done)
            .count()
            == 1
    })
    .await;

    core.set_draft(DraftStateRequest { pr, draft: false })
        .await
        .expect("set_draft false");
    wait_until(|| {
        f.store
            .outbox_snapshot()
            .iter()
            .filter(|i| i.op == "set_draft" && i.status == OutboxStatus::Done)
            .count()
            == 2
    })
    .await;
}

#[tokio::test]
async fn update_branch_enqueues_and_refreshes() {
    let f = fixtures();
    let (core, _recv, pr, head) = open_fixture_pr(&f).await;
    f.host.push_get_pr(Ok(Fetched::Modified {
        etag: Some("e".into()),
        value: testkit::sample_pr_detail(&fixture_pr_ref(), &Sha::new("base000"), &head),
    }));

    core.update_branch(pr).await.expect("update_branch");
    wait_until(|| {
        f.store
            .outbox_snapshot()
            .iter()
            .any(|i| i.op == "update_branch" && i.status == OutboxStatus::Done)
    })
    .await;
}

#[tokio::test]
async fn react_adds_and_removes_a_reaction() {
    let f = fixtures();
    let (core, _recv, pr, head) = open_fixture_pr(&f).await;
    for _ in 0..2 {
        f.host.push_get_pr(Ok(Fetched::Modified {
            etag: Some("e".into()),
            value: testkit::sample_pr_detail(&fixture_pr_ref(), &Sha::new("base000"), &head),
        }));
    }

    core.react(ReactRequest {
        pr: pr.clone(),
        comment: CommentId::new("c1"),
        emoji: "+1".into(),
        on: true,
    })
    .await
    .expect("react on");
    core.react(ReactRequest {
        pr,
        comment: CommentId::new("c1"),
        emoji: "+1".into(),
        on: false,
    })
    .await
    .expect("react off");

    wait_until(|| {
        f.store
            .outbox_snapshot()
            .iter()
            .filter(|i| i.op == "react" && i.status == OutboxStatus::Done)
            .count()
            == 2
    })
    .await;
}

#[tokio::test]
async fn edit_comment_edits_or_deletes_depending_on_body() {
    let f = fixtures();
    let (core, _recv, pr, head) = open_fixture_pr(&f).await;
    for _ in 0..2 {
        f.host.push_get_pr(Ok(Fetched::Modified {
            etag: Some("e".into()),
            value: testkit::sample_pr_detail(&fixture_pr_ref(), &Sha::new("base000"), &head),
        }));
    }

    core.edit_comment(EditCommentRequest {
        pr: pr.clone(),
        comment: CommentId::new("c1"),
        body: Some("edited".into()),
    })
    .await
    .expect("edit");
    core.edit_comment(EditCommentRequest {
        pr,
        comment: CommentId::new("c1"),
        body: None,
    })
    .await
    .expect("delete");

    wait_until(|| {
        f.store
            .outbox_snapshot()
            .iter()
            .filter(|i| i.op == "edit_comment" && i.status == OutboxStatus::Done)
            .count()
            == 2
    })
    .await;
}

// ── commit_suggestions ───────────────────────────────────────────────────────

fn suggestion_comment(id: &str, body: &str) -> Comment {
    Comment {
        id: CommentId::new(id),
        author: testkit::sample_person("author"),
        body: body.into(),
        created_at: 0,
        edited: false,
        reactions: Vec::new(),
    }
}

fn suggestion_anchor(path: &str, line: u32, start_line: Option<u32>) -> Anchor {
    Anchor {
        rev: Sha::new("head111"),
        path: path.into(),
        side: Side::New,
        line,
        start_line,
        start_side: None,
    }
}

#[tokio::test]
async fn commit_suggestions_applies_and_pushes_one_commit_on_the_authors_own_pr() {
    let f = fixtures();
    // The viewer is the PR's own author, so suggestions may be committed
    // directly instead of drafted.
    f.host.push_viewer(Ok(testkit::sample_person("author")));
    let (core, _recv, pr, head) = open_fixture_pr(&f).await;

    std::fs::create_dir_all(f.git.worktree_path(&head)).unwrap();
    let note_path = f.git.worktree_path(&head).join("note.txt");
    std::fs::write(&note_path, "one\ntwo\nthree\n").unwrap();

    let thread = Thread {
        id: ThreadId::new("t1"),
        anchor: Some(suggestion_anchor("note.txt", 2, None)),
        resolved: false,
        outdated: false,
        comments: vec![suggestion_comment("c1", "```suggestion\nTWO FIXED\n```")],
    };
    let mut detail = testkit::sample_pr_detail(&fixture_pr_ref(), &Sha::new("base000"), &head);
    detail.threads = vec![thread];
    f.store
        .apply(vec![NewAction {
            model: models::PR_DETAIL.into(),
            model_id: pr.0.clone(),
            action: SyncActionCode::U,
            data: Some(serde_json::to_value(&detail).unwrap()),
        }])
        .unwrap();

    core.commit_suggestions(CommitSuggestionsRequest {
        pr,
        comments: vec![CommentId::new("c1")],
        message: "apply suggestion".into(),
    })
    .await
    .expect("commit_suggestions");

    let content = std::fs::read_to_string(&note_path).unwrap();
    assert_eq!(content, "one\nTWO FIXED\nthree\n");
    let push = f.git.last_push.lock().unwrap().clone().expect("pushed");
    assert_eq!(push.0, "feature");
    assert_eq!(push.1, "apply suggestion");
}

#[tokio::test]
async fn commit_suggestions_rejects_on_someone_elses_pr() {
    let f = fixtures();
    // Default viewer is "me"; the fixture PR's author is "author" — not own.
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;

    let err = core
        .commit_suggestions(CommitSuggestionsRequest {
            pr,
            comments: vec![CommentId::new("c1")],
            message: "m".into(),
        })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("own PR"));
}

#[tokio::test]
async fn commit_suggestions_rejects_overlapping_ranges() {
    let f = fixtures();
    f.host.push_viewer(Ok(testkit::sample_person("author")));
    let (core, _recv, pr, head) = open_fixture_pr(&f).await;

    // The overlap check runs before any file is touched, so this test
    // never needs `note.txt` to exist on disk.
    let thread1 = Thread {
        id: ThreadId::new("t1"),
        anchor: Some(suggestion_anchor("note.txt", 3, Some(1))),
        resolved: false,
        outdated: false,
        comments: vec![suggestion_comment("c1", "```suggestion\nX\n```")],
    };
    let thread2 = Thread {
        id: ThreadId::new("t2"),
        anchor: Some(suggestion_anchor("note.txt", 2, None)),
        resolved: false,
        outdated: false,
        comments: vec![suggestion_comment("c2", "```suggestion\nY\n```")],
    };
    let mut detail = testkit::sample_pr_detail(&fixture_pr_ref(), &Sha::new("base000"), &head);
    detail.threads = vec![thread1, thread2];
    f.store
        .apply(vec![NewAction {
            model: models::PR_DETAIL.into(),
            model_id: pr.0.clone(),
            action: SyncActionCode::U,
            data: Some(serde_json::to_value(&detail).unwrap()),
        }])
        .unwrap();

    let err = core
        .commit_suggestions(CommitSuggestionsRequest {
            pr,
            comments: vec![CommentId::new("c1"), CommentId::new("c2")],
            message: "m".into(),
        })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("overlapping"));
}

// ── get_skill / save_skill ───────────────────────────────────────────────────

#[tokio::test]
async fn get_skill_returns_builtins_read_only() {
    let f = fixtures();
    let (core, _recv) = started_core(&f).await;
    let body = core.get_skill("bugs", None).await.expect("get_skill");
    assert!(!body.editable);
    assert_eq!(body.info.source, "built-in");
    assert!(!body.body.is_empty());
}

#[tokio::test]
async fn get_skill_errors_for_an_unknown_id_with_no_pr() {
    let f = fixtures();
    let (core, _recv) = started_core(&f).await;
    assert!(core.get_skill("no-such-skill", None).await.is_err());
}

#[tokio::test]
async fn save_skill_user_scope_writes_to_the_home_skills_dir() {
    let f = fixtures();
    let (core, _recv) = started_core(&f).await;

    let info = core
        .save_skill(SaveSkillRequest {
            id: "my-skill".into(),
            scope: "user".into(),
            pr: None,
            body: "---\nname: my-skill\ndescription: d\n---\nbody".into(),
        })
        .await
        .expect("save_skill");
    assert_eq!(info.source, "~/.claude/skills");

    let saved = core
        .get_skill("my-skill", None)
        .await
        .expect("get_skill after save");
    assert!(saved.editable);
    assert_eq!(saved.info.description, "d");
}

#[tokio::test]
async fn a_forked_built_in_skill_replaces_the_built_in() {
    let f = fixtures();
    let (core, _recv) = started_core(&f).await;
    core.save_skill(SaveSkillRequest {
        id: "bugs".into(),
        scope: "user".into(),
        pr: None,
        body: "---\nname: bugs\ndescription: mine\n---\nmy version".into(),
    })
    .await
    .expect("fork");

    let got = core.get_skill("bugs", None).await.expect("get");
    assert!(got.editable, "your copy, not the read-only built-in");
    assert_eq!(
        got.body.trim_end(),
        "---\nname: bugs\ndescription: mine\n---\nmy version"
    );

    let list = core.list_skills(None).await.expect("list");
    let bugs: Vec<_> = list.iter().filter(|s| s.id == "bugs").collect();
    assert_eq!(bugs.len(), 1);
    assert_eq!(bugs[0].source, "~/.claude/skills");
}

#[tokio::test]
async fn save_skill_repo_scope_writes_into_the_worktree_and_toasts() {
    let f = fixtures();
    let (core, mut recv, pr, _head) = open_fixture_pr(&f).await;

    let info = core
        .save_skill(SaveSkillRequest {
            id: "repo-skill".into(),
            scope: "repo".into(),
            pr: Some(pr.clone()),
            body: "---\nname: repo-skill\ndescription: d\n---\nbody".into(),
        })
        .await
        .expect("save_skill");
    assert!(info.source.contains(".claude/skills"));

    let event = expect_ephemeral(&mut recv, |e| matches!(e, Ephemeral::Toast { .. })).await;
    assert!(matches!(
        event,
        Ephemeral::Toast {
            level: ToastLevel::Info,
            ..
        }
    ));

    let saved = core
        .get_skill("repo-skill", Some(pr))
        .await
        .expect("get_skill after repo save");
    assert!(saved.editable);
}

// ── budgets (dropped) ─────────────────────────────────────────────────────────

fn put_setting(f: &Fixtures, key: &str, scope: &str, value: serde_json::Value) {
    let setting = Setting {
        key: key.into(),
        scope: scope.into(),
        value,
    };
    f.store
        .apply(vec![NewAction {
            model: models::SETTING.into(),
            model_id: format!("{scope}:{key}"),
            action: SyncActionCode::I,
            data: Some(serde_json::to_value(&setting).unwrap()),
        }])
        .unwrap();
}

/// Budgets were dropped (owner, 2026-10-22): a `budget.per_pr_usd` row
/// left over from an older version, already exceeded, never blocks a run.
#[tokio::test]
async fn a_leftover_budget_setting_no_longer_blocks_a_run() {
    let f = fixtures();
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;
    put_setting(&f, "budget.per_pr_usd", "user", json!(1.0));
    put_setting(&f, "budget.per_pr_usd", "repo:acme/widgets", json!(1.0));
    f.store
        .apply(vec![NewAction {
            model: models::SKILL_RUN.into(),
            model_id: "run-1".into(),
            action: SyncActionCode::I,
            // A row from before spend was dropped, still carrying usage.
            data: Some(json!({
                "id": "run-1", "pr": pr, "skill": "bugs", "agents": ["claude"],
                "status": "done", "usage": { "input_tokens": 0, "output_tokens": 0, "usd": 2.0 },
                "error": null, "started_at": 0,
            })),
        }])
        .unwrap();

    core.run_skill(RunSkillRequest {
        pr,
        skill: "bugs".into(),
        agents: vec![],
    })
    .await
    .expect("over an old budget, the run still starts");
}

// ── repo settings (.review/config.yaml) ──────────────────────────────────────

#[tokio::test]
async fn repo_settings_yaml_overrides_the_user_scoped_agent_setting() {
    let f = fixtures();
    let head = Sha::new("repo-settings-head");
    let base = Sha::new("repo-settings-base");
    f.git.set_head(head.clone());
    let pr_ref = fixture_pr_ref();
    let detail = testkit::sample_pr_detail(&pr_ref, &base, &head);
    f.host.push_get_pr(Ok(Fetched::Modified {
        etag: Some("etag-repo-settings".into()),
        value: detail,
    }));

    // The repo layer is read at the PR's base (docs/config.md "Which
    // config"), as committed.
    f.git.put_file(
        &base,
        ".review/config.yaml",
        b"agents:\n  grouping: codex\nbudget:\n  per_pr_usd: 3.5\n",
    );

    let (core, _recv) = Core::start(f.deps());
    core.open_pr(OpenPrRequest {
        url: "https://github.com/acme/widgets/pull/7".into(),
    })
    .await
    .expect("open_pr");

    let rows = f
        .store
        .query(models::SETTING, "key", "agents.grouping")
        .unwrap();
    let repo_row = rows
        .iter()
        .find(|r| r.data["scope"] == json!("repo:acme/widgets"))
        .expect("repo-scoped agents.grouping row");
    assert_eq!(repo_row.data["value"], json!("codex"));

    // Budgets are gone: a legacy file's budget key is ignored.
    let budget_rows = f
        .store
        .query(models::SETTING, "key", "budget.per_pr_usd")
        .unwrap();
    assert!(budget_rows.is_empty(), "{budget_rows:?}");

    wait_until(|| f.grouper.last_agent() == Some(AgentId::Codex)).await;
}

// ── bootstrap must not block the async executor ──────────────────────────

/// Wraps a `Store` whose `last_sync_id` blocks the calling thread for a
/// while, standing in for a slow disk. If `bootstrap` ever calls it without
/// `spawn_blocking`, that block lands on the async executor thread itself.
struct SlowLastSyncId(Arc<testkit::FakeStore>);

/// How long `SlowLastSyncId::last_sync_id` blocks its calling thread for —
/// comfortably longer than the measurement window below so a stolen async
/// worker thread shows up unmistakably.
const SLOW_STORE_BLOCK: Duration = Duration::from_millis(1000);

impl Store for SlowLastSyncId {
    fn apply(
        &self,
        actions: Vec<NewAction>,
    ) -> theodore_ports::StoreResult<Vec<theodore_ports::SyncAction>> {
        self.0.apply(actions)
    }
    fn last_sync_id(&self) -> theodore_ports::StoreResult<u64> {
        std::thread::sleep(SLOW_STORE_BLOCK);
        self.0.last_sync_id()
    }
    fn since(
        &self,
        sync_id: u64,
        limit: u32,
    ) -> theodore_ports::StoreResult<Vec<theodore_ports::SyncAction>> {
        self.0.since(sync_id, limit)
    }
    fn get(
        &self,
        model: &str,
        id: &str,
    ) -> theodore_ports::StoreResult<Option<theodore_ports::ModelRow>> {
        self.0.get(model, id)
    }
    fn query(
        &self,
        model: &str,
        field: &str,
        value: &str,
    ) -> theodore_ports::StoreResult<Vec<theodore_ports::ModelRow>> {
        self.0.query(model, field, value)
    }
    fn all(&self, model: &str) -> theodore_ports::StoreResult<Vec<theodore_ports::ModelRow>> {
        self.0.all(model)
    }
    fn outbox_push(
        &self,
        op: &str,
        payload: serde_json::Value,
    ) -> theodore_ports::StoreResult<u64> {
        self.0.outbox_push(op, payload)
    }
    fn outbox_take(
        &self,
        now: i64,
    ) -> theodore_ports::StoreResult<Option<theodore_ports::OutboxItem>> {
        self.0.outbox_take(now)
    }
    fn outbox_done(&self, id: u64) -> theodore_ports::StoreResult<()> {
        self.0.outbox_done(id)
    }
    fn outbox_retry(
        &self,
        id: u64,
        not_before: i64,
        error: &str,
    ) -> theodore_ports::StoreResult<()> {
        self.0.outbox_retry(id, not_before, error)
    }
    fn outbox_fail(&self, id: u64, error: &str) -> theodore_ports::StoreResult<()> {
        self.0.outbox_fail(id, error)
    }
    fn kv_get(&self, key: &str) -> theodore_ports::StoreResult<Option<serde_json::Value>> {
        self.0.kv_get(key)
    }
    fn kv_set(&self, key: &str, value: &serde_json::Value) -> theodore_ports::StoreResult<()> {
        self.0.kv_set(key, value)
    }
}

#[tokio::test(flavor = "current_thread")]
async fn bootstrap_does_not_block_the_executor_on_a_slow_store() {
    // `current_thread`: the whole runtime (task polling + the timer that
    // drives `tokio::time::sleep`) runs on exactly one OS thread. If
    // `bootstrap` ever calls a blocking `Store` method inline instead of
    // through `spawn_blocking`, that call captures the only thread for its
    // whole duration and even an unrelated 5ms timer can't fire until it's
    // done.
    let f = fixtures();
    let mut deps = f.deps();
    deps.store = Arc::new(SlowLastSyncId(f.store.clone()));
    let (core, _recv) = Core::start(deps);

    let core2 = core.clone();
    let bootstrap = tokio::spawn(async move { core2.bootstrap(BootstrapScope::Global).await });

    // 20 x 5ms = 100ms nominal. If the executor's single thread is ever
    // stolen for `SLOW_STORE_BLOCK` (1s) by an un-`spawn_blocking`ed
    // call, this loop's total wall time jumps well past that, regardless
    // of exactly when the stolen call lands relative to this loop.
    let start = std::time::Instant::now();
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let elapsed = start.elapsed();
    // Well under the 1s block, well over 100ms: a loaded CI runner's timer
    // slack (seen at 202ms) must not read as a blocked executor.
    assert!(
        elapsed < Duration::from_millis(600),
        "20x5ms timers took {elapsed:?}: bootstrap blocked the executor"
    );

    bootstrap.await.unwrap().expect("bootstrap");
}

#[tokio::test]
async fn global_bootstrap_includes_projects() {
    let f = fixtures();
    let (core, _recv) = Core::start(f.deps());
    let project = add_ready_local_project(&core, &f, "/tmp/theodore-bootstrap-project").await;
    let boot = core.bootstrap(BootstrapScope::Global).await.unwrap();
    assert!(
        boot.rows
            .iter()
            .any(|r| r.model == models::PROJECT && r.id == project.id),
        "projects must survive a restart: the sidebar reads them from bootstrap"
    );
}

// ── failed agent runs (ask / run_skill), enable/disable, worktree leases ────

fn chat_messages(f: &Fixtures, pr: &PrId) -> Vec<theodore_contract::ChatMessage> {
    f.store
        .all(models::CHAT_MESSAGE)
        .unwrap()
        .into_iter()
        .filter_map(|r| serde_json::from_value::<theodore_contract::ChatMessage>(r.data).ok())
        .filter(|m| &m.pr == pr)
        .collect()
}

#[tokio::test]
async fn ask_persists_a_failed_chat_message_on_agent_error() {
    let f = fixtures();
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;
    f.agents.push_script(vec![AgentEvent::Error {
        message: "boom".into(),
        stderr_tail: String::new(),
    }]);

    core.ask(theodore_contract::AskRequest {
        pr: pr.clone(),
        question: "why?".into(),
        about: None,
        group: None,
        agent: Some(AgentId::Claude),
        attachments: None,
    })
    .await
    .expect("ask");

    wait_until(|| {
        chat_messages(&f, &pr)
            .iter()
            .any(|m| m.failed.as_deref() == Some("boom"))
    })
    .await;
}

#[tokio::test]
async fn ask_stores_its_attachments_and_hands_them_to_the_agent() {
    let f = fixtures();
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;
    let att = core
        .save_attachment(theodore_contract::SaveAttachmentRequest {
            name: "trace.pdf".into(),
            mime: String::new(),
            data: "JVBERg==".into(),
        })
        .await
        .expect("save_attachment");
    f.agents.push_script(vec![AgentEvent::Done {
        final_text: "It's the retry loop.".into(),
        usage: Default::default(),
    }]);

    let id = core
        .ask(theodore_contract::AskRequest {
            pr: pr.clone(),
            question: "why does this trace loop?".into(),
            about: None,
            group: None,
            agent: Some(AgentId::Claude),
            attachments: Some(vec![att.id.clone()]),
        })
        .await
        .expect("ask");

    wait_until(|| chat_messages(&f, &pr).len() == 2).await;
    let question = chat_messages(&f, &pr)
        .into_iter()
        .find(|m| m.id == id)
        .unwrap();
    assert_eq!(question.text, "why does this trace loop?");
    assert_eq!(question.attachments, vec![att.clone()]);
    assert_eq!(att.mime, "application/pdf");
    let req = f.agents.last_request().unwrap();
    assert_eq!(
        req.prompt,
        format!(
            "why does this trace loop?\n\nAttached files (read them; images and PDFs too):\n\
             - {} (application/pdf, trace.pdf)",
            att.path
        )
    );
    assert_eq!(req.attachments.len(), 1);
    assert_eq!(req.attachments[0].path.display().to_string(), att.path);
}

#[tokio::test]
async fn ask_with_an_unknown_attachment_is_refused() {
    let f = fixtures();
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;
    let err = core
        .ask(theodore_contract::AskRequest {
            pr: pr.clone(),
            question: "why?".into(),
            about: None,
            group: None,
            agent: Some(AgentId::Claude),
            attachments: Some(vec!["nope".into()]),
        })
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Invalid(_)), "{err:?}");
    assert!(chat_messages(&f, &pr).is_empty());
}

#[tokio::test]
async fn ask_persists_a_failed_chat_message_on_cancel() {
    let f = fixtures();
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;
    f.agents.hang_next_run();

    let message_id = core
        .ask(theodore_contract::AskRequest {
            pr: pr.clone(),
            question: "why?".into(),
            about: None,
            group: None,
            agent: Some(AgentId::Claude),
            attachments: None,
        })
        .await
        .expect("ask");
    core.cancel_run(format!("ask:{message_id}"))
        .await
        .expect("cancel_run");

    wait_until(|| {
        chat_messages(&f, &pr)
            .iter()
            .any(|m| m.failed.as_deref() == Some("cancelled"))
    })
    .await;
}

#[tokio::test]
async fn run_skill_persists_a_failed_chat_message_on_agent_error() {
    let f = fixtures();
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;
    f.agents.push_script(vec![AgentEvent::Error {
        message: "crashed".into(),
        stderr_tail: String::new(),
    }]);

    let run_id = core
        .run_skill(RunSkillRequest {
            pr: pr.clone(),
            skill: "security".into(),
            agents: vec![AgentId::Claude],
        })
        .await
        .expect("run_skill");

    wait_until(|| {
        f.store
            .get(models::SKILL_RUN, &run_id)
            .ok()
            .flatten()
            .and_then(|r| r.data.get("status").cloned())
            .map(|s| s == json!("failed"))
            .unwrap_or(false)
    })
    .await;
    assert!(
        chat_messages(&f, &pr)
            .iter()
            .any(|m| m.failed.as_deref() == Some("crashed")),
        "a failed skill run must also leave a failed chat message"
    );
}

#[tokio::test]
async fn an_offline_host_does_not_block_an_agent_run() {
    let f = fixtures();
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;
    // The host is down; the agent's own API may not be: the run starts,
    // and if that's down too it says so plainly.
    core.note_host::<()>(&Err(theodore_ports::HostError::Network("dns".into())));
    assert!(!core.host_online());
    let offline = theodore_ports::offline_message("claude");
    f.agents.push_script(vec![AgentEvent::Error {
        message: offline.clone(),
        stderr_tail: "getaddrinfo ENOTFOUND api.anthropic.com".into(),
    }]);

    let run_id = core
        .run_skill(RunSkillRequest {
            pr: pr.clone(),
            skill: "security".into(),
            agents: vec![AgentId::Claude],
        })
        .await
        .expect("an offline host doesn't block the run");

    wait_until(|| {
        f.store
            .get(models::SKILL_RUN, &run_id)
            .ok()
            .flatten()
            .and_then(|r| r.data.get("error").cloned())
            .is_some_and(|e| e == json!(offline))
    })
    .await;
}

#[tokio::test]
async fn run_skill_refuses_when_every_requested_agent_is_disabled() {
    let f = fixtures();
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;
    put_setting(&f, "agents.claude.enabled", "user", json!(false));

    let err = core
        .run_skill(RunSkillRequest {
            pr: pr.clone(),
            skill: "security".into(),
            agents: vec![AgentId::Claude],
        })
        .await;
    assert!(matches!(err, Err(AppError::Invalid(_))));
}

#[tokio::test]
async fn run_skill_holds_the_prs_worktree_lease_for_the_run() {
    let f = fixtures();
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;
    let pr_ref = fixture_pr_ref();
    f.agents.hang_next_run();

    let run_id = core
        .run_skill(RunSkillRequest {
            pr: pr.clone(),
            skill: "security".into(),
            agents: vec![AgentId::Claude],
        })
        .await
        .expect("run_skill");

    wait_until(|| f.worktree_lease.acquired().contains(&pr_ref)).await;
    assert!(
        f.worktree_lease.is_held(&pr_ref),
        "the lease must still be held while the run is in flight"
    );

    core.cancel_run(run_id).await.expect("cancel_run");
    wait_until(|| !f.worktree_lease.is_held(&pr_ref)).await;
}

#[tokio::test]
async fn ask_holds_the_prs_worktree_lease_for_the_run() {
    let f = fixtures();
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;
    let pr_ref = fixture_pr_ref();
    f.agents.hang_next_run();

    let message_id = core
        .ask(theodore_contract::AskRequest {
            pr: pr.clone(),
            question: "why?".into(),
            about: None,
            group: None,
            agent: Some(AgentId::Claude),
            attachments: None,
        })
        .await
        .expect("ask");

    wait_until(|| f.worktree_lease.acquired().contains(&pr_ref)).await;
    assert!(f.worktree_lease.is_held(&pr_ref));

    core.cancel_run(format!("ask:{message_id}"))
        .await
        .expect("cancel_run");
    wait_until(|| !f.worktree_lease.is_held(&pr_ref)).await;
}

#[tokio::test]
async fn ask_points_the_agent_at_the_review_tools_for_the_diff() {
    // The worktree has only the head: without being told where the change
    // is, the agent answers "I couldn't diff against the parent".
    let f = fixtures();
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;
    core.ask(theodore_contract::AskRequest {
        pr: pr.clone(),
        question: "why?".into(),
        about: None,
        group: None,
        agent: Some(AgentId::Claude),
        attachments: None,
    })
    .await
    .expect("ask");

    wait_until(|| f.agents.last_request().is_some()).await;
    let req = f.agents.last_request().unwrap();
    assert_eq!(req.prompt, "why?");
    let system = req.system.expect("a system prompt");
    assert!(system.contains("get_pr_context"));
    assert!(system.contains("get_base_file"));
}

#[tokio::test]
async fn regroup_falls_back_to_an_enabled_installed_agent_when_configured_one_is_disabled() {
    let f = fixtures();
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;
    put_setting(&f, "agents.grouping", "user", json!("claude"));
    put_setting(&f, "agents.claude.enabled", "user", json!(false));
    wait_grouping_idle(&f, &core, &pr).await;
    core.regroup(pr).await.expect("regroup");

    assert_ne!(
        f.grouper.last_agent(),
        Some(AgentId::Claude),
        "a disabled grouping agent must not be used"
    );
}

#[tokio::test]
async fn regroup_holds_the_prs_worktree_lease() {
    let f = fixtures();
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;
    let pr_ref = fixture_pr_ref();

    wait_grouping_idle(&f, &core, &pr).await;
    core.regroup(pr).await.expect("regroup");

    assert!(f.worktree_lease.acquired().contains(&pr_ref));
    assert!(
        !f.worktree_lease.is_held(&pr_ref),
        "the lease must be released once the (synchronous, fast-fake) regroup returns"
    );
}

#[tokio::test]
async fn agent_enabled_defaults_to_true_and_honours_the_setting() {
    let f = fixtures();
    let (core, _recv) = Core::start(f.deps());

    assert!(core.agent_enabled(AgentId::Claude).await);

    put_setting(&f, "agents.claude.enabled", "user", json!(false));
    assert!(!core.agent_enabled(AgentId::Claude).await);
}

/// `agents.user_mcp` (issue #16): on unless the user turns it off, and a
/// repo's config can't turn it either way.
#[tokio::test]
async fn user_mcp_defaults_to_on_and_only_the_user_scope_counts() {
    let f = fixtures();
    let (core, _recv) = Core::start(f.deps());
    assert!(core.user_mcp_allowed().await);

    put_setting(&f, "agents.user_mcp", "repo:acme/app", json!(false));
    assert!(core.user_mcp_allowed().await, "a repo row must not count");

    put_setting(&f, "agents.user_mcp", "user", json!(false));
    assert!(!core.user_mcp_allowed().await);
}

/// Only spaces get the user's MCP servers: grouping and a fix run never do,
/// even with the setting on.
#[tokio::test]
async fn review_runs_never_carry_the_user_mcp_servers() {
    let f = fixtures();
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;
    wait_grouping_idle(&f, &core, &pr).await;
    assert!(core.user_mcp_allowed().await);
    core.regroup(pr.clone()).await.expect("regroup");
    assert_eq!(f.grouper.last_user_mcp(), Some(false));

    let task = sample_task("t-mcp", &pr, None, None, f.clock.now());
    put_task(&f, &task);
    f.agents.push_script(vec![AgentEvent::Done {
        final_text: "ok".into(),
        usage: Usage::default(),
    }]);
    core.delegate_task(DelegateRequest {
        task: task.id.clone(),
        agent: Some(AgentId::Claude),
    })
    .await
    .expect("delegate_task");
    wait_until(|| f.agents.last_request().is_some()).await;
    assert!(!f.agents.last_request().unwrap().user_mcp);
}

#[tokio::test]
async fn stall_after_ms_defaults_and_is_overridable() {
    let f = fixtures();
    let (core, _recv) = Core::start(f.deps());

    assert_eq!(
        core.stall_after_ms().await,
        crate::settings::DEFAULT_STALL_AFTER_MS
    );

    put_setting(&f, "agents.stall_after_ms", "user", json!(5_000));
    assert_eq!(core.stall_after_ms().await, 5_000);
}

#[tokio::test]
async fn delegate_task_persists_a_failed_chat_message_on_agent_error() {
    let f = fixtures();
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;
    let task = sample_task("t-fail", &pr, None, None, f.clock.now());
    put_task(&f, &task);
    f.agents.push_script(vec![AgentEvent::Error {
        message: "edit crashed".into(),
        stderr_tail: String::new(),
    }]);

    core.delegate_task(DelegateRequest {
        task: task.id.clone(),
        agent: Some(AgentId::Codex),
    })
    .await
    .expect("delegate_task");

    // The task starts in `Todo` too: wait for the failure record itself,
    // not the status, or this races the run.
    wait_until(|| {
        chat_messages(&f, &pr)
            .iter()
            .any(|m| m.failed.as_deref() == Some("edit crashed"))
    })
    .await;
    assert_eq!(task_row(&f, &task.id).status, TaskStatus::Todo);
    assert!(
        chat_messages(&f, &pr)
            .iter()
            .any(|m| m.failed.as_deref() == Some("edit crashed")),
        "a failed fix run must leave a failed chat message"
    );
}

#[tokio::test]
async fn delegate_task_holds_the_prs_worktree_lease_for_the_run() {
    let f = fixtures();
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;
    let pr_ref = fixture_pr_ref();
    let task = sample_task("t-lease", &pr, None, None, f.clock.now());
    put_task(&f, &task);
    f.agents.hang_next_run();

    let run_id = core
        .delegate_task(DelegateRequest {
            task: task.id.clone(),
            agent: Some(AgentId::Codex),
        })
        .await
        .expect("delegate_task");

    wait_until(|| f.worktree_lease.acquired().contains(&pr_ref)).await;
    assert!(f.worktree_lease.is_held(&pr_ref));

    core.cancel_run(run_id).await.expect("cancel_run");
    wait_until(|| !f.worktree_lease.is_held(&pr_ref)).await;
}

#[tokio::test]
async fn open_pr_refs_reports_every_pr_open_in_this_session() {
    let f = fixtures();
    let (core, _recv, _pr, _head) = open_fixture_pr(&f).await;

    let open = core.open_pr_refs().await;
    assert_eq!(open, vec![fixture_pr_ref()]);
}

#[tokio::test]
async fn get_blob_returns_base64_and_mime_for_an_image_path() {
    let f = fixtures();
    let (core, _recv, pr, head) = open_fixture_pr(&f).await;
    let png_bytes: &[u8] = &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    f.git.put_file(&head, "logo.png", png_bytes);

    let resp: BlobResponse = core
        .get_blob(GetBlobRequest {
            pr,
            path: "logo.png".into(),
            side: DiffSide::Head,
        })
        .await
        .expect("get_blob");
    assert_eq!(resp.mime, "image/png");
    assert_eq!(resp.bytes, png_bytes.len() as u64);
    use base64::Engine;
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(resp.base64)
            .unwrap(),
        png_bytes
    );
}

#[tokio::test]
async fn get_blob_rejects_a_file_over_the_size_cap() {
    let f = fixtures();
    let (core, _recv, pr, head) = open_fixture_pr(&f).await;
    let huge = vec![0u8; 10 * 1024 * 1024 + 1];
    f.git.put_file(&head, "huge.png", &huge);

    let err = core
        .get_blob(GetBlobRequest {
            pr,
            path: "huge.png".into(),
            side: DiffSide::Head,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Invalid(_)));
}

#[tokio::test]
async fn get_blob_on_a_missing_path_is_not_found() {
    let f = fixtures();
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;

    let err = core
        .get_blob(GetBlobRequest {
            pr,
            path: "nope.png".into(),
            side: DiffSide::Head,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[tokio::test]
async fn conflicts_maps_git_entries_to_wire_conflict_files() {
    let f = fixtures();
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;
    f.git.set_conflicts(Ok(vec![ConflictEntry {
        path: "f.txt".into(),
        kind: PortConflictKind::Content,
    }]));

    let files = core.conflicts(pr).await.expect("conflicts");
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "f.txt");
    assert_eq!(files[0].kind, WireConflictKind::Content);
}

#[tokio::test]
async fn conflicts_propagates_unsupported_on_an_old_git() {
    let f = fixtures();
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;
    f.git
        .set_conflicts(Err(GitError::Unsupported("merge_tree_conflicts")));

    let err = core.conflicts(pr).await.unwrap_err();
    assert!(matches!(err, AppError::Git(GitError::Unsupported(_))));
}

#[tokio::test]
async fn opening_a_pr_regroups_a_grouping_made_by_an_older_algorithm() {
    let f = fixtures();
    // A stored grouping of the same head, written before rows carried an
    // algorithm version (`algo` missing = 0).
    let pr_id = fixture_pr_ref().id();
    f.store
        .apply(vec![NewAction {
            model: models::GROUPING.into(),
            model_id: pr_id.0.clone(),
            action: SyncActionCode::U,
            data: Some(json!({ "rev": "head111", "groups": [] })),
        }])
        .unwrap();

    let (_core, _recv, pr, _head) = open_fixture_pr(&f).await;
    wait_until(|| {
        f.store
            .get(models::GROUPING, &pr.0)
            .ok()
            .flatten()
            .is_some_and(|r| {
                theodore_domain::grouping::row_algo(&r.data)
                    == theodore_domain::grouping::ALGO_VERSION
            })
    })
    .await;
}

#[tokio::test]
async fn ensure_open_recovers_a_branch_review_of_a_host_project_after_a_restart() {
    // "Review branch" in a GitHub project keeps `github:` in its id but has
    // no PR on the host; reopening it must not ask the host (the fake
    // answers NotFound, like GitHub's 404).
    let f = fixtures();
    let (core, _recv) = started_core(&f).await;
    let repo = RepoRef {
        host: HostKind::Github,
        owner: "acme".into(),
        name: "widgets".into(),
    };
    core.add_project(AddProjectRequest {
        repo: Some(repo),
        path: None,
    })
    .await
    .expect("add_project");
    wait_until(|| {
        f.store
            .get(models::PROJECT, "github:acme/widgets")
            .unwrap()
            .is_some_and(|r| r.data["status"] == json!("ready"))
    })
    .await;

    f.git.set_resolve("feature", Sha::new("feat-head"));
    f.git.set_resolve("main", Sha::new("main-base"));
    f.git
        .set_commit(Sha::new("feat-head"), feature_branch_commit());
    f.git.set_commit_log(vec![feature_branch_commit()]);
    let resp = core
        .open_branch(OpenBranchRequest {
            project: "github:acme/widgets".into(),
            branch: "feature".into(),
            base: Some("main".into()),
        })
        .await
        .expect("open_branch");
    assert!(resp.pr.0.starts_with("github:"));

    let (core2, _recv2) = started_core(&f).await;
    core2
        .ensure_open(&resp.pr)
        .await
        .expect("a branch review reopens without the host");
}

#[tokio::test]
async fn reading_a_branch_review_marks_files_viewed_without_telling_the_host() {
    let f = fixtures();
    let (core, _recv) = started_core(&f).await;
    let repo = RepoRef {
        host: HostKind::Github,
        owner: "acme".into(),
        name: "widgets".into(),
    };
    core.add_project(AddProjectRequest {
        repo: Some(repo),
        path: None,
    })
    .await
    .expect("add_project");
    wait_until(|| {
        f.store
            .get(models::PROJECT, "github:acme/widgets")
            .unwrap()
            .is_some_and(|r| r.data["status"] == json!("ready"))
    })
    .await;
    f.git.set_resolve("feature", Sha::new("feat-head"));
    f.git.set_resolve("main", Sha::new("main-base"));
    f.git
        .set_commit(Sha::new("feat-head"), feature_branch_commit());
    f.git.set_commit_log(vec![feature_branch_commit()]);
    let resp = core
        .open_branch(OpenBranchRequest {
            project: "github:acme/widgets".into(),
            branch: "feature".into(),
            base: Some("main".into()),
        })
        .await
        .expect("open_branch");
    let revision = core
        .get_revision(RevisionRequest {
            pr: resp.pr.clone(),
            head: resp.head,
        })
        .await
        .unwrap();
    let hunks = testkit::hunk_ids(&revision);
    assert!(!hunks.is_empty());

    for (i, h) in hunks.iter().enumerate() {
        let r = core
            .transact(hunk_read_tx(&format!("read-{i}"), &resp.pr, h))
            .await;
        assert!(matches!(r, TransactionResult::Ok { .. }));
    }
    wait_until(|| {
        f.store
            .query(models::FILE_VIEWED, "pr", &resp.pr.0)
            .is_ok_and(|rows| !rows.is_empty())
    })
    .await;
    assert!(
        f.store
            .outbox_snapshot()
            .iter()
            .all(|i| i.op != "set_viewed"),
        "a branch review has no host PR to mark viewed on"
    );
}

#[tokio::test]
async fn opening_a_pr_keeps_its_inbox_reasons() {
    let f = fixtures();
    // As if an inbox refresh had stored it with a reason.
    let pr_id = fixture_pr_ref().id();
    let mut summary = serde_json::to_value(testkit::sample_pr_summary(
        &fixture_pr_ref(),
        &Sha::new("head111"),
    ))
    .unwrap();
    summary["reasons"] = json!(["review_requested"]);
    f.store
        .apply(vec![NewAction {
            model: models::PR.into(),
            model_id: pr_id.0.clone(),
            action: SyncActionCode::U,
            data: Some(summary),
        }])
        .unwrap();
    // The startup inbox refresh still finds it (otherwise it would rightly
    // clear the reasons of a PR no search returns).
    let mut found = testkit::sample_pr_summary(&fixture_pr_ref(), &Sha::new("head111"));
    found.reasons = vec![theodore_domain::InboxReason::ReviewRequested];
    f.host.push_search(Ok(vec![found]));

    let (_core, _recv, pr, _head) = open_fixture_pr(&f).await;
    let row = f.store.get(models::PR, &pr.0).unwrap().unwrap();
    assert_eq!(row.data["reasons"], json!(["review_requested"]));
}

#[tokio::test]
async fn host_status_for_reports_a_saved_gitlab_token() {
    let f = fixtures();
    let (core, _recv) = started_core(&f).await;
    let none = core
        .host_status_for(HostKind::Gitlab)
        .await
        .expect("status");
    assert!(!none.connected);
    assert_eq!(none.source, TokenSource::None);

    f.secrets
        .set(theodore_secrets::GITLAB_TOKEN, "glpat-x")
        .unwrap();
    let saved = core
        .host_status_for(HostKind::Gitlab)
        .await
        .expect("status");
    assert_eq!(saved.source, TokenSource::Keychain);
    assert_eq!(saved.host, HostKind::Gitlab);
}

#[tokio::test]
async fn refreshing_a_project_keeps_inbox_reasons() {
    let f = fixtures();
    let (core, _recv) = started_core(&f).await;
    let repo = RepoRef {
        host: HostKind::Github,
        owner: "acme".into(),
        name: "widgets".into(),
    };
    let pr_ref = PrRef {
        repo: repo.clone(),
        number: 9,
        host_id: Some("node-9".into()),
    };
    // The inbox stored it with a reason; the project search has none.
    let mut inbox =
        serde_json::to_value(testkit::sample_pr_summary(&pr_ref, &Sha::new("h9"))).unwrap();
    inbox["reasons"] = json!(["authored"]);
    f.store
        .apply(vec![NewAction {
            model: models::PR.into(),
            model_id: pr_ref.id().0,
            action: SyncActionCode::U,
            data: Some(inbox),
        }])
        .unwrap();
    f.host.push_search(Ok(vec![testkit::sample_pr_summary(
        &pr_ref,
        &Sha::new("h9"),
    )]));

    core.add_project(AddProjectRequest {
        repo: Some(repo),
        path: None,
    })
    .await
    .expect("add_project");
    wait_until(|| {
        f.store
            .get(models::PROJECT, "github:acme/widgets")
            .unwrap()
            .is_some_and(|r| r.data.get("last_fetched_at").is_some())
    })
    .await;
    let row = f.store.get(models::PR, &pr_ref.id().0).unwrap().unwrap();
    assert_eq!(row.data["reasons"], json!(["authored"]));
}

#[tokio::test]
async fn an_older_search_result_does_not_roll_back_a_merged_pr() {
    let f = fixtures();
    let (core, _recv) = started_core(&f).await;
    let pr_ref = fixture_pr_ref();
    // Stored: merged at t=2000 (e.g. our own merge, already applied).
    let mut merged =
        serde_json::to_value(testkit::sample_pr_summary(&pr_ref, &Sha::new("h"))).unwrap();
    merged["state"] = json!("merged");
    merged["updated_at"] = json!(2000);
    f.store
        .apply(vec![NewAction {
            model: models::PR.into(),
            model_id: pr_ref.id().0,
            action: SyncActionCode::U,
            data: Some(merged),
        }])
        .unwrap();
    // A search that started before the merge answers with the open PR.
    let mut stale = testkit::sample_pr_summary(&pr_ref, &Sha::new("h"));
    stale.updated_at = 1000;
    stale.reasons = vec![theodore_domain::InboxReason::ReviewRequested];
    f.host.push_search(Ok(vec![stale]));

    core.refresh_inbox().await.expect("refresh_inbox");
    let row = f.store.get(models::PR, &pr_ref.id().0).unwrap().unwrap();
    assert_eq!(row.data["state"], json!("merged"));
    assert_eq!(row.data["reasons"], json!(["review_requested"]));
}

#[tokio::test]
async fn refresh_inbox_clears_reasons_of_prs_no_search_returns() {
    let f = fixtures();
    let (core, _recv) = started_core(&f).await;
    let pr_ref = fixture_pr_ref();
    let mut row =
        serde_json::to_value(testkit::sample_pr_summary(&pr_ref, &Sha::new("h"))).unwrap();
    row["reasons"] = json!(["review_requested"]);
    f.store
        .apply(vec![NewAction {
            model: models::PR.into(),
            model_id: pr_ref.id().0,
            action: SyncActionCode::U,
            data: Some(row),
        }])
        .unwrap();
    // Every section answers, none with this PR (you reviewed it).
    for _ in 0..4 {
        f.host.push_search(Ok(vec![]));
    }
    core.refresh_inbox().await.expect("refresh_inbox");
    let stored = f.store.get(models::PR, &pr_ref.id().0).unwrap().unwrap();
    assert_eq!(stored.data["reasons"], json!([]));
}

#[tokio::test]
async fn a_failed_inbox_section_keeps_reasons() {
    let f = fixtures();
    let (core, _recv) = started_core(&f).await;
    let pr_ref = fixture_pr_ref();
    let mut row =
        serde_json::to_value(testkit::sample_pr_summary(&pr_ref, &Sha::new("h"))).unwrap();
    row["reasons"] = json!(["review_requested"]);
    f.store
        .apply(vec![NewAction {
            model: models::PR.into(),
            model_id: pr_ref.id().0,
            action: SyncActionCode::U,
            data: Some(row),
        }])
        .unwrap();
    f.host
        .push_search(Err(theodore_ports::HostError::Network("down".into())));
    for _ in 0..3 {
        f.host.push_search(Ok(vec![]));
    }
    core.refresh_inbox().await.expect("refresh_inbox");
    let stored = f.store.get(models::PR, &pr_ref.id().0).unwrap().unwrap();
    assert_eq!(stored.data["reasons"], json!(["review_requested"]));
}

#[tokio::test]
async fn triage_take_drops_team_requests_on_a_pr_never_opened() {
    let f = fixtures();
    let (core, _recv) = started_core(&f).await;
    let pr_ref = fixture_pr_ref();
    // Only the inbox row exists: no stored pr_detail to read teams from.
    f.store
        .apply(vec![NewAction {
            model: models::PR.into(),
            model_id: pr_ref.id().0,
            action: SyncActionCode::U,
            data: Some(
                serde_json::to_value(testkit::sample_pr_summary(&pr_ref, &Sha::new("h"))).unwrap(),
            ),
        }])
        .unwrap();
    let mut detail = testkit::sample_pr_detail(&pr_ref, &Sha::new("base000"), &Sha::new("h"));
    detail.reviewers = vec![theodore_domain::Reviewer {
        person: testkit::sample_person("web-core"),
        state: theodore_domain::ReviewerState::Requested,
        team: Some("web-core".into()),
    }];
    f.host.push_viewer(Ok(testkit::sample_person("me")));
    f.host.push_get_pr(Ok(Fetched::Modified {
        etag: Some("e".into()),
        value: detail,
    }));
    f.host.push_get_pr(Ok(Fetched::NotModified));

    core.triage(TriageRequest {
        pr: pr_ref.id(),
        action: TriageAction::Take,
    })
    .await
    .expect("triage take");
    wait_until(|| {
        f.store
            .outbox_snapshot()
            .iter()
            .any(|i| i.op == "triage" && i.status == OutboxStatus::Done)
    })
    .await;
    assert_eq!(f.host.remove_reviewer_calls(), vec!["web-core".to_string()]);
}

#[tokio::test]
async fn a_rejected_token_on_a_read_raises_the_auth_banner_and_clears_after() {
    let f = fixtures();
    let (core, mut recv) = started_core(&f).await;
    for _ in 0..4 {
        f.host.push_search(Err(theodore_ports::HostError::Auth));
    }
    core.refresh_inbox().await.expect("refresh_inbox");
    expect_ephemeral(&mut recv, |e| {
        matches!(e, Ephemeral::HostStatus { auth_ok: false, .. })
    })
    .await;

    for _ in 0..4 {
        f.host.push_search(Ok(vec![]));
    }
    core.refresh_inbox().await.expect("refresh_inbox");
    expect_ephemeral(&mut recv, |e| {
        matches!(
            e,
            Ephemeral::HostStatus {
                auth_ok: true,
                online: true,
                rate_limited_until: None
            }
        )
    })
    .await;
}

#[tokio::test]
async fn a_finding_reported_from_a_skill_run_belongs_to_its_skill() {
    let f = fixtures();
    let (core, _recv, pr, head) = open_fixture_pr(&f).await;
    f.store
        .apply(vec![NewAction {
            model: models::SKILL_RUN.into(),
            model_id: "run-1".into(),
            action: SyncActionCode::I,
            data: Some(json!({ "id": "run-1", "pr": pr.0, "skill": "bugs" })),
        }])
        .unwrap();
    let finding = theodore_domain::Finding {
        id: theodore_domain::FindingId("f-1".into()),
        pr: pr.clone(),
        // What the MCP tools know: the run, not the skill.
        skill: theodore_domain::SkillId("run-1".into()),
        agents: vec![AgentId::Claude],
        anchor: theodore_domain::Anchor {
            rev: head,
            path: "src/a.ts".into(),
            side: theodore_domain::Side::New,
            line: 1,
            start_line: None,
            start_side: None,
        },
        severity: theodore_domain::Severity::Warn,
        title: "t".into(),
        body: "b".into(),
        state: theodore_domain::FindingState::Open,
        fix: None,
        outdated: false,
    };
    let id = core.add_finding(pr, finding).await.expect("add_finding");
    let row = f.store.get(models::FINDING, &id.0).unwrap().unwrap();
    assert_eq!(row.data["skill"], json!("bugs"));
}

#[tokio::test]
async fn a_write_that_gets_through_clears_the_offline_banner() {
    let f = fixtures();
    let (core, mut recv) = started_core(&f).await;
    for _ in 0..4 {
        f.host
            .push_search(Err(theodore_ports::HostError::Network("down".into())));
    }
    core.refresh_inbox().await.expect("refresh_inbox");
    expect_ephemeral(&mut recv, |e| {
        matches!(e, Ephemeral::HostStatus { online: false, .. })
    })
    .await;

    // Back online: a queued write succeeds before any read does.
    let pr = fixture_pr_ref().id();
    f.store
        .apply(vec![NewAction {
            model: models::PR.into(),
            model_id: pr.0.clone(),
            action: SyncActionCode::U,
            data: Some(
                serde_json::to_value(testkit::sample_pr_summary(
                    &fixture_pr_ref(),
                    &Sha::new("h"),
                ))
                .unwrap(),
            ),
        }])
        .unwrap();
    f.host.push_viewer(Ok(testkit::sample_person("me")));
    core.triage(TriageRequest {
        pr,
        action: TriageAction::Decline,
    })
    .await
    .expect("triage");
    expect_ephemeral(&mut recv, |e| {
        matches!(e, Ephemeral::HostStatus { online: true, .. })
    })
    .await;
}

// ── streamed grouping ────────────────────────────────────────────────────

/// The grouping rows written for one PR, oldest first.
async fn grouping_writes(core: &Core, pr: &PrId) -> Vec<theodore_domain::Grouping> {
    core.catch_up(0)
        .await
        .expect("catch_up")
        .actions
        .into_iter()
        .filter(|a| a.model == models::GROUPING && a.model_id == pr.0)
        .filter_map(|a| serde_json::from_value(a.data?).ok())
        .collect()
}

fn group_of(id: &str, hunks: Vec<HunkId>) -> theodore_domain::Group {
    theodore_domain::Group {
        id: theodore_domain::GroupId::new(id),
        title: format!("Group {id}"),
        why: format!("Why {id}."),
        kind: theodore_domain::GroupKind::Normal,
        risk: None,
        check: vec![],
        hunks,
        outliers: vec![],
    }
}

fn in_progress(groups: Vec<theodore_domain::Group>) -> theodore_domain::Grouping {
    theodore_domain::Grouping {
        rev: Sha::new("head111"),
        groups,
        in_progress: true,
    }
}

fn stored_grouping(f: &Fixtures, pr: &PrId) -> Option<serde_json::Value> {
    f.store
        .get(models::GROUPING, &pr.0)
        .unwrap()
        .map(|r| r.data)
}

/// Streamed grouping: the rows written while the grouper runs are its
/// provisional groupings, in order and marked in progress, and the
/// finished grouping lands last, current for the head.
#[tokio::test]
async fn opening_a_pr_publishes_streamed_groupings_then_the_finished_one() {
    let f = fixtures();
    let rev = testkit::two_hunk_revision(&Sha::new("base000"), &Sha::new("head111"), "src/lib.rs");
    let ids = testkit::hunk_ids(&rev);
    let head = Sha::new("head111");
    let g1 = group_of("g1", vec![ids[0].clone()]);
    let g2 = group_of("g2", vec![ids[1].clone()]);
    f.grouper.set_partials(vec![
        in_progress(vec![g1.clone()]),
        in_progress(vec![g1.clone(), g2.clone()]),
    ]);
    f.grouper.set_delay_ms(5);
    let finished = theodore_domain::Grouping::new(head.clone(), vec![g2.clone(), g1.clone()]);
    f.grouper.set(finished.clone());

    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;
    wait_grouping_idle(&f, &core, &pr).await;

    let writes = grouping_writes(&core, &pr).await;
    // Partials queued faster than they are written collapse to the newest,
    // so the writes are the published sequence in order, maybe with gaps;
    // the first group always shows before the run ends.
    let published = [(true, 1), (true, 2), (false, 2)];
    let seen: Vec<(bool, usize)> = writes
        .iter()
        .map(|g| (g.in_progress, g.groups.len()))
        .collect();
    let mut rest = published.iter();
    assert!(
        seen.iter().all(|s| rest.any(|p| p == s)),
        "{seen:?} is not in publish order"
    );
    assert!(seen.len() >= 2 && seen[0].0, "{seen:?}");
    assert_eq!(writes.last(), Some(&finished));
    let row = stored_grouping(&f, &pr).unwrap();
    assert!(crate::usecases::grouping::row_is_current(&row, &head, None));
    assert_eq!(
        serde_json::from_value::<theodore_domain::Grouping>(row).unwrap(),
        finished
    );
}

/// A row a run left in progress (it failed, or the app quit) is not
/// reused: the PR is grouped again on open.
#[tokio::test]
async fn opening_a_pr_regroups_a_grouping_left_in_progress() {
    let f = fixtures();
    let pr_id = fixture_pr_ref().id();
    f.store
        .apply(vec![NewAction {
            model: models::GROUPING.into(),
            model_id: pr_id.0.clone(),
            action: SyncActionCode::U,
            data: Some(json!({
                "rev": "head111", "groups": [], "in_progress": true,
                "algo": theodore_domain::grouping::ALGO_VERSION,
            })),
        }])
        .unwrap();
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;
    wait_until(|| f.grouper.calls() == 1).await;
    wait_grouping_idle(&f, &core, &pr).await;
    wait_until(|| {
        stored_grouping(&f, &pr).is_some_and(|d| {
            crate::usecases::grouping::row_is_current(&d, &Sha::new("head111"), None)
        })
    })
    .await;
}

/// A run that fails mid-stream with nothing stored before it leaves its
/// provisional groups up, still marked in progress: never looking final,
/// and regrouped on the next open.
#[tokio::test]
async fn a_failed_streamed_grouping_stays_marked_in_progress() {
    let f = fixtures();
    let rev = testkit::two_hunk_revision(&Sha::new("base000"), &Sha::new("head111"), "src/lib.rs");
    let ids = testkit::hunk_ids(&rev);
    f.grouper.set_partials(vec![in_progress(vec![group_of(
        "g1",
        vec![ids[0].clone()],
    )])]);
    f.grouper.fail_next("rate limited");

    let (core, mut recv, pr, _head) = open_fixture_pr(&f).await;
    let failed = expect_ephemeral(&mut recv, |e| {
        matches!(
            e,
            Ephemeral::Grouping {
                running: false,
                error: Some(_),
                ..
            }
        )
    })
    .await;
    assert!(format!("{failed:?}").contains("rate limited"), "{failed:?}");
    wait_grouping_idle(&f, &core, &pr).await;
    let row = stored_grouping(&f, &pr).unwrap();
    let left: theodore_domain::Grouping = serde_json::from_value(row.clone()).unwrap();
    assert!(left.in_progress);
    assert_eq!(left.groups.len(), 1);
    assert!(!crate::usecases::grouping::row_is_current(
        &row,
        &Sha::new("head111"),
        None
    ));
}

/// A regroup that fails mid-stream puts back the grouping it was
/// replacing: the provisional groups don't outlive the run.
#[tokio::test]
async fn a_failed_regroup_puts_the_previous_grouping_back() {
    let f = fixtures();
    let rev = testkit::two_hunk_revision(&Sha::new("base000"), &Sha::new("head111"), "src/lib.rs");
    let ids = testkit::hunk_ids(&rev);
    let before =
        theodore_domain::Grouping::new(Sha::new("head111"), vec![group_of("all", ids.clone())]);
    f.grouper.set(before.clone());
    let (core, _recv, pr, _head) = open_fixture_pr(&f).await;
    wait_grouping_idle(&f, &core, &pr).await;

    f.grouper.set_partials(vec![in_progress(vec![group_of(
        "g1",
        vec![ids[0].clone()],
    )])]);
    f.grouper.fail_next("rate limited");
    let err = core.regroup(pr.clone()).await.unwrap_err();
    assert!(err.to_string().contains("rate limited"), "{err}");

    let writes = grouping_writes(&core, &pr).await;
    assert!(
        writes.iter().any(|g| g.in_progress),
        "the streamed group was shown: {writes:?}"
    );
    let row = stored_grouping(&f, &pr).unwrap();
    assert_eq!(
        serde_json::from_value::<theodore_domain::Grouping>(row).unwrap(),
        before
    );
}

// ── grouping across a push ───────────────────────────────────────────────

/// The author pushes `head` (the same two hunks, at a new head); returns
/// once the poller has taken it in.
async fn push_head(f: &Fixtures, recv: &mut Receivers, head: &str) -> Sha {
    let base = Sha::new("base000");
    let head = Sha::new(head);
    f.git.set_head(head.clone());
    f.git
        .set_revision(testkit::two_hunk_revision(&base, &head, "src/lib.rs"));
    f.host.push_get_pr(Ok(Fetched::Modified {
        etag: Some(format!("etag-{head}")),
        value: testkit::sample_pr_detail(&fixture_pr_ref(), &base, &head),
    }));
    f.clock.advance(15_000);
    let pushed = format!("pushed {head}");
    expect_ephemeral(
        recv,
        |e| matches!(e, Ephemeral::Toast { text, .. } if text.contains(&pushed)),
    )
    .await;
    head
}

fn stored_grouping_of(f: &Fixtures, pr: &PrId) -> theodore_domain::Grouping {
    serde_json::from_value(stored_grouping(f, pr).expect("a grouping row")).unwrap()
}

/// Issue #74: a push while the PR is being grouped never starts a second
/// run alongside it. The run going finishes, then exactly one more groups
/// the new head (continuing from the first), and the spinner stays on
/// across both — one `running: false`, at the end.
#[tokio::test]
async fn a_push_during_grouping_runs_one_follow_up_on_the_new_head() {
    let f = fixtures();
    f.grouper.hold();
    let (core, mut recv, pr, head) = open_fixture_pr(&f).await;
    wait_until(|| f.grouper.calls() == 1).await;
    let mut events = core.0.ephemeral_tx.subscribe();

    let new_head = push_head(&f, &mut recv, "head222").await;
    assert_eq!(f.grouper.calls(), 1, "no second run while one is going");
    assert_eq!(
        core.0.current_head.read().await.get(&pr),
        Some(&new_head),
        "the push is taken in without waiting for the run"
    );

    f.grouper.release();
    wait_until(|| f.grouper.calls() == 2).await;
    f.grouper.release();
    wait_until(|| core.0.grouping_inflight.lock().unwrap().is_empty()).await;

    assert_eq!(
        f.grouper.runs(),
        vec![(head, false), (new_head.clone(), true)],
        "the old head, then the new one, continuing from the first"
    );
    assert_eq!(f.grouper.max_active(), 1);
    let row = stored_grouping(&f, &pr).unwrap();
    assert!(crate::usecases::grouping::row_is_current(
        &row, &new_head, None
    ));
    assert!(!stored_grouping_of(&f, &pr).in_progress);
    let mut grouping_events = vec![];
    while let Ok(e) = events.try_recv() {
        if let Ephemeral::Grouping { running, error, .. } = e {
            grouping_events.push((running, error));
        }
    }
    assert_eq!(grouping_events, vec![(false, None)]);
}

/// Several pushes during one run queue one more run, not one each: it
/// groups the latest head.
#[tokio::test]
async fn pushes_during_grouping_coalesce_into_one_run_on_the_latest_head() {
    let f = fixtures();
    f.grouper.hold();
    let (core, mut recv, pr, head) = open_fixture_pr(&f).await;
    wait_until(|| f.grouper.calls() == 1).await;

    push_head(&f, &mut recv, "head222").await;
    let latest = push_head(&f, &mut recv, "head333").await;
    assert_eq!(f.grouper.calls(), 1);

    f.grouper.release();
    wait_until(|| f.grouper.calls() == 2).await;
    f.grouper.release();
    wait_until(|| core.0.grouping_inflight.lock().unwrap().is_empty()).await;

    assert_eq!(
        f.grouper.runs(),
        vec![(head, false), (latest.clone(), true)]
    );
    assert_eq!(f.grouper.max_active(), 1);
    let row = stored_grouping(&f, &pr).unwrap();
    assert!(crate::usecases::grouping::row_is_current(
        &row, &latest, None
    ));
}

/// A run that fails with a push queued behind it still hands over: the
/// new head is grouped from scratch (the failed run's groups never
/// finished) and the row ends finished, for the new head.
#[tokio::test]
async fn a_failed_run_with_a_push_queued_still_groups_the_new_head() {
    let f = fixtures();
    let rev = testkit::two_hunk_revision(&Sha::new("base000"), &Sha::new("head111"), "src/lib.rs");
    let ids = testkit::hunk_ids(&rev);
    f.grouper.set_partials(vec![in_progress(vec![group_of(
        "g1",
        vec![ids[0].clone()],
    )])]);
    f.grouper.fail_next("rate limited");
    f.grouper.hold();
    let (core, mut recv, pr, head) = open_fixture_pr(&f).await;
    wait_until(|| f.grouper.calls() == 1).await;
    wait_until(|| stored_grouping(&f, &pr).is_some()).await;

    let new_head = push_head(&f, &mut recv, "head222").await;
    f.grouper.release();
    wait_until(|| f.grouper.calls() == 2).await;
    f.grouper.release();
    wait_until(|| core.0.grouping_inflight.lock().unwrap().is_empty()).await;

    assert_eq!(
        f.grouper.runs(),
        vec![(head, false), (new_head.clone(), false)]
    );
    let row = stored_grouping(&f, &pr).unwrap();
    assert!(crate::usecases::grouping::row_is_current(
        &row, &new_head, None
    ));
    assert!(!stored_grouping_of(&f, &pr).in_progress);
}

/// The regroup after a push fails: the UI is told (no spinner left on),
/// the PR is free to group again, and the previous finished grouping
/// stays.
#[tokio::test]
async fn a_failed_regroup_after_a_push_says_so_and_frees_the_pr() {
    let f = fixtures();
    f.grouper.hold();
    let (core, mut recv, pr, head) = open_fixture_pr(&f).await;
    wait_until(|| f.grouper.calls() == 1).await;
    push_head(&f, &mut recv, "head222").await;
    f.grouper.release();
    wait_until(|| f.grouper.calls() == 2).await;
    f.grouper.fail_next("rate limited");
    f.grouper.release();

    let stopped = expect_ephemeral(&mut recv, |e| {
        matches!(e, Ephemeral::Grouping { running: false, .. })
    })
    .await;
    assert!(
        format!("{stopped:?}").contains("rate limited"),
        "{stopped:?}"
    );
    wait_until(|| core.0.grouping_inflight.lock().unwrap().is_empty()).await;
    let left = stored_grouping_of(&f, &pr);
    assert_eq!(left.rev, head);
    assert!(!left.in_progress);
    assert!(core.claim_grouping(&pr, false).is_some(), "the PR is free");
}

/// A run that ends without releasing its slot (a panic, a dropped task)
/// still frees the PR and turns its spinner off.
#[tokio::test]
async fn a_grouping_slot_dropped_mid_run_frees_the_pr_and_stops_the_spinner() {
    let f = fixtures();
    let (core, mut recv, pr, _head) = open_fixture_pr(&f).await;
    wait_grouping_idle(&f, &core, &pr).await;

    let mut slot = core.claim_grouping(&pr, false).expect("free");
    assert!(core.claim_grouping(&pr, true).is_none(), "one at a time");
    slot.announce();
    drop(slot);

    expect_ephemeral(&mut recv, |e| {
        matches!(
            e,
            Ephemeral::Grouping {
                running: false,
                error: Some(_),
                ..
            }
        )
    })
    .await;
    assert!(core.0.grouping_inflight.lock().unwrap().is_empty());
}

/// A finished grouping of the PR's current head is never replaced by a
/// run's grouping of an older head.
#[tokio::test]
async fn a_stale_runs_grouping_never_replaces_the_current_heads() {
    let f = fixtures();
    let (core, mut recv, pr, head) = open_fixture_pr(&f).await;
    wait_grouping_idle(&f, &core, &pr).await;
    let new_head = push_head(&f, &mut recv, "head222").await;
    wait_until(|| {
        stored_grouping(&f, &pr)
            .is_some_and(|r| crate::usecases::grouping::row_is_current(&r, &new_head, None))
    })
    .await;
    wait_until(|| core.0.grouping_inflight.lock().unwrap().is_empty()).await;

    let stale =
        theodore_domain::grouping::to_row(&theodore_domain::Grouping::new(head.clone(), vec![]))
            .unwrap();
    core.write_grouping_row(&pr, &head, stale)
        .await
        .expect("write");
    assert_eq!(stored_grouping_of(&f, &pr).rev, new_head);
}
