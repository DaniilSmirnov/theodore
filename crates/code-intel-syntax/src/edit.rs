//! Symbol edits for spaces (`replace_symbol_body`, `insert_after_symbol`,
//! docs/code-intel.md §7.2): pure string functions over contract ranges.
//!
//! ```text
//! replace_range   aaa[old]bbb      ─▶ aaa new bbb
//! insert_after    fn a() {…}⏎      ─▶ fn a() {…}⏎ new⏎
//!                 fn b() {…}          fn b() {…}
//! ```

use theodore_ports::code_intel::{CodeIntelError, CodeIntelResult, Range};

use crate::pos::byte_offset;

fn offsets(text: &str, range: Range) -> CodeIntelResult<(usize, usize)> {
    let start = byte_offset(text, range.start);
    let end = byte_offset(text, range.end);
    match (start, end) {
        (Some(s), Some(e)) if s <= e => Ok((s, e)),
        _ => Err(CodeIntelError::Other(format!(
            "range {}:{}–{}:{} is outside the file",
            range.start.line, range.start.col, range.end.line, range.end.col
        ))),
    }
}

/// `text` with `range` replaced by `new`. Errs when the range is outside
/// the text or ends before it starts.
pub fn replace_range(text: &str, range: Range, new: &str) -> CodeIntelResult<String> {
    let (s, e) = offsets(text, range)?;
    let mut out = String::with_capacity(text.len() - (e - s) + new.len());
    out.push_str(&text[..s]);
    out.push_str(new);
    out.push_str(&text[e..]);
    Ok(out)
}

/// `text` with `new` inserted as whole lines after the line where `range`
/// ends: after that line's `\n` (one is added at the end of a file without
/// it), and `new` gets a trailing `\n` if it has none. A range ending at
/// column 1 ends on the line before.
pub fn insert_after(text: &str, range: Range, new: &str) -> CodeIntelResult<String> {
    let (_, e) = offsets(text, range)?;
    let at = if e == 0 || text.as_bytes()[e - 1] == b'\n' {
        Some(e)
    } else {
        memchr::memchr(b'\n', &text.as_bytes()[e..]).map(|i| e + i + 1)
    };
    let mut out = String::with_capacity(text.len() + new.len() + 2);
    match at {
        Some(at) => {
            out.push_str(&text[..at]);
            out.push_str(new);
            if !new.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&text[at..]);
        }
        None => {
            out.push_str(text);
            out.push('\n');
            out.push_str(new);
            if !new.ends_with('\n') {
                out.push('\n');
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use theodore_ports::code_intel::Position;

    fn r(sl: u32, sc: u32, el: u32, ec: u32) -> Range {
        Range {
            start: Position { line: sl, col: sc },
            end: Position { line: el, col: ec },
        }
    }

    #[test]
    fn replace_uses_char_columns() {
        let text = "let имя = 1;\nlet 😀x = 2;\n";
        // `имя` is cols 5..8 (3 chars, 6 bytes).
        assert_eq!(
            replace_range(text, r(1, 5, 1, 8), "name").unwrap(),
            "let name = 1;\nlet 😀x = 2;\n"
        );
        // `x` after the emoji is col 6.
        assert_eq!(
            replace_range(text, r(2, 6, 2, 7), "y").unwrap(),
            "let имя = 1;\nlet 😀y = 2;\n"
        );
        // Across lines.
        assert_eq!(
            replace_range(text, r(1, 13, 2, 12), "").unwrap(),
            "let имя = 1;\n"
        );
        assert!(replace_range(text, r(1, 40, 1, 41), "z").is_err());
        assert!(replace_range(text, r(2, 3, 1, 1), "z").is_err());
    }

    #[test]
    fn insert_after_adds_whole_lines() {
        let text = "fn a() {\n}\nfn b() {}\n";
        assert_eq!(
            insert_after(text, r(1, 1, 2, 2), "fn c() {}").unwrap(),
            "fn a() {\n}\nfn c() {}\nfn b() {}\n"
        );
        // End at column 1: the range ended with the previous line.
        assert_eq!(
            insert_after(text, r(1, 1, 3, 1), "// x\n").unwrap(),
            "fn a() {\n}\n// x\nfn b() {}\n"
        );
        // Last line without a newline.
        assert_eq!(
            insert_after("fn a() {}", r(1, 1, 1, 10), "fn b() {}").unwrap(),
            "fn a() {}\nfn b() {}\n"
        );
    }
}
