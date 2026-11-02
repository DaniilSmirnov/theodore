//! Grammars: download, cache, load (docs/code-intel.md §4.1).
//!
//! ```text
//! <root>/grammars/<package>-<version>/        one dir per grammar package
//!   ├─ grammar.dylib                           sha256 checked before every first load
//!   └─ <language>.tags.scm                     one per language of the package
//!
//! dylib ─▶ dlopen (kept for the process) ─▶ tree_sitter_<lang>() ─▶ ABI in range? ─▶ Language
//!                                                                    └─ no ─▶ error, not loaded
//! ```

use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

use theodore_ports::code_intel::{CodeIntelError, CodeIntelResult, LanguageId};

use crate::download::{fetch_verified, io_err, sha256_file};
use crate::manifest::GrammarSpec;

/// A grammar ready to parse with.
#[derive(Debug, Clone)]
pub struct LoadedGrammar {
    pub language_id: LanguageId,
    pub language: tree_sitter::Language,
    /// The tags query (`tags.scm`): definitions and references.
    pub tags_query: String,
}

/// The grammar ABI versions the linked tree-sitter runtime can use.
pub fn supported_abi() -> RangeInclusive<usize> {
    tree_sitter::MIN_COMPATIBLE_LANGUAGE_VERSION..=tree_sitter::LANGUAGE_VERSION
}

/// `abi` inside `supported`, else an error naming both.
pub fn check_abi(abi: usize, supported: &RangeInclusive<usize>) -> CodeIntelResult<()> {
    if supported.contains(&abi) {
        Ok(())
    } else {
        Err(CodeIntelError::Unsupported(format!(
            "grammar ABI {abi}, this build supports {}..={}",
            supported.start(),
            supported.end()
        )))
    }
}

pub(crate) const LIB_FILE: &str = "grammar.dylib";

pub(crate) fn tags_file(language: &LanguageId) -> String {
    format!("{language}.tags.scm")
}

/// Download the package's dylib and every tags query into `dir`, atomically.
pub(crate) async fn install(
    http: &reqwest::Client,
    root: &Path,
    spec: &GrammarSpec,
    platform: &str,
    dir: &Path,
) -> CodeIntelResult<()> {
    let dl = spec.downloads.get(platform).ok_or_else(|| {
        CodeIntelError::Unsupported(format!("{}: no build for {platform}", spec.package))
    })?;
    let tmp_dir = root.join("tmp");
    let lib = fetch_verified(http, dl, &tmp_dir).await?;
    let mut tags = Vec::new();
    for l in &spec.languages {
        tags.push((
            tags_file(&l.id),
            fetch_verified(http, &l.tags_query, &tmp_dir).await?,
        ));
    }

    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || -> CodeIntelResult<()> {
        let partial = partial_of(&dir);
        if partial.exists() {
            std::fs::remove_dir_all(&partial).map_err(|e| io_err(&partial, e))?;
        }
        std::fs::create_dir_all(&partial).map_err(|e| io_err(&partial, e))?;
        let place = |tmp: tempfile::NamedTempFile, name: &str| -> CodeIntelResult<()> {
            let to = partial.join(name);
            tmp.persist(&to).map_err(|e| io_err(&to, e.error))?;
            Ok(())
        };
        let result = place(lib, LIB_FILE)
            .and_then(|()| {
                tags.into_iter()
                    .try_for_each(|(name, tmp)| place(tmp, &name))
            })
            .and_then(|()| {
                if dir.exists() {
                    std::fs::remove_dir_all(&dir).map_err(|e| io_err(&dir, e))?;
                }
                std::fs::rename(&partial, &dir).map_err(|e| io_err(&dir, e))
            });
        if result.is_err() {
            let _ = std::fs::remove_dir_all(&partial);
        }
        result
    })
    .await
    .map_err(|e| CodeIntelError::Other(format!("grammar install: {e}")))?
}

/// The files in `dir` still match the manifest's hashes. Blocking.
pub(crate) fn verify(dir: &Path, spec: &GrammarSpec, platform: &str) -> bool {
    let Some(dl) = spec.downloads.get(platform) else {
        return false;
    };
    let matches = |p: PathBuf, sha: &str| sha256_file(&p).is_ok_and(|got| got == sha);
    matches(dir.join(LIB_FILE), &dl.sha256)
        && spec
            .languages
            .iter()
            .all(|l| matches(dir.join(tags_file(&l.id)), &l.tags_query.sha256))
}

