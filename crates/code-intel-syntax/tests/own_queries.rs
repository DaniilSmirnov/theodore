//! Our own tags queries (`crates/code-intel-toolchain/queries/*.scm`),
//! compiled against the languages' own static grammar crates as
//! dev-dependencies — the same bytes `scripts/build-grammars.sh` packages
//! next to each dylib, exercised the same way the toolchain uses them
//! (`Grammar` + `SyntaxIndex`), without needing a built dylib at all.
//!
//! json, yaml, toml, bash, markdown ship no upstream `tags.scm` (§4.1);
//! these are the queries that fill that gap. rust's is upstream's own
//! query with our additions appended (`tags_query_local_mode = "append"`
//! in grammars.toml) — scoped calls and generic/scoped impl containers
//! upstream's query misses.

mod common;

use common::write_tree;
use theodore_code_intel_syntax::{
    DEFAULT_MAX_FILE_BYTES, Grammar, Snapshot, SyntaxIndex, blob_sha,
};
use theodore_ports::code_intel::{CodeSymbolKind as K, LanguageId};

/// Indexes `path` of `snap` with `grammar` (bypassing language detection:
/// these languages aren't in `common::grammar_for`).
fn index_one(idx: &SyntaxIndex, grammar: &Grammar, snap: &Snapshot, path: &str) {
    let src = std::fs::read(snap.root().join(path)).unwrap();
    idx.index_blob(&blob_sha(&src), grammar, &src, DEFAULT_MAX_FILE_BYTES)
        .unwrap();
}

fn names(idx: &SyntaxIndex, snap: &Snapshot, path: &str) -> Vec<(String, K)> {
    idx.file_symbols(snap, path)
        .unwrap()
        .into_iter()
        .map(|s| (s.name, s.kind))
        .collect()
}

#[test]
fn json_only_top_level_object_keys_are_definitions() {
    let grammar = Grammar::new(
        LanguageId::new("json"),
        tree_sitter_json::LANGUAGE,
        include_str!("../../code-intel-toolchain/queries/json-tags.scm"),
    );
    let dir = tempfile::tempdir().unwrap();
    let src = r#"{"name": "theodore", "nested": {"inner": 1}, "count": 2}"#;
    let snap = write_tree(dir.path(), &[("a.json", src)]);
    let idx = SyntaxIndex::open_in_memory().unwrap();
    index_one(&idx, &grammar, &snap, "a.json");
    assert_eq!(
        names(&idx, &snap, "a.json"),
        vec![
            ("name".to_string(), K::Field),
            ("nested".to_string(), K::Field),
            ("count".to_string(), K::Field),
        ],
        "nested's own key (\"inner\") must not show up as top-level"
    );
}

#[test]
fn yaml_mapping_keys_two_levels_deep() {
    let grammar = Grammar::new(
        LanguageId::new("yaml"),
        tree_sitter_yaml::LANGUAGE,
        include_str!("../../code-intel-toolchain/queries/yaml-tags.scm"),
    );
    let dir = tempfile::tempdir().unwrap();
    let src = "top:\n  mid: 1\n  deep:\n    deeper: 2\nother: 3\n";
    let snap = write_tree(dir.path(), &[("a.yaml", src)]);
    let idx = SyntaxIndex::open_in_memory().unwrap();
    index_one(&idx, &grammar, &snap, "a.yaml");
    let found: Vec<String> = names(&idx, &snap, "a.yaml")
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert_eq!(
        found,
        vec!["top", "mid", "deep", "other"],
        "\"deeper\" is a third level and must be left out"
    );
}

