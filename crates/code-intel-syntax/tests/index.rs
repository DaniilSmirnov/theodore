//! The tags index and the queries over a snapshot.

mod common;

use common::{rust, write_tree};
use pretty_assertions::assert_eq;
use theodore_code_intel_syntax::{
    DEFAULT_MAX_FILE_BYTES, IndexOutcome, Snapshot, SyntaxIndex, blob_sha,
};
use theodore_ports::code_intel::{CodeSymbolKind as K, Position, Range, Source};

fn p(line: u32, col: u32) -> Position {
    Position { line, col }
}

fn r(sl: u32, sc: u32, el: u32, ec: u32) -> Range {
    Range {
        start: p(sl, sc),
        end: p(el, ec),
    }
}

/// Indexes every file of `snap` with the grammar its language names.
fn index_all(idx: &SyntaxIndex, snap: &Snapshot) {
    for f in snap.files() {
        let Some(g) = f.language.as_ref().and_then(common::grammar_for) else {
            continue;
        };
        let src = std::fs::read(snap.root().join(&f.path)).unwrap();
        idx.index_blob(&f.blob_sha, &g, &src, DEFAULT_MAX_FILE_BYTES)
            .unwrap();
    }
}

fn kinds(idx: &SyntaxIndex, snap: &Snapshot, path: &str) -> Vec<(String, K, Option<String>)> {
    idx.file_symbols(snap, path)
        .unwrap()
        .into_iter()
        .map(|s| (s.name, s.kind, s.container))
        .collect()
}

const RUST: &str = "\
pub struct Point { x: i32 }
pub enum Shape { Dot }
pub trait Draw { fn draw(&self); }
type Alias = Point;
mod inner {
    pub fn helper() {}
}
macro_rules! shout { () => {} }
impl Point {
    pub fn new() -> Self { helper(); Point { x: 0 } }
}
impl Display for Point {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { Ok(()) }
}
fn main() { Point::new(); shout!(); }
";

#[test]
fn rust_kinds_and_containers() {
    let dir = tempfile::tempdir().unwrap();
    let snap = write_tree(dir.path(), &[("src/lib.rs", RUST)]);
    let idx = SyntaxIndex::open_in_memory().unwrap();
    index_all(&idx, &snap);
    let s = |n: &str, k: K, c: Option<&str>| (n.to_string(), k, c.map(str::to_string));
    assert_eq!(
        kinds(&idx, &snap, "src/lib.rs"),
        vec![
            s("Point", K::Struct, None),
            s("Shape", K::Enum, None),
            s("Draw", K::Trait, None),
            s("Alias", K::Type, None),
            s("inner", K::Module, None),
            s("helper", K::Function, Some("inner")),
            s("shout", K::Macro, None),
            s("new", K::Method, Some("Point")),
            // A trait impl's methods belong to the type, not the trait.
            s("fmt", K::Method, Some("Point")),
            s("main", K::Function, None),
        ]
    );
}

#[test]
fn python_and_typescript_kinds() {
    let dir = tempfile::tempdir().unwrap();
    let snap = write_tree(
        dir.path(),
        &[
            (
                "app.py",
                "LIMIT = 3\n\nclass Store:\n    def save(self):\n        flush()\n\ndef flush():\n    pass\n",
            ),
            (
                "app.ts",
                "interface Repo { get(): void }\nclass Store {\n  save() { flush(); }\n}\nfunction flush() {}\n",
            ),
        ],
    );
    let idx = SyntaxIndex::open_in_memory().unwrap();
    index_all(&idx, &snap);
    let names = |path| {
        kinds(&idx, &snap, path)
            .into_iter()
            .map(|(n, k, _)| (n, k))
            .collect::<Vec<_>>()
    };
    let s = |n: &str, k: K| (n.to_string(), k);
    assert_eq!(
        names("app.py"),
        vec![
            s("LIMIT", K::Constant),
            s("Store", K::Class),
            s("save", K::Method),
            s("flush", K::Function),
        ]
    );
    assert_eq!(
        names("app.ts"),
        vec![
            s("Repo", K::Interface),
            s("get", K::Method),
            s("Store", K::Class),
            s("save", K::Method),
            s("flush", K::Function),
        ]
    );
}

