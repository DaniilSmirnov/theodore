//! `merge_info`, `merge_pr`, `assignable_users`, `set_reviewers`,
//! `set_draft`, `update_branch`, `react`, `edit_comment` (plan §6.5, §6.7):
//! reads go straight to the `HostAdapter`; writes go through the outbox
//! like `submit_review` (retry/backoff, compensating toast on failure), and
//! on success refresh the PR's `pr`/`pr_detail` rows. A `HostKind::Local`
//! PR has no host behind it, so every write rejects synchronously instead
//! of failing later out of the outbox.

use theodore_contract::{
    ConflictFile, ConflictKind as WireConflictKind, DraftStateRequest, EditCommentRequest,
    MergeInfo, MergeMethod as WireMergeMethod, MergeRequest, MergeState as WireMergeState,
    ReactRequest, RerunCheckRequest, ReviewersRequest,
};
use theodore_domain::{HostKind, Person, PrId, PrRef};
use theodore_ports::{
    ConflictKind as PortConflictKind, MergeMethod as PortMergeMethod, MergeState as PortMergeState,
};

use crate::core::Core;
use crate::error::{AppError, AppResult};
use crate::outbox::{
    DraftPayload, EditCommentPayload, MergePayload, ReactPayload, RerunCheckPayload,
    ReviewersPayload, UpdateBranchPayload,
};

fn to_wire_method(m: PortMergeMethod) -> WireMergeMethod {
    match m {
        PortMergeMethod::Merge => WireMergeMethod::Merge,
        PortMergeMethod::Squash => WireMergeMethod::Squash,
        PortMergeMethod::Rebase => WireMergeMethod::Rebase,
    }
}

fn to_port_method(m: WireMergeMethod) -> PortMergeMethod {
    match m {
        WireMergeMethod::Merge => PortMergeMethod::Merge,
        WireMergeMethod::Squash => PortMergeMethod::Squash,
        WireMergeMethod::Rebase => PortMergeMethod::Rebase,
    }
}

fn to_wire_state(s: PortMergeState) -> WireMergeState {
    match s {
        PortMergeState::Clean => WireMergeState::Clean,
        PortMergeState::Blocked => WireMergeState::Blocked,
        PortMergeState::Behind => WireMergeState::Behind,
        PortMergeState::Dirty => WireMergeState::Dirty,
        PortMergeState::Unstable => WireMergeState::Unstable,
        PortMergeState::Unknown => WireMergeState::Unknown,
    }
}

fn to_wire_conflict_kind(k: PortConflictKind) -> WireConflictKind {
    match k {
        PortConflictKind::Content => WireConflictKind::Content,
        PortConflictKind::AddAdd => WireConflictKind::AddAdd,
        PortConflictKind::ModifyDelete => WireConflictKind::ModifyDelete,
        PortConflictKind::RenameDelete => WireConflictKind::RenameDelete,
        PortConflictKind::RenameRename => WireConflictKind::RenameRename,
        PortConflictKind::DirectoryFile => WireConflictKind::DirectoryFile,
        PortConflictKind::Submodule => WireConflictKind::Submodule,
        PortConflictKind::Other => WireConflictKind::Other,
    }
}

pub(crate) fn reject_if_local(pr_ref: &PrRef, action: &str) -> AppResult<()> {
    if pr_ref.repo.host == HostKind::Local {
        return Err(AppError::Invalid(format!(
            "{action} isn't possible on a local branch review (there is no host behind it)"
        )));
    }
    Ok(())
}

impl Core {
    pub async fn merge_info(&self, pr: PrId) -> AppResult<MergeInfo> {
        let pr_ref = self
            .pr_ref(&pr)
            .await
            .ok_or_else(|| AppError::NotFound(format!("pr {pr}")))?;
        reject_if_local(&pr_ref, "merging")?;
        let status = self.0.deps.host.merge_status(&pr_ref).await?;
        Ok(MergeInfo {
            mergeable: status.mergeable,
            state: to_wire_state(status.state),
            allowed_methods: status
                .allowed_methods
                .into_iter()
                .map(to_wire_method)
                .collect(),
            auto_merge_enabled: status.auto_merge_enabled,
            in_queue: status.in_queue,
            required_approvals: status.required_approvals,
            auto_merge_allowed: status.auto_merge_allowed,
            merge_queue: status.merge_queue,
            default_method: status.default_method.map(to_wire_method),
        })
    }

    pub async fn assignable_users(&self, pr: PrId) -> AppResult<Vec<Person>> {
        let pr_ref = self
            .pr_ref(&pr)
            .await
            .ok_or_else(|| AppError::NotFound(format!("pr {pr}")))?;
        reject_if_local(&pr_ref, "picking reviewers")?;
        Ok(self.0.deps.host.assignable_users(&pr_ref.repo).await?)
    }

    pub async fn merge_pr(&self, req: MergeRequest) -> AppResult<()> {
        let pr_ref = self
            .pr_ref(&req.pr)
            .await
            .ok_or_else(|| AppError::NotFound(format!("pr {}", req.pr)))?;
        reject_if_local(&pr_ref, "merging")?;
        let method = crate::usecases::telemetry::wire_str(&req.method);
        let payload = MergePayload {
            pr: req.pr,
            method: to_port_method(req.method),
            title: req.title,
            body: req.body,
            auto: req.auto,
            queue: req.queue,
        };
        let data = serde_json::to_value(&payload).map_err(|e| AppError::Invalid(e.to_string()))?;
        self.enqueue_outbox("merge_pr", data).await?;
        theodore_telemetry::product(theodore_telemetry::ProductEvent::Merge { method });
        Ok(())
    }

