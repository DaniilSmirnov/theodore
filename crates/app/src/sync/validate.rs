//! Per-model validation of transaction ops (plan §3): hunk reads must stay
//! inside the current revision, task transitions follow the lifecycle,
//! drafts need a commentable anchor. Every model not covered below is
//! core-owned (written only by the sync engine, the poller or a usecase) and
//! is rejected outright — a client transaction never reaches it.

use theodore_contract::{CheckTick, HunkRead, MutationOp, models};
use theodore_domain::{Anchor, Draft, FindingState, PrId, Task, TaskStatus};

use crate::core::Core;

pub(super) async fn validate_op(core: &Core, op: &MutationOp) -> Result<(), String> {
    let model = match op {
        MutationOp::Create { model, .. }
        | MutationOp::Update { model, .. }
        | MutationOp::Archive { model, .. }
        | MutationOp::Delete { model, .. } => model.as_str(),
    };
    if !models::ALL.contains(&model) {
        return Err(format!("unknown model {model}"));
    }
    // Threads are host truth (inside `pr_detail`); they change only through
    // reply/resolve commands, never through a client transaction.
    if model == models::THREAD {
        return Err("threads are changed through reply_thread / resolve_thread".into());
    }
    match (model, op) {
        // ── hunk_read: marks one hunk of the current revision read ────────
        (m, MutationOp::Create { data, .. }) if m == models::HUNK_READ => {
            validate_hunk_read(core, data).await
        }

        // ── file_viewed: local mirror of a Viewed toggle; the host write
        // itself goes through the `set_viewed` outbox op, not here.
        (m, MutationOp::Create { .. } | MutationOp::Update { .. }) if m == models::FILE_VIEWED => {
            Ok(())
        }

        // ── check_tick: a "What to check" item ticked (created) or unticked
        // (deleted); local review state, like a read mark ──────────────────
        (m, MutationOp::Create { data, .. }) if m == models::CHECK_TICK => {
            serde_json::from_value::<CheckTick>(data.clone())
                .map(|_| ())
                .map_err(|e| format!("bad check_tick: {e}"))
        }
        (m, MutationOp::Delete { .. }) if m == models::CHECK_TICK => Ok(()),

        // ── draft: a comment being composed, not yet submitted ────────────
        (m, MutationOp::Create { data, .. }) if m == models::DRAFT => {
            validate_draft_create(core, data).await
        }
        (m, MutationOp::Update { id, .. }) if m == models::DRAFT => {
            validate_draft_update(core, id).await
        }
        (m, MutationOp::Delete { .. } | MutationOp::Archive { .. }) if m == models::DRAFT => Ok(()),

        // ── finding: agent-authored; the client may only move `state`
        // (ignore it, or mark it drafted/maybe-fixed) ──────────────────────
        (m, MutationOp::Update { data, .. }) if m == models::FINDING => {
            validate_finding_state(data)
        }

        // ── task: client creates one from a finding, then only ever moves
        // `status` and/or reassigns it; title/body/anchor/fix are core's ──
        (m, MutationOp::Create { data, .. }) if m == models::TASK => validate_task_create(data),
        (m, MutationOp::Update { id, data, .. }) if m == models::TASK => {
            validate_task_update(core, id, data).await
        }

        // ── skill_run: the client may only dismiss (archive) a run; the
        // run itself, its output and its usage are written by the agent
        // pipeline ──────────────────────────────────────────────────────────
        (m, MutationOp::Archive { .. }) if m == models::SKILL_RUN => Ok(()),

        // ── setting / inbox_state: plain client-owned local state ─────────
        (m, _) if m == models::SETTING || m == models::INBOX_STATE => Ok(()),

        // ── everything else (pr, pr_detail, grouping, project,
        // chat_message, thread, and any op on hunk_read / draft / finding /
        // task / skill_run not matched above) is core-owned ────────────────
        (m, op) => Err(format!(
            "{m} cannot be changed by a client transaction ({})",
            op_kind(op)
        )),
    }
}

