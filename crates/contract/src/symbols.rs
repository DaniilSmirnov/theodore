//! Go-to-definition / find-references (slice `symbols`): `find_symbol`
//! asks code intelligence (docs/code-intel.md) when it is on, else greps
//! the PR head worktree for a name and classifies each hit as a
//! definition or a plain reference.

use serde::{Deserialize, Serialize};
use specta::Type;
use theodore_domain::PrId;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct FindSymbolRequest {
    pub pr: PrId,
    pub name: String,
    /// Narrows the search to one language's definition patterns (e.g.
    /// `"rust"`, `"typescript"`); `None` tries every supported language.
    pub lang: Option<String>,
    /// The file the click came from. Hits in it, or near it, rank first.
    pub from_path: Option<String>,
    /// Where in `from_path` the click was (1-based line, char column):
    /// lets code intelligence resolve the exact symbol instead of the name.
    #[serde(default)]
    pub line: Option<u32>,
    #[serde(default)]
    pub col: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Definition,
    Reference,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct SymbolHit {
    pub path: String,
    pub line: u32,
    pub kind: SymbolKind,
    /// The matched source line, trimmed.
    pub preview: String,
    /// Who found it; the grep fallback is `Text`.
    #[serde(default)]
    pub source: Option<SymbolSource>,
}

/// How far to trust a hit (docs/code-intel.md §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum SymbolSource {
    /// A language server: exact.
    Lsp,
    /// tree-sitter: matched by name.
    Syntax,
    /// Whole-word text search.
    Text,
}
