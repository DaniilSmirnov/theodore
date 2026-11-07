//! Code intelligence for the UI (docs/code-intel.md §5.1, §9): project
//! settings → Languages lists each language of a project with its grammar,
//! server and index progress; `code_intel.install: ask` raises an install suggestion
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
    /// Effective for the project: its local file, else your global file,
    /// else the default.
    pub on: bool,
    /// The global or local config file that sets it; `None`: the default.
    /// A repo's file never does. The switch writes the local file, so it
    /// wins over the global one.
    pub from_config: Option<String>,
}

/// `code_intel_set_lsp`: project settings → Languages' switches. Writes
/// `code_intel.lsp.<scope>` into the project's local config file
/// (`config.local.yaml`), never the repo's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct CodeIntelLspRequest {
    pub project: String,
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
    /// How to get it when Theodore can't: `gem install ruby-lsp`.
    pub hint: Option<String>,
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
    /// No download for this platform, and not on this machine.
    Unavailable,
    /// Needs a runtime that is missing or too old; nothing is downloaded
    /// for a server that can't run.
    NeedsRuntime {
        /// `java`, `node`, `ruby`, `php`, `dotnet`, `dotnet-sdk`.
        runtime: String,
        /// The oldest that works: `21`, `8.2`.
        min: String,
        /// What is on this machine, if one ran.
        found: Option<String>,
    },
    /// Runs the project's code, so it serves spaces and never a review.
    TrustedOnly,
    /// You said no to the install suggestion.
    Declined,
    Failed {
        error: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum CodeServerState {
    /// The project's switch keeps language servers off here.
    Off,
    NotRunning,
    Warming,
    Ready,
    Failed,
}

/// What an install is downloading (`Ephemeral::CodeIntelProgress`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum CodeInstallItem {
    Grammar,
    Server,
}

/// One step of an install: `downloading` a few times a second at most,
/// then `installed` or `failed` once per item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CodeInstallProgress {
    Downloading {
        received: u64,
        /// `None` when the server didn't say.
        total: Option<u64>,
    },
    Installed,
    Failed {
        error: String,
    },
}

/// `code_intel_hover`: the identifier under the pointer in a PR's diff, at
/// the PR's head.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct CodeHoverRequest {
    pub pr: theodore_domain::PrId,
    /// Repo-relative.
    pub path: String,
    /// 1-based line and char column.
    pub line: u32,
    pub col: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct CodeHover {
    /// Markdown.
    pub text: String,
    /// `lsp`: the language server's hover. `syntax`: the signature of a
    /// definition with that name (matched by name: may be another symbol).
    pub source: crate::SymbolSource,
}
