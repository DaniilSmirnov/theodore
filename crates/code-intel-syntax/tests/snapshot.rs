//! `git_snapshot` against a real repository.

mod common;

use common::{git, language_of};
use pretty_assertions::assert_eq;
use theodore_code_intel_syntax::{file_entry, git_snapshot};

#[test]
fn snapshot_uses_tree_ids_and_hashes_dirty_files_like_git() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/kept.rs"), "fn kept() {}\n").unwrap();
    std::fs::write(root.join("src/edited.rs"), "fn before() {}\n").unwrap();
    std::fs::write(root.join("gone.rs"), "fn gone() {}\n").unwrap();
    std::fs::write(root.join("notes.txt"), "Привет\n").unwrap();
    std::fs::write(root.join(".gitignore"), "target/\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "init"]);

    std::fs::write(root.join("src/edited.rs"), "fn after() { 😀(); }\n").unwrap();
    std::fs::remove_file(root.join("gone.rs")).unwrap();
    std::fs::write(root.join("src/new.rs"), "fn new() {}\n").unwrap();
    std::fs::write(root.join("staged.py"), "def staged(): pass\n").unwrap();
    git(root, &["add", "staged.py"]);
    std::fs::create_dir_all(root.join("target")).unwrap();
    std::fs::write(root.join("target/ignored.rs"), "fn ignored() {}\n").unwrap();

    let snap = git_snapshot(root, &language_of).unwrap();
    let paths: Vec<&str> = snap.files().iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        vec![
            ".gitignore",
            "notes.txt",
            "src/edited.rs",
            "src/kept.rs",
            "src/new.rs",
            "staged.py"
        ]
    );
    let sha = |p: &str| snap.get(p).unwrap().blob_sha.clone();
    // Committed and untouched: the tree's id.
    assert_eq!(
        sha("src/kept.rs"),
        git(root, &["rev-parse", "HEAD:src/kept.rs"])
    );
    // Dirty, untracked, staged: hashed by us, equal to git's own hash.
    for p in ["src/edited.rs", "src/new.rs", "staged.py", "notes.txt"] {
        assert_eq!(sha(p), git(root, &["hash-object", p]), "{p}");
    }
    assert_eq!(
        snap.get("staged.py")
            .unwrap()
            .language
            .as_ref()
            .map(|l| l.as_str()),
        Some("python")
    );
    assert_eq!(snap.get("notes.txt").unwrap().language, None);

    // The watcher path: one file re-hashed.
    let e = file_entry(root, "src/edited.rs", &language_of)
        .unwrap()
        .unwrap();
    assert_eq!(e.blob_sha, sha("src/edited.rs"));
    assert!(file_entry(root, "gone.rs", &language_of).unwrap().is_none());
}

#[test]
fn snapshot_of_a_repo_without_commits_lists_untracked_files() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    std::fs::write(dir.path().join("a.rs"), "fn a() {}\n").unwrap();
    let snap = git_snapshot(dir.path(), &language_of).unwrap();
    assert_eq!(snap.files().len(), 1);
    assert_eq!(
        snap.files()[0].blob_sha,
        git(dir.path(), &["hash-object", "a.rs"])
    );
}
