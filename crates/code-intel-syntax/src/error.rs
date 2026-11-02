//! The crate's error, convertible into the port's `CodeIntelError`.

use theodore_ports::code_intel::CodeIntelError;

#[derive(Debug, thiserror::Error)]
pub enum SyntaxError {
    #[error("index: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// The tags query does not compile against the grammar.
    #[error("tags query for {language}: {message}")]
    Grammar { language: String, message: String },
    #[error("git: {0}")]
    Git(String),
    /// The workspace closed while a file was being parsed.
    #[error("parse cancelled")]
    Cancelled,
    /// A query-level answer the port already has a variant for
    /// (`NoSymbol`, `FileNotFound`, …).
    #[error(transparent)]
    CodeIntel(#[from] CodeIntelError),
}

pub type Result<T> = std::result::Result<T, SyntaxError>;

impl From<SyntaxError> for CodeIntelError {
    fn from(e: SyntaxError) -> Self {
        match e {
            SyntaxError::CodeIntel(e) => e,
            other => CodeIntelError::Other(other.to_string()),
        }
    }
}