/// Load `language` of the package installed in `dir`. Blocking.
pub(crate) fn load(
    dir: &Path,
    spec: &GrammarSpec,
    language: &LanguageId,
) -> CodeIntelResult<LoadedGrammar> {
    let gl = spec.language(language).ok_or_else(|| {
        CodeIntelError::Unsupported(format!("{}: has no language {language}", spec.package))
    })?;
    let tags_path = dir.join(tags_file(language));
    let tags_query = std::fs::read_to_string(&tags_path).map_err(|e| io_err(&tags_path, e))?;
    let lib = dir.join(LIB_FILE);
    let language_obj = loader::load_language(&lib, &gl.symbol)
        .map_err(|e| CodeIntelError::Other(format!("{}: {e}", lib.display())))?;
    check_abi(language_obj.abi_version(), &supported_abi())
        .map_err(|e| CodeIntelError::Unsupported(format!("{}: {e}", spec.package)))?;
    Ok(LoadedGrammar {
        language_id: language.clone(),
        language: language_obj,
        tags_query,
    })
}

pub(crate) fn partial_of(dir: &Path) -> PathBuf {
    let mut name = dir.file_name().unwrap_or_default().to_owned();
    name.push(".partial");
    dir.with_file_name(name)
}

pub(crate) mod loader {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, OnceLock};

    use libloading::Library;
    use tree_sitter::Language;
    use tree_sitter_language::LanguageFn;

    type LanguageCtor = unsafe extern "C" fn() -> *const ();

    /// Every grammar library opened by this process, never closed: a
    /// `Language` points into its library's static data, so the library has
    /// to outlive every `Language`, parser and tree made from it.
    static LIBRARIES: OnceLock<Mutex<HashMap<PathBuf, Library>>> = OnceLock::new();

    /// Open `path` (once per process) and build the `Language` its C function
    /// `symbol` returns. The caller checks the ABI before using it.
    #[allow(unsafe_code)]
    pub(crate) fn load_language(path: &Path, symbol: &str) -> Result<Language, String> {
        let mut libs = LIBRARIES
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if !libs.contains_key(path) {
            // SAFETY: opening runs the library's initializers. Only grammar
            // dylibs whose sha256 matched the manifest compiled into this
            // binary get here, and tree-sitter grammars have no initializers
            // beyond static data.
            let lib = unsafe { Library::new(path) }.map_err(|e| e.to_string())?;
            libs.insert(path.to_path_buf(), lib);
        }
        let lib = &libs[path];
        // SAFETY: every tree-sitter grammar exports
        // `const TSLanguage *tree_sitter_<name>(void)`, which is this type.
        // The fn pointer is copied out; it stays valid because the library is
        // never unloaded (`LIBRARIES`).
        let ctor: LanguageCtor = unsafe { lib.get::<LanguageCtor>(symbol) }
            .map(|s| *s)
            .map_err(|e| e.to_string())?;
        // SAFETY: as above; the function only returns a pointer to static data.
        if unsafe { ctor() }.is_null() {
            return Err(format!("{symbol} returned null"));
        }
        // SAFETY: `ctor` returns a valid, non-null `TSLanguage` pointer
        // (checked above) that lives for the process.
        Ok(Language::new(unsafe { LanguageFn::from_raw(ctor) }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_range() {
        let supported = 13..=15;
        assert!(check_abi(13, &supported).is_ok());
        assert!(check_abi(15, &supported).is_ok());
        let e = check_abi(16, &supported).unwrap_err().to_string();
        assert!(e.contains("ABI 16") && e.contains("13..=15"), "{e}");
        assert!(check_abi(12, &supported).is_err());
        assert!(supported_abi().contains(&tree_sitter::LANGUAGE_VERSION));
    }

    #[test]
    fn loading_a_non_library_is_an_error_not_a_crash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LIB_FILE);
        std::fs::write(&path, b"not a dylib").unwrap();
        assert!(loader::load_language(&path, "tree_sitter_json").is_err());
    }

    #[test]
    fn partial_dir_name() {
        assert_eq!(
            partial_of(Path::new("/r/grammars/tree-sitter-json-0.24.8")),
            PathBuf::from("/r/grammars/tree-sitter-json-0.24.8.partial")
        );
    }
}
