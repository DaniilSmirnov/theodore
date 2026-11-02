//! Positions and paths at the LSP edge.
//!
//! ```text
//! contract  line 1-based · col 1-based in chars
//!    ▲ ▼    (needs the line's text)
//! LSP       line 0-based · character in UTF-16 units or UTF-8 bytes
//! ```
//!
//! Paths: the contract speaks repo-relative `/` paths, LSP speaks `file://`
//! URIs. A path that leaves the root (absolute, `..`) is rejected on the way
//! in; a location outside the root is dropped on the way out.

use std::path::{Component, Path, PathBuf};

use theodore_ports::{CodeIntelError, CodeIntelResult, Position};

/// The unit LSP `character` offsets count, as negotiated at `initialize`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PositionEncoding {
    Utf8,
    Utf16,
}

impl PositionEncoding {
    /// From the server's `capabilities.positionEncoding`; absent = UTF-16.
    pub(crate) fn from_lsp(kind: Option<&str>) -> Self {
        match kind {
            Some("utf-8") => PositionEncoding::Utf8,
            _ => PositionEncoding::Utf16,
        }
    }

    fn units(self, c: char) -> u32 {
        match self {
            PositionEncoding::Utf8 => c.len_utf8() as u32,
            PositionEncoding::Utf16 => c.len_utf16() as u32,
        }
    }
}

/// Line `line` (0-based) of `text`, without its terminator.
fn nth_line(text: &str, line: u32) -> Option<&str> {
    text.split('\n')
        .nth(line as usize)
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
}

/// An LSP position in `text` as a contract position. A `character` past the
/// line's end clamps to it; one inside a multi-unit char rounds down to it.
pub(crate) fn from_lsp(text: &str, pos: lsp_types::Position, enc: PositionEncoding) -> Position {
    let Some(line) = nth_line(text, pos.line) else {
        return Position {
            line: pos.line + 1,
            col: pos.character + 1,
        };
    };
    let mut units = 0;
    let mut col = 0;
    for c in line.chars() {
        let next = units + enc.units(c);
        if next > pos.character {
            break;
        }
        units = next;
        col += 1;
    }
    Position {
        line: pos.line + 1,
        col: col + 1,
    }
}

/// A contract position in `text` as an LSP position. A column past the
/// line's end clamps to it.
pub(crate) fn to_lsp(text: &str, pos: Position, enc: PositionEncoding) -> lsp_types::Position {
    let line_ix = pos.line.saturating_sub(1);
    let col = pos.col.saturating_sub(1) as usize;
    let character = match nth_line(text, line_ix) {
        Some(line) => line.chars().take(col).map(|c| enc.units(c)).sum(),
        None => col as u32,
    };
    lsp_types::Position {
        line: line_ix,
        character,
    }
}

/// An LSP range in `text` as a contract range.
pub(crate) fn range_from_lsp(
    text: &str,
    r: lsp_types::Range,
    enc: PositionEncoding,
) -> theodore_ports::Range {
    theodore_ports::Range {
        start: from_lsp(text, r.start, enc),
        end: from_lsp(text, r.end, enc),
    }
}

/// `rel` under `root`, refusing anything that would leave it.
pub(crate) fn resolve(root: &Path, rel: &str) -> CodeIntelResult<PathBuf> {
    let p = Path::new(rel);
    let inside = !rel.is_empty()
        && p.components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    if !inside {
        return Err(CodeIntelError::OutsideWorkspace(rel.to_string()));
    }
    Ok(root.join(p))
}

