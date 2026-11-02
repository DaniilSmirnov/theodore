//! Grammars as inputs, compiled once per language into a tags
//! configuration, and the tags syntax-type → `CodeSymbolKind` mapping.
//!
//! ```text
//! toolchain ─▶ Grammar { id, Language, tags.scm } ─▶ fingerprint
//!                   └─▶ TagsConfiguration (cached per language while the
//!                       fingerprint stays the same)
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};
use theodore_ports::code_intel::{CodeSymbolKind, LanguageId};
use tree_sitter::Language;
use tree_sitter_tags::TagsConfiguration;

use crate::error::{Result, SyntaxError};

/// Bumped when extraction changes what is stored for the same grammar, so
/// every blob is parsed again.
const EXTRACTOR_VERSION: u32 = 1;

/// One language's grammar, as `code-intel-toolchain` loaded it.
#[derive(Clone)]
pub struct Grammar {
    pub language_id: LanguageId,
    pub language: Language,
    /// The grammar's `tags.scm` (`@definition.*`, `@reference.*`,
    /// `@name`). For a grammar that extends another (TypeScript extends
    /// JavaScript) the toolchain concatenates both.
    pub tags_query: String,
}

impl std::fmt::Debug for Grammar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Grammar")
            .field("language_id", &self.language_id)
            .field("abi", &self.language.abi_version())
            .finish_non_exhaustive()
    }
}

impl Grammar {
    pub fn new(
        language_id: LanguageId,
        language: impl Into<Language>,
        tags_query: impl Into<String>,
    ) -> Self {
        Self {
            language_id,
            language: language.into(),
            tags_query: tags_query.into(),
        }
    }

    /// Identifies what this grammar would extract: a blob indexed under
    /// another fingerprint (a new grammar version or tags query) is parsed
    /// again.
    pub fn fingerprint(&self) -> String {
        let mut h = Sha256::new();
        h.update(self.language_id.as_str());
        h.update([0]);
        h.update(self.language.abi_version().to_le_bytes());
        h.update(self.language.node_kind_count().to_le_bytes());
        if let Some(m) = self.language.metadata() {
            h.update([m.major_version, m.minor_version, m.patch_version]);
        }
        h.update(self.language.name().unwrap_or_default());
        h.update([0]);
        h.update(self.tags_query.as_bytes());
        h.update(EXTRACTOR_VERSION.to_le_bytes());
        hex::encode(&h.finalize()[..8])
    }
}

/// A grammar with its tags query compiled, ready to extract tags. Share it
/// (`Arc`) across threads: compiling a query costs milliseconds.
pub struct CompiledGrammar {
    pub(crate) language_id: LanguageId,
    pub(crate) fingerprint: String,
    pub(crate) config: TagsConfiguration,
}

impl std::fmt::Debug for CompiledGrammar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledGrammar")
            .field("language_id", &self.language_id)
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

impl CompiledGrammar {
    pub fn compile(g: &Grammar) -> Result<Self> {
        let config =
            TagsConfiguration::new(g.language.clone(), &g.tags_query, "").map_err(|e| {
                SyntaxError::Grammar {
                    language: g.language_id.to_string(),
                    message: e.to_string(),
                }
            })?;
        Ok(Self {
            language_id: g.language_id.clone(),
            fingerprint: g.fingerprint(),
            config,
        })
    }

