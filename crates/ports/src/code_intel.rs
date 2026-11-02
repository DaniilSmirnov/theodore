//! Code intelligence (docs/code-intel.md): symbols, references, definitions
//! and diagnostics over a worktree. Shared by the syntax layer
//! (tree-sitter), the semantic layer (LSP), the toolchain that installs
//! grammars and servers, and `app`, which exposes them as MCP tools.
//!
//! ```text
//! question ─▶ server ready? ── yes ─▶ lsp     (Source::Lsp)
//!                            └─ no ──▶ syntax  (Source::Syntax)
//!                                      └─ gaps ─▶ text search (Source::Text)
//! ```
//!
//! Positions: `line` and `col` are 1-based; `col` counts Unicode scalar
//! values (chars) in the line, not bytes and not UTF-16 units. Each layer
//! converts at its edge (tree-sitter bytes, LSP UTF-16 or UTF-8).

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A manifest language id: `"rust"`, `"typescript"`, `"python"`, …
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LanguageId(pub String);

impl LanguageId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for LanguageId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A point in a file. 1-based line and char column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Position {
    pub line: u32,
    pub col: u32,
}

/// Half-open: `end` is the position just past the last char.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

/// A range in a repo-relative file (`/` separators).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Location {
    pub path: String,
    pub range: Range,
}

/// Which layer produced an answer, so the agent knows how far to trust it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// A language server: exact.
    Lsp,
    /// tree-sitter tags: matched by name.
    Syntax,
    /// Plain whole-word text search: a fallback.
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeSymbolKind {
    Function,
    Method,
    Class,
    Struct,
    Enum,
    Interface,
    Trait,
    Type,
    Constant,
    Variable,
    Field,
    Module,
    Macro,
    Other,
}

/// A definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeSymbol {
    pub name: String,
    pub kind: CodeSymbolKind,
    /// The whole definition (signature and body).
    pub range: Range,
    /// Just the name token inside `range`.
    pub name_range: Range,
    pub path: String,
    /// Enclosing symbol's name (`impl Foo`, `class Bar`), when known.
    pub container: Option<String>,
    pub source: Source,
}

/// A use, call site or definition hit with a little context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeHit {
    pub location: Location,
    /// The hit's line, trimmed, plus up to a couple of lines around it.
    pub preview: String,
    /// A definition rather than a plain use (best effort for `Text`).
    pub is_definition: bool,
    pub source: Source,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Info,
    Hint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub location: Location,
    pub severity: DiagnosticSeverity,
    pub message: String,
    pub code: Option<String>,
    /// The server that reported it (`"rust-analyzer"`).
    pub server: String,
}

/// One file edit of a rename or a symbol edit, in the order to apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextEdit {
    pub path: String,
    pub range: Range,
    pub new_text: String,
}

/// What a query returns: its items and how complete they are.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer<T> {
    pub items: Vec<T>,
    /// The best layer that contributed.
    pub source: Source,
    /// Share of the worktree's indexable files indexed so far, 0–100.
    pub index_percent: u8,
    /// One line for the agent when the answer is partial: "rust-analyzer
    /// warming up", "C++: no grammar installed", …
    pub note: Option<String>,
}

/// What a worktree may let its tools run (docs/code-intel.md §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrustMode {
    /// A review run on someone's PR: no build scripts, no proc macros, no
    /// server plugins, no binaries from inside the worktree.
    Review,
    /// A space whose repo commands the user has allowed.
    Trusted,
}

/// Where language servers may run (`code_intel.lsp`, docs/code-intel.md
/// §8): two switches, one per [`TrustMode`]. Off → that kind of worktree
/// gets syntax and text answers only. A server is a process over the
/// worktree, and most can run the project's code whatever their options
/// say (rust-analyzer runs `cargo metadata`, which honours a
/// `.cargo/config.toml` wrapper; typescript-language-server loads the
/// worktree's `node_modules/typescript`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct LspPolicy {
    /// `TrustMode::Review` (someone's PR). Default off: on, a PR's code may
    /// run; each server's review options still apply.
    pub review: bool,
    /// `TrustMode::Trusted` (spaces). Default on.
    pub spaces: bool,
}

impl Default for LspPolicy {
    fn default() -> Self {
        Self {
            review: false,
            spaces: true,
        }
    }
}

impl LspPolicy {
    /// Whether a worktree of `trust` may start language servers.
    pub fn allows(&self, trust: TrustMode) -> bool {
        match trust {
            TrustMode::Review => self.review,
            TrustMode::Trusted => self.spaces,
        }
    }
}

