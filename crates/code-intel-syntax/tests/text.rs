//! The text fallback.

mod common;

use common::git;
use pretty_assertions::assert_eq;
use theodore_code_intel_syntax::text_search;
use theodore_ports::code_intel::{Position, Source};

#[test]
fn text_search_finds_whole_words_and_respects_gitignore() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    std::fs::write(root.join(".gitignore"), "build/\n").unwrap();
    std::fs::create_dir_all(root.join("build")).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("build/out.c"), "int compute(void);\n").unwrap();
    std::fs::write(
        root.join("src/a.c"),
        "// Привет compute\nint compute(int x) {\n  return recompute(x) + compute_all(x);\n}\n",
    )
    .unwrap();
    std::fs::write(root.join("src/b.c"), "x = compute(1);\n").unwrap();
    std::fs::write(root.join("src/c.bin"), b"compute\0\x01").unwrap();
    std::fs::write(root.join("src/d.rs"), "fn compute() {}\n").unwrap();
    std::fs::write(root.join("src/e.py"), "def compute():\n    pass\n").unwrap();

    let only_c = |p: &str| !p.ends_with(".rs");
    let hits = text_search(root, "compute", 100, &only_c);
    let found: Vec<(&str, Position, bool)> = hits
        .iter()
        .map(|h| {
            (
                h.location.path.as_str(),
                h.location.range.start,
                h.is_definition,
            )
        })
        .collect();
    assert_eq!(
        found,
        vec![
            ("src/a.c", Position { line: 1, col: 11 }, false),
            ("src/a.c", Position { line: 2, col: 5 }, false),
            ("src/b.c", Position { line: 1, col: 5 }, false),
            ("src/e.py", Position { line: 1, col: 5 }, true),
        ]
    );
    assert!(hits.iter().all(|h| h.source == Source::Text));
    assert_eq!(hits[0].location.range.end, Position { line: 1, col: 18 });
    assert_eq!(
        hits[1].preview,
        "// Привет compute\nint compute(int x) {\n  return recompute(x) + compute_all(x);"
    );
    assert_eq!(text_search(root, "compute", 2, &only_c).len(), 2);
}
