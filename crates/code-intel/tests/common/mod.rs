//! Fixtures: static grammars instead of downloads, temp git repos, a fake
//! language server on a private `$PATH`, polling helpers.

#![allow(dead_code)]

use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use theodore_code_intel::{CodeIntel, CodeIntelOptions, GrammarSource};
use theodore_code_intel_lsp::SessionOptions;
use theodore_code_intel_toolchain::{LoadedGrammar, Manifest, Toolchain};
use theodore_ports::code_intel::{
    CodeIntelError, CodeIntelPort, CodeIntelResult, InstallState, LanguageId, LanguageStatus,
    LspPolicy, ServerState, TrustMode, Workspace,
};

/// Grammars linked into the test binary, each with its upstream tags query
/// as the toolchain would download it (TypeScript's alone, not joined).
pub struct StaticGrammars {
    withheld: HashSet<String>,
}

impl StaticGrammars {
    pub fn all() -> Arc<Self> {
        Self::without(&[])
    }

    /// Every grammar but `ids`: those languages have no grammar.
    pub fn without(ids: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            withheld: ids.iter().map(|s| s.to_string()).collect(),
        })
    }

    fn load(id: &str) -> Option<(tree_sitter::Language, &'static str)> {
        Some(match id {
            "rust" => (
                tree_sitter_rust::LANGUAGE.into(),
                tree_sitter_rust::TAGS_QUERY,
            ),
            "python" => (
                tree_sitter_python::LANGUAGE.into(),
                tree_sitter_python::TAGS_QUERY,
            ),
            "javascript" => (
                tree_sitter_javascript::LANGUAGE.into(),
                tree_sitter_javascript::TAGS_QUERY,
            ),
            "typescript" => (
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                tree_sitter_typescript::TAGS_QUERY,
            ),
            "tsx" => (
                tree_sitter_typescript::LANGUAGE_TSX.into(),
                tree_sitter_typescript::TAGS_QUERY,
            ),
            _ => return None,
        })
    }
}

#[async_trait]
impl GrammarSource for StaticGrammars {
    async fn grammar(&self, language: &LanguageId) -> CodeIntelResult<LoadedGrammar> {
        let found = (!self.withheld.contains(language.as_str()))
            .then(|| Self::load(language.as_str()))
            .flatten();
        let (lang, tags) = found
            .ok_or_else(|| CodeIntelError::Unsupported(format!("{language}: no test grammar")))?;
        Ok(LoadedGrammar {
            language_id: language.clone(),
            language: lang,
            tags_query: tags.to_string(),
        })
    }

    fn state(&self, language: &LanguageId) -> InstallState {
        if !self.withheld.contains(language.as_str()) && Self::load(language.as_str()).is_some() {
            InstallState::Installed {
                version: "static".into(),
            }
        } else {
            InstallState::Unavailable
        }
    }
}

/// `git` in `dir` without the user's config (hooks, signing).
pub fn git(dir: &Path, args: &[&str]) {
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
}

/// A committed git repo with `files`.
pub fn repo(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (path, content) in files {
        let full = dir.path().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, content).unwrap();
    }
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["add", "-A"]);
    git(dir.path(), &["commit", "-q", "-m", "init"]);
    dir
}

pub fn workspace(root: &Path, trust: TrustMode) -> Workspace {
    Workspace {
        repo_key: format!("local:{}", root.display()),
        root: root.to_path_buf(),
        trust,
        languages: vec![],
        focus_paths: vec![],
        lsp: None,
    }
}

/// Everything a facade needs on disk: a data dir and an empty `$PATH`, so
/// no server installed on this machine is found.
pub struct Env {
    pub tmp: tempfile::TempDir,
    pub data: PathBuf,
    pub bin: PathBuf,
}

impl Env {
    pub fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let env = Self {
            data: base.join("code-intel"),
            bin: base.join("bin"),
            tmp,
        };
        std::fs::create_dir_all(&env.bin).unwrap();
        env
    }

    pub fn options(&self, lsp: LspPolicy) -> CodeIntelOptions {
        CodeIntelOptions {
            lsp,
            ..CodeIntelOptions::new(&self.data)
        }
    }

    /// The builtin manifest; nothing on `$PATH`.
    pub fn facade(&self, grammars: Arc<StaticGrammars>) -> CodeIntel {
        let tc = Toolchain::builder(&self.data)
            .search_path(self.bin.clone().into_os_string())
            .build();
        CodeIntel::builder(self.options(LspPolicy::default()))
            .toolchain(tc)
            .grammar_source(grammars)
            .index_workers(2)
            .build()
            .unwrap()
    }

    /// Rust served by `fake-lsp`, found on this env's `$PATH`. Every start
    /// appends the server's cwd (the worktree) to [`Self::launch_log`].
    pub fn fake_lsp_facade(&self, lsp: LspPolicy) -> CodeIntel {
        let real = env!("CARGO_BIN_EXE_code-intel-fake-lsp");
        let script = self.bin.join("fake-lsp");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\necho \"$PWD\" >> '{}'\nexec '{real}' \"$@\"\n",
                self.launch_log().display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let yaml = r#"
servers:
  - name: fake-lsp
    detect: [["fake-lsp"]]
    launch: ["{bin}"]
    init_options: { mode: "trusted" }
    review_init_options: { mode: "review" }
languages:
  - id: rust
    globs: ["*.rs"]
    servers: [fake-lsp]
  - id: go
    globs: ["*.go"]
"#;
        let manifest = Manifest::parse(yaml, r#"{"schema":1,"grammars":[]}"#)
            .unwrap()
            .0;
        let tc = Toolchain::builder(&self.data)
            .manifest(manifest)
            .search_path(self.bin.clone().into_os_string())
            .build();
        CodeIntel::builder(self.options(lsp))
            .toolchain(tc)
            .grammar_source(StaticGrammars::all())
            .session_options(SessionOptions {
                request_timeout: Duration::from_secs(5),
                init_timeout: Duration::from_secs(5),
                ready_settle: Duration::from_millis(50),
                ready_max_wait: Duration::from_secs(5),
                max_open_docs: 8,
                shutdown_grace: Duration::from_millis(500),
                diagnostics_wait: Duration::from_secs(1),
            })
            .index_workers(2)
            .build()
            .unwrap()
    }

    pub fn launch_log(&self) -> PathBuf {
        self.tmp.path().join("launches.log")
    }

    /// Worktrees the fake server was started in.
    pub fn launches(&self) -> Vec<PathBuf> {
        std::fs::read_to_string(self.launch_log())
            .unwrap_or_default()
            .lines()
            .map(PathBuf::from)
            .collect()
    }
}

pub async fn eventually<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let until = Instant::now() + Duration::from_secs(10);
    while Instant::now() < until {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

pub async fn status_of(ci: &CodeIntel, root: &Path, lang: &str) -> LanguageStatus {
    ci.status(root)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.language.as_str() == lang)
        .unwrap_or_else(|| panic!("no status for {lang}"))
}

/// Until every language with a grammar is fully indexed.
pub async fn indexed(ci: &CodeIntel, root: &Path) {
    eventually("index", || async {
        ci.status(root)
            .await
            .unwrap()
            .iter()
            .all(|s| !matches!(s.grammar, InstallState::Installed { .. }) || s.index_percent == 100)
    })
    .await;
}

pub async fn server_state(ci: &CodeIntel, root: &Path, lang: &str) -> Option<ServerState> {
    status_of(ci, root, lang).await.server.map(|s| s.state)
}
