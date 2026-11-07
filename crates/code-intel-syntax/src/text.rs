//! Plain text: whole-word search (the fallback for files without a
//! grammar), previews around a hit, and the identifier under a cursor.
//!
//! ```text
//! walk (ignore: .gitignore, hidden) ─▶ include(path)? ─▶ not binary, ≤ max
//!   ─▶ memmem(name) ─▶ word boundary? ─▶ CodeHit { Source::Text }
//! ```

use std::path::Path;

use memchr::memmem;
use theodore_ports::code_intel::{CodeHit, Location, Position, Range, Source};

use crate::DEFAULT_MAX_FILE_BYTES;
use crate::index::is_binary;
use crate::pos::{LineIndex, char_count, to_u32};

/// Longest preview line kept, in chars.
const MAX_PREVIEW_LINE: usize = 200;

/// Bytes that continue an identifier. Non-ASCII counts, so `name` does not
/// match inside `имяname`.
pub(crate) fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 0x80
}

/// Byte offsets where `name` occurs as a whole word in `hay`.
pub(crate) fn word_matches<'a>(
    hay: &'a [u8],
    finder: &'a memmem::Finder<'a>,
) -> impl Iterator<Item = usize> + 'a {
    let n = finder.needle().len();
    finder.find_iter(hay).filter(move |&i| {
        let before = i.checked_sub(1).map(|j| hay[j]);
        let after = hay.get(i + n).copied();
        !before.is_some_and(is_ident_byte) && !after.is_some_and(is_ident_byte)
    })
}

/// Whether `hay` mentions `name` as a whole word.
pub(crate) fn mentions(hay: &[u8], finder: &memmem::Finder<'_>) -> bool {
    word_matches(hay, finder).next().is_some()
}

