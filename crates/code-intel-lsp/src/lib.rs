//! The semantic layer of code intelligence (docs/code-intel.md §6): one LSP
//! client for every language server, and a pool that keeps a few of them
//! alive per repository.
//!
//! ```text
//! get_or_spawn ─▶ pool[(repo, server)] ── alive ─▶ LspSession ─▶ request
//!                                       └─ none ──▶ spawn (stdio) ─▶ initialize
//!                                                   ─▶ Warming ─▶ Ready
//! crash ─▶ Failed ─▶ next get_or_spawn restarts it once ─▶ second crash: Failed
//! idle / LRU over max_servers ─▶ shutdown ─▶ exit ─▶ kill after the grace
//! ```
//!
//! This crate does not find or install servers: `code-intel-toolchain` does
//! and hands over a [`LaunchConfig`]. Every path in and out is repo-relative
//! to the worktree root a call names; positions follow the contract in
//! `theodore_ports::code_intel` and are converted at this edge.

mod convert;
mod pool;
mod rpc;
mod session;
mod text;

use serde::{Deserialize, Serialize};
use theodore_ports::LanguageId;

pub use pool::LspPool;
pub use session::{LspSession, SessionOptions};
pub use text::PositionEncoding;

/// How to start one language server: what the toolchain resolved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LaunchConfig {
    /// The server's name (`"rust-analyzer"`): the pool key and the
    /// `Diagnostic::server` of what it reports.
    pub server: String,
    /// The program and its arguments.
    pub argv: Vec<String>,
    /// Extra environment for the process.
    pub env: Vec<(String, String)>,
    /// Sent as `initializationOptions`, and the source `workspace/configuration`
    /// is answered from (see [`LspSession`]). `null` sends none.
    pub init_options: serde_json::Value,
    /// The server takes several roots (`workspaceFolders`): worktrees of one
    /// repository share it instead of getting one each.
    pub workspace_folders: bool,
    /// The languages it serves: manifest id, LSP `languageId`
    /// (`"typescriptreact"`) and the file extensions, without the dot.
    pub language_ids: Vec<(LanguageId, String, Vec<String>)>,
}

impl LaunchConfig {
    /// The LSP `languageId` of a file, by its extension.
    pub fn language_id_for(&self, path: &std::path::Path) -> Option<&str> {
        let ext = path.extension()?.to_str()?;
        self.language_ids
            .iter()
            .find(|(_, _, exts)| exts.iter().any(|e| e.trim_start_matches('.') == ext))
            .map(|(_, id, _)| id.as_str())
    }
}
