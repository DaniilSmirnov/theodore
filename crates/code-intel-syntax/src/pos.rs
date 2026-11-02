//! Byte offsets ⇄ contract positions (1-based line, 1-based char column).
//!
//! ```text
//! "fn Привет() {}"   byte 3 ─▶ (1, 4)   byte 15 ─▶ (1, 10)
//!  ^^^ 3 bytes, ^^^^^^ 6 chars / 12 bytes
//! ```
//!
//! tree-sitter reports bytes; the contract counts Unicode scalar values, so
//! a column is the number of chars between the line start and the offset,
//! plus one. A `\r` before `\n` is an ordinary char of its line.

use memchr::{memchr, memchr_iter};
use theodore_ports::code_intel::Position;

/// Line starts of one source, for many byte → position conversions.
pub(crate) struct LineIndex<'a> {
    src: &'a [u8],
    starts: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    pub(crate) fn new(src: &'a [u8]) -> Self {
        let mut starts = Vec::with_capacity(src.len() / 32 + 1);
        starts.push(0);
        starts.extend(memchr_iter(b'\n', src).map(|i| i + 1));
        Self { src, starts }
    }

    /// The position of `byte` (clamped to the source length).
    pub(crate) fn position(&self, byte: usize) -> Position {
        let byte = byte.min(self.src.len());
        let line = match self.starts.binary_search(&byte) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let col = char_count(&self.src[self.starts[line]..byte]) + 1;
        Position {
            line: to_u32(line + 1),
            col: to_u32(col),
        }
    }
}

/// Chars in a UTF-8 slice: every byte that does not continue a sequence.
/// Matches `str::chars().count()` on valid UTF-8 and stays total on
/// invalid input.
pub(crate) fn char_count(bytes: &[u8]) -> usize {
    bytes.iter().filter(|b| (**b & 0xC0) != 0x80).count()
}

pub(crate) fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The byte offset of `pos` in `text`, or `None` when it is outside it.
///
/// Valid columns run from 1 to the line's char count + 1 (just past its
/// last char, before the `\n`). The line after a final `\n` exists, with
/// only column 1.
pub fn byte_offset(text: &str, pos: Position) -> Option<usize> {
    if pos.line == 0 || pos.col == 0 {
        return None;
    }
    let bytes = text.as_bytes();
    let mut start = 0usize;
    for _ in 1..pos.line {
        start += memchr(b'\n', &bytes[start..])? + 1;
    }
    let line_end = memchr(b'\n', &bytes[start..]).map_or(text.len(), |i| start + i);
    let line = &text[start..line_end];
    let want = (pos.col - 1) as usize;
    match line.char_indices().nth(want) {
        Some((i, _)) => Some(start + i),
        None if line.chars().count() == want => Some(line_end),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(line: u32, col: u32) -> Position {
        Position { line, col }
    }

    #[test]
    fn positions_count_chars_not_bytes() {
        let src = "ab\nПривет 😀x\n".as_bytes();
        let idx = LineIndex::new(src);
        assert_eq!(idx.position(0), p(1, 1));
        assert_eq!(idx.position(2), p(1, 3));
        assert_eq!(idx.position(3), p(2, 1));
        // "Привет " is 13 bytes, 7 chars; the emoji is 4 bytes, 1 char.
        assert_eq!(idx.position(3 + 13), p(2, 8));
        assert_eq!(idx.position(3 + 13 + 4), p(2, 9));
        assert_eq!(idx.position(src.len()), p(3, 1));
    }

    #[test]
    fn byte_offset_round_trips() {
        let text = "ab\nПривет 😀x\n";
        let idx = LineIndex::new(text.as_bytes());
        for (i, _) in text.char_indices().chain([(text.len(), ' ')]) {
            assert_eq!(byte_offset(text, idx.position(i)), Some(i), "byte {i}");
        }
    }

    #[test]
    fn byte_offset_rejects_outside_positions() {
        let text = "ab\ncd";
        assert_eq!(byte_offset(text, p(1, 3)), Some(2));
        assert_eq!(byte_offset(text, p(1, 4)), None);
        assert_eq!(byte_offset(text, p(3, 1)), None);
        assert_eq!(byte_offset(text, p(0, 1)), None);
        assert_eq!(byte_offset("a\n", p(2, 1)), Some(2));
        assert_eq!(byte_offset("", p(1, 1)), Some(0));
    }
}
