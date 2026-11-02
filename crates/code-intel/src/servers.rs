//! Which language server serves a workspace's language, whether the policy
//! lets it start, and how (docs/code-intel.md §6, §8).
//!
//! ```text
//! open ─▶ server_for(lang) ─▶ (spec, install state)
//!          └─ allowed(policy, trust)? ── no ──▶ never resolved, never started
//!               └─ yes: on the machine or downloaded? ─▶ resolve ─▶ LaunchConfig ─▶ pool
//!                   └─ missing ─▶ waits for install()   (no download here)
//! ```
//!
//! | worktree        | switch                          | default | options sent                    |
//! | --------------- | ------------------------------- | ------- | ------------------------------- |
//! | Review (a PR)   | `code_intel.lsp.review`         | off     | `init` + `review_init_options`  |
//! | Trusted (space) | `code_intel.lsp.spaces`         | on      | `init_options`                  |
//!
//! Review sessions live in a pool slot of their own (`<repo>#review`).

use std::path::Path;

use theodore_code_intel_lsp::LaunchConfig;
use theodore_code_intel_toolchain::{ResolvedServer, ServerSpec, Toolchain};
use theodore_ports::code_intel::{InstallState, LanguageId, LspPolicy, TrustMode};

/// The trust gate. Every spawn, every query and every `file_changed`
/// forwarded to a server goes through here.
pub(crate) fn allowed(policy: LspPolicy, trust: TrustMode) -> bool {
    policy.allows(trust)
}

/// Why [`allowed`] said no, for `Answer::note`.
pub(crate) fn denied_note(trust: TrustMode) -> &'static str {
    match trust {
        TrustMode::Review => "language servers are off for reviews (code_intel.lsp.review)",
        TrustMode::Trusted => "language servers are off for spaces (code_intel.lsp.spaces)",
    }
}

/// The pool's repo key. Review sessions never share a process with trusted
/// ones: a trusted session runs with build scripts and proc macros on, and
/// a review worktree joining it as a folder would get them too.
pub(crate) fn pool_key(repo_key: &str, trust: TrustMode) -> String {
    match trust {
        TrustMode::Trusted => repo_key.to_string(),
        TrustMode::Review => format!("{repo_key}#review"),
    }
}

/// One language's server in one workspace.
#[derive(Debug, Clone)]
pub(crate) struct ServerPlan {
    pub spec: ServerSpec,
    pub install: InstallState,
    /// Set only while the policy allows it and the binary is on disk.
    pub resolved: Option<ResolvedServer>,
    /// The last failure to start it.
    pub error: Option<String>,
}

/// The server for `language` in the worktree at `root`, resolved to a
/// binary only when the policy allows starting it. `None`: the manifest
/// lists no server. Probes run (`--version`); nothing is downloaded.
pub(crate) async fn plan(
    toolchain: &Toolchain,
    language: &LanguageId,
    root: &Path,
    policy: LspPolicy,
    trust: TrustMode,
) -> Option<ServerPlan> {
    let (spec, install) = toolchain.server_for(language, None, root, trust).await?;
    let on_disk = matches!(
        install,
        InstallState::System { .. } | InstallState::Installed { .. }
    );
    let resolved = if on_disk && allowed(policy, trust) {
        toolchain.resolve_server(&spec, root, trust).await
    } else {
        None
    };
    Some(ServerPlan {
        spec,
        install,
        resolved,
        error: None,
    })
}

/// The pool's launch config for `plan`, serving `languages`. `None` when it
/// is not resolved or its argv is not UTF-8. `trust` picks the options:
/// review runs get `review_init_options` merged in.
pub(crate) fn launch(
    toolchain: &Toolchain,
    plan: &ServerPlan,
    trust: TrustMode,
    languages: &[LanguageId],
) -> Option<LaunchConfig> {
    let resolved = plan.resolved.as_ref()?;
    let cfg = toolchain.launch_config(resolved, &plan.spec, trust);
    let argv = cfg
        .argv
        .into_iter()
        .map(|a| a.into_string().ok())
        .collect::<Option<Vec<String>>>()?;
    let manifest = toolchain.manifest();
    let language_ids = languages
        .iter()
        .filter_map(|l| manifest.language(l))
        .map(|l| {
            (
                l.id.clone(),
                l.lsp_language_id().to_string(),
                extensions(&l.globs),
            )
        })
        .collect();
    Some(LaunchConfig {
        server: plan.spec.name.clone(),
        argv,
        env: Vec::new(),
        init_options: cfg.init_options,
        workspace_folders: cfg.workspace_folders,
        language_ids,
    })
}

/// File extensions of plain `*.ext` globs (`Dockerfile` has none).
fn extensions(globs: &[String]) -> Vec<String> {
    globs
        .iter()
        .filter_map(|g| g.strip_prefix("*."))
        .filter(|e| !e.contains(['*', '?', '/']))
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_switch_gates_its_own_trust_mode() {
        use TrustMode::*;
        // (review, spaces) → (Review starts, Trusted starts)
        for (review, spaces, want_review, want_trusted) in [
            (false, false, false, false),
            (false, true, false, true),
            (true, false, true, false),
            (true, true, true, true),
        ] {
            let p = LspPolicy { review, spaces };
            assert_eq!(allowed(p, Review), want_review, "{p:?}");
            assert_eq!(allowed(p, Trusted), want_trusted, "{p:?}");
        }
        assert_eq!(
            LspPolicy::default(),
            LspPolicy {
                review: false,
                spaces: true
            }
        );
        assert_ne!(
            pool_key("github:o/r", Review),
            pool_key("github:o/r", Trusted)
        );
    }

    #[test]
    fn extensions_from_globs() {
        let globs: Vec<String> = ["*.ts", "*.d.ts", "Dockerfile", "*.c++", "src/*.x", "*.*"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(extensions(&globs), vec!["ts", "d.ts", "c++"]);
    }
}