fn op_kind(op: &MutationOp) -> &'static str {
    match op {
        MutationOp::Create { .. } => "create",
        MutationOp::Update { .. } => "update",
        MutationOp::Archive { .. } => "archive",
        MutationOp::Delete { .. } => "delete",
    }
}

async fn validate_hunk_read(core: &Core, data: &serde_json::Value) -> Result<(), String> {
    let hr: HunkRead =
        serde_json::from_value(data.clone()).map_err(|e| format!("bad hunk_read: {e}"))?;
    let head = core.0.current_head.read().await.get(&hr.pr).cloned();
    let Some(head) = head else {
        return Err("hunk_read: pr is not open".into());
    };
    let revisions = core.0.revisions.read().await;
    let Some(entry) = revisions.get(&(hr.pr.clone(), head)) else {
        return Err("hunk_read: revision not cached".into());
    };
    if entry.revision.hunks().any(|h| h.id == hr.hunk) {
        Ok(())
    } else {
        Err(format!(
            "hunk_read: {} is not a hunk of the current revision",
            hr.hunk
        ))
    }
}

/// An anchor must stay inside the current diff. Shared by `draft` create
/// (the submitted anchor) and `draft` update (the anchor already on file —
/// an edit doesn't move it, but the diff may have since outrun it).
async fn check_anchor_commentable(core: &Core, pr: &PrId, anchor: &Anchor) -> Result<(), String> {
    let head = core.0.current_head.read().await.get(pr).cloned();
    let Some(head) = head else {
        return Err("draft: pr is not open".into());
    };
    let revisions = core.0.revisions.read().await;
    let Some(entry) = revisions.get(&(pr.clone(), head)) else {
        return Err("draft: revision not cached".into());
    };
    if theodore_domain::anchors::is_commentable(anchor, &entry.revision) {
        Ok(())
    } else {
        Err("draft: anchor is not inside the current diff".into())
    }
}

async fn validate_draft_create(core: &Core, data: &serde_json::Value) -> Result<(), String> {
    let draft: Draft =
        serde_json::from_value(data.clone()).map_err(|e| format!("bad draft: {e}"))?;
    check_anchor_commentable(core, &draft.pr, &draft.anchor).await
}

async fn validate_draft_update(core: &Core, id: &str) -> Result<(), String> {
    let store = core.0.deps.store.clone();
    let id_owned = id.to_string();
    let row = tokio::task::spawn_blocking(move || store.get(models::DRAFT, &id_owned))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let Some(row) = row else {
        return Err("draft: not found".into());
    };
    let draft: Draft =
        serde_json::from_value(row.data).map_err(|e| format!("bad draft row: {e}"))?;
    check_anchor_commentable(core, &draft.pr, &draft.anchor).await
}

fn validate_finding_state(data: &serde_json::Value) -> Result<(), String> {
    let Some(obj) = data.as_object() else {
        return Err("finding update: expected an object".into());
    };
    if obj.keys().any(|k| k != "state") {
        return Err("finding: only `state` can be changed by a client transaction".into());
    }
    let Some(state_val) = obj.get("state") else {
        return Err("finding update: missing `state`".into());
    };
    serde_json::from_value::<FindingState>(state_val.clone())
        .map(|_| ())
        .map_err(|e| format!("bad finding state: {e}"))
}

fn validate_task_create(data: &serde_json::Value) -> Result<(), String> {
    let task: Task = serde_json::from_value(data.clone()).map_err(|e| format!("bad task: {e}"))?;
    if task.status != TaskStatus::Todo {
        return Err("task: a new task must start in todo".into());
    }
    if task.fix.is_some() {
        return Err("task: a new task cannot already carry a fix".into());
    }
    Ok(())
}

const TASK_UPDATE_FIELDS: &[&str] = &["status", "assignee"];

