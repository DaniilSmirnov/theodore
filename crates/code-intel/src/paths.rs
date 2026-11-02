//! Repo-relative paths in, files read safely.
//!
//! ```text
//! "src/./a.rs" ─▶ "src/a.rs"          normalized
//! "../x" · "/etc/x" · "a\\b" · ""  ─▶ OutsideWorkspace
//! read: canonical(root/rel) under canonical(root)? ─▶ text   (a symlink out is refused)
//! ```

use std::path::{Component, Path, PathBuf};

use theodore_ports::code_intel::{CodeIntelError, CodeIntelResult};

/// `path` as the index stores it (`/` separators, no `.`), or
/// `OutsideWorkspace` for anything that could leave the root.
pub(crate) fn normalize(path: &str) -> CodeIntelResult<String> {
    let outside = || CodeIntelError::OutsideWorkspace(path.to_string());
    if path.contains('\\') || path.contains('\0') {
        return Err(outside());
    }
    let mut parts = Vec::new();
    for c in Path::new(path).components() {
        match c {
            Component::Normal(s) => parts.push(s.to_str().ok_or_else(outside)?),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(outside());
            }
        }
    }
    if parts.is_empty() {
        return Err(outside());
    }
    Ok(parts.join("/"))
}

/// The worktree root as every map keys it.
pub(crate) fn canonical(root: &Path) -> PathBuf {
    std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf())
}

/// The text of `rel` under `root` (canonical), if it is a file that really
/// lives there. Blocking.
pub(crate) fn read_inside(root: &Path, rel: &str) -> CodeIntelResult<String> {
    let full = std::fs::canonicalize(root.join(rel))
        .map_err(|_| CodeIntelError::FileNotFound(rel.to_string()))?;
    if !full.starts_with(root) {
        return Err(CodeIntelError::OutsideWorkspace(rel.to_string()));
    }
    let bytes = std::fs::read(&full).map_err(|_| CodeIntelError::FileNotFound(rel.to_string()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_or_rejects() {
        assert_eq!(normalize("src/./a.rs").unwrap(), "src/a.rs");
        assert_eq!(normalize("./a.rs").unwrap(), "a.rs");
        for bad in [
            "../a",
            "a/../../b",
            "a/../b",
            "/etc/passwd",
            "a\\b",
            "",
            ".",
            "a\0",
        ] {
            assert!(
                matches!(normalize(bad), Err(CodeIntelError::OutsideWorkspace(_))),
                "{bad:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn reads_only_inside() {
        let dir = tempfile::tempdir().unwrap();
        let root = canonical(dir.path());
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "s").unwrap();
        std::fs::write(root.join("a.rs"), "fn a() {}").unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), root.join("link")).unwrap();
        assert_eq!(read_inside(&root, "a.rs").unwrap(), "fn a() {}");
        assert!(matches!(
            read_inside(&root, "link"),
            Err(CodeIntelError::OutsideWorkspace(_))
        ));
        assert!(matches!(
            read_inside(&root, "missing"),
            Err(CodeIntelError::FileNotFound(_))
        ));
    }
}
