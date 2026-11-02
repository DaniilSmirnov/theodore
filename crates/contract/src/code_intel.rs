//! Code intelligence for the UI (docs/code-intel.md §5.1, §9): Settings →
//! Languages lists each language of a project with its grammar, server and
//! index progress; `code_intel.install: ask` raises an install suggestion
//! (`Ephemeral::CodeIntelSuggest`) the user accepts or declines.
//!
//! ```text
//! worktree opened ─▶ server missing ─▶ install: ask ─▶ CodeIntelSuggest ─▶ code_intel_install
//!                                                                     └──▶ code_intel_decline (remembered)
//! ```

use serde::{Deserialize, Serialize};
use specta::Type;

/// `code_intel_status(project)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct CodeIntelStatus {
    /// `code_intel.enabled` for the project and a backend in this build.
    pub enabled: bool,
    pub install: CodeInstallPolicy,
    /// Where language servers may run (`code_intel.lsp`).
    pub lsp: CodeLsp,
    /// Empty when no worktree of the project is open yet.
    pub languages: Vec<CodeLanguage>,
}

/// `code_intel.lsp.review` and `code_intel.lsp.spaces`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct CodeLsp {
    /// In review runs on PRs. On: a PR's code may run (build scripts,
    /// macros), written by whoever opened the PR.
    pub review: CodeLspToggle,
    /// In spaces.
    pub spaces: CodeLspToggle,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct CodeLspToggle {
    pub on: bool,
    /// The global or local config file that sets it: the file wins over
    /// the toggle until the line is removed. A repo's file never does.
    pub from_config: Option<String>,
}

/// `code_intel_set_lsp`: Settings → Languages' toggles.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct CodeIntelLspRequest {
    pub scope: CodeLspScope,
    pub on: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum CodeLspScope {
    Review,
    Spaces,
}

/// `code_intel.install`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum CodeInstallPolicy {
    Ask,
    Always,
    Never,
}

/// One language of a project's worktree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct CodeLanguage {
    /// A manifest id: `rust`, `typescript`, …
    pub language: String,
    /// Files of this language in the worktree.
    pub files: u32,
    pub grammar: CodeInstall,
    /// The server that would serve it, when the manifest has one.
    pub server: Option<CodeServer>,
    /// 0–100.
    pub index_percent: u8,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct CodeServer {
    pub name: String,
    pub install: CodeInstall,
    pub state: CodeServerState,
}

/// Where a grammar or a server stands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CodeInstall {
    /// Not on disk; can be downloaded.
    Missing,
    Downloading,
    /// Found on the machine (PATH, rustup, xcrun, …).
    System {
        path: String,
        version: Option<String>,
    },
    /// Downloaded by Theodore.
    Installed {
        version: String,
    },
    /// No download for this platform.
    Unavailable,
    /// You said no to the install suggestion.
    Declined,
    Failed {
        error: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum CodeServerState {
    NotRunning,
    Warming,
    Ready,
    Failed,
}
