//! A worktree as a list of (path, blob sha, language): what queries join
//! the index against.
//!
//! ```text
//! git ls-tree -r -z HEAD          committed blob ids (nothing hashed)
//!   + git status --porcelain -z   dirty and untracked paths
//!       ├─ on disk ─▶ sha1("blob <len>\0" + content)   = git hash-object
//!       └─ gone    ─▶ dropped
//!   ─▶ language_of(path) ─▶ Snapshot
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha1::{Digest, Sha1};
use theodore_ports::code_intel::LanguageId;

use crate::error::{Result, SyntaxError};

/// One file of a worktree.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FileEntry {
    /// Repo-relative, `/` separators.
    pub path: String,
    /// Git blob id (40 hex chars).
    pub blob_sha: String,
    /// `None`: no language owns the file; it is never indexed.
    pub language: Option<LanguageId>,
}

/// A worktree's files at one moment. Immutable: a change makes a new
/// snapshot ([`with_file`](Self::with_file)).
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    root: PathBuf,
    files: Vec<FileEntry>,
    by_path: HashMap<String, usize>,
    by_blob: HashMap<String, Vec<usize>>,
}

impl Snapshot {
    /// A later entry for the same path wins.
    pub fn new(root: impl Into<PathBuf>, files: Vec<FileEntry>) -> Self {
        let mut dedup: Vec<FileEntry> = Vec::with_capacity(files.len());
        let mut by_path: HashMap<String, usize> = HashMap::with_capacity(files.len());
        for f in files {
            match by_path.get(&f.path) {
                Some(&i) => dedup[i] = f,
                None => {
                    by_path.insert(f.path.clone(), dedup.len());
                    dedup.push(f);
                }
            }
        }
        let mut by_blob: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, f) in dedup.iter().enumerate() {
            by_blob.entry(f.blob_sha.clone()).or_default().push(i);
        }
        Self {
            root: root.into(),
            files: dedup,
            by_path,
            by_blob,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn files(&self) -> &[FileEntry] {
        &self.files
    }

    pub fn get(&self, path: &str) -> Option<&FileEntry> {
        self.by_path.get(path).map(|&i| &self.files[i])
    }

    /// Every path whose content is `sha` (identical files share a blob).
    pub fn paths_for_blob<'a>(&'a self, sha: &str) -> impl Iterator<Item = &'a FileEntry> + 'a {
        self.by_blob
            .get(sha)
            .into_iter()
            .flatten()
            .map(|&i| &self.files[i])
    }

    pub fn contains_blob(&self, sha: &str) -> bool {
        self.by_blob.contains_key(sha)
    }

    /// A copy with `entry` added or replacing the entry at its path.
    pub fn with_file(&self, entry: FileEntry) -> Self {
        let mut files = self.files.clone();
        files.push(entry);
        Self::new(self.root.clone(), files)
    }

    /// A copy without `path`.
    pub fn without(&self, path: &str) -> Self {
        let files = self
            .files
            .iter()
            .filter(|f| f.path != path)
            .cloned()
            .collect();
        Self::new(self.root.clone(), files)
    }

    /// The languages present, each once.
    pub fn languages(&self) -> Vec<LanguageId> {
        let mut langs: Vec<LanguageId> = self
            .files
            .iter()
            .filter_map(|f| f.language.clone())
            .collect();
        langs.sort();
        langs.dedup();
        langs
    }
}

/// Git's blob id of `content`: what `git hash-object` prints for a file
/// without filters (no `text`/`eol` conversion attributes).
pub fn blob_sha(content: &[u8]) -> String {
    let mut h = Sha1::new();
    h.update(format!("blob {}\0", content.len()).as_bytes());
    h.update(content);
    hex::encode(h.finalize())
}

/// The entry for one file on disk, hashed by us; `None` when it is not a
/// regular file (gone, a directory, a symlink). For the spaces watcher.
pub fn file_entry(
    root: &Path,
    path: &str,
    language_of: &dyn Fn(&str) -> Option<LanguageId>,
) -> Result<Option<FileEntry>> {
    let full = root.join(path);
    let meta = match std::fs::symlink_metadata(&full) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if !meta.is_file() {
        return Ok(None);
    }
    let content = std::fs::read(&full)?;
    Ok(Some(FileEntry {
        path: path.to_string(),
        blob_sha: blob_sha(&content),
        language: language_of(path),
    }))
}

/// The snapshot of the worktree at `root` (its top-level directory):
/// committed files from `HEAD`'s tree, dirty and untracked ones hashed from
/// disk, deleted ones dropped. Ignored files are not listed. Blocks on
/// `git`: call it from a blocking thread.
pub fn git_snapshot(
    root: &Path,
    language_of: &dyn Fn(&str) -> Option<LanguageId>,
) -> Result<Snapshot> {
    let mut files: HashMap<String, String> = HashMap::new();
    // No HEAD yet (a fresh repo): only dirty files.
    if let Ok(tree) = git(root, &["ls-tree", "-r", "-z", "--full-tree", "HEAD"]) {
        for rec in tree.split(|b| *b == 0).filter(|r| !r.is_empty()) {
            // `<mode> SP <type> SP <sha> TAB <path>`
            let Some(tab) = memchr::memchr(b'\t', rec) else {
                continue;
            };
            let meta = String::from_utf8_lossy(&rec[..tab]);
            let mut parts = meta.split(' ');
            let (Some(mode), Some(kind), Some(sha)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            // Symlinks (120000) and submodules (commit) are not files.
            if kind != "blob" || mode == "120000" {
                continue;
            }
            let Ok(path) = std::str::from_utf8(&rec[tab + 1..]) else {
                continue;
            };
            files.insert(path.to_string(), sha.to_string());
        }
    }
    let status = git(
        root,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--no-renames",
        ],
    )?;
    for rec in status.split(|b| *b == 0).filter(|r| r.len() > 3) {
        // `XY <path>`
        let Ok(path) = std::str::from_utf8(&rec[3..]) else {
            continue;
        };
        match file_entry(root, path, &|_| None)? {
            Some(e) => {
                files.insert(e.path, e.blob_sha);
            }
            None => {
                files.remove(path);
            }
        }
    }
    let mut entries: Vec<FileEntry> = files
        .into_iter()
        .map(|(path, blob_sha)| FileEntry {
            language: language_of(&path),
            path,
            blob_sha,
        })
        .collect();
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Snapshot::new(root, entries))
}

fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()?;
    if !out.status.success() {
        return Err(SyntaxError::Git(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_sha_matches_git_for_known_content() {
        // `printf 'hello\n' | git hash-object --stdin`
        assert_eq!(
            blob_sha(b"hello\n"),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
        assert_eq!(blob_sha(b""), "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391");
    }

    #[test]
    fn snapshot_maps_shared_blobs_and_replaces_paths() {
        let e = |p: &str, s: &str| FileEntry {
            path: p.into(),
            blob_sha: s.into(),
            language: None,
        };
        let snap = Snapshot::new("/r", vec![e("a", "1"), e("b", "1"), e("c", "2")]);
        assert_eq!(snap.paths_for_blob("1").count(), 2);
        let snap = snap.with_file(e("b", "3"));
        assert_eq!(snap.paths_for_blob("1").count(), 1);
        assert_eq!(snap.get("b").map(|f| f.blob_sha.as_str()), Some("3"));
        assert_eq!(snap.without("a").files().len(), 2);
    }
}
