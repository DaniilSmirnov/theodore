//! Test grammars (linked statically here; the app loads dylibs) and small
//! worktree helpers.

#![allow(dead_code)]

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use theodore_code_intel_syntax::{FileEntry, Grammar, GrammarProvider, Snapshot, blob_sha};
use theodore_ports::code_intel::LanguageId;

pub fn rust() -> Grammar {
    Grammar::new(
        LanguageId::new("rust"),
        tree_sitter_rust::LANGUAGE,
        tree_sitter_rust::TAGS_QUERY,
    )
}

pub fn python() -> Grammar {
    Grammar::new(
        LanguageId::new("python"),
        tree_sitter_python::LANGUAGE,
        tree_sitter_python::TAGS_QUERY,
    )
}

/// TypeScript's own tags.scm only adds to JavaScript's; this prepends the
/// JavaScript patterns the tests need, as the toolchain does.
const JS_TAGS: &str = r#"
(function_declaration name: (identifier) @name) @definition.function
(class_declaration name: (type_identifier) @name) @definition.class
(method_definition name: (property_identifier) @name) @definition.method
(call_expression function: (identifier) @name) @reference.call
(call_expression function: (member_expression property: (property_identifier) @name)) @reference.call
"#;

pub fn typescript() -> Grammar {
    Grammar::new(
        LanguageId::new("typescript"),
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
        format!("{JS_TAGS}\n{}", tree_sitter_typescript::TAGS_QUERY),
    )
}

/// The language a path's extension maps to (the toolchain's globs).
pub fn language_of(path: &str) -> Option<LanguageId> {
    let ext = Path::new(path).extension()?.to_str()?;
    match ext {
        "rs" => Some(LanguageId::new("rust")),
        "py" => Some(LanguageId::new("python")),
        "ts" => Some(LanguageId::new("typescript")),
        "cob" => Some(LanguageId::new("cobol")),
        _ => None,
    }
}

pub fn grammar_for(lang: &LanguageId) -> Option<Grammar> {
    match lang.as_str() {
        "rust" => Some(rust()),
        "python" => Some(python()),
        "typescript" => Some(typescript()),
        _ => None,
    }
}

/// A provider with every test grammar, answering at once.
pub fn provider() -> GrammarProvider {
    Arc::new(|lang: LanguageId| Box::pin(async move { grammar_for(&lang) }))
}

/// Writes `files` under `root` and returns their snapshot (hashed, no git).
pub fn write_tree(root: &Path, files: &[(&str, &str)]) -> Snapshot {
    let mut entries = Vec::new();
    for (path, content) in files {
        let full = root.join(path);
        if let Some(dir) = full.parent() {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(&full, content).unwrap();
        entries.push(FileEntry {
            path: path.to_string(),
            blob_sha: blob_sha(content.as_bytes()),
            language: language_of(path),
        });
    }
    Snapshot::new(root, entries)
}

/// `git` in `dir` without the user's config (hooks, signing).
pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}