#[test]
fn toml_tables_and_keys() {
    let grammar = Grammar::new(
        LanguageId::new("toml"),
        tree_sitter_toml_ng::LANGUAGE,
        include_str!("../../code-intel-toolchain/queries/toml-tags.scm"),
    );
    let dir = tempfile::tempdir().unwrap();
    let src = "title = \"demo\"\n\n[server]\nhost = \"localhost\"\nport = 8080\n\n[[servers]]\nname = \"a\"\n";
    let snap = write_tree(dir.path(), &[("a.toml", src)]);
    let idx = SyntaxIndex::open_in_memory().unwrap();
    index_one(&idx, &grammar, &snap, "a.toml");
    assert_eq!(
        names(&idx, &snap, "a.toml"),
        vec![
            ("title".to_string(), K::Field),
            ("server".to_string(), K::Module),
            ("host".to_string(), K::Field),
            ("port".to_string(), K::Field),
            ("servers".to_string(), K::Module),
            ("name".to_string(), K::Field),
        ]
    );
}

#[test]
fn bash_function_definitions_and_command_calls() {
    let grammar = Grammar::new(
        LanguageId::new("bash"),
        tree_sitter_bash::LANGUAGE,
        include_str!("../../code-intel-toolchain/queries/bash-tags.scm"),
    );
    let dir = tempfile::tempdir().unwrap();
    let src = "greet() {\n  echo \"hi\"\n}\ngreet\n";
    let snap = write_tree(dir.path(), &[("a.sh", src)]);
    let idx = SyntaxIndex::open_in_memory().unwrap();
    index_one(&idx, &grammar, &snap, "a.sh");
    let tags = idx.file_symbols(&snap, "a.sh").unwrap();
    assert_eq!(tags.len(), 1);
    assert_eq!(
        (tags[0].name.as_str(), tags[0].kind),
        ("greet", K::Function)
    );

    // `references` finds both the definition and the two command calls
    // (`echo` and the bare `greet` invocation) as references by name.
    let greet_refs = idx.references(&snap, "a.sh", "greet", 10).unwrap();
    assert_eq!(
        greet_refs.len(),
        2,
        "the definition and the call: {greet_refs:?}"
    );
    assert!(greet_refs.iter().any(|h| h.is_definition));
    assert!(greet_refs.iter().any(|h| !h.is_definition));

    let echo_refs = idx.references(&snap, "a.sh", "echo", 10).unwrap();
    assert_eq!(echo_refs.len(), 1);
    assert!(!echo_refs[0].is_definition);
}

#[test]
fn markdown_headings_are_module_definitions() {
    let grammar = Grammar::new(
        LanguageId::new("markdown"),
        tree_sitter_md::LANGUAGE,
        include_str!("../../code-intel-toolchain/queries/markdown-tags.scm"),
    );
    let dir = tempfile::tempdir().unwrap();
    let src = "# Title\n\nSome text.\n\n## Sub\n\nHeading\n=======\n";
    let snap = write_tree(dir.path(), &[("a.md", src)]);
    let idx = SyntaxIndex::open_in_memory().unwrap();
    index_one(&idx, &grammar, &snap, "a.md");
    let headings = names(&idx, &snap, "a.md");
    assert_eq!(headings.len(), 3);
    assert!(headings.iter().all(|(_, k)| *k == K::Module));
    assert_eq!(headings[0].0.trim(), "Title");
    assert_eq!(headings[1].0.trim(), "Sub");
    assert_eq!(headings[2].0.trim(), "Heading");
}

/// Upstream tree-sitter-rust's own `tags.scm` (the crate's `TAGS_QUERY`,
/// matching `common::rust()`), with our additions from rust-tags.scm
/// appended — exactly `tags_query_local_mode = "append"` from
/// grammars.toml.
fn rust_override() -> Grammar {
    Grammar::new(
        LanguageId::new("rust"),
        tree_sitter_rust::LANGUAGE,
        format!(
            "{}\n{}",
            tree_sitter_rust::TAGS_QUERY,
            include_str!("../../code-intel-toolchain/queries/rust-tags.scm")
        ),
    )
}