/// `abs` relative to `root` with `/` separators; `None` outside it.
pub(crate) fn relative(root: &Path, abs: &Path) -> Option<String> {
    let rel = match abs.strip_prefix(root) {
        Ok(rel) => rel.to_path_buf(),
        // The server may report a path through a symlink the root resolved
        // (`/var` vs `/private/var` on macOS).
        Err(_) => std::fs::canonicalize(abs)
            .ok()?
            .strip_prefix(root)
            .ok()?
            .to_path_buf(),
    };
    let parts: Vec<String> = rel
        .components()
        .map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Option<_>>()?;
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// The `file://` URI of an absolute path.
pub(crate) fn file_uri(abs: &Path) -> String {
    url::Url::from_file_path(abs)
        .map(String::from)
        .unwrap_or_else(|()| format!("file://{}", abs.display()))
}

/// The path of a `file://` URI; `None` for other schemes.
pub(crate) fn uri_path(uri: &str) -> Option<PathBuf> {
    url::Url::parse(uri).ok()?.to_file_path().ok()
}

/// A `file://` URI as `lsp_types::Uri`.
pub(crate) fn lsp_uri(abs: &Path) -> lsp_types::Uri {
    file_uri(abs)
        .parse()
        .unwrap_or_else(|_| "file:///".parse().expect("static uri"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const TEXT: &str = "fn a() {}\r\nlet привет = 1; x\nlet 🦀🦀 = y;\n";

    fn lsp(line: u32, character: u32) -> lsp_types::Position {
        lsp_types::Position { line, character }
    }

    fn pos(line: u32, col: u32) -> Position {
        Position { line, col }
    }

    #[test]
    fn cyrillic_counts_one_utf16_unit_and_two_bytes() {
        // `x` on line 2: "let привет = 1; " is 16 chars.
        let x = pos(2, 17);
        assert_eq!(to_lsp(TEXT, x, PositionEncoding::Utf16), lsp(1, 16));
        assert_eq!(to_lsp(TEXT, x, PositionEncoding::Utf8), lsp(1, 22));
        assert_eq!(from_lsp(TEXT, lsp(1, 16), PositionEncoding::Utf16), x);
        assert_eq!(from_lsp(TEXT, lsp(1, 22), PositionEncoding::Utf8), x);
    }

    #[test]
    fn emoji_counts_two_utf16_units_and_four_bytes() {
        // `y` on line 3: "let 🦀🦀 = " is 9 chars, 11 UTF-16 units, 15 bytes.
        let y = pos(3, 10);
        assert_eq!(to_lsp(TEXT, y, PositionEncoding::Utf16), lsp(2, 11));
        assert_eq!(to_lsp(TEXT, y, PositionEncoding::Utf8), lsp(2, 15));
        assert_eq!(from_lsp(TEXT, lsp(2, 11), PositionEncoding::Utf16), y);
        assert_eq!(from_lsp(TEXT, lsp(2, 15), PositionEncoding::Utf8), y);
    }

    #[test]
    fn offsets_inside_a_char_round_down_and_past_the_end_clamp() {
        // Between the two halves of the first crab.
        assert_eq!(
            from_lsp(TEXT, lsp(2, 5), PositionEncoding::Utf16),
            pos(3, 5)
        );
        // Line 1 has 9 chars; `\r` is not one of them.
        assert_eq!(
            from_lsp(TEXT, lsp(0, 99), PositionEncoding::Utf16),
            pos(1, 10)
        );
        assert_eq!(to_lsp(TEXT, pos(1, 99), PositionEncoding::Utf16), lsp(0, 9));
    }

    #[test]
    fn rejects_paths_that_leave_the_root() {
        let root = Path::new("/r");
        assert_eq!(resolve(root, "src/a.rs").unwrap(), Path::new("/r/src/a.rs"));
        for bad in ["../x", "/etc/passwd", "a/../../b", ""] {
            assert!(
                matches!(resolve(root, bad), Err(CodeIntelError::OutsideWorkspace(_))),
                "{bad}"
            );
        }
        assert_eq!(
            relative(root, Path::new("/r/src/a.rs")).as_deref(),
            Some("src/a.rs")
        );
        assert_eq!(relative(root, Path::new("/elsewhere/a.rs")), None);
    }

    #[test]
    fn uris_round_trip_with_spaces_and_unicode() {
        let p = Path::new("/tmp/a dir/файл.rs");
        let uri = file_uri(p);
        assert!(uri.starts_with("file:///tmp/a%20dir/"), "{uri}");
        assert_eq!(uri_path(&uri).unwrap(), p);
        assert_eq!(uri_path(lsp_uri(p).as_str()).unwrap(), p);
    }
}