/// The hit's line plus one line before and after, dedented together and
/// with trailing whitespace dropped; blank neighbours are left out.
pub fn preview(text: &str, line: u32) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let Some(at) = (line as usize).checked_sub(1).filter(|&i| i < lines.len()) else {
        return String::new();
    };
    let from = if at > 0 && !lines[at - 1].trim().is_empty() {
        at - 1
    } else {
        at
    };
    let to = if at + 1 < lines.len() && !lines[at + 1].trim().is_empty() {
        at + 1
    } else {
        at
    };
    let block = &lines[from..=to];
    let indent = block
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    block
        .iter()
        .map(|l| {
            let l = l.get(indent..).unwrap_or_else(|| l.trim_start()).trim_end();
            match l.char_indices().nth(MAX_PREVIEW_LINE) {
                Some((i, _)) => format!("{}…", &l[..i]),
                None => l.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The identifier around `at` in `text` and its range, if the cursor is on
/// one (or just past its last char).
pub(crate) fn word_at(text: &str, at: Position) -> Option<(String, Range)> {
    let offset = crate::pos::byte_offset(text, at)?;
    let bytes = text.as_bytes();
    let mut start = offset;
    while start > 0 && is_ident_byte(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = offset;
    while end < bytes.len() && is_ident_byte(bytes[end]) {
        end += 1;
    }
    if start == end {
        return None;
    }
    let word = std::str::from_utf8(&bytes[start..end]).ok()?;
    let line_start = text[..offset].rfind('\n').map_or(0, |i| i + 1);
    let col = |b: usize| to_u32(char_count(&bytes[line_start..b]) + 1);
    Some((
        word.to_string(),
        Range {
            start: Position {
                line: at.line,
                col: col(start),
            },
            end: Position {
                line: at.line,
                col: col(end),
            },
        },
    ))
}

/// Keywords that, right before a name on its line, make a text hit look
/// like a definition. Best effort: the text layer cannot know. Written for
/// the languages that have no grammar to say so (Scala, Elixir, Perl,
/// Haskell, OCaml, Clojure, Julia, Groovy, PowerShell, Solidity, Dart,
/// SQL, ...) next to the common ones.
const DEF_KEYWORDS: &[&str] = &[
    // C family, Rust, Go, Swift, Kotlin, Python, JS, Ruby, Lua, Zig
    "fn",
    "def",
    "class",
    "struct",
    "enum",
    "trait",
    "interface",
    "type",
    "function",
    "func",
    "fun",
    "const",
    "let",
    "var",
    "val",
    "static",
    "mod",
    "module",
    "namespace",
    "protocol",
    "object",
    "macro_rules!",
    // Perl `sub foo`, `package Foo`; Julia `macro foo`
    "sub",
    "package",
    "macro",
    // Elixir
    "defmodule",
    "defp",
    "defmacro",
    "defmacrop",
    "defprotocol",
    "defimpl",
    "defstruct",
    "defdelegate",
    "defguard",
    "defexception",
    // Clojure `(defn foo`, `(ns foo`
    "defn",
    "defn-",
    "defmulti",
    "defmethod",
    "defrecord",
    "deftype",
    "defonce",
    "ns",
    // Haskell, OCaml, Scala 3, Dart, Swift, Objective-C (`@interface Foo`)
    "data",
    "newtype",
    "rec",
    "exception",
    "given",
    "mixin",
    "extension",
    "typedef",
    "typealias",
    "record",
    "actor",
    "implementation",
    // Solidity
    "contract",
    "library",
    "event",
    "modifier",
    "error",
    // PowerShell
    "filter",
    // SQL (also upper-case, see `looks_like_definition`)
    "table",
    "view",
    "procedure",
    "trigger",
    "schema",
    "sequence",
];

/// Where a plain `Type name(..) {` is not a signature: the words before
/// the name that make it a call or a condition.
const NOT_A_TYPE: &[&str] = &[
    "return",
    "new",
    "throw",
    "await",
    "else",
    "elif",
    "if",
    "while",
    "for",
    "switch",
    "match",
    "case",
    "catch",
    "when",
    "in",
    "is",
    "as",
    "not",
    "and",
    "or",
    "yield",
    "do",
    "print",
    "echo",
    "with",
    "using",
    "unless",
    "until",
    "go",
    "defer",
    "delete",
    "typeof",
    "sizeof",
    "extends",
    "implements",
    "from",
    "import",
    "export",
];

/// Erlang attributes that define their first argument: `-module(foo).`.
const ERLANG_ATTRIBUTES: &[&str] = &[
    "-module(",
    "-record(",
    "-define(",
    "-type(",
    "-opaque(",
    "-callback(",
];

/// `before`: the line up to the name; `after`: the rest of the line.
///
/// ```text
/// def foo, class Foo, fun foo, sub foo, defmodule Foo, (defn foo   a keyword before the name
/// CREATE TABLE foo                                                 the same, upper-cased SQL
/// -module(foo).  -record(foo, ..)                                  Erlang attribute
/// foo :: Int -> Int                                                Haskell signature
/// foo(X) ->                                                        Erlang clause
/// foo <- function(x)                                               R
/// void foo(int x) {   Future<void> foo() async {                   a typed signature
/// ```
///
/// A call, `x = foo(1)`, `return foo(1)`, `self.foo(1)` or `foo(1);`, is
/// none of these.
fn looks_like_definition(before: &str, after: &str) -> bool {
    let tokens: Vec<&str> = before.split_whitespace().collect();
    let Some(&last) = tokens.last() else {
        return starts_a_definition(after);
    };
    // `(defn`, `@interface`, `[def`.
    let word = last.trim_start_matches(['(', '@', '[', '#']);
    if DEF_KEYWORDS.contains(&word) || ERLANG_ATTRIBUTES.contains(&last) {
        return true;
    }
    // `CREATE TABLE users`, `create or replace FUNCTION f`: a SQL
    // statement is either all lower case or all upper case.
    if word.len() > 1
        && word.bytes().all(|b| b.is_ascii_uppercase())
        && DEF_KEYWORDS.contains(&word.to_ascii_lowercase().as_str())
    {
        return true;
    }
    is_typed_signature(&tokens, after)
}

/// A name at the start of its line, where what follows says "defined here".
fn starts_a_definition(after: &str) -> bool {
    let rest = after.trim_start();
    // Haskell / Elm: `foo :: Int`. `Foo::bar()` (C++, Rust) has no space.
    if rest.starts_with(":: ") {
        return true;
    }
    // Erlang: `foo(X) ->`, `foo(X) when is_atom(X) ->`.
    if rest.starts_with('(') {
        let rest = rest.trim_end();
        return rest.ends_with("->") || rest.contains(") ->") || rest.contains(") when ");
    }
    // R: `foo <- function(x)`, `foo = function(x)`.
    let assigned = rest
        .strip_prefix("<-")
        .or_else(|| rest.strip_prefix('=').filter(|r| !r.starts_with('=')));
    assigned.is_some_and(|r| r.trim_start().starts_with("function"))
}

/// `[modifiers] Type name(..) {`: every word before the name looks like a
/// modifier or a type, none of them is a keyword that makes this a call or
/// a condition, and the line opens a body (`{`) or is an arrow function.
fn is_typed_signature(tokens: &[&str], after: &str) -> bool {
    let after = after.trim_start();
    if !after.starts_with('(') {
        return false;
    }
    let line = after.trim_end();
    if !(line.ends_with('{') || line.contains(") =>")) {
        return false;
    }
    let type_char = |c: char| {
        c.is_alphanumeric() || matches!(c, '_' | '<' | '>' | ',' | '?' | '[' | ']' | '*' | '&')
    };
    tokens.iter().all(|t| {
        !NOT_A_TYPE.contains(t)
            && t.chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '*' | '&'))
            && t.chars().all(type_char)
    })
}

/// Whole-word occurrences of `name` under `root`, respecting `.gitignore`
/// and skipping hidden, binary and oversized files. `include` filters
/// repo-relative paths (e.g. to languages without a grammar). Stops at
/// `cap` hits; files are walked in path order. Blocking.
pub fn text_search(
    root: &Path,
    name: &str,
    cap: usize,
    include: &dyn Fn(&str) -> bool,
) -> Vec<CodeHit> {
    let mut hits = Vec::new();
    if name.is_empty() || cap == 0 {
        return hits;
    }
    let finder = memmem::Finder::new(name.as_bytes());
    let walk = ignore::WalkBuilder::new(root)
        .sort_by_file_path(|a, b| a.cmp(b))
        .require_git(false)
        .build();
    for entry in walk.flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if !include(&rel) {
            continue;
        }
        if entry
            .metadata()
            .is_ok_and(|m| m.len() > DEFAULT_MAX_FILE_BYTES as u64)
        {
            continue;
        }
        let Ok(bytes) = std::fs::read(entry.path()) else {
            continue;
        };
        if is_binary(&bytes) || !mentions(&bytes, &finder) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let lines = LineIndex::new(text.as_bytes());
        for at in word_matches(text.as_bytes(), &finder) {
            let start = lines.position(at);
            let end = lines.position(at + name.len());
            let line_start = text[..at].rfind('\n').map_or(0, |i| i + 1);
            let after_name = &text[at + name.len()..];
            let line_end = after_name.find('\n').unwrap_or(after_name.len());
            hits.push(CodeHit {
                location: Location {
                    path: rel.clone(),
                    range: Range { start, end },
                },
                preview: preview(&text, start.line),
                is_definition: looks_like_definition(
                    &text[line_start..at],
                    after_name[..line_end].trim_end_matches('\r'),
                ),
                source: Source::Text,
            });
            if hits.len() >= cap {
                return hits;
            }
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_keeps_neighbours_and_dedents() {
        let text = "fn a() {\n    let x = 1;\n    call(x);\n}\n";
        assert_eq!(preview(text, 3), "    let x = 1;\n    call(x);\n}");
        assert_eq!(preview(text, 2), "fn a() {\n    let x = 1;\n    call(x);");
        assert_eq!(preview(text, 1), "fn a() {\n    let x = 1;");
        assert_eq!(preview(text, 9), "");
    }

    #[test]
    fn word_boundaries_exclude_partial_and_unicode_neighbours() {
        let f = memmem::Finder::new("foo");
        let hay = "foo foobar barfoo фоfoo (foo) foo".as_bytes();
        let hits: Vec<usize> = word_matches(hay, &f).collect();
        assert_eq!(hits.len(), 3);
    }

    /// Whether the first whole-word `name` on `line` looks like a definition.
    fn is_def(line: &str, name: &str) -> bool {
        let f = memmem::Finder::new(name);
        let at = word_matches(line.as_bytes(), &f).next().unwrap();
        looks_like_definition(&line[..at], &line[at + name.len()..])
    }

    #[test]
    fn definitions_in_languages_without_a_grammar_look_like_definitions() {
        for (line, name) in [
            // the ones that always worked
            ("fn foo() {", "foo"),
            ("    def foo(self):", "foo"),
            ("  public class Foo {", "Foo"),
            ("func foo() {", "foo"),
            ("fun foo() = 1", "foo"),
            // added
            ("sub foo {", "foo"),
            ("package Foo::Bar;", "Foo"),
            ("defmodule Foo do", "Foo"),
            ("  defp foo(x), do: x", "foo"),
            ("  def foo(x) do", "foo"),
            ("object Foo extends App {", "Foo"),
            ("case class Foo(a: Int)", "Foo"),
            ("  val foo = 1", "foo"),
            ("trait Foo {", "Foo"),
            ("(defn foo [x]", "foo"),
            ("(ns foo.core", "foo"),
            ("data Foo = Foo Int", "Foo"),
            ("let rec foo x =", "foo"),
            ("module Foo = struct", "Foo"),
            ("contract Foo {", "Foo"),
            ("    function foo() public {", "foo"),
            ("    modifier onlyOwner() {", "onlyOwner"),
            ("function Get-Foo {", "Get-Foo"),
            ("@interface Foo : NSObject", "Foo"),
            ("@implementation Foo", "Foo"),
            ("@mixin foo($x) {", "foo"),
            ("CREATE TABLE users (", "users"),
            ("create view users as", "users"),
            ("CREATE OR REPLACE FUNCTION foo()", "foo"),
            ("mixin Foo {", "Foo"),
            ("extension Foo on String {", "Foo"),
            ("-module(foo).", "foo"),
            ("-record(foo, {a, b}).", "foo"),
            ("foo(X) ->", "foo"),
            ("foo(X) when is_atom(X) ->", "foo"),
            ("foo :: Int -> Int", "foo"),
            ("foo <- function(x) {", "foo"),
            ("foo = function(x) {", "foo"),
            ("void foo(int x) {", "foo"),
            ("  Future<void> foo() async {", "foo"),
            ("  static String foo(String a, int b) {", "foo"),
            ("  int add(int a) => a + 1;", "add"),
        ] {
            assert!(is_def(line, name), "{line:?} should look like a definition");
        }
    }

    #[test]
    fn calls_and_uses_do_not_look_like_definitions() {
        for (line, name) in [
            ("    foo(1);", "foo"),
            ("  x = foo(1)", "foo"),
            ("  return foo(1) {", "foo"),
            ("    self.foo(1)", "foo"),
            ("  if foo(x) {", "foo"),
            ("  else if foo(x) {", "foo"),
            ("  } else foo() {", "foo"),
            ("  new Foo() {", "Foo"),
            ("  await foo(1);", "foo"),
            ("  Foo::bar()", "Foo"),
            ("Foo::new()", "Foo"),
            ("  x |> foo()", "foo"),
            ("  Enum.map(xs, fn x -> foo(x) end)", "foo"),
            ("  case foo(a) =>", "foo"),
            ("  x => foo(x)", "foo"),
            ("foo == function(x) {", "foo"),
            ("  print foo", "foo"),
            ("  use foo;", "foo"),
            ("import foo from 'x'", "foo"),
        ] {
            assert!(
                !is_def(line, name),
                "{line:?} should not look like a definition"
            );
        }
    }

    #[test]
    fn word_at_finds_the_identifier_under_the_cursor() {
        let text = "let Привет = call_me(1);";
        let (w, r) = word_at(text, Position { line: 1, col: 16 }).unwrap();
        assert_eq!(w, "call_me");
        assert_eq!((r.start.col, r.end.col), (14, 21));
        let (w, _) = word_at(text, Position { line: 1, col: 6 }).unwrap();
        assert_eq!(w, "Привет");
        // Just past a word's last char still counts as on it.
        let (w, _) = word_at(text, Position { line: 1, col: 4 }).unwrap();
        assert_eq!(w, "let");
        assert_eq!(word_at(text, Position { line: 1, col: 13 }), None);
    }
}