#[test]
fn columns_count_chars_in_multibyte_lines() {
    let src = "// Привет, мир 😀\nfn здравствуй() {}\nconst S: &str = \"😀😀\"; fn after() { здравствуй(); }\n";
    let dir = tempfile::tempdir().unwrap();
    let snap = write_tree(dir.path(), &[("a.rs", src)]);
    let idx = SyntaxIndex::open_in_memory().unwrap();
    index_all(&idx, &snap);
    let syms = idx.file_symbols(&snap, "a.rs").unwrap();
    let cyr = &syms[0];
    assert_eq!(cyr.name, "здравствуй");
    assert_eq!(cyr.name_range, r(2, 4, 2, 14));
    assert_eq!(cyr.range, r(2, 1, 2, 19));
    let after = &syms[1];
    assert_eq!(after.name, "after");
    // `const S: &str = "😀😀"; fn ` is 25 chars (the emoji are 4 bytes each).
    assert_eq!(after.name_range, r(3, 26, 3, 31));
    // The call inside `after` is a reference at char columns too.
    let refs = idx.references(&snap, "a.rs", "здравствуй", 10).unwrap();
    let call = refs.iter().find(|h| !h.is_definition).unwrap();
    assert_eq!(call.location.range, r(3, 36, 3, 46));
    // Column ranges agree with the text: slicing by them gives the name.
    let line = src.lines().nth(2).unwrap();
    let name: String = line.chars().skip(35).take(10).collect();
    assert_eq!(name, "здравствуй");
}

#[test]
fn second_index_of_a_blob_is_a_cache_hit() {
    let idx = SyntaxIndex::open_in_memory().unwrap();
    let src = b"fn a() {}\nfn b() { a(); }\n";
    let sha = blob_sha(src);
    let first = idx.index_blob(&sha, &rust(), src, 1 << 20).unwrap();
    assert_eq!(first, IndexOutcome::Indexed { tags: 3 });
    let counts = idx.counts().unwrap();
    assert_eq!(
        idx.index_blob(&sha, &rust(), src, 1 << 20).unwrap(),
        IndexOutcome::Cached
    );
    assert_eq!(idx.counts().unwrap(), counts);
    // Another tags query is another fingerprint: the blob is parsed again,
    // replacing its tags.
    let mut defs_only = rust();
    defs_only.tags_query = "(function_item name: (identifier) @name) @definition.function".into();
    assert_eq!(
        idx.index_blob(&sha, &defs_only, src, 1 << 20).unwrap(),
        IndexOutcome::Indexed { tags: 2 }
    );
    assert_eq!(idx.counts().unwrap(), (1, 2));
}

#[test]
fn oversized_and_binary_blobs_are_recorded_not_parsed() {
    let idx = SyntaxIndex::open_in_memory().unwrap();
    let big = "fn a() {}\n".repeat(20);
    let sha = blob_sha(big.as_bytes());
    assert_eq!(
        idx.index_blob(&sha, &rust(), big.as_bytes(), 50).unwrap(),
        IndexOutcome::TooLarge
    );
    assert_eq!(
        idx.index_blob(&sha, &rust(), big.as_bytes(), 50).unwrap(),
        IndexOutcome::Cached
    );
    // A larger limit retries it.
    assert_eq!(
        idx.index_blob(&sha, &rust(), big.as_bytes(), 1 << 20)
            .unwrap(),
        IndexOutcome::Indexed { tags: 20 }
    );
    let bin = b"fn a() {}\0\x01\x02";
    assert_eq!(
        idx.index_blob(&blob_sha(bin), &rust(), bin, 1 << 20)
            .unwrap(),
        IndexOutcome::Binary
    );
}

#[test]
fn snapshots_share_blobs() {
    let base_dir = tempfile::tempdir().unwrap();
    let head_dir = tempfile::tempdir().unwrap();
    let shared = "pub fn shared() {}\n";
    let base = write_tree(
        base_dir.path(),
        &[("lib.rs", shared), ("old.rs", "pub fn gone() {}\n")],
    );
    let head = write_tree(
        head_dir.path(),
        &[
            ("lib.rs", shared),
            ("new.rs", "pub fn fresh() { shared(); }\n"),
        ],
    );
    let idx = SyntaxIndex::open_in_memory().unwrap();
    index_all(&idx, &base);
    index_all(&idx, &head);
    // Three distinct blobs, not four files.
    assert_eq!(idx.counts().unwrap().0, 3);
    let in_base = idx.find_definitions(&base, "shared", None).unwrap();
    let in_head = idx.find_definitions(&head, "shared", None).unwrap();
    assert_eq!(in_base.len(), 1);
    assert_eq!(in_head.len(), 1);
    // Each snapshot sees only its own blobs.
    assert!(
        idx.find_definitions(&head, "gone", None)
            .unwrap()
            .is_empty()
    );
    assert!(
        idx.find_definitions(&base, "fresh", None)
            .unwrap()
            .is_empty()
    );
    // Identical files at two paths answer for both.
    let twin = write_tree(base_dir.path(), &[("a.rs", shared), ("b.rs", shared)]);
    let paths: Vec<String> = idx
        .find_definitions(&twin, "shared", Some(K::Function))
        .unwrap()
        .into_iter()
        .map(|s| s.path)
        .collect();
    assert_eq!(paths, vec!["a.rs", "b.rs"]);
    assert!(
        idx.find_definitions(&twin, "shared", Some(K::Struct))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn schema_version_change_rebuilds_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("code-intel/index.sqlite");
    let src = b"fn a() {}\n";
    {
        let idx = SyntaxIndex::open(&path).unwrap();
        idx.index_blob(&blob_sha(src), &rust(), src, 1 << 20)
            .unwrap();
        assert_eq!(idx.counts().unwrap(), (1, 1));
    }
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", 999).unwrap();
    }
    let idx = SyntaxIndex::open(&path).unwrap();
    assert_eq!(idx.counts().unwrap(), (0, 0));
    // Garbage instead of a database is replaced.
    drop(idx);
    std::fs::write(&path, b"not a database at all, not even close........").unwrap();
    let _ = std::fs::remove_file(dir.path().join("code-intel/index.sqlite-wal"));
    let idx = SyntaxIndex::open(&path).unwrap();
    assert_eq!(idx.counts().unwrap(), (0, 0));
}

