//! One file's tags: tree-sitter-tags output converted to contract
//! positions, with kinds refined and each tag's container resolved.
//!
//! ```text
//! source ─▶ TagsContext (per thread) ─▶ Tag { byte ranges, syntax type }
//!        ─▶ kind (+ keyword refinement) ─▶ container (innermost enclosing
//!           definition or impl) ─▶ Extracted { char positions }
//! ```

use std::cell::RefCell;
use std::ops::Range as Bytes;
use std::sync::atomic::AtomicUsize;

use theodore_ports::code_intel::{CodeSymbolKind, Range};
use tree_sitter_tags::TagsContext;

use crate::error::{Result, SyntaxError};
use crate::grammar::{CompiledGrammar, kind_for_syntax_type, refine_kind};
use crate::pos::LineIndex;

/// A tag ready to store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Extracted {
    pub name: String,
    pub kind: CodeSymbolKind,
    /// The tags capture's type: `function`, `call`, `implementation`, …
    pub syntax: String,
    pub is_def: bool,
    pub range: Range,
    pub name_range: Range,
    pub container: Option<String>,
}

thread_local! {
    /// A parser per thread: indexing runs on the blocking pool, and a
    /// `TagsContext` is cheap to keep but not `Sync`.
    static CONTEXT: RefCell<TagsContext> = RefCell::new(TagsContext::new());
}

struct RawTag {
    name: String,
    kind: CodeSymbolKind,
    syntax: String,
    is_def: bool,
    range: Bytes<usize>,
    name_range: Bytes<usize>,
}

/// Extracts every tag of `source`. `cancel` non-zero aborts the parse.
pub(crate) fn extract(
    grammar: &CompiledGrammar,
    source: &[u8],
    cancel: Option<&AtomicUsize>,
) -> Result<Vec<Extracted>> {
    let raw = CONTEXT.with(|ctx| -> Result<Vec<RawTag>> {
        let mut ctx = ctx.borrow_mut();
        let config = &grammar.config;
        let (tags, _has_error) = ctx
            .generate_tags(config, source, cancel)
            .map_err(map_tags_error)?;
        let mut out = Vec::new();
        for tag in tags {
            let tag = tag.map_err(map_tags_error)?;
            let Ok(name) = std::str::from_utf8(&source[tag.name_range.clone()]) else {
                continue;
            };
            let syntax = config.syntax_type_name(tag.syntax_type_id).to_string();
            let mut kind = kind_for_syntax_type(&syntax);
            if tag.is_definition {
                kind = refine_kind(kind, &source[tag.range.start..tag.name_range.start]);
            }
            out.push(RawTag {
                name: name.to_string(),
                kind,
                syntax,
                is_def: tag.is_definition,
                range: tag.range,
                name_range: tag.name_range,
            });
        }
        Ok(out)
    })?;
    Ok(finish(raw, source))
}

/// What a function definition directly inside a container is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Inside {
    Methods,
    Functions,
    Unknown,
}

fn map_tags_error(e: tree_sitter_tags::Error) -> SyntaxError {
    match e {
        tree_sitter_tags::Error::Cancelled => SyntaxError::Cancelled,
        other => SyntaxError::Grammar {
            language: String::new(),
            message: other.to_string(),
        },
    }
}

/// Resolves containers and converts byte ranges to positions.
///
/// Containers come from a sweep over tags sorted by (start asc, end desc):
/// the stack holds the open candidates (definitions and `impl` blocks), so
/// the innermost one still containing a tag is on top.
fn finish(mut raw: Vec<RawTag>, source: &[u8]) -> Vec<Extracted> {
    raw.sort_by(|a, b| {
        (a.range.start, std::cmp::Reverse(a.range.end))
            .cmp(&(b.range.start, std::cmp::Reverse(b.range.end)))
    });
    let lines = LineIndex::new(source);
    // (range, label, what a `fn` directly inside it is)
    let mut stack: Vec<(Bytes<usize>, String, Inside)> = Vec::new();
    let mut out = Vec::with_capacity(raw.len());
    for mut t in raw {
        while stack.last().is_some_and(|(r, ..)| r.end < t.range.end) {
            stack.pop();
        }
        let parent = stack.last().filter(|(r, ..)| *r != t.range);
        let container = parent.map(|(_, n, _)| n.clone());
        // Python tags every `def` a function; inside a class it is a
        // method. Rust tags every `fn` in a `{ }` item list a method; inside
        // a `mod` it is a function. With no known parent (an `impl` the
        // tags query missed) the tag's own kind stands.
        let inside = parent.map_or(Inside::Unknown, |(.., i)| *i);
        if t.is_def && t.kind == CodeSymbolKind::Function && inside == Inside::Methods {
            t.kind = CodeSymbolKind::Method;
        } else if t.is_def && t.kind == CodeSymbolKind::Method && inside == Inside::Functions {
            t.kind = CodeSymbolKind::Function;
        }
        if t.is_def || t.syntax == "implementation" {
            let label = if t.syntax == "implementation" {
                impl_target(source, &t.range).unwrap_or_else(|| t.name.clone())
            } else {
                t.name.clone()
            };
            let inside = if t.syntax == "implementation" {
                Inside::Methods
            } else {
                match t.kind {
                    CodeSymbolKind::Class
                    | CodeSymbolKind::Struct
                    | CodeSymbolKind::Interface
                    | CodeSymbolKind::Trait => Inside::Methods,
                    CodeSymbolKind::Module | CodeSymbolKind::Function => Inside::Functions,
                    _ => Inside::Unknown,
                }
            };
            stack.push((t.range.clone(), label, inside));
        }
        out.push(Extracted {
            range: Range {
                start: lines.position(t.range.start),
                end: lines.position(t.range.end),
            },
            name_range: Range {
                start: lines.position(t.name_range.start),
                end: lines.position(t.name_range.end),
            },
            name: t.name,
            kind: t.kind,
            syntax: t.syntax,
            is_def: t.is_def,
            container,
        });
    }
    out
}

/// The type an `impl` block is for: `impl<T> Display for Foo<T> {` → `Foo`.
/// Rust's tags name a trait impl after the trait; its methods belong to the
/// type. `None` when the header has no `for`.
fn impl_target(source: &[u8], range: &Bytes<usize>) -> Option<String> {
    let body = &source[range.clone()];
    let header = &body[..memchr::memchr(b'{', body).unwrap_or(body.len())];
    let header = std::str::from_utf8(header).ok()?;
    let mut words = header.split(|c: char| !(c.is_alphanumeric() || c == '_'));
    words.find(|w| *w == "for")?;
    words.find(|w| !w.is_empty()).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn impl_target_reads_the_type_after_for() {
        let src = b"impl<T: Clone> fmt::Display for Foo<T> { fn x() {} }";
        assert_eq!(impl_target(src, &(0..src.len())).as_deref(), Some("Foo"));
        let src = b"impl Foo { }";
        assert_eq!(impl_target(src, &(0..src.len())), None);
    }
}
