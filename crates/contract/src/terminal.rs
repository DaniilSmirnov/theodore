//! A space's terminals (docs/spaces.md §3 "Terminal"): interactive PTYs in
//! the space's worktree, independent of the agent's turns. Several per
//! space (tabs); they survive navigating away from the space page and are
//! closed on archive and on a clean app quit.

use serde::{Deserialize, Serialize};
use specta::Type;
use theodore_domain::Millis;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum TerminalStatus {
    Running,
    /// The shell exited; `exit_code` says how.
    Exited,
}

/// One terminal tab. `scrollback` is the backend's bounded raw buffer
/// (base64, ANSI intact), written into the terminal emulator once before
/// live `Ephemeral::TerminalOutput` events (by `seq`) pick up where it left
/// off — the same seed-then-live pattern `space_logs` uses for command
/// output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SpaceTerminalInfo {
    pub id: String,
    pub space: String,
    pub title: String,
    pub shell: String,
    pub status: TerminalStatus,
    #[serde(default)]
    pub exit_code: Option<i32>,
    /// Set when the shell could not even be started (a missing cwd, an
    /// unknown `$SHELL`); `status` is `exited`.
    #[serde(default)]
    pub error: Option<String>,
    pub cols: u16,
    pub rows: u16,
    pub created_at: Millis,
    /// The last output event already folded into `scrollback`; live events
    /// at or before this `seq` are redundant.
    pub seq: u64,
    #[serde(default)]
    pub scrollback: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct OpenTerminalRequest {
    pub space: String,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct WriteTerminalRequest {
    pub terminal: String,
    /// What was typed or pasted, as `xterm.js`'s `onData` hands it over.
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct ResizeTerminalRequest {
    pub terminal: String,
    pub cols: u16,
    pub rows: u16,
}
