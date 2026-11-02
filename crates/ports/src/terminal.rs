//! Terminal port (docs/spaces.md): one interactive PTY in a space's
//! worktree — the user's login shell, raw bytes in both directions, plus
//! resize. Unlike `CommandPort` (one shot, line-buffered, no stdin) this
//! stays open for input and carries control sequences through untouched,
//! which `xterm.js` needs to render it faithfully.
//!
//! ```text
//! spawn (shell -l, cwd, env, cols×rows) ─▶ Output(bytes)… ─▶ Exit
//!    │                     ▲                     ▲
//!    ├─ input   ───────────┘ (keystrokes, paste)  │
//!    ├─ resize  ──────────────────────────────────┘ (SIGWINCH)
//!    └─ kill    ─▶ SIGTERM ─▶ grace ─▶ SIGKILL, whole process group
//! ```

use std::path::PathBuf;

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalSpec {
    /// The shell binary, e.g. `$SHELL`.
    pub shell: String,
    /// Extra argv, e.g. `["-l"]` for a login shell.
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// Added on top of the filtered inherited environment (Theodore's own
    /// secrets are never inherited), same as `CommandSpec::env`.
    pub env: Vec<(String, String)>,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalEvent {
    /// Raw bytes read from the PTY, ANSI and all — never split mid UTF-8 on
    /// purpose, but a terminal emulator has to tolerate that anyway.
    Output(Vec<u8>),
    /// Always the last event, exactly once.
    Exit(TerminalExit),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalExit {
    pub code: Option<i32>,
    /// Set when the shell could not be started at all.
    pub error: Option<String>,
}

/// A running PTY. Dropping it does not stop it; `kill` does.
pub struct TerminalHandle {
    pub events: mpsc::Receiver<TerminalEvent>,
    /// Bytes typed or pasted into the terminal.
    pub input: mpsc::UnboundedSender<Vec<u8>>,
    /// New size, in cells.
    pub resize: mpsc::UnboundedSender<(u16, u16)>,
    pub kill: CancellationToken,
    /// The shell's own pid, known as soon as it's spawned; `None` if it
    /// never started. Spawned as its own process group leader, so this
    /// doubles as its pgid.
    pub pid: Option<u32>,
}

#[async_trait]
pub trait TerminalPort: Send + Sync {
    /// Starts `spec`. A shell that can't be spawned still yields a handle
    /// whose only event is an `Exit` with `error` set.
    async fn spawn(&self, spec: TerminalSpec) -> TerminalHandle;
}
