//! Programmatic triggers for fixture demo events (plan §12.1).
//!
//! These are the only writers of PR/head state besides the host's own
//! review-write calls: they mutate the same `SharedState` that `FixtureHost`
//! and `FixtureGit` read, so the effect is visible through the ports, not
//! just through `Triggers` itself.

use std::sync::{Arc, Mutex};

use theodore_domain::{AgentId, CheckRollup, CheckStatus, PrRef, Sha};
use theodore_ports::HostError;

use crate::state::SharedState;

#[derive(Debug, thiserror::Error)]
pub enum TriggerError {
    #[error("unknown pr {0}")]
    UnknownPr(String),
    #[error("no revision recorded for head {0}")]
    UnknownHead(Sha),
}

pub type TriggerResult<T> = Result<T, TriggerError>;

/// Fires demo events against the fixture state shared with `FixtureHost`
/// and `FixtureGit`.
#[derive(Debug)]
pub struct Triggers {
    state: Arc<Mutex<SharedState>>,
}

impl Triggers {
    pub(crate) fn new(state: Arc<Mutex<SharedState>>) -> Self {
        Self { state }
    }

    /// Author pushed: the PR's head moves to `new_head` (which must already
    /// be a revision loaded from `git/<sanitized-pr-id>/<new_head>.json`),
    /// and the PR is reported by the next `changed_since`, with a new
    /// `get_pr` ETag.
    pub fn author_push(&self, pr: &PrRef, new_head: Sha) -> TriggerResult<()> {
        let mut state = self.state.lock().unwrap();
        let pr_id = pr.id();

        if !state.revisions_by_head.contains_key(&new_head) {
            return Err(TriggerError::UnknownHead(new_head));
        }
        if !state.prs.contains_key(&pr_id) {
            return Err(TriggerError::UnknownPr(pr_id.to_string()));
        }

        state.heads.insert(pr_id.clone(), new_head.clone());

        let revision = state.revisions_by_head.get(&new_head).cloned();
        if let Some(detail) = state.prs.get_mut(&pr_id) {
            detail.summary.head_sha = new_head;
            if let Some(revision) = revision {
                detail.summary.changed_files = revision.files.len() as u32;
                detail.summary.additions = revision.files.iter().map(|f| f.additions).sum();
                detail.summary.deletions = revision.files.iter().map(|f| f.deletions).sum();
            }
            // A push dismisses previously viewed files (plan §4).
            for v in &mut detail.viewed {
                v.state = theodore_domain::ViewedState::Dismissed;
            }
        }

        state.change_log.push(pr.clone());
        Ok(())
    }

    /// Checks turn green for `pr`.
    pub fn checks_green(&self, pr: &PrRef) -> TriggerResult<()> {
        let mut state = self.state.lock().unwrap();
        let pr_id = pr.id();
        let detail = state
            .prs
            .get_mut(&pr_id)
            .ok_or_else(|| TriggerError::UnknownPr(pr_id.to_string()))?;

        detail.summary.checks = CheckRollup::Pass;
        for check in &mut detail.checks {
            check.status = CheckStatus::Pass;
        }

        state.change_log.push(pr.clone());
        Ok(())
    }

    /// The next `run()` of `agent` fails instead of playing its script.
    pub fn agent_crash(&self, agent: AgentId) {
        self.state.lock().unwrap().crash_next.insert(agent);
    }

    /// The next calls of host `method` (e.g. `"submit_review"`) fail with
    /// `errors`, one per call, oldest first; later calls succeed again. The
    /// call is still recorded in `FixtureHost::calls`.
    pub fn fail_host(&self, method: &str, errors: Vec<HostError>) {
        self.state
            .lock()
            .unwrap()
            .host_faults
            .entry(method.to_string())
            .or_default()
            .extend(errors);
    }
}
