//! `refresh_inbox` (plan §6.5, §6.6): one `HostAdapter::search` per inbox
//! section, written as `pr` rows plus `inbox_state` rows that are created
//! once and never overwritten afterwards (so `done`/`snooze` survive).
//! Called once in the background on `Core::start` and again whenever the
//! poller's `changed_since` reports something changed. A complete pass also
//! asks after open rows outside any project that it no longer returned
//! (`reconcile.rs`).

use std::collections::BTreeSet;

use theodore_contract::{Ephemeral, InboxState, ToastLevel, models};
use theodore_domain::{InboxReason, PrId, PrState, PrSummary};
use theodore_ports::{InboxSection, NewAction, SyncActionCode};

use crate::core::Core;
use crate::error::{AppError, AppResult};
use crate::usecases::reconcile::SearchedRows;

const SECTIONS: [InboxSection; 4] = [
    InboxSection::ReviewRequested,
    InboxSection::Assigned,
    InboxSection::Authored,
    InboxSection::Mentioned,
];

impl Core {
    pub async fn refresh_inbox(&self) -> AppResult<()> {
        let mut actions = Vec::new();
        let mut seen = BTreeSet::new();
        let mut complete = true;

        for section in &SECTIONS {
            let result = self.0.deps.host.search_checked(None, section).await;
            self.note_host(&result);
            let summaries = match result {
                Ok(found) => {
                    complete &= found.complete;
                    found.prs
                }
                Err(e) => {
                    complete = false;
                    // The banner already says the host is offline, the token
                    // was rejected or the rate limit hit — no toast per section.
                    if Self::is_connection_error(&e) {
                        continue;
                    }
                    self.emit(Ephemeral::Toast {
                        level: ToastLevel::Warn,
                        text: format!("inbox refresh failed ({section:?}): {e}"),
                    });
                    continue;
                }
            };
            for summary in summaries {
                let pr_id = summary.pr.id();
                if !seen.insert(pr_id.clone()) {
                    // Already queued by an earlier section in this pass.
                    continue;
                }
                let pr_s = pr_id.0.clone();
                if self.is_stale_summary(&pr_s, summary.updated_at).await {
                    // Keep the newer row; only the inbox's reasons are news.
                    actions.push(NewAction {
                        model: models::PR.into(),
                        model_id: pr_s.clone(),
                        action: SyncActionCode::C,
                        data: Some(serde_json::json!({ "reasons": summary.reasons })),
                    });
                } else {
                    crate::poller::wake_if_pushed(self, &pr_id, &summary.head_sha).await;
                    let data = serde_json::to_value(&summary)
                        .map_err(|e| AppError::Invalid(e.to_string()))?;
                    actions.push(NewAction {
                        model: models::PR.into(),
                        model_id: pr_s.clone(),
                        action: SyncActionCode::U,
                        data: Some(data),
                    });
                }

                let store = self.0.deps.store.clone();
                let existing =
                    tokio::task::spawn_blocking(move || store.get(models::INBOX_STATE, &pr_s))
                        .await
                        .map_err(AppError::join)??;
                if existing.is_none() {
                    let state = InboxState {
                        pr: pr_id.clone(),
                        done: false,
                        snooze: None,
                    };
                    let data = serde_json::to_value(&state)
                        .map_err(|e| AppError::Invalid(e.to_string()))?;
                    actions.push(NewAction {
                        model: models::INBOX_STATE.into(),
                        model_id: pr_id.0.clone(),
                        action: SyncActionCode::I,
                        data: Some(data),
                    });
                }
            }
        }

        // A PR no search returned any more (you reviewed it, it was
        // reassigned, …) has no reason to be in the inbox: clear its stored
        // reasons. Only after a complete pass — a failed section, or one the
        // host cut short, would otherwise empty the inbox.
        if complete {
            let store = self.0.deps.store.clone();
            let rows = tokio::task::spawn_blocking(move || store.all(models::PR))
                .await
                .map_err(AppError::join)??;
            for row in rows {
                let had_reasons = row
                    .data
                    .get("reasons")
                    .and_then(|v| v.as_array())
                    .is_some_and(|a| !a.is_empty());
                if had_reasons && !seen.contains(&theodore_domain::PrId(row.id.clone())) {
                    actions.push(NewAction {
                        model: models::PR.into(),
                        model_id: row.id,
                        action: SyncActionCode::C,
                        data: Some(serde_json::json!({ "reasons": [] })),
                    });
                }
            }
        }

        self.apply(actions).await?;
        // After the writes: a row the search did return is settled by now.
        if complete {
            let found = seen.into_iter().collect();
            self.reconcile_open_prs(SearchedRows::OutsideProjects, &found)
                .await?;
        }
        self.refresh_stacks().await?;
        Ok(())
    }