#[test]
fn symbol_at_definition_and_body() {
    let dir = tempfile::tempdir().unwrap();
    let snap = write_tree(
        dir.path(),
        &[
            (
                "src/a/caller.rs",
                "fn run() {\n    target(1);\n    local_var;\n}\n",
            ),
            (
                "src/a/near.rs",
                "pub fn target(x: i32) -> i32 {\n    x + 1\n}\n",
            ),
            ("src/far/away.rs", "pub fn target() {}\n"),
            ("tools/t.py", "def target():\n    pass\n"),
        ],
    );
    let idx = SyntaxIndex::open_in_memory().unwrap();
    index_all(&idx, &snap);

    let at = idx
        .symbol_at(&snap, "src/a/caller.rs", p(2, 7))
        .unwrap()
        .unwrap();
    assert_eq!((at.name.as_str(), at.source), ("target", Source::Syntax));
    assert_eq!(at.range, r(2, 5, 2, 11));
    // Not a tag: read off the line.
    let word = idx
        .symbol_at(&snap, "src/a/caller.rs", p(3, 6))
        .unwrap()
        .unwrap();
    assert_eq!(
        (word.name.as_str(), word.source),
        ("local_var", Source::Text)
    );

    let defs = idx
        .definition(&snap, "src/a/caller.rs", p(2, 7), 10)
        .unwrap();
    let paths: Vec<&str> = defs.iter().map(|h| h.location.path.as_str()).collect();
    // Same language and nearest directory first, other languages last.
    assert_eq!(
        paths,
        vec!["src/a/near.rs", "src/far/away.rs", "tools/t.py"]
    );
    assert_eq!(defs[0].location.range, r(1, 8, 1, 14));
    assert_eq!(defs[0].preview, "pub fn target(x: i32) -> i32 {\n    x + 1");
    assert!(defs.iter().all(|h| h.is_definition));

    let (sym, body) = idx.symbol_body(&snap, "src/a/near.rs", "target").unwrap();
    assert_eq!(sym.kind, K::Function);
    assert_eq!(body, "pub fn target(x: i32) -> i32 {\n    x + 1\n}");
    assert!(idx.symbol_body(&snap, "src/a/near.rs", "nope").is_err());
}

#[test]
fn references_rank_same_file_then_definers_then_importers() {
    let dir = tempfile::tempdir().unwrap();
    let snap = write_tree(
        dir.path(),
        &[
            (
                "src/store/persist.rs",
                "pub fn save() {}\npub fn twice() { save(); }\n",
            ),
            ("src/store/sibling.rs", "fn s() { save(); }\n"),
            ("src/api/handler.rs", "fn h() { save(); }\n"),
            (
                "src/zzz/importer.rs",
                "use crate::store::persist::save;\nfn i() { save(); }\n",
            ),
            ("src/aaa/unrelated.rs", "fn u() { save(); }\n"),
        ],
    );
    let idx = SyntaxIndex::open_in_memory().unwrap();
    index_all(&idx, &snap);
    let hits = idx
        .references(&snap, "src/api/handler.rs", "save", 50)
        .unwrap();
    let order: Vec<(&str, u32, bool)> = hits
        .iter()
        .map(|h| {
            (
                h.location.path.as_str(),
                h.location.range.start.line,
                h.is_definition,
            )
        })
        .collect();
    assert_eq!(
        order,
        vec![
            ("src/api/handler.rs", 1, false),
            ("src/store/persist.rs", 1, true),
            ("src/store/persist.rs", 2, false),
            ("src/store/sibling.rs", 1, false),
            ("src/zzz/importer.rs", 2, false),
            ("src/aaa/unrelated.rs", 1, false),
        ]
    );
    assert!(hits.iter().all(|h| h.source == Source::Syntax));
    let capped = idx
        .references(&snap, "src/api/handler.rs", "save", 2)
        .unwrap();
    assert_eq!(capped.len(), 2);
}