    pub async fn set_reviewers(&self, req: ReviewersRequest) -> AppResult<()> {
        let pr_ref = self
            .pr_ref(&req.pr)
            .await
            .ok_or_else(|| AppError::NotFound(format!("pr {}", req.pr)))?;
        reject_if_local(&pr_ref, "changing reviewers")?;
        let payload = ReviewersPayload {
            pr: req.pr,
            add: req.add,
            add_teams: req.add_teams,
            remove: req.remove,
        };
        let data = serde_json::to_value(&payload).map_err(|e| AppError::Invalid(e.to_string()))?;
        self.enqueue_outbox("set_reviewers", data).await?;
        Ok(())
    }

    pub async fn set_draft(&self, req: DraftStateRequest) -> AppResult<()> {
        let pr_ref = self
            .pr_ref(&req.pr)
            .await
            .ok_or_else(|| AppError::NotFound(format!("pr {}", req.pr)))?;
        reject_if_local(&pr_ref, "changing draft state")?;
        let payload = DraftPayload {
            pr: req.pr,
            draft: req.draft,
        };
        let data = serde_json::to_value(&payload).map_err(|e| AppError::Invalid(e.to_string()))?;
        self.enqueue_outbox("set_draft", data).await?;
        Ok(())
    }

    pub async fn update_branch(&self, pr: PrId) -> AppResult<()> {
        let pr_ref = self
            .pr_ref(&pr)
            .await
            .ok_or_else(|| AppError::NotFound(format!("pr {pr}")))?;
        reject_if_local(&pr_ref, "updating the branch")?;
        let payload = UpdateBranchPayload { pr };
        let data = serde_json::to_value(&payload).map_err(|e| AppError::Invalid(e.to_string()))?;
        self.enqueue_outbox("update_branch", data).await?;
        Ok(())
    }

    pub async fn react(&self, req: ReactRequest) -> AppResult<()> {
        let pr_ref = self
            .pr_ref(&req.pr)
            .await
            .ok_or_else(|| AppError::NotFound(format!("pr {}", req.pr)))?;
        reject_if_local(&pr_ref, "reacting to a comment")?;
        let payload = ReactPayload {
            pr: req.pr,
            comment: req.comment,
            emoji: req.emoji,
            on: req.on,
        };
        let data = serde_json::to_value(&payload).map_err(|e| AppError::Invalid(e.to_string()))?;
        self.enqueue_outbox("react", data).await?;
        Ok(())
    }

    pub async fn edit_comment(&self, req: EditCommentRequest) -> AppResult<()> {
        let pr_ref = self
            .pr_ref(&req.pr)
            .await
            .ok_or_else(|| AppError::NotFound(format!("pr {}", req.pr)))?;
        reject_if_local(&pr_ref, "editing a comment")?;
        let payload = EditCommentPayload {
            pr: req.pr,
            comment: req.comment,
            body: req.body,
        };
        let data = serde_json::to_value(&payload).map_err(|e| AppError::Invalid(e.to_string()))?;
        self.enqueue_outbox("edit_comment", data).await?;
        Ok(())
    }

    /// Re-runs one check (plan "re-run a check"). `req.check` is resolved
    /// back to the full `Check` from the stored `pr_detail` when the outbox
    /// item runs, so a stale UI value never reaches the adapter.
    pub async fn rerun_check(&self, req: RerunCheckRequest) -> AppResult<()> {
        let pr_ref = self
            .pr_ref(&req.pr)
            .await
            .ok_or_else(|| AppError::NotFound(format!("pr {}", req.pr)))?;
        reject_if_local(&pr_ref, "re-running a check")?;
        let payload = RerunCheckPayload {
            pr: req.pr,
            check: req.check,
        };
        let data = serde_json::to_value(&payload).map_err(|e| AppError::Invalid(e.to_string()))?;
        self.enqueue_outbox("rerun_check", data).await?;
        Ok(())
    }

    /// Conflicting paths of merging this PR's base into its head, computed
    /// locally (`GitPort::merge_tree_conflicts`) rather than trusting only
    /// `PrSummary.conflicts`'s bare bool (plan "conflicts"). No host call,
    /// so it works for a `HostKind::Local` PR too. `GitError::Unsupported`
    /// (an older local `git`) propagates as-is; the UI hides the list.
    pub async fn conflicts(&self, pr: PrId) -> AppResult<Vec<ConflictFile>> {
        self.ensure_open(&pr).await?;
        let detail = self.load_pr_detail(&pr).await?;
        let repo_path = self.0.deps.git.ensure_repo(&detail.summary.pr.repo).await?;
        let head = self
            .0
            .current_head
            .read()
            .await
            .get(&pr)
            .cloned()
            .unwrap_or_else(|| detail.summary.head_sha.clone());
        // The host computes mergeability against the base branch as it is
        // now; `base_sha` is the base as of the PR's last update and can be
        // far behind (kdb#83: GitHub reported conflicts, the stale base had
        // none). Prefer the fetched branch tip.
        let base_ref = format!("refs/heads/{}", detail.summary.base_ref);
        let base = self
            .0
            .deps
            .git
            .resolve_ref(&repo_path, &base_ref)
            .await
            .unwrap_or_else(|_| detail.base_sha.clone());
        let entries = self
            .0
            .deps
            .git
            .merge_tree_conflicts(&repo_path, &base, &head)
            .await?;
        Ok(entries
            .into_iter()
            .map(|e| ConflictFile {
                path: e.path,
                kind: to_wire_conflict_kind(e.kind),
            })
            .collect())
    }
}
