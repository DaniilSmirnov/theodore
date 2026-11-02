//! The syntax layer of code intelligence (docs/code-intel.md §1, §5.2):
//! tree-sitter tags (definitions and references) for every file of a
//! worktree, cached in SQLite by git blob sha so worktrees of one repo and
//! revisions of one PR parse each distinct file once.
//!
//! ```text
//! git ls-tree + status ─▶ Snapshot [path, blob sha, language]
//!                              │
//!            Indexer ◀─────────┘   queue: 1 diff ─▶ 2 mentions ─▶ 3 rest
//!               │ grammar provider (toolchain): LanguageId ─▶ Grammar?
//!               ▼
//!   read file ─▶ tree-sitter-tags ─▶ SyntaxIndex (index.sqlite, by blob)
//!                                         │
//!   query(snapshot, …) ─▶ tags ⋈ snapshot blobs ─▶ CodeSymbol / CodeHit
//!                                         └─ no grammar ─▶ text_search
//! ```
//!
//! Grammars are inputs: `code-intel-toolchain` loads them and hands over a
//! [`Grammar`]. Positions follow the contract in `theodore_ports::code_intel`:
//! 1-based lines, columns counted in chars. Edit helpers ([`replace_range`],
//! [`insert_after`]) use the same columns.

mod edit;
mod error;
mod extract;
mod grammar;
mod index;
mod indexer;
mod pos;
mod query;
mod rank;
mod snapshot;
mod text;

pub use edit::{insert_after, replace_range};
pub use error::{Result, SyntaxError};
pub use grammar::{CompiledGrammar, Grammar, kind_for_syntax_type};
pub use index::{IndexOutcome, SyntaxIndex};
pub use indexer::{
    GrammarFuture, GrammarProvider, Indexer, IndexerConfig, Priority, Progress, WorkspaceIndex,
};
pub use pos::byte_offset;
pub use query::SymbolAt;
pub use snapshot::{FileEntry, Snapshot, blob_sha, file_entry, git_snapshot};
pub use text::{preview, text_search};

/// Files larger than this are not indexed unless the caller says otherwise
/// (`code_intel.max_file_kb`, docs/code-intel.md §9).
pub const DEFAULT_MAX_FILE_BYTES: usize = 1024 * 1024;
