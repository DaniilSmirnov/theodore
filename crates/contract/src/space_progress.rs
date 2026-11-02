//! The agent's progress tracker above a space's composer (docs/spaces.md
//! "Tools the agent gets", `update_progress`): replaced on every call,
//! cleared when the turn ends.

use serde::{Deserialize, Serialize};
use specta::Type;
use theodore_domain::Millis;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SpaceProgressState {
    Pending,
    Active,
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SpaceProgressStep {
    pub label: String,
    pub state: SpaceProgressState,
}

/// `Space::progress`: the steps the agent last reported in this turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SpaceProgress {
    pub steps: Vec<SpaceProgressStep>,
    #[serde(default)]
    pub note: Option<String>,
    pub at: Millis,
}