    /// Your own open PRs, on the poller's cadence: the host sends no
    /// notification for what you do yourself, so a PR you opened, closed
    /// or that went into conflict elsewhere would wait for a Refresh.
    ///
    /// ```text
    /// search author:@me is:open ─▶ rows written (reasons kept, snoozes woken)
    ///        │ complete
    ///        ▼
    /// stored open rows of yours it didn't return ─▶ get_pr each (ETag'd)
    /// ```
    pub async fn refresh_authored(&self) -> AppResult<()> {
        let result = self
            .0
            .deps
            .host
            .search_checked(None, &InboxSection::Authored)
            .await;
        self.note_host(&result);
        let found = match result {
            Ok(found) => found,
            Err(e) if Self::is_connection_error(&e) => return Ok(()),
            Err(e) => return Err(AppError::Host(e)),
        };

        let mut actions = Vec::new();
        let mut seen = BTreeSet::new();
        for summary in &found.prs {
            let pr_id = summary.pr.id();
            if !seen.insert(pr_id.clone())
                || self.is_stale_summary(&pr_id.0, summary.updated_at).await
            {
                continue;
            }
            crate::poller::wake_if_pushed(self, &pr_id, &summary.head_sha).await;
            let mut data =
                serde_json::to_value(summary).map_err(|e| AppError::Invalid(e.to_string()))?;
            // This search speaks for "authored" only: keep the reasons the
            // full inbox pass found (a mention, …) alongside it.
            let stored = self.pr_row(&pr_id.0).await;
            if let Some(old) = stored
                .as_ref()
                .and_then(|r| r.data.get("reasons"))
                .and_then(|v| v.as_array())
                && let Some(new) = data.get_mut("reasons").and_then(|v| v.as_array_mut())
            {
                for reason in old {
                    if !new.contains(reason) {
                        new.push(reason.clone());
                    }
                }
            }
            actions.push(NewAction {
                model: models::PR.into(),
                model_id: pr_id.0.clone(),
                action: SyncActionCode::U,
                data: Some(data),
            });
            if self.inbox_state_row(&pr_id.0).await.is_none() {
                let state = InboxState {
                    pr: pr_id.clone(),
                    done: false,
                    snooze: None,
                };
                actions.push(NewAction {
                    model: models::INBOX_STATE.into(),
                    model_id: pr_id.0.clone(),
                    action: SyncActionCode::I,
                    data: Some(
                        serde_json::to_value(&state)
                            .map_err(|e| AppError::Invalid(e.to_string()))?,
                    ),
                });
            }
        }
        self.apply(actions).await?;

        // Gone from the search: closed or merged since. A conditional fetch
        // each (a 304 is free); open in a tab, the PR poller has it already.
        if found.complete {
            let store = self.0.deps.store.clone();
            let rows = tokio::task::spawn_blocking(move || store.all(models::PR))
                .await
                .map_err(AppError::join)??;
            let open_tabs = self.0.current_head.read().await.clone();
            for row in rows {
                if row.archived || seen.contains(&PrId(row.id.clone())) {
                    continue;
                }
                let Ok(summary) = serde_json::from_value::<PrSummary>(row.data) else {
                    continue;
                };
                let pr = summary.pr.id();
                if summary.reasons.contains(&InboxReason::Authored)
                    && matches!(summary.state, PrState::Open | PrState::Draft)
                    && summary.pr.repo.host == self.0.deps.host.kind()
                    && !open_tabs.contains_key(&pr)
                {
                    self.refresh_pr(&pr, &summary.pr).await;
                }
            }
        }
        self.refresh_stacks().await?;
        Ok(())
    }

    async fn pr_row(&self, id: &str) -> Option<theodore_ports::ModelRow> {
        let store = self.0.deps.store.clone();
        let id = id.to_string();
        tokio::task::spawn_blocking(move || store.get(models::PR, &id))
            .await
            .ok()
            .and_then(|r| r.ok())
            .flatten()
    }

    async fn inbox_state_row(&self, id: &str) -> Option<theodore_ports::ModelRow> {
        let store = self.0.deps.store.clone();
        let id = id.to_string();
        tokio::task::spawn_blocking(move || store.get(models::INBOX_STATE, &id))
            .await
            .ok()
            .and_then(|r| r.ok())
            .flatten()
    }
}