#[test]
fn rust_override_adds_scoped_calls_and_generic_impl_containers() {
    let grammar = rust_override();
    let dir = tempfile::tempdir().unwrap();
    let src = "\
struct Foo<T> { x: T }

impl<T> Foo<T> {
    pub fn new(x: T) -> Self { Foo { x } }
}

impl<T: std::fmt::Debug> std::fmt::Display for Foo<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { Ok(()) }
}

fn main() {
    let f = Foo::new(1);
}
";
    let snap = write_tree(dir.path(), &[("lib.rs", src)]);
    let idx = SyntaxIndex::open_in_memory().unwrap();
    index_one(&idx, &grammar, &snap, "lib.rs");

    let s = |n: &str, k: K, c: Option<&str>| (n.to_string(), k, c.map(str::to_string));
    let got: Vec<_> = idx
        .file_symbols(&snap, "lib.rs")
        .unwrap()
        .into_iter()
        .map(|sym| (sym.name, sym.kind, sym.container))
        .collect();
    assert_eq!(
        got,
        vec![
            s("Foo", K::Struct, None),
            // `impl<T> Foo<T>`: an inherent impl of a generic type. Without
            // the override this is invisible to upstream's query, `new`
            // stays kind `function` with no container.
            s("new", K::Method, Some("Foo")),
            // `impl<T: ..> std::fmt::Display for Foo<T>`: a scoped, generic
            // trait impl. `impl_target` reads "Foo" off the header text.
            s("fmt", K::Method, Some("Foo")),
            s("main", K::Function, None),
        ]
    );

    // `Foo::new(1)` is a scoped call: upstream only matches a bare
    // identifier as a call's function, so without the override this
    // reference does not exist at all.
    let new_refs = idx.references(&snap, "lib.rs", "new", 10).unwrap();
    assert_eq!(
        new_refs.len(),
        2,
        "the definition and the Foo::new(1) call: {new_refs:?}"
    );
    assert!(new_refs.iter().any(|h| h.is_definition));
    let call = new_refs.iter().find(|h| !h.is_definition).unwrap();
    assert_eq!(call.location.path, "lib.rs");
}

/// Without the override, upstream's query alone does neither: the
/// regression this whole file guards against.
#[test]
fn rust_upstream_alone_misses_both_gaps() {
    let grammar = Grammar::new(
        LanguageId::new("rust"),
        tree_sitter_rust::LANGUAGE,
        tree_sitter_rust::TAGS_QUERY,
    );
    let dir = tempfile::tempdir().unwrap();
    let src = "\
struct Foo<T> { x: T }

impl<T> Foo<T> {
    pub fn new(x: T) -> Self { Foo { x } }
}

fn main() {
    let f = Foo::new(1);
}
";
    let snap = write_tree(dir.path(), &[("lib.rs", src)]);
    let idx = SyntaxIndex::open_in_memory().unwrap();
    index_one(&idx, &grammar, &snap, "lib.rs");
    let got: Vec<_> = idx
        .file_symbols(&snap, "lib.rs")
        .unwrap()
        .into_iter()
        .map(|sym| (sym.name, sym.kind, sym.container))
        .collect();
    // `new` is still tagged kind `method` — upstream's method rule matches
    // any `fn` directly inside a `{ }` item list, impl or not — but with no
    // container: the generic inherent impl produced no
    // `@reference.implementation` tag upstream, so nothing in
    // `extract.rs`'s container stack ever claims it.
    assert_eq!(
        got,
        vec![
            ("Foo".to_string(), K::Struct, None),
            ("new".to_string(), K::Method, None),
            ("main".to_string(), K::Function, None),
        ]
    );
    // The `Foo::new(1)` call is invisible: no reference by that name beyond
    // the definition itself.
    let new_refs = idx.references(&snap, "lib.rs", "new", 10).unwrap();
    assert_eq!(new_refs.len(), 1);
    assert!(new_refs[0].is_definition);
}
