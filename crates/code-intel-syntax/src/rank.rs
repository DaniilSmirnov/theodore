//! Path heuristics for ranking name-matched answers: the syntax layer
//! cannot resolve imports, so nearness in the tree stands in for them.

use std::path::Path;

/// Tree distance between two files' directories: 0 when they share one,
/// growing with each component they diverge by on either side.
pub(crate) fn dir_distance(a: &str, b: &str) -> u32 {
    let dir = |p: &str| -> Vec<String> {
        Path::new(p)
            .parent()
            .map(|d| {
                d.components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    };
    let (a, b) = (dir(a), dir(b));
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    ((a.len() - common) + (b.len() - common)) as u32
}

pub(crate) fn same_dir(a: &str, b: &str) -> bool {
    Path::new(a).parent() == Path::new(b).parent()
}

/// The name other files use to import `path`'s module: its stem, or its
/// directory's name for an index file (`mod.rs`, `index.ts`,
/// `__init__.py`, `lib.rs`).
pub(crate) fn module_name(path: &str) -> Option<String> {
    let p = Path::new(path);
    let stem = p.file_stem()?.to_str()?;
    if matches!(stem, "mod" | "index" | "__init__" | "lib" | "main") {
        let mut dir = p.parent()?;
        // `crates/foo-bar/src/lib.rs` is imported as `foo_bar`.
        if dir.file_name()? == "src" {
            dir = dir.parent()?;
        }
        return dir.file_name()?.to_str().map(|s| s.replace('-', "_"));
    }
    Some(stem.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distance_counts_diverging_components() {
        assert_eq!(dir_distance("a/b/x.rs", "a/b/y.rs"), 0);
        assert_eq!(dir_distance("a/b/x.rs", "a/c/y.rs"), 2);
        assert_eq!(dir_distance("x.rs", "a/b/y.rs"), 2);
    }

    #[test]
    fn module_names_follow_index_files() {
        assert_eq!(
            module_name("src/usecases/symbols.rs").as_deref(),
            Some("symbols")
        );
        assert_eq!(module_name("src/store/mod.rs").as_deref(), Some("store"));
        assert_eq!(module_name("pkg/__init__.py").as_deref(), Some("pkg"));
        assert_eq!(
            module_name("crates/code-intel/src/lib.rs").as_deref(),
            Some("code_intel")
        );
    }
}
