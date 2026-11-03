//! Going back in a space (docs/spaces.md §3 "Undo, rewind, revert"): to
//! any earlier turn, or one file to the base or to before the last turn.

use serde::{Deserialize, Serialize};
use specta::Type;

/// "Back to here" on an agent reply: the space goes back to right after
/// that turn; every later turn is marked undone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct RewindSpaceRequest {
    pub space: String,
    /// `SpaceTurn::reply` of the turn to keep.
    pub reply: String,
}

/// Where "Revert" takes a file back to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SpaceRevertFrom {
    /// As it is in the base: the space's change to it is gone.
    Base,
    /// As it was before the last agent turn: only that turn's change goes.
    LastTurn,
}

/// "Revert to base" / "Revert the last turn's change" on a file of the
/// space's diff: the files are restored, checkpointed as an entry of the
/// turn log (undoable), and the next turn's prompt says so.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct RevertSpaceFileRequest {
    pub space: String,
    /// The file, and for a rename its old path too.
    pub paths: Vec<String>,
    pub from: SpaceRevertFrom,
}