async fn validate_task_update(
    core: &Core,
    id: &str,
    data: &serde_json::Value,
) -> Result<(), String> {
    let Some(obj) = data.as_object() else {
        return Err("task update: expected an object".into());
    };
    if let Some(bad) = obj
        .keys()
        .find(|k| !TASK_UPDATE_FIELDS.contains(&k.as_str()))
    {
        return Err(format!(
            "task: field '{bad}' cannot be changed by a client transaction"
        ));
    }
    if let Some(status_val) = data.get("status") {
        // A valid `TaskStatus`, nothing more: the lifecycle itself (todo ->
        // progress -> review -> done) is enforced where it's actually
        // driven — `delegate_task`/`decide_task` apply directly, bypassing
        // this validator entirely. Gating a *forward* move here too would
        // also gate the UI's undo/redo, which resends this same op shape
        // with the field's old value to roll a change back (plan "sync":
        // undo is a normal transaction, not a local-only rollback) — e.g.
        // undoing `assignTask`'s `progress` sends `status: "todo"` back,
        // which is not a legal forward transition but must still succeed.
        serde_json::from_value::<TaskStatus>(status_val.clone())
            .map_err(|e| format!("bad status: {e}"))?;
    }
    let store = core.0.deps.store.clone();
    let id_owned = id.to_string();
    let row = tokio::task::spawn_blocking(move || store.get(models::TASK, &id_owned))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    if row.is_none() {
        return Err("task: not found".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Table-driven audit of the allowlist against every op shape the UI
    //! actually sends (`grep -rn "\.transact(" ui/src` — `ui/src/actions.ts`
    //! and `ui/src/ui/SidePanel.tsx`), plus the undo inverses
    //! `ui/src/sync/UndoQueue.ts`/`TransactionQueue.ts`'s `buildInverse`
    //! produces for the ones currently wrapped in `undo.run` (`assignTask`,
    //! `markDone`, `snoozePrs`, `unsnooze`), and a representative set of
    //! core-owned writes that must stay rejected.

    use theodore_contract::{RevisionRequest, Transaction, TransactionResult, models};
    use theodore_domain::{HostKind, PrRef, RepoRef, Sha};
    use theodore_ports::{Fetched, NewAction, Store, SyncActionCode};

    use crate::core::Core;
    use crate::testkit::{self, fixtures};

    use super::MutationOp;

    fn put(store: &dyn Store, model: &str, id: &str, data: serde_json::Value) {
        store
            .apply(vec![NewAction {
                model: model.into(),
                model_id: id.into(),
                action: SyncActionCode::I,
                data: Some(data),
            }])
            .unwrap();
    }

    #[tokio::test]
    async fn allowlist_matches_every_op_shape_the_ui_sends() {
        let f = fixtures();
        let (core, _recv) = Core::start(f.deps());

        let pr_ref = PrRef {
            repo: RepoRef {
                host: HostKind::Github,
                owner: "acme".into(),
                name: "widgets".into(),
            },
            number: 7,
            host_id: Some("node-7".into()),
        };
        let detail = testkit::sample_pr_detail(&pr_ref, &Sha::new("base000"), &Sha::new("head111"));
        f.host.push_get_pr(Ok(Fetched::Modified {
            etag: Some("etag-1".into()),
            value: detail,
        }));

        let resp = core
            .open_pr(theodore_contract::OpenPrRequest {
                url: "https://github.com/acme/widgets/pull/7".into(),
            })
            .await
            .expect("open_pr");
        let pr = resp.pr;
        let revision = core
            .get_revision(RevisionRequest {
                pr: pr.clone(),
                head: resp.head,
            })
            .await
            .expect("get_revision");
        let hunks = testkit::hunk_ids(&revision);
        let anchor = testkit::sample_anchor("src/lib.rs", 2);

        // Pre-existing rows the "update"/"delete" cases below act on —
        // planted directly in the store, like `editDraft`/`setTaskStatus`
        // etc. act on a row that already synced down from an earlier
        // transaction.
        put(
            f.store.as_ref(),
            models::DRAFT,
            "d1",
            serde_json::json!({
                "id": "d1", "pr": pr.0, "anchor": anchor, "body": "orig",
                "source": {"kind": "me"},
            }),
        );
        put(
            f.store.as_ref(),
            models::FINDING,
            "find1",
            serde_json::json!({
                "id": "find1", "pr": pr.0, "skill": "s1", "agents": [],
                "anchor": anchor, "severity": "warn", "title": "t", "body": "b",
                "state": "open", "fix": null,
            }),
        );
        put(
            f.store.as_ref(),
            models::TASK,
            "task1",
            serde_json::json!({
                "id": "task1", "pr": pr.0, "title": "t", "body": "b",
                "anchor": null, "thread": null, "status": "todo",
                "assignee": {"kind": "human", "id": "author"}, "fix": null,
                "created_at": 0,
            }),
        );
        put(
            f.store.as_ref(),
            models::SKILL_RUN,
            "run1",
            serde_json::json!({"id": "run1", "pr": pr.0}),
        );
        put(
            f.store.as_ref(),
            models::INBOX_STATE,
            &pr.0,
            serde_json::json!({"pr": pr.0, "done": false, "snooze": {"kind": "next_push"}}),
        );

        struct Case {
            desc: &'static str,
            op: MutationOp,
            accepted: bool,
        }

        let cases = vec![
            // ── markHunkRead ────────────────────────────────────────────
            Case {
                desc: "markHunkRead: create hunk_read",
                op: MutationOp::Create {
                    model: models::HUNK_READ.into(),
                    id: format!("{}:{}", pr.0, hunks[0]),
                    data: serde_json::json!({"pr": pr.0, "hunk": hunks[0]}),
                },
                accepted: true,
            },
            // ── setCheckTicked: a "What to check" tick on, then off ─────
            Case {
                desc: "setCheckTicked: create check_tick",
                op: MutationOp::Create {
                    model: models::CHECK_TICK.into(),
                    id: format!("{}:b:h:g1:item", pr.0),
                    data: serde_json::json!({"pr": pr.0, "scope": "b:h:g1", "item": "item"}),
                },
                accepted: true,
            },
            Case {
                desc: "setCheckTicked: create check_tick without its pr",
                op: MutationOp::Create {
                    model: models::CHECK_TICK.into(),
                    id: "x".into(),
                    data: serde_json::json!({"scope": "b:h:g1"}),
                },
                accepted: false,
            },
            Case {
                desc: "setCheckTicked: delete check_tick",
                op: MutationOp::Delete {
                    model: models::CHECK_TICK.into(),
                    id: format!("{}:b:h:g1:item", pr.0),
                },
                accepted: true,
            },
            // ── setFileViewed (rust-review audit: previously untested) ──
            Case {
                desc: "setFileViewed: create file_viewed",
                op: MutationOp::Create {
                    model: models::FILE_VIEWED.into(),
                    id: format!("{}:src/lib.rs", pr.0),
                    data: serde_json::json!({"pr": pr.0, "path": "src/lib.rs", "state": "viewed"}),
                },
                accepted: true,
            },
            Case {
                desc: "setFileViewed: update file_viewed",
                op: MutationOp::Update {
                    model: models::FILE_VIEWED.into(),
                    id: format!("{}:src/lib.rs", pr.0),
                    data: serde_json::json!({"pr": pr.0, "path": "src/lib.rs", "state": "unviewed"}),
                },
                accepted: true,
            },
            // ── addDraft / editDraft / removeDraft ──────────────────────
            Case {
                desc: "addDraft: create draft",
                op: MutationOp::Create {
                    model: models::DRAFT.into(),
                    id: "d-new".into(),
                    data: serde_json::json!({
                        "id": "d-new", "pr": pr.0, "anchor": anchor, "body": "x",
                        "source": {"kind": "me"},
                    }),
                },
                accepted: true,
            },
            Case {
                desc: "editDraft: update draft body",
                op: MutationOp::Update {
                    model: models::DRAFT.into(),
                    id: "d1".into(),
                    data: serde_json::json!({"body": "edited"}),
                },
                accepted: true,
            },
            Case {
                desc: "removeDraft: delete draft",
                op: MutationOp::Delete {
                    model: models::DRAFT.into(),
                    id: "d1".into(),
                },
                accepted: true,
            },
            // ── ignoreFinding / findingToDraft's finding update ─────────
            Case {
                desc: "ignoreFinding: update finding state to ignored",
                op: MutationOp::Update {
                    model: models::FINDING.into(),
                    id: "find1".into(),
                    data: serde_json::json!({"state": "ignored"}),
                },
                accepted: true,
            },
            Case {
                desc: "findingToDraft: update finding state to drafted",
                op: MutationOp::Update {
                    model: models::FINDING.into(),
                    id: "find1".into(),
                    data: serde_json::json!({"state": "drafted"}),
                },
                accepted: true,
            },
            // ── findingToTask ────────────────────────────────────────────
            Case {
                desc: "findingToTask: create task starting in todo",
                op: MutationOp::Create {
                    model: models::TASK.into(),
                    id: "task-new".into(),
                    data: serde_json::json!({
                        "id": "task-new", "pr": pr.0, "title": "t", "body": "b",
                        "anchor": anchor, "thread": null, "status": "todo",
                        "assignee": {"kind": "human", "id": "me"}, "fix": null,
                        "created_at": 0,
                    }),
                },
                accepted: true,
            },
            // ── setTaskStatus ────────────────────────────────────────────
            Case {
                desc: "setTaskStatus: update status",
                op: MutationOp::Update {
                    model: models::TASK.into(),
                    id: "task1".into(),
                    data: serde_json::json!({"status": "review"}),
                },
                accepted: true,
            },
            // ── assignTask (undo-wrapped) + its undo inverse ────────────
            Case {
                desc: "assignTask: update assignee + status",
                op: MutationOp::Update {
                    model: models::TASK.into(),
                    id: "task1".into(),
                    data: serde_json::json!({
                        "assignee": {"kind": "agent", "id": "claude"},
                        "status": "progress",
                    }),
                },
                accepted: true,
            },
            Case {
                desc: "assignTask undo: not a forward transition, still accepted",
                op: MutationOp::Update {
                    model: models::TASK.into(),
                    id: "task1".into(),
                    data: serde_json::json!({
                        "assignee": {"kind": "human", "id": "author"},
                        "status": "todo",
                    }),
                },
                accepted: true,
            },
            // ── SidePanel's skill_run dismiss ────────────────────────────
            Case {
                desc: "dismiss skill_run: archive",
                op: MutationOp::Archive {
                    model: models::SKILL_RUN.into(),
                    id: "run1".into(),
                },
                accepted: true,
            },
            Case {
                desc: "skill_run update is not allowed (only archive is)",
                op: MutationOp::Update {
                    model: models::SKILL_RUN.into(),
                    id: "run1".into(),
                    data: serde_json::json!({"id": "run1"}),
                },
                accepted: false,
            },
            // ── markDone (undo-wrapped) + its undo inverse (delete) ─────
            Case {
                desc: "markDone: update inbox_state",
                op: MutationOp::Update {
                    model: models::INBOX_STATE.into(),
                    id: pr.0.clone(),
                    data: serde_json::json!({"done": true, "snooze": null}),
                },
                accepted: true,
            },
            // ── unsnooze (undo-wrapped) ──────────────────────────────────
            Case {
                desc: "unsnooze: update snooze to null",
                op: MutationOp::Update {
                    model: models::INBOX_STATE.into(),
                    id: pr.0.clone(),
                    data: serde_json::json!({"snooze": null}),
                },
                accepted: true,
            },
            Case {
                desc: "unsnooze undo: restores the old snooze value",
                op: MutationOp::Update {
                    model: models::INBOX_STATE.into(),
                    id: pr.0.clone(),
                    data: serde_json::json!({"snooze": {"kind": "next_push"}}),
                },
                accepted: true,
            },
            Case {
                desc: "inbox_state create (markDone when no row exists yet)",
                op: MutationOp::Create {
                    model: models::INBOX_STATE.into(),
                    id: "github:acme/widgets#999".into(),
                    data: serde_json::json!({"pr": "github:acme/widgets#999", "done": true, "snooze": null}),
                },
                accepted: true,
            },
            Case {
                desc: "its undo inverse: delete inbox_state",
                op: MutationOp::Delete {
                    model: models::INBOX_STATE.into(),
                    id: "github:acme/widgets#999".into(),
                },
                accepted: true,
            },
            // ── setSetting ───────────────────────────────────────────────
            Case {
                desc: "setSetting: create setting",
                op: MutationOp::Create {
                    model: models::SETTING.into(),
                    id: "user:custom.key".into(),
                    data: serde_json::json!({"key": "custom.key", "scope": "user", "value": 1}),
                },
                accepted: true,
            },
            Case {
                desc: "setSetting: update setting",
                op: MutationOp::Update {
                    model: models::SETTING.into(),
                    id: "user:custom.key".into(),
                    data: serde_json::json!({"key": "custom.key", "scope": "user", "value": 2}),
                },
                accepted: true,
            },
            // ── core-owned models: the client never writes these ────────
            Case {
                desc: "core-owned: pr create",
                op: MutationOp::Create {
                    model: models::PR.into(),
                    id: "x".into(),
                    data: serde_json::json!({}),
                },
                accepted: false,
            },
            Case {
                desc: "core-owned: pr_detail create",
                op: MutationOp::Create {
                    model: models::PR_DETAIL.into(),
                    id: "x".into(),
                    data: serde_json::json!({}),
                },
                accepted: false,
            },
            Case {
                desc: "core-owned: grouping create",
                op: MutationOp::Create {
                    model: models::GROUPING.into(),
                    id: "x".into(),
                    data: serde_json::json!({}),
                },
                accepted: false,
            },
            Case {
                desc: "core-owned: project create",
                op: MutationOp::Create {
                    model: models::PROJECT.into(),
                    id: "x".into(),
                    data: serde_json::json!({}),
                },
                accepted: false,
            },
            Case {
                desc: "core-owned: chat_message create",
                op: MutationOp::Create {
                    model: models::CHAT_MESSAGE.into(),
                    id: "x".into(),
                    data: serde_json::json!({}),
                },
                accepted: false,
            },
            Case {
                desc: "core-owned: thread update (goes through reply/resolve commands)",
                op: MutationOp::Update {
                    model: models::THREAD.into(),
                    id: "x".into(),
                    data: serde_json::json!({}),
                },
                accepted: false,
            },
            Case {
                desc: "core-owned: task delete (lifecycle is core-driven, not a client op)",
                op: MutationOp::Delete {
                    model: models::TASK.into(),
                    id: "task1".into(),
                },
                accepted: false,
            },
            Case {
                desc: "finding: fields other than state are core-owned",
                op: MutationOp::Update {
                    model: models::FINDING.into(),
                    id: "find1".into(),
                    data: serde_json::json!({"title": "hacked"}),
                },
                accepted: false,
            },
            Case {
                desc: "task: a new task cannot skip todo",
                op: MutationOp::Create {
                    model: models::TASK.into(),
                    id: "task-bad".into(),
                    data: serde_json::json!({
                        "id": "task-bad", "pr": pr.0, "title": "t", "body": "b",
                        "anchor": null, "thread": null, "status": "done",
                        "assignee": {"kind": "human", "id": "me"}, "fix": null,
                        "created_at": 0,
                    }),
                },
                accepted: false,
            },
            Case {
                desc: "unknown model is rejected",
                op: MutationOp::Create {
                    model: "no_such_model".into(),
                    id: "x".into(),
                    data: serde_json::json!({}),
                },
                accepted: false,
            },
        ];

        for (i, case) in cases.into_iter().enumerate() {
            let tx = Transaction {
                id: format!("tx-{i}"),
                ops: vec![case.op],
            };
            let result = core.transact(tx).await;
            let ok = matches!(result, TransactionResult::Ok { .. });
            assert_eq!(
                ok, case.accepted,
                "{}: expected accepted={}, got {result:?}",
                case.desc, case.accepted
            );
        }
    }
}
