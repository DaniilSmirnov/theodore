//! PR rows that say "open" after the host stopped saying so. Searches only
//! add and update rows, so a PR closed past the merged/closed caps, or in a
//! repo nothing refreshes, stayed open here for good. A complete `is:open`
//! search says which rows to doubt; only the PR's own answer changes one.
//!
//! ```text
//! complete search ─▶ stored rows that claim open and it didn't return
//!                      │ not a branch review, not a live space's
//!                      ▼ the ones asked longest ago, up to MAX_PER_RUN
//!                   get_pr ─┬─ a PR ──────────▶ written as the host says
//!                           ├─ 404 ───────────▶ row removed
//!                           ├─ connection ────▶ stop, retry next run
//!                           └─ anything else ─▶ left as is
//! ```

use std::collections::HashSet;

use theodore_contract::{SpaceStatus, models};
use theodore_domain::{PrId, PrRef, PrState, PrSummary, RepoRef};
use theodore_ports::{Fetched, HostError, NewAction, SyncActionCode};

use crate::core::Core;
use crate::error::{AppError, AppResult};

/// `get_pr` is several host calls: a large backlog clears over a few runs.
const MAX_PER_RUN: usize = 10;
/// A row the host still calls open (a search can lag, and the inbox doesn't
/// return every open PR you were once on) isn't asked again before this.
const ASK_AGAIN_AFTER_MS: i64 = 60 * 60_000;

/// Which rows a complete search speaks for.
pub(crate) enum SearchedRows<'a> {
    /// A project's `is:open` search: every row of its repo.
    Repo(&'a RepoRef),
    /// The inbox: its host's rows outside any project. A project's rows
    /// include other people's PRs, which the inbox never returns.
    OutsideProjects,
}

impl Core {
    /// Reconciles the rows `searched` speaks for against `found`, the ids a
    /// complete search returned. Whether any row changed.
    pub(crate) async fn reconcile_open_prs(
        &self,
        searched: SearchedRows<'_>,
        found: &HashSet<PrId>,
    ) -> AppResult<bool> {
        let doubted = self.doubted_rows(searched, found).await?;
        let now = self.0.deps.clock.now();
        let mut changed = false;
        for pr in doubted {
            let id = pr.id();
            let fetched = self.0.deps.host.get_pr(&pr, None).await;
            self.note_host(&fetched);
            match fetched {
                // `id` guards against a host answering for another PR: the
                // write goes by the answer's id, and this row would stay.
                Ok(Fetched::Modified { value, .. }) if value.summary.pr.id() == id => {
                    self.apply_pr_detail(&value).await?;
                    changed = true;
                }
                Ok(_) => {}
                Err(HostError::NotFound(_)) => changed |= self.remove_gone_pr(&id).await?,
                Err(e) if Self::is_connection_error(&e) => break,
                Err(e) => {
                    tracing::warn!(pr = %id, error.code = e.code(), "couldn't reconcile a PR");
                }
            }
            self.reconciled_map().insert(id, now);
        }
        Ok(changed)
    }

    /// The rows to ask about this run, the ones asked longest ago first.
    async fn doubted_rows(
        &self,
        searched: SearchedRows<'_>,
        found: &HashSet<PrId>,
    ) -> AppResult<Vec<PrRef>> {
        let store = self.0.deps.store.clone();
        let rows = tokio::task::spawn_blocking(move || store.all(models::PR))
            .await
            .map_err(AppError::join)??;
        let behind_a_space = self.prs_behind_live_spaces().await?;
        let in_scope: Box<dyn Fn(&RepoRef) -> bool + Send> = match searched {
            SearchedRows::Repo(repo) => {
                let repo = repo.clone();
                Box::new(move |r| same_repo(r, &repo))
            }
            SearchedRows::OutsideProjects => {
                let store = self.0.deps.store.clone();
                let projects = tokio::task::spawn_blocking(move || store.all(models::PROJECT))
                    .await
                    .map_err(AppError::join)??;
                let projects: Vec<RepoRef> = projects
                    .into_iter()
                    .filter_map(|r| serde_json::from_value(r.data.get("repo")?.clone()).ok())
                    .collect();
                let host = self.0.deps.host.kind();
                Box::new(move |r| r.host == host && !projects.iter().any(|p| same_repo(r, p)))
            }
        };

        let mut doubted: Vec<PrRef> = rows
            .into_iter()
            .filter(|row| !row.archived)
            .filter_map(|row| serde_json::from_value::<PrSummary>(row.data).ok())
            .filter(|s| matches!(s.state, PrState::Open | PrState::Draft))
            // No URL: a branch or space review, with no PR on the host to ask.
            .filter(|s| !s.url.is_empty())
            .map(|s| s.pr)
            .filter(|pr| in_scope(&pr.repo))
            .filter(|pr| !found.contains(&pr.id()) && !behind_a_space.contains(&pr.id()))
            .collect();

        let now = self.0.deps.clock.now();
        let asked = self.reconciled_map();
        doubted.retain(|pr| {
            asked
                .get(&pr.id())
                .is_none_or(|at| now - at >= ASK_AGAIN_AFTER_MS)
        });
        doubted
            .sort_by_cached_key(|pr| (asked.get(&pr.id()).copied().unwrap_or(i64::MIN), pr.id().0));
        doubted.truncate(MAX_PER_RUN);
        Ok(doubted)
    }

    /// A live space's PR is kept fresh by `sweep_spaces`, and that sweep is
    /// what marks the space done. An archived space's has no such sweep.
    async fn prs_behind_live_spaces(&self) -> AppResult<HashSet<PrId>> {
        Ok(self
            .list_spaces(None)
            .await?
            .into_iter()
            .filter(|s| s.status != SpaceStatus::Archived)
            .flat_map(|s| [s.pr, s.review_pr])
            .flatten()
            .collect())
    }

    /// Drops what `close_branch_review` would, for a PR the host can't find.
    /// Threads, drafts and the inbox's done or snooze stay, so if the PR comes
    /// back (access to the repo restored) nothing of yours is lost. A PR open
    /// in a tab keeps its row until the next launch: its review reads it.
    async fn remove_gone_pr(&self, pr: &PrId) -> AppResult<bool> {
        // Same lock opening a PR holds: a patch to this row in flight (the
        // stack links) would otherwise fail on the missing row.
        let _guard = self.0.open_lock.lock().await;
        if self.0.current_head.read().await.contains_key(pr) {
            return Ok(false);
        }
        let mut actions = Vec::new();
        for model in [models::PR, models::PR_DETAIL, models::GROUPING] {
            if model == models::PR || self.get_row(model, &pr.0).await?.is_some() {
                actions.push(NewAction {
                    model: model.into(),
                    model_id: pr.0.clone(),
                    action: SyncActionCode::D,
                    data: None,
                });
            }
        }
        self.apply(actions).await?;
        Ok(true)
    }

    fn reconciled_map(&self) -> std::sync::MutexGuard<'_, std::collections::HashMap<PrId, i64>> {
        self.0.reconciled.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// Hosts match owner and name in any case, and a project's spelling can
/// differ from the one a row was stored under.
fn same_repo(a: &RepoRef, b: &RepoRef) -> bool {
    a.host == b.host
        && a.owner.eq_ignore_ascii_case(&b.owner)
        && a.name.eq_ignore_ascii_case(&b.name)
}
