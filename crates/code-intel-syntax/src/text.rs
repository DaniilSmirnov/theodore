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
/// like a definition. Best effort: the text layer cannot know.
const DEF_KEYWORDS: &[&str] = &[
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
];

fn looks_like_definition(line_before_name: &str) -> bool {
    line_before_name
        .split_whitespace()
        .last()
        .is_some_and(|w| DEF_KEYWORDS.contains(&w))
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
            hits.push(CodeHit {
                location: Location {
                    path: rel.clone(),
                    range: Range { start, end },
                },
                preview: preview(&text, start.line),
                is_definition: looks_like_definition(&text[line_start..at]),
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