    pub fn language_id(&self) -> &LanguageId {
        &self.language_id
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
}

/// Compiled grammars by language; recompiled when a language's
/// fingerprint changes.
#[derive(Default)]
pub(crate) struct GrammarCache(Mutex<HashMap<LanguageId, Arc<CompiledGrammar>>>);

impl GrammarCache {
    pub(crate) fn get(&self, g: &Grammar) -> Result<Arc<CompiledGrammar>> {
        let fp = g.fingerprint();
        if let Some(c) = self.lock().get(&g.language_id)
            && c.fingerprint == fp
        {
            return Ok(c.clone());
        }
        // Compiled outside the lock: another language's lookup need not
        // wait for this one.
        let c = Arc::new(CompiledGrammar::compile(g)?);
        self.lock().insert(g.language_id.clone(), c.clone());
        Ok(c)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<LanguageId, Arc<CompiledGrammar>>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The kind a tags capture's syntax type (`@definition.<type>`,
/// `@reference.<type>`) stands for. Tags queries are coarse — Rust's tags
/// call a struct a `class` — so [`refine_kind`] narrows it by the keyword
/// before the name.
pub fn kind_for_syntax_type(syntax_type: &str) -> CodeSymbolKind {
    match syntax_type {
        "function" | "call" | "constructor" => CodeSymbolKind::Function,
        "method" => CodeSymbolKind::Method,
        "class" | "object" => CodeSymbolKind::Class,
        "struct" | "union" | "record" => CodeSymbolKind::Struct,
        "enum" => CodeSymbolKind::Enum,
        "interface" | "protocol" => CodeSymbolKind::Interface,
        "trait" => CodeSymbolKind::Trait,
        "type" | "typedef" | "type_alias" => CodeSymbolKind::Type,
        "constant" | "const" | "enum_variant" => CodeSymbolKind::Constant,
        "variable" | "var" => CodeSymbolKind::Variable,
        "field" | "property" => CodeSymbolKind::Field,
        "module" | "namespace" | "package" => CodeSymbolKind::Module,
        "macro" => CodeSymbolKind::Macro,
        _ => CodeSymbolKind::Other,
    }
}

/// Narrows a type-ish definition by the last declaration keyword between
/// its start and its name: `pub struct Foo` tagged `class` → `Struct`,
/// `trait Foo` tagged `interface` → `Trait`, `typedef struct Foo` →
/// `Struct`.
pub(crate) fn refine_kind(kind: CodeSymbolKind, before_name: &[u8]) -> CodeSymbolKind {
    if !matches!(
        kind,
        CodeSymbolKind::Class | CodeSymbolKind::Interface | CodeSymbolKind::Type
    ) {
        return kind;
    }
    let mut refined = kind;
    for word in before_name
        .split(|b| !(b.is_ascii_alphanumeric() || *b == b'_'))
        .filter(|w| !w.is_empty())
    {
        refined = match word {
            b"struct" | b"union" => CodeSymbolKind::Struct,
            b"enum" => CodeSymbolKind::Enum,
            b"trait" => CodeSymbolKind::Trait,
            b"interface" | b"protocol" => CodeSymbolKind::Interface,
            b"type" | b"typedef" | b"typealias" => CodeSymbolKind::Type,
            b"class" => CodeSymbolKind::Class,
            _ => refined,
        };
    }
    refined
}

pub(crate) fn kind_str(kind: CodeSymbolKind) -> &'static str {
    match kind {
        CodeSymbolKind::Function => "function",
        CodeSymbolKind::Method => "method",
        CodeSymbolKind::Class => "class",
        CodeSymbolKind::Struct => "struct",
        CodeSymbolKind::Enum => "enum",
        CodeSymbolKind::Interface => "interface",
        CodeSymbolKind::Trait => "trait",
        CodeSymbolKind::Type => "type",
        CodeSymbolKind::Constant => "constant",
        CodeSymbolKind::Variable => "variable",
        CodeSymbolKind::Field => "field",
        CodeSymbolKind::Module => "module",
        CodeSymbolKind::Macro => "macro",
        CodeSymbolKind::Other => "other",
    }
}

pub(crate) fn parse_kind(s: &str) -> CodeSymbolKind {
    match s {
        "function" => CodeSymbolKind::Function,
        "method" => CodeSymbolKind::Method,
        "class" => CodeSymbolKind::Class,
        "struct" => CodeSymbolKind::Struct,
        "enum" => CodeSymbolKind::Enum,
        "interface" => CodeSymbolKind::Interface,
        "trait" => CodeSymbolKind::Trait,
        "type" => CodeSymbolKind::Type,
        "constant" => CodeSymbolKind::Constant,
        "variable" => CodeSymbolKind::Variable,
        "field" => CodeSymbolKind::Field,
        "module" => CodeSymbolKind::Module,
        "macro" => CodeSymbolKind::Macro,
        _ => CodeSymbolKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syntax_types_map_to_kinds() {
        for (t, k) in [
            ("function", CodeSymbolKind::Function),
            ("method", CodeSymbolKind::Method),
            ("class", CodeSymbolKind::Class),
            ("interface", CodeSymbolKind::Interface),
            ("module", CodeSymbolKind::Module),
            ("macro", CodeSymbolKind::Macro),
            ("constant", CodeSymbolKind::Constant),
            ("type", CodeSymbolKind::Type),
            ("struct", CodeSymbolKind::Struct),
            ("enum", CodeSymbolKind::Enum),
            ("trait", CodeSymbolKind::Trait),
            ("call", CodeSymbolKind::Function),
            ("implementation", CodeSymbolKind::Other),
            ("nonsense", CodeSymbolKind::Other),
        ] {
            assert_eq!(kind_for_syntax_type(t), k, "{t}");
        }
    }

    #[test]
    fn keywords_refine_coarse_kinds() {
        let class = CodeSymbolKind::Class;
        assert_eq!(refine_kind(class, b"pub struct "), CodeSymbolKind::Struct);
        assert_eq!(
            refine_kind(class, b"pub(crate) enum "),
            CodeSymbolKind::Enum
        );
        assert_eq!(refine_kind(class, b"type "), CodeSymbolKind::Type);
        assert_eq!(refine_kind(class, b"export class "), CodeSymbolKind::Class);
        assert_eq!(
            refine_kind(CodeSymbolKind::Interface, b"pub trait "),
            CodeSymbolKind::Trait
        );
        assert_eq!(
            refine_kind(CodeSymbolKind::Type, b"typedef struct "),
            CodeSymbolKind::Struct
        );
        // Functions are never refined: `fn struct_name` is still a function.
        assert_eq!(
            refine_kind(CodeSymbolKind::Function, b"struct "),
            CodeSymbolKind::Function
        );
    }

    #[test]
    fn kind_strings_round_trip() {
        for t in [
            "function",
            "method",
            "class",
            "struct",
            "enum",
            "interface",
            "trait",
            "type",
            "constant",
            "variable",
            "field",
            "module",
            "macro",
            "other",
        ] {
            assert_eq!(kind_str(parse_kind(t)), t);
        }
    }
}
