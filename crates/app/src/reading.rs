//! Side effects of reads (plan §3): once every hunk of a file is read, the
//! file becomes locally Viewed. That, and a Viewed / Not viewed toggle by
//! hand, queue a `set_viewed` outbox item that tells the host — unless it
//! is a branch review, which has no PR on the host.
//!
//! ```text
//! file_viewed row written ─▶ set_viewed {pr, path} pending? ─ yes ─▶ rides on it
//!                                      │ no
//!                                      ▼
//!                                    queued ─▶ worker sends the row's state when it runs
//! ```

use std::collections::{BTreeSet, HashMap};

use theodore_contract::{FileViewedRow, HunkRead, MutationOp, Transaction, models};
use theodore_domain::{HunkId, PrId, ViewedState};
use theodore_ports::{NewAction, SyncActionCode};

use crate::core::Core;
use crate::outbox::SetViewedPayload;

pub(crate) async fn handle_reads(core: &Core, tx: &Transaction) {
    let mut per_pr: HashMap<PrId, Vec<HunkId>> = HashMap::new();
    let mut toggled: Vec<(PrId, String)> = Vec::new();
    for op in &tx.ops {
        match op {
            MutationOp::Create { model, data, .. } if model == models::HUNK_READ => {
                if let Ok(hr) = serde_json::from_value::<HunkRead>(data.clone()) {
                    per_pr.entry(hr.pr).or_default().push(hr.hunk);
                }
            }
            MutationOp::Create { model, data, .. } | MutationOp::Update { model, data, .. }
                if model == models::FILE_VIEWED =>
            {
                if let Ok(fv) = serde_json::from_value::<FileViewedRow>(data.clone()) {
                    toggled.push((fv.pr, fv.path));
                }
            }
            _ => {}
        }
    }
    for (pr, path) in toggled {
        queue_viewed_sync(core, &pr, &path).await;
    }
    for (pr, new_hunks) in per_pr {
        handle_pr_reads(core, &pr, new_hunks).await;
    }
}

/// Tells the host about `path`'s local Viewed state, once: the worker reads
/// the row when it runs, so a toggle while an item is still pending (queued
/// or backing off) needs no item of its own. Call it after the row is written.
async fn queue_viewed_sync(core: &Core, pr: &PrId, path: &str) {
    // A branch review's number is a hash, not a host PR: telling the host
    // would fail and roll the local Viewed back.
    if core.is_branch_review(pr).await {
        return;
    }
    let payload = serde_json::to_value(SetViewedPayload {
        pr: pr.clone(),
        path: path.to_string(),
    })
    .expect("serializable");
    let store = core.0.deps.store.clone();
    let probe = payload.clone();
    let pending =
        tokio::task::spawn_blocking(move || store.outbox_has_pending("set_viewed", &probe))
            .await
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or(false);
    if !pending {
        let _ = core.enqueue_outbox("set_viewed", payload).await;
    }
}

async fn handle_pr_reads(core: &Core, pr: &PrId, new_hunks: Vec<HunkId>) {
    let Some(head) = core.0.current_head.read().await.get(pr).cloned() else {
        return;
    };
    let revision = {
        let revisions = core.0.revisions.read().await;
        match revisions.get(&(pr.clone(), head)) {
            Some(e) => e.revision.clone(),
            None => return,
        }
    };

    let store = core.0.deps.store.clone();
    let pr_s = pr.0.clone();
    let rows = match tokio::task::spawn_blocking(move || {
        store.query(models::HUNK_READ, "pr", &pr_s)
    })
    .await
    {
        Ok(Ok(rows)) => rows,
        _ => return,
    };
    let mut after: BTreeSet<HunkId> = BTreeSet::new();
    for row in &rows {
        if let Ok(hr) = serde_json::from_value::<HunkRead>(row.data.clone()) {
            after.insert(hr.hunk);
        }
    }
    let new_set: BTreeSet<HunkId> = new_hunks.into_iter().collect();
    let before: BTreeSet<HunkId> = after.difference(&new_set).cloned().collect();

    let paths = {
        use theodore_domain::reading::{ReadSet, newly_viewed};
        newly_viewed(&revision, &ReadSet(before), &ReadSet(after))
    };
    for path in paths {
        let row_id = format!("{}:{}", pr.0, path);
        let payload = FileViewedRow {
            pr: pr.clone(),
            path: path.clone(),
            state: ViewedState::Viewed,
        };
        let data = serde_json::to_value(&payload).expect("serializable");
        let action = NewAction {
            model: models::FILE_VIEWED.into(),
            model_id: row_id,
            action: SyncActionCode::U,
            data: Some(data),
        };
        let _ = core.apply(vec![action]).await;
        queue_viewed_sync(core, pr, &path).await;
    }
}

/// The host's per-file Viewed (a file marked on the host's own UI) as
/// `file_viewed` writes, on every fresh `PrDetail`. Only rows that differ
/// from the stored state; a path whose `set_viewed` is still pending keeps
/// the local choice — the host hasn't heard it yet.
pub(crate) async fn host_viewed_actions(
    core: &Core,
    pr: &PrId,
    host: &[theodore_domain::FileViewed],
) -> Vec<NewAction> {
    if host.is_empty() {
        return Vec::new();
    }
    let store = core.0.deps.store.clone();
    let (pr, host) = (pr.clone(), host.to_vec());
    tokio::task::spawn_blocking(move || {
        let stored: HashMap<String, ViewedState> = store
            .query(models::FILE_VIEWED, "pr", &pr.0)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|r| serde_json::from_value::<FileViewedRow>(r.data).ok())
            .map(|r| (r.path, r.state))
            .collect();
        host.into_iter()
            .filter(|f| {
                let local = stored.get(&f.path).unwrap_or(&ViewedState::Unviewed);
                *local != f.state
            })
            .filter(|f| {
                let probe = serde_json::to_value(SetViewedPayload {
                    pr: pr.clone(),
                    path: f.path.clone(),
                })
                .expect("serializable");
                !store
                    .outbox_has_pending("set_viewed", &probe)
                    .unwrap_or(true)
            })
            .map(|f| NewAction {
                model: models::FILE_VIEWED.into(),
                model_id: format!("{}:{}", pr.0, f.path),
                action: SyncActionCode::U,
                data: Some(
                    serde_json::to_value(FileViewedRow {
                        pr: pr.clone(),
                        path: f.path,
                        state: f.state,
                    })
                    .expect("serializable"),
                ),
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}
