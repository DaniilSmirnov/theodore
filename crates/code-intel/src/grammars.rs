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

use std::sync::Arc;

use async_trait::async_trait;
use theodore_code_intel_syntax::{CompiledGrammar, Grammar, GrammarProvider};
use theodore_code_intel_toolchain::{LoadedGrammar, Manifest, Toolchain};
use theodore_ports::code_intel::{CodeIntelResult, InstallState, LanguageId};

/// Loads grammars. [`Toolchain`] in the app (downloads, no consent asked:
/// grammars run nothing); static grammars in tests.
#[async_trait]
pub trait GrammarSource: Send + Sync + 'static {
    /// The language's grammar with its own tags query; downloaded first if
    /// needed.
    async fn grammar(&self, language: &LanguageId) -> CodeIntelResult<LoadedGrammar>;
    /// Without loading or downloading anything.
    fn state(&self, language: &LanguageId) -> InstallState;
}

#[async_trait]
impl GrammarSource for Toolchain {
    async fn grammar(&self, language: &LanguageId) -> CodeIntelResult<LoadedGrammar> {
        self.ensure_grammar(language).await
    }

    fn state(&self, language: &LanguageId) -> InstallState {
        self.grammar_state(language)
    }
}

/// The indexer's provider over `source`, joining tags queries per the
/// manifest's `tags_extends`. `None` (no grammar: the text layer answers)
/// when the source fails.
pub(crate) fn provider(source: Arc<dyn GrammarSource>, manifest: Manifest) -> GrammarProvider {
    Arc::new(move |language: LanguageId| {
        let source = source.clone();
        let base = manifest
            .language(&language)
            .and_then(|l| l.tags_extends.clone());
        Box::pin(async move { load(source.as_ref(), language, base).await })
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
