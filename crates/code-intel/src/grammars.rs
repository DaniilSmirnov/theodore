//! Where grammars come from, and the provider the indexer asks.
//!
//! ```text
//! indexer ─▶ provider(lang) ─▶ GrammarSource::grammar(lang)          own tags.scm
//!                          └─▶ manifest tags_extends? ─▶ grammar(base) base tags.scm
//!                              ─▶ base + own compiles? ── yes ─▶ joined query
//!                                                      └─ no ──▶ own query, warning
//! ```
//!
//! The toolchain hands over each language's own tags query as upstream
//! ships it. Upstream TypeScript's `tags.scm` only extends JavaScript's — on
//! its own it finds no class or function — so the manifest says
//! `tags_extends: javascript` and the join happens here, one level up from
//! the toolchain, where a test can swap the source for static grammars.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use theodore_code_intel_syntax::{CompiledGrammar, Grammar, GrammarProvider};
use theodore_code_intel_toolchain::{LoadedGrammar, Manifest, OnBytes, Toolchain};
use theodore_ports::code_intel::{CodeIntelResult, InstallState, LanguageId};

use crate::workspace::lock;

/// Loads grammars. [`Toolchain`] in the app (downloads, no consent asked:
/// grammars run nothing); static grammars in tests.
#[async_trait]
pub trait GrammarSource: Send + Sync + 'static {
    /// The language's grammar with its own tags query; downloaded first if
    /// needed.
    async fn grammar(&self, language: &LanguageId) -> CodeIntelResult<LoadedGrammar>;
    /// Without loading or downloading anything.
    fn state(&self, language: &LanguageId) -> InstallState;

    /// [`grammar`](Self::grammar), telling `on_bytes` how a download goes.
    async fn grammar_with(
        &self,
        language: &LanguageId,
        on_bytes: Option<&OnBytes>,
    ) -> CodeIntelResult<LoadedGrammar> {
        let _ = on_bytes;
        self.grammar(language).await
    }

    /// Delete the language's grammar package from disk; the languages it
    /// held. Nothing to delete by default (static grammars).
    fn remove(&self, language: &LanguageId) -> CodeIntelResult<Vec<LanguageId>> {
        let _ = language;
        Ok(Vec::new())
    }
}

#[async_trait]
impl GrammarSource for Toolchain {
    async fn grammar(&self, language: &LanguageId) -> CodeIntelResult<LoadedGrammar> {
        self.ensure_grammar(language).await
    }

    fn state(&self, language: &LanguageId) -> InstallState {
        self.grammar_state(language)
    }

    async fn grammar_with(
        &self,
        language: &LanguageId,
        on_bytes: Option<&OnBytes>,
    ) -> CodeIntelResult<LoadedGrammar> {
        self.ensure_grammar_with(language, on_bytes).await
    }

    fn remove(&self, language: &LanguageId) -> CodeIntelResult<Vec<LanguageId>> {
        self.remove_grammar(language)
    }
}

/// Languages you removed (project settings → Languages → Remove): their grammar is
/// not fetched on its own again — grammars otherwise download without
/// asking — until `install()`. Kept in `<data dir>/removed.json`, so it
/// holds across restarts.
pub(crate) struct Removed {
    path: PathBuf,
    set: Mutex<BTreeSet<LanguageId>>,
}

impl Removed {
    pub fn load(path: PathBuf) -> Self {
        let set = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<BTreeSet<LanguageId>>(&b).ok())
            .unwrap_or_default();
        Self {
            path,
            set: Mutex::new(set),
        }
    }

    pub fn contains(&self, language: &LanguageId) -> bool {
        lock(&self.set).contains(language)
    }

    pub fn insert(&self, languages: &[LanguageId]) {
        self.update(|set| set.extend(languages.iter().cloned()));
    }

    pub fn forget(&self, languages: &[LanguageId]) {
        self.update(|set| {
            for l in languages {
                set.remove(l);
            }
        });
    }

    fn update(&self, f: impl FnOnce(&mut BTreeSet<LanguageId>)) {
        let mut set = lock(&self.set);
        let before = set.clone();
        f(&mut set);
        if *set == before {
            return;
        }
        let write = serde_json::to_vec(&*set)
            .map_err(std::io::Error::other)
            .and_then(|json| {
                if let Some(dir) = self.path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(&self.path, json)
            });
        if let Err(e) = write {
            tracing::warn!(target: "code_intel", path = %self.path.display(), error = %e, "removed languages not saved");
        }
    }
}

/// The indexer's provider over `source`, joining tags queries per the
/// manifest's `tags_extends`. `None` (no grammar: the text layer answers)
/// when the source fails or the language was removed.
pub(crate) fn provider(
    source: Arc<dyn GrammarSource>,
    manifest: Manifest,
    removed: Arc<Removed>,
) -> GrammarProvider {
    Arc::new(move |language: LanguageId| {
        let source = source.clone();
        let base = manifest
            .language(&language)
            .and_then(|l| l.tags_extends.clone())
            .filter(|b| !removed.contains(b));
        let removed = removed.contains(&language);
        Box::pin(async move {
            if removed {
                tracing::info!(target: "code_intel", %language, "grammar removed; text search only");
                return None;
            }
            load(source.as_ref(), language, base).await
        })
    })
}

async fn load(
    source: &dyn GrammarSource,
    language: LanguageId,
    base: Option<LanguageId>,
) -> Option<Grammar> {
    let own = match source.grammar(&language).await {
        Ok(g) => g,
        Err(e) => {
            tracing::info!(target: "code_intel", %language, error = %e, "no grammar; text search only");
            return None;
        }
    };
    let Some(base) = base else {
        return Some(Grammar::new(language, own.language, own.tags_query));
    };
    let base_query = match source.grammar(&base).await {
        Ok(b) => b.tags_query,
        Err(e) => {
            tracing::warn!(target: "code_intel", %language, %base, error = %e, "base tags query unavailable; own query only");
            return Some(Grammar::new(language, own.language, own.tags_query));
        }
    };
    let joined = Grammar::new(
        language.clone(),
        own.language.clone(),
        format!("{base_query}\n{}", own.tags_query),
    );
    // A base pattern naming a node this grammar lacks would make the whole
    // query fail to compile, and lose the language's own patterns with it.
    let check = joined.clone();
    let compiles = tokio::task::spawn_blocking(move || CompiledGrammar::compile(&check))
        .await
        .map(|r| r.map_err(|e| e.to_string()));
    match compiles {
        Ok(Ok(_)) => Some(joined),
        Ok(Err(e)) => {
            tracing::warn!(target: "code_intel", %language, %base, error = %e, "joined tags query does not compile; own query only");
            Some(Grammar::new(language, own.language, own.tags_query))
        }
        Err(_) => Some(Grammar::new(language, own.language, own.tags_query)),
    }
}
