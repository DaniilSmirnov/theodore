//! Trims an Actions job log to what's worth putting in an agent's prompt
//! (docs/spaces.md "Fix CI"): each line's `2024-01-01T12:00:00.0000000Z `
//! timestamp and ANSI color codes stripped, then only the tail kept — a
//! full job log is often thousands of lines, and the failure is almost
//! always near the end.

/// Lines kept from the end, after cleanup.
pub(crate) const DEFAULT_TAIL_LINES: usize = 300;

/// Strips the per-line ISO-8601 timestamp GitHub Actions prefixes every log
/// line with (`2024-01-01T12:00:00.1234567Z <line>`).
fn strip_timestamp(line: &str) -> &str {
    let Some((ts, rest)) = line.split_once(' ') else {
        return line;
    };
    if is_iso8601(ts) { rest } else { line }
}

/// `2024-01-01T12:00:00.1234567Z` (GitHub always uses `Z`, fractional
/// seconds included).
fn is_iso8601(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 20
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && b[16] == b':'
        && s.ends_with('Z')
        && b[..4].iter().all(u8::is_ascii_digit)
}

/// Drops `ESC [ ... <final byte>` (CSI) sequences — the color codes GitHub
/// Actions logs are full of — and any bare `ESC`. Not a full ANSI parser,
/// just enough for log coloring.
fn strip_ansi(line: &str) -> String {
    if !line.contains('\u{1b}') {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        if chars.peek() == Some(&'[') {
            chars.next();
            // CSI: parameter bytes, then a final byte (0x40-0x7E) ends it.
            for c in chars.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&c) {
                    break;
                }
            }
        }
        // A bare ESC (or one not followed by `[`) is simply dropped.
    }
    out
}

/// The last `max_lines` of `log`, each cleaned of its timestamp and ANSI
/// codes, trailing blank lines dropped first.
pub(crate) fn tail(log: &str, max_lines: usize) -> String {
    let cleaned: Vec<String> = log
        .lines()
        .map(|l| strip_ansi(strip_timestamp(l)))
        .collect();
    let end = cleaned
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map_or(0, |i| i + 1);
    let cleaned = &cleaned[..end];
    let start = cleaned.len().saturating_sub(max_lines);
    cleaned[start..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_the_actions_timestamp_prefix() {
        assert_eq!(
            strip_timestamp("2024-01-01T12:00:00.1234567Z cargo test failed"),
            "cargo test failed"
        );
        assert_eq!(strip_timestamp("no timestamp here"), "no timestamp here");
        assert_eq!(strip_timestamp(""), "");
    }

    #[test]
    fn strips_ansi_color_codes() {
        assert_eq!(strip_ansi("\u{1b}[31merror\u{1b}[0m: boom"), "error: boom");
        assert_eq!(strip_ansi("plain text"), "plain text");
    }

    #[test]
    fn keeps_only_the_tail_after_cleanup() {
        let log = (1..=10)
            .map(|i| format!("2024-01-01T00:00:0{i}Z line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let t = tail(&log, 3);
        assert_eq!(t, "line 8\nline 9\nline 10");
    }

    #[test]
    fn drops_trailing_blank_lines_before_counting_the_tail() {
        let log = "line 1\nline 2\n\n\n";
        assert_eq!(tail(log, 300), "line 1\nline 2");
    }

    #[test]
    fn a_short_log_is_returned_whole() {
        assert_eq!(tail("only line", 300), "only line");
        assert_eq!(tail("", 300), "");
    }
}
