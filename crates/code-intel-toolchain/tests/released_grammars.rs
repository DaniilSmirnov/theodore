//! Every grammar of the builtin manifest, downloaded from its real release,
//! verified and loaded. Network: run by hand after a grammar bump.
//!
//! `cargo test -p theodore-code-intel-toolchain --test released_grammars -- --ignored`

use theodore_code_intel_toolchain::Toolchain;
use theodore_ports::code_intel::{InstallState, LanguageId};

#[tokio::test]
#[ignore = "downloads every grammar from GitHub"]
async fn every_released_grammar_downloads_verifies_and_loads() {
    let dir = tempfile::tempdir().unwrap();
    let tc = Toolchain::new(dir.path());
    let ids: Vec<LanguageId> = tc
        .manifest()
        .grammars()
        .iter()
        .flat_map(|g| g.languages.iter().map(|l| l.id.clone()))
        .collect();
    assert!(!ids.is_empty());
    for id in ids {
        let g = tc
            .ensure_grammar(&id)
            .await
            .unwrap_or_else(|e| panic!("{id}: {e}"));
        assert!(!g.tags_query.is_empty(), "{id}: empty tags query");
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&g.language).unwrap();
        assert!(
            matches!(tc.grammar_state(&id), InstallState::Installed { .. }),
            "{id}"
        );
    }
}
