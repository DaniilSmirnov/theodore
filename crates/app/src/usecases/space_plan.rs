//! Plan mode (docs/spaces.md §5 "Plan first"): a turn that explores and
//! ends in a plan, then Approve or Revise.
//!
//! ```text
//! Plan turn ─▶ plan card ─┬─ Approve ─▶ next turn in build_mode (Edit/Run):
//!                         │             "Plan approved, implement it" + plan
//!                         └─ Revise  ─▶ your feedback, another Plan turn
//! ```

use theodore_contract::{SpaceMode, SpaceSendRequest, SpaceStatus};

use crate::core::Core;
use crate::error::{AppError, AppResult};

/// What the transcript shows for an approval.
pub(crate) const APPROVED_TEXT: &str = "Plan approved, implement it.";

/// Added to a Plan turn's system prompt, for every agent (Claude Code's
/// `-p` plan mode has no `ExitPlanMode` to hand the plan in with).
const PLAN_RULES: &str = "This turn is in Plan mode: read and explore as much as you \
     need, but do not edit any file and do not run commands that change anything. End \
     the turn with the plan itself as your final message: a short context line, then \
     numbered steps naming the files and what changes in each, then how to verify it. \
     The user approves or revises it; an approved plan is implemented in the next turn.";

/// The system prompt of a turn in `mode`.
pub(crate) fn with_plan_rules(system: String, mode: SpaceMode) -> String {
    match mode {
        SpaceMode::Plan => format!("{system}\n{PLAN_RULES}"),
        SpaceMode::Ask | SpaceMode::Edit | SpaceMode::Run => system,
    }
}

/// The mode an approved plan is implemented in: the space's last Edit or
/// Run, Edit when it had neither.
pub(crate) fn build_mode(mode: Option<SpaceMode>) -> SpaceMode {
    match mode {
        Some(SpaceMode::Run) => SpaceMode::Run,
        _ => SpaceMode::Edit,
    }
}

/// What a Plan turn's reply holds: the plan the agent handed in, else its
/// final message. `None` when it said nothing.
pub(crate) fn plan_of(handed_in: Option<String>, text: &str) -> Option<String> {
    handed_in
        .filter(|p| !p.trim().is_empty())
        .or_else(|| (!text.trim().is_empty()).then(|| text.to_string()))
}

fn approved_prompt(plan: &str) -> String {
    format!(
        "The user approved your plan. Implement it now, all of it, then summarize what \
         you changed.\n\nThe plan:\n{plan}"
    )
}

impl Core {
    /// Approve on the plan card: the next turn implements the space's
    /// latest plan in its build mode. Refused while a turn runs and when
    /// the latest turn has no plan.
    pub async fn approve_space_plan(&self, space: &str) -> AppResult<String> {
        let _queue = self.space_queue_lock(space).await;
        let row = self.space_row(space).await?;
        if row.status == SpaceStatus::Running {
            return Err(AppError::Invalid(
                "the agent is still working in this space".into(),
            ));
        }
        let plan = row
            .turn_log
            .iter()
            .rev()
            .find(|t| !t.undone)
            .and_then(|t| t.plan.clone())
            .ok_or_else(|| AppError::Invalid("the last turn has no plan to approve".into()))?;
        self.space_send_as(
            SpaceSendRequest {
                space: row.id.clone(),
                text: APPROVED_TEXT.into(),
                agent: None,
                model: None,
                mode: Some(build_mode(row.build_mode)),
                attachments: None,
            },
            Some(approved_prompt(&plan)),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_approved_plan_is_built_in_the_last_writing_mode() {
        assert_eq!(build_mode(None), SpaceMode::Edit);
        assert_eq!(build_mode(Some(SpaceMode::Edit)), SpaceMode::Edit);
        assert_eq!(build_mode(Some(SpaceMode::Run)), SpaceMode::Run);
    }

    #[test]
    fn the_handed_in_plan_wins_over_the_final_text() {
        assert_eq!(
            plan_of(Some("1. a".into()), "I wrote the plan to a file"),
            Some("1. a".into())
        );
        assert_eq!(plan_of(None, "1. b"), Some("1. b".into()));
        assert_eq!(plan_of(Some(" ".into()), "1. c"), Some("1. c".into()));
        assert_eq!(plan_of(None, ""), None);
    }

    #[test]
    fn only_plan_turns_get_the_plan_rules() {
        assert!(with_plan_rules("base".into(), SpaceMode::Plan).contains("do not edit"));
        assert_eq!(with_plan_rules("base".into(), SpaceMode::Edit), "base");
    }
}