/// A worktree code intelligence is asked about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    /// Stable per repository (`github:owner/name`, a local folder's path):
    /// worktrees of one repo share a cache and, where possible, a server.
    pub repo_key: String,
    pub root: PathBuf,
    pub trust: TrustMode,
    /// The languages to serve; empty = detected from the files.
    pub languages: Vec<LanguageId>,
    /// Repo-relative files to index first: a PR's diff, a space's changed
    /// files. Names they define are indexed next (docs/code-intel.md §5.2).
    /// Empty: no priority, the whole worktree in path order.
    #[serde(default)]
    pub focus_paths: Vec<String>,
    /// The project's own `code_intel.lsp.*` (its global and local config
    /// layers). `None`: the facade's default policy.
    #[serde(default)]
    pub lsp: Option<LspPolicy>,
}

/// State of one installable thing (a grammar or a server).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum InstallState {
    /// Not on disk; can be downloaded.
    Missing,
    Downloading,
    /// Found on the machine (PATH, rustup, xcrun, …).
    System {
        path: PathBuf,
        version: Option<String>,
    },
    /// Downloaded by Theodore.
    Installed {
        version: String,
    },
    /// The manifest has no download for this platform and nothing is
    /// installed.
    Unavailable,
    /// The user said no (`code_intel.install: ask`).
    Declined,
    Failed {
        error: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerState {
    NotRunning,
    Warming,
    Ready,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerStatus {
    pub name: String,
    pub install: InstallState,
    pub state: ServerState,
}

/// One language of a workspace, for `status()` and Settings → Languages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanguageStatus {
    pub language: LanguageId,
    /// Files of this language in the worktree.
    pub files: usize,
    pub grammar: InstallState,
    /// The server that would serve it, if the manifest has one.
    pub server: Option<ServerStatus>,
    pub index_percent: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodeIntelError {
    #[error("workspace not open: {0}")]
    NotOpen(PathBuf),
    #[error("path outside the workspace: {0}")]
    OutsideWorkspace(String),
    #[error("file not found: {0}")]
    FileNotFound(String),
    #[error("no symbol `{name}` in {path}")]
    NoSymbol { path: String, name: String },
    #[error("{0}")]
    Unsupported(String),
    #[error("download failed: {0}")]
    Download(String),
    #[error("checksum mismatch for {0}")]
    Checksum(String),
    #[error("language server {server}: {message}")]
    Server { server: String, message: String },
    #[error("{0}")]
    Other(String),
}

pub type CodeIntelResult<T> = Result<T, CodeIntelError>;

/// Code intelligence over open workspaces. Implemented by
/// `crates/code-intel`; `app` turns it into MCP tools and UI commands.
/// Every `path` is repo-relative.
#[async_trait]
pub trait CodeIntelPort: Send + Sync {
    /// Start serving a worktree: plan languages, queue indexing, start
    /// nothing that needs consent. Idempotent per `root`.
    async fn open(&self, ws: Workspace) -> CodeIntelResult<()>;
    /// Stop indexing it and release its servers.
    async fn close(&self, root: &Path);

    /// Definitions named `name` (exact match, case-sensitive).
    async fn find_symbol(
        &self,
        root: &Path,
        name: &str,
        kind: Option<CodeSymbolKind>,
    ) -> CodeIntelResult<Answer<CodeSymbol>>;
    /// Top-level and nested symbols of one file.
    async fn file_symbols(&self, root: &Path, path: &str) -> CodeIntelResult<Answer<CodeSymbol>>;
    /// The source text of symbol `name` in `path` and its range.
    async fn symbol_body(
        &self,
        root: &Path,
        path: &str,
        name: &str,
    ) -> CodeIntelResult<(CodeSymbol, String)>;
    async fn definition(
        &self,
        root: &Path,
        path: &str,
        at: Position,
    ) -> CodeIntelResult<Answer<CodeHit>>;
    async fn references(
        &self,
        root: &Path,
        path: &str,
        at: Position,
    ) -> CodeIntelResult<Answer<CodeHit>>;
    /// Diagnostics of one file, or of every file the server reported on.
    async fn diagnostics(
        &self,
        root: &Path,
        path: Option<&str>,
    ) -> CodeIntelResult<Answer<Diagnostic>>;
    /// The edits a rename would make. Applying them is the caller's job.
    async fn rename(
        &self,
        root: &Path,
        path: &str,
        at: Position,
        new_name: &str,
    ) -> CodeIntelResult<Vec<TextEdit>>;

    /// Download `language`'s grammar and preferred server: the user said
    /// yes, or `code_intel.install: always`.
    async fn install(&self, language: &LanguageId) -> CodeIntelResult<()>;

    /// A file in the worktree changed on disk (spaces' watcher).
    async fn file_changed(&self, root: &Path, path: &str);
    async fn status(&self, root: &Path) -> CodeIntelResult<Vec<LanguageStatus>>;
}
