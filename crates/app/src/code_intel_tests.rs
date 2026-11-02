//! Code intelligence above the port (docs/code-intel.md) against `Core`
//! and the testkit's `FakeCodeIntel`: which worktrees open under which
//! trust, the install flow, which runs get which code tools, the edits
//! (Ask mode refused, back-to-front, multibyte, paths kept inside) and
//! `find_symbol` falling back to grep.

use std::path::{Path, PathBuf};
use std::time::Duration;

use theodore_contract::{
    Ephemeral, FindSymbolRequest, OpenPrRequest, RunSkillRequest, SpaceSendRequest,
    SpaceTrustRequest, SymbolKind, SymbolSource, models,
};
use theodore_domain::{AgentId, HostKind, PrRef, RepoRef, Sha};
use theodore_ports::{
    CodeHit, CodeIntelError, CodeSymbol, CodeSymbolKind, Fetched, GrepHit, InstallState,
    LanguageId, LanguageStatus, Location, NewAction, Position, Range, ServerState, ServerStatus,
    Source, SyncActionCode, TextEdit,
};

use crate::config::InstallPolicy;
use crate::deps::CodeTools;
use crate::space_commands_tests::{Env, done, start_env_with, wait_until};
use crate::testkit::{self, Fixtures, fixtures};
use crate::usecases::code_intel::CodeScope;
use crate::{AppError, Core, Receivers};

async fn code_env() -> Env {
    start_env_with(Fixtures::deps_with_code).await
}

fn range(start: (u32, u32), end: (u32, u32)) -> Range {
    Range {
        start: Position {
            line: start.0,
            col: start.1,
        },
        end: Position {
            line: end.0,
            col: end.1,
        },
    }
}

fn symbol(path: &str, name: &str, whole: Range, name_at: Range) -> CodeSymbol {
    CodeSymbol {
        name: name.into(),
        kind: CodeSymbolKind::Function,
        range: whole,
        name_range: name_at,
        path: path.into(),
        container: None,
        source: Source::Syntax,
    }
}

fn opened(e: &Env, needle: &str) -> bool {
    e.f.code_intel.calls().iter().any(|c| c.starts_with(needle))
}

/// A space with the given mode in its row (the current turn's).
async fn set_mode(e: &Env, space: &str, mode: &str) {
    e.core
        .apply(vec![NewAction {
            model: models::SPACE.into(),
            model_id: space.into(),
            action: SyncActionCode::C,
            data: Some(serde_json::json!({ "mode": mode })),
        }])
        .await
        .unwrap();
}

/// No event matching `pred` within a short while.
async fn no_ephemeral(recv: &mut Receivers, pred: impl Fn(&Ephemeral) -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(150);
    while let Ok(Ok(event)) = tokio::time::timeout_at(deadline, recv.ephemeral.recv()).await {
        assert!(!pred(&event), "unexpected {event:?}");
    }
}

fn suggestion(e: &Ephemeral) -> bool {
    matches!(e, Ephemeral::CodeIntelSuggest { .. })
}

fn missing_server(lang: &str, server: &str) -> LanguageStatus {
    LanguageStatus {
        language: LanguageId::new(lang),
        files: 12,
        grammar: InstallState::Installed {
            version: "0.23.6".into(),
        },
        server: Some(ServerStatus {
            name: server.into(),
            install: InstallState::Missing,
            state: ServerState::NotRunning,
        }),
        index_percent: 40,
    }
}

// ── config ──────────────────────────────────────────────────────────────────

#[test]
fn code_intel_config_defaults_and_layers() {
    use crate::config::{LayerText, merge};
    use theodore_contract::ConfigLayer;
    let layer = |layer, text: &str| LayerText {
        layer,
        path: PathBuf::from(format!("/{layer:?}.yaml")),
        legacy: false,
        text: Some(text.into()),
    };
    let none = merge(&[]).config.code_intel;
    assert!(none.enabled);
    assert_eq!(none.install, InstallPolicy::Ask);
    assert!(none.languages.is_empty());
    assert_eq!(
        (none.max_servers, none.idle_minutes, none.max_file_kb),
        (4, 10, 1024)
    );

    let m = merge(&[
        layer(
            ConfigLayer::Global,
            "code_intel:\n  install: always\n  languages: [rust, go]\n  servers: {python: pyright}\n",
        ),
        layer(
            ConfigLayer::Repo,
            "code_intel:\n  languages: [typescript, ' rust ', typescript, '']\n  max_file_kb: 256\n  servers: {c++: clangd}\n",
        ),
        layer(ConfigLayer::Local, "code_intel: {enabled: false}\n"),
    ]);
    let c = &m.config.code_intel;
    assert!(!c.enabled);
    assert_eq!(c.install, InstallPolicy::Always);
    assert_eq!(c.language_ids(), ["typescript", "rust"], "lists replace");
    assert_eq!(c.max_file_kb, 256);
    assert_eq!(c.servers["python"], "pyright");
    assert_eq!(c.servers["c++"], "clangd");
    assert_eq!(m.layer_of("code_intel.install"), Some(ConfigLayer::Global));
    assert_eq!(m.layer_of("code_intel.enabled"), Some(ConfigLayer::Local));
    assert!(m.unknown.is_empty(), "{:?}", m.unknown);
}

#[test]
fn a_bad_code_intel_section_is_dropped_alone_and_typos_are_unknown() {
    use crate::config::{LayerText, editor, merge};
    use theodore_contract::ConfigLayer;
    let text = "spaces: {ports: 2}\ncode_intel:\n  install: sometimes\n  max_serverz: 3\n";
    let m = merge(&[LayerText {
        layer: ConfigLayer::Repo,
        path: "/r".into(),
        legacy: false,
        text: Some(text.into()),
    }]);
    assert_eq!(m.config.spaces.ports, Some(2), "spaces still applies");
    assert_eq!(m.config.code_intel.install, InstallPolicy::Ask);
    assert_eq!(m.errors.len(), 1);
    assert_eq!(m.errors[0].0, "code_intel");
    assert_eq!(
        m.unknown,
        vec![("/r".to_string(), "code_intel.max_serverz".to_string())]
    );
    let problems = editor::validate(text);
    assert!(
        problems
            .iter()
            .any(|p| p.line == Some(3) && p.message.starts_with("`code_intel.install`")),
        "{problems:?}"
    );
    // The editor writes the section as commented examples when unset.
    let rendered = editor::render(&editor::header("x"), &serde_json::json!({}));
    assert!(
        rendered.contains("\n# code_intel:\n#   enabled: true\n"),
        "{rendered}"
    );
    assert!(editor::validate(crate::config::GLOBAL_TEMPLATE).is_empty());
}

// ── open / close ────────────────────────────────────────────────────────────

/// The fixture PR, opened with code intelligence wired in.
async fn open_pr_with_code(f: &Fixtures) -> (Core, Receivers, theodore_domain::PrId) {
    let pr_ref = PrRef {
        repo: RepoRef {
            host: HostKind::Github,
            owner: "acme".into(),
            name: "widgets".into(),
        },
        number: 7,
        host_id: Some("node-7".into()),
    };
    let detail = testkit::sample_pr_detail(&pr_ref, &Sha::new("base000"), &Sha::new("head111"));
    f.host.push_get_pr(Ok(Fetched::Modified {
        etag: Some("etag-1".into()),
        value: detail,
    }));
    let (core, recv) = Core::start(f.deps_with_code());
    let resp = core
        .open_pr(OpenPrRequest {
            url: "https://github.com/acme/widgets/pull/7".into(),
        })
        .await
        .expect("open_pr");
    (core, recv, resp.pr)
}

#[tokio::test]
async fn an_opened_pr_is_served_in_review_mode_and_its_runs_get_read_tools() {
    let f = fixtures();
    let (core, _recv, pr) = open_pr_with_code(&f).await;
    let root = f.git.worktree_path(&Sha::new("head111"));
    let want = format!("open {} Review []", root.display());
    wait_until(|| f.code_intel.calls().contains(&want)).await;

    f.agents.push_script(done("fine"));
    core.run_skill(RunSkillRequest {
        pr: pr.clone(),
        skill: "security".into(),
        agents: vec![AgentId::Claude],
    })
    .await
    .unwrap();
    wait_until(|| !f.tools.registered().is_empty()).await;
    assert_eq!(f.tools.registered()[0].code, CodeTools::Read);
    wait_until(|| f.agents.last_request().is_some()).await;
    let system = f.agents.last_request().unwrap().system.unwrap_or_default();
    assert!(system.contains("find_symbol"), "{system}");
    assert!(!system.contains("rename_symbol"), "{system}");
    // Opened once, however many times it is asked for.
    let opens = f
        .code_intel
        .calls()
        .iter()
        .filter(|c| c.starts_with("open "))
        .count();
    assert_eq!(opens, 1, "{:?}", f.code_intel.calls());

    // The worktree went away (GC): the sweep closes it.
    core.code_intel_sweep().await;
    assert!(
        f.code_intel
            .calls()
            .contains(&format!("close {}", root.display()))
    );
}

#[tokio::test]
async fn code_intel_off_in_config_opens_nothing_and_lists_no_tools() {
    let e = code_env().await;
    e.global("code_intel: {enabled: false}");
    e.f.agents.push_script(done("ok"));
    let s = e.create("Fix it").await;
    e.until(&s.id, |s| s.turns == 1 && s.run.is_none()).await;
    assert_eq!(e.f.tools.registered()[0].code, CodeTools::Off);
    assert!(!opened(&e, "open "), "{:?}", e.f.code_intel.calls());
    let err = e
        .core
        .code_status(&CodeScope::Space(s.id.clone()))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("code intelligence is off"),
        "{err}"
    );
}

#[tokio::test]
async fn a_space_is_trusted_unless_repo_commands_wait_for_approval() {
    let e = code_env().await;
    e.global("code_intel: {languages: [rust]}");
    let s = e.create("").await;
    let want = format!("open {} Trusted [rust]", s.worktree);
    wait_until(|| e.f.code_intel.calls().contains(&want)).await;

    // A repo `check` not allowed yet: Review until it is.
    e.repo_file(".theodore/config.yaml", "spaces:\n  check: make check\n");
    let s2 = e.create("").await;
    let review = format!("open {} Review [rust]", s2.worktree);
    wait_until(|| e.f.code_intel.calls().contains(&review)).await;
    let hash = e
        .core
        .space_config(&s2.id)
        .await
        .unwrap()
        .repo_commands
        .unwrap()
        .hash;
    e.core
        .space_trust(SpaceTrustRequest {
            space: s2.id.clone(),
            allow: true,
            hash,
        })
        .await
        .unwrap();
    e.core
        .code_status(&CodeScope::Space(s2.id.clone()))
        .await
        .unwrap();
    let calls = e.f.code_intel.calls();
    let close = calls
        .iter()
        .position(|c| c == &format!("close {}", s2.worktree))
        .expect("closed before reopening");
    let trusted = calls
        .iter()
        .position(|c| c == &format!("open {} Trusted [rust]", s2.worktree))
        .expect("reopened trusted");
    assert!(close < trusted, "{calls:?}");

    // Archive lets go of it.
    e.core.archive_space(&s.id).await.unwrap();
    assert!(
        e.f.code_intel
            .calls()
            .contains(&format!("close {}", s.worktree))
    );
}

#[tokio::test]
async fn a_turns_mode_decides_whether_its_code_tools_edit() {
    let e = code_env().await;
    let s = e.create("").await;
    for (mode, want) in [
        (theodore_contract::SpaceMode::Edit, CodeTools::Edit),
        (theodore_contract::SpaceMode::Ask, CodeTools::Read),
        (theodore_contract::SpaceMode::Run, CodeTools::Edit),
    ] {
        let before = e.f.tools.registered().len();
        e.f.agents.push_script(done("ok"));
        e.core
            .space_send(SpaceSendRequest {
                space: s.id.clone(),
                text: "go".into(),
                agent: None,
                model: None,
                mode: Some(mode),
                attachments: None,
            })
            .await
            .unwrap();
        wait_until(|| e.f.tools.registered().len() > before).await;
        e.until(&s.id, |s| s.run.is_none()).await;
        assert_eq!(e.f.tools.registered()[before].code, want, "{mode:?}");
        let system = e.f.agents.last_request().unwrap().system.unwrap();
        assert_eq!(system.contains("rename_symbol"), want == CodeTools::Edit);
        assert!(system.contains("find_symbol"));
    }
}

/// Files the agent wrote with its own tools are reindexed when the turn's
/// checkpoint moves.
#[tokio::test]
async fn a_turn_reindexes_the_files_it_changed() {
    let e = code_env().await;
    let s = e.create("").await;
    let files: Vec<String> = theodore_ports::GitPort::diff(
        &*e.f.git,
        Path::new("."),
        &theodore_domain::Sha::new("a"),
        &theodore_domain::Sha::new("b"),
        &Default::default(),
    )
    .await
    .unwrap()
    .files
    .into_iter()
    .map(|f| f.path)
    .collect();
    assert!(!files.is_empty());
    e.f.git.push_checkpoint(theodore_domain::Sha::new("turn1"));
    e.f.agents.push_script(done("ok"));
    e.core
        .space_send(SpaceSendRequest {
            space: s.id.clone(),
            text: "go".into(),
            agent: None,
            model: None,
            mode: Some(theodore_contract::SpaceMode::Edit),
            attachments: None,
        })
        .await
        .unwrap();
    e.until(&s.id, |s| s.run.is_none()).await;
    wait_until(|| {
        let calls = e.f.code_intel.calls();
        files.iter().all(|p| {
            calls
                .iter()
                .any(|c| c.starts_with("file_changed") && c.ends_with(p.as_str()))
        })
    })
    .await;
}

// ── code_intel.lsp ──────────────────────────────────────────────────────────

#[test]
fn a_repo_file_cannot_turn_on_language_servers() {
    use crate::config::{LayerText, editor, merge};
    use theodore_contract::{ConfigLayer, ConfigProblemLevel};
    let repo = "code_intel:\n  languages: [rust]\n  lsp:\n    review: true\n    spaces: false\n";
    let layer = |layer, text: &str| LayerText {
        layer,
        path: PathBuf::from(format!("/{layer:?}.yaml")),
        legacy: false,
        text: Some(text.into()),
    };
    let m = merge(&[layer(ConfigLayer::Repo, repo)]);
    let c = &m.config.code_intel;
    assert_eq!(c.lsp, crate::config::LspConfig::default(), "ignored");
    assert_eq!(
        c.language_ids(),
        ["rust"],
        "the rest of the section applies"
    );
    assert_eq!(
        m.ignored,
        vec![
            (
                "/Repo.yaml".to_string(),
                "code_intel.lsp.review".to_string()
            ),
            (
                "/Repo.yaml".to_string(),
                "code_intel.lsp.spaces".to_string()
            ),
        ]
    );
    assert!(m.unknown.is_empty() && m.errors.is_empty());
    assert_eq!(m.layer_of("code_intel.lsp.review"), None);
    // A dotted key or a scalar is ignored as well.
    let m = merge(&[layer(ConfigLayer::Repo, "code_intel.lsp.review: true\n")]);
    assert_eq!(m.config.code_intel.lsp.review, None);
    let m = merge(&[layer(ConfigLayer::Repo, "code_intel: {lsp: true}\n")]);
    assert_eq!(m.ignored[0].1, "code_intel.lsp");
    assert!(m.errors.is_empty(), "{:?}", m.errors);

    // Your own files set it.
    let m = merge(&[
        layer(ConfigLayer::Global, "code_intel: {lsp: {review: true}}\n"),
        layer(ConfigLayer::Repo, repo),
        layer(ConfigLayer::Local, "code_intel: {lsp: {spaces: false}}\n"),
    ]);
    assert_eq!(m.config.code_intel.lsp.review, Some(true));
    assert_eq!(m.config.code_intel.lsp.spaces, Some(false));
    assert_eq!(
        m.layer_of("code_intel.lsp.spaces"),
        Some(ConfigLayer::Local)
    );

    // The editor warns on the repo's file only.
    let warned = editor::validate_layer(repo, ConfigLayer::Repo);
    assert_eq!(
        warned.iter().map(|p| (p.line, p.level)).collect::<Vec<_>>(),
        [
            (Some(4), ConfigProblemLevel::Warning),
            (Some(5), ConfigProblemLevel::Warning)
        ],
        "{warned:?}"
    );
    assert!(warned[0].message.contains("ignored in the repo's file"));
    assert!(editor::validate_layer(repo, ConfigLayer::Local).is_empty());
}

#[tokio::test]
async fn a_repo_lsp_key_is_reported_once() {
    let mut e = code_env().await;
    e.repo_file(
        ".theodore/config.yaml",
        "code_intel: {lsp: {review: true}}\n",
    );
    e.create("").await;
    let toast = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(Ephemeral::Toast { text, .. }) = e.recv.ephemeral.recv().await
                && text.contains("can't set")
            {
                return text;
            }
        }
    })
    .await
    .expect("a toast");
    assert!(toast.contains("code_intel.lsp.review"), "{toast}");
}

#[tokio::test]
async fn lsp_toggles_sit_under_your_files_and_reopen_what_they_change() {
    let e = code_env().await;
    let status = e.core.code_intel_status(&e.project.id).await.unwrap();
    assert_eq!(
        (status.lsp.review.on, status.lsp.spaces.on),
        (false, true),
        "the defaults"
    );
    assert!(status.lsp.review.from_config.is_none());

    e.core
        .code_intel_set_lsp(theodore_contract::CodeIntelLspRequest {
            scope: theodore_contract::CodeLspScope::Review,
            on: true,
        })
        .await
        .unwrap();
    let status = e.core.code_intel_status(&e.project.id).await.unwrap();
    assert!(status.lsp.review.on);

    // The global file wins over the toggle and says so.
    e.global("code_intel: {lsp: {review: false, spaces: false}}");
    let status = e.core.code_intel_status(&e.project.id).await.unwrap();
    assert!(!status.lsp.review.on && !status.lsp.spaces.on);
    let global = e.f.data_dir.path().join("config.yaml");
    assert_eq!(
        status.lsp.spaces.from_config.as_deref(),
        Some(global.display().to_string().as_str())
    );

    // A space served once is reopened when its policy changes.
    std::fs::remove_file(&global).unwrap();
    let s = e.create("").await;
    let open = format!("open {} Trusted []", s.worktree);
    wait_until(|| e.f.code_intel.calls().contains(&open)).await;
    let scope = CodeScope::Space(s.id.clone());
    e.core.code_status(&scope).await.unwrap();
    let count = |e: &Env| {
        e.f.code_intel
            .calls()
            .iter()
            .filter(|c| **c == open)
            .count()
    };
    assert_eq!(count(&e), 1, "unchanged: not reopened");
    e.core
        .code_intel_set_lsp(theodore_contract::CodeIntelLspRequest {
            scope: theodore_contract::CodeLspScope::Spaces,
            on: false,
        })
        .await
        .unwrap();
    e.core.code_status(&scope).await.unwrap();
    assert_eq!(count(&e), 2, "{:?}", e.f.code_intel.calls());
    assert!(
        e.f.code_intel
            .calls()
            .contains(&format!("close {}", s.worktree))
    );
}

#[tokio::test]
async fn the_lsp_policy_reads_review_and_spaces_apart() {
    let e = code_env().await;
    let cfg = crate::config::CodeIntelConfig::default();
    assert_eq!(
        e.core.lsp_policy(&cfg).await,
        crate::LspPolicy {
            review: false,
            spaces: true
        }
    );
    let mut cfg = cfg;
    cfg.lsp.review = Some(true);
    cfg.lsp.spaces = Some(false);
    assert_eq!(
        e.core.lsp_policy(&cfg).await,
        crate::LspPolicy {
            review: true,
            spaces: false
        }
    );
}

// ── install ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn install_follows_the_policy_and_remembers_a_no() {
    let mut e = code_env().await;
    *e.f.code_intel.status.lock().unwrap() = vec![
        missing_server("rust", "rust-analyzer"),
        LanguageStatus {
            server: Some(ServerStatus {
                name: "ty".into(),
                install: InstallState::System {
                    path: "/usr/bin/ty".into(),
                    version: None,
                },
                state: ServerState::Ready,
            }),
            ..missing_server("python", "ty")
        },
    ];
    let root = Path::new("/tmp/wt");

    e.core
        .code_install_flow(root, InstallPolicy::Never, None)
        .await;
    assert!(e.f.code_intel.calls().is_empty(), "never: not even asked");

    e.core
        .code_install_flow(root, InstallPolicy::Always, None)
        .await;
    let installs: Vec<String> =
        e.f.code_intel
            .calls()
            .into_iter()
            .filter(|c| c.starts_with("install"))
            .collect();
    assert_eq!(installs, ["install rust"], "only the missing server");

    e.core
        .code_install_flow(root, InstallPolicy::Ask, Some("p1".into()))
        .await;
    let got = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(ev) = e.recv.ephemeral.recv().await
                && suggestion(&ev)
            {
                return ev;
            }
        }
    })
    .await
    .expect("a suggestion");
    assert_eq!(
        got,
        Ephemeral::CodeIntelSuggest {
            project: Some("p1".into()),
            language: "rust".into(),
            server: "rust-analyzer".into(),
        }
    );
    // Once per session.
    e.core
        .code_install_flow(root, InstallPolicy::Ask, None)
        .await;
    no_ephemeral(&mut e.recv, suggestion).await;
    assert_eq!(
        e.f.code_intel
            .calls()
            .iter()
            .filter(|c| c.starts_with("install"))
            .count(),
        1,
        "ask never installs by itself"
    );
}

#[tokio::test]
async fn a_declined_language_is_not_suggested_and_install_forgets_the_no() {
    let mut e = code_env().await;
    *e.f.code_intel.status.lock().unwrap() = vec![missing_server("cpp", "clangd")];
    e.core.code_intel_decline("cpp").await.unwrap();
    e.core
        .code_intel_install("go")
        .await
        .expect("install of another language");
    e.core
        .code_install_flow(Path::new("/tmp/wt"), InstallPolicy::Ask, None)
        .await;
    no_ephemeral(&mut e.recv, suggestion).await;

    // Settings → Languages shows it declined.
    let s = e.create("").await;
    wait_until(|| opened(&e, &format!("open {}", s.worktree))).await;
    let status = e.core.code_intel_status(&e.project.id).await.unwrap();
    assert!(status.enabled);
    assert_eq!(status.languages.len(), 1);
    assert_eq!(
        status.languages[0].server.as_ref().unwrap().install,
        theodore_contract::CodeInstall::Declined
    );

    e.core.code_intel_install("cpp").await.unwrap();
    assert!(e.f.code_intel.calls().contains(&"install cpp".to_string()));
    let status = e.core.code_intel_status(&e.project.id).await.unwrap();
    assert_eq!(
        status.languages[0].server.as_ref().unwrap().install,
        theodore_contract::CodeInstall::Missing
    );
}

#[tokio::test]
async fn without_a_port_settings_says_off() {
    let e = start_env_with(Fixtures::deps).await;
    let status = e.core.code_intel_status(&e.project.id).await.unwrap();
    assert!(!status.enabled && status.languages.is_empty());
    assert!(matches!(
        e.core.code_intel_install("rust").await,
        Err(AppError::Invalid(_))
    ));
}

// ── edits ───────────────────────────────────────────────────────────────────

const LIB: &str = "fn héllo() {\n    1\n}\n\nfn other() {}\n";

async fn edit_space(e: &Env) -> (theodore_contract::Space, PathBuf) {
    let s = e.create("").await;
    let root = PathBuf::from(&s.worktree);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), LIB).unwrap();
    *e.f.code_intel.symbols.lock().unwrap() = vec![
        symbol(
            "src/lib.rs",
            "héllo",
            range((1, 1), (3, 2)),
            range((1, 4), (1, 9)),
        ),
        symbol(
            "src/lib.rs",
            "other",
            range((5, 1), (5, 14)),
            range((5, 4), (5, 9)),
        ),
    ];
    e.f.code_intel.bodies.lock().unwrap().extend([
        ("héllo".to_string(), "fn héllo() {\n    1\n}".to_string()),
        ("other".to_string(), "fn other() {}".to_string()),
    ]);
    (s, root)
}

#[tokio::test]
async fn edits_are_refused_in_ask_mode_and_on_a_review() {
    let e = code_env().await;
    let (s, root) = edit_space(&e).await;
    set_mode(&e, &s.id, "ask").await;
    let scope = CodeScope::Space(s.id.clone());
    let err = e
        .core
        .code_replace_symbol(&scope, "src/lib.rs", "héllo", "fn héllo() {}")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("Ask mode"), "{err}");
    let pr = CodeScope::Pr(s.review_pr.clone().unwrap());
    let err = e
        .core
        .code_rename_symbol(&pr, "src/lib.rs", Position { line: 1, col: 4 }, "x")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("read-only"), "{err}");
    assert_eq!(
        std::fs::read_to_string(root.join("src/lib.rs")).unwrap(),
        LIB
    );
    // Reads still work in Ask mode.
    let (sym, body) = e
        .core
        .code_get_symbol(&scope, "src/lib.rs", "other")
        .await
        .unwrap();
    assert_eq!(
        (sym.name.as_str(), body.as_str()),
        ("other", "fn other() {}")
    );
}

#[tokio::test]
async fn replace_and_insert_edit_the_file_and_tell_the_port() {
    let e = code_env().await;
    let (s, root) = edit_space(&e).await;
    let scope = CodeScope::Space(s.id.clone());
    let done = e
        .core
        .code_replace_symbol(
            &scope,
            "src/lib.rs",
            "héllo",
            "fn héllo() -> u8 {\n    2\n}",
        )
        .await
        .unwrap();
    assert_eq!(done.files, ["src/lib.rs"]);
    assert_eq!(
        std::fs::read_to_string(root.join("src/lib.rs")).unwrap(),
        "fn héllo() -> u8 {\n    2\n}\n\nfn other() {}\n"
    );
    assert!(
        e.f.code_intel
            .calls()
            .contains(&format!("file_changed {} src/lib.rs", s.worktree))
    );

    // The index (the fake's ranges) is now stale for `héllo`: refused.
    let err = e
        .core
        .code_replace_symbol(&scope, "src/lib.rs", "héllo", "fn x() {}")
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("changed since it was indexed"),
        "{err}"
    );

    // Back to the text the fake's ranges describe.
    std::fs::write(root.join("src/lib.rs"), LIB).unwrap();
    e.core
        .code_insert_after_symbol(&scope, "src/lib.rs", "other", "fn third() {}\n")
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("src/lib.rs")).unwrap(),
        "fn héllo() {\n    1\n}\n\nfn other() {}\nfn third() {}\n"
    );
}

fn rename_edit(path: &str, line: u32, from: u32, to: u32) -> TextEdit {
    TextEdit {
        path: path.into(),
        range: range((line, from), (line, to)),
        new_text: "simple".into(),
    }
}

#[tokio::test]
async fn a_rename_edits_every_file_back_to_front_by_chars() {
    let e = code_env().await;
    let (s, root) = edit_space(&e).await;
    std::fs::write(root.join("a.rs"), "let naïve = 1; naïve + naïve\n").unwrap();
    std::fs::write(root.join("b.rs"), "naïve\n").unwrap();
    *e.f.code_intel.renames.lock().unwrap() = vec![
        rename_edit("a.rs", 1, 5, 10),
        rename_edit("b.rs", 1, 1, 6),
        rename_edit("a.rs", 1, 24, 29),
        rename_edit("a.rs", 1, 16, 21),
    ];
    let done = e
        .core
        .code_rename_symbol(
            &CodeScope::Space(s.id.clone()),
            "a.rs",
            Position { line: 1, col: 5 },
            "simple",
        )
        .await
        .unwrap();
    assert_eq!(done.files, ["a.rs", "b.rs"]);
    assert_eq!(done.edits, 4);
    assert_eq!(
        std::fs::read_to_string(root.join("a.rs")).unwrap(),
        "let simple = 1; simple + simple\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("b.rs")).unwrap(),
        "simple\n"
    );
}

#[tokio::test]
async fn an_edit_leaving_the_worktree_writes_nothing() {
    let e = code_env().await;
    let (s, root) = edit_space(&e).await;
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("x.rs"), "naïve\n").unwrap();
    std::os::unix::fs::symlink(outside.path().join("x.rs"), root.join("link.rs")).unwrap();
    std::fs::write(root.join("z.rs"), "naïve\n").unwrap();
    let scope = CodeScope::Space(s.id.clone());
    for bad in ["../x.rs", "link.rs", "/etc/hosts"] {
        *e.f.code_intel.renames.lock().unwrap() =
            vec![rename_edit("z.rs", 1, 1, 6), rename_edit(bad, 1, 1, 6)];
        let err = e
            .core
            .code_rename_symbol(&scope, "z.rs", Position { line: 1, col: 1 }, "simple")
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Invalid(_)), "{bad}: {err}");
    }
    assert_eq!(
        std::fs::read_to_string(root.join("z.rs")).unwrap(),
        "naïve\n"
    );
    assert_eq!(
        std::fs::read_to_string(outside.path().join("x.rs")).unwrap(),
        "naïve\n"
    );
    // The agent's own path is checked too.
    let err = e
        .core
        .code_get_symbol(&scope, "../src/lib.rs", "héllo")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("relative"), "{err}");
}

// ── find_symbol (the UI) ────────────────────────────────────────────────────

fn find(pr: &theodore_domain::PrId) -> FindSymbolRequest {
    FindSymbolRequest {
        pr: pr.clone(),
        name: "widget".into(),
        lang: None,
        from_path: Some("src/lib.rs".into()),
        line: None,
        col: None,
    }
}

#[tokio::test]
async fn find_symbol_asks_code_intel_first_and_greps_when_it_has_nothing() {
    let f = fixtures();
    let (core, _recv, pr) = open_pr_with_code(&f).await;
    *f.git.grep_hits.lock().unwrap() = Some(vec![GrepHit {
        path: "src/lib.rs".into(),
        line: 9,
        text: "  pub fn widget() {".into(),
    }]);
    *f.code_intel.symbols.lock().unwrap() = vec![symbol(
        "src/lib.rs",
        "widget",
        range((3, 1), (5, 2)),
        range((3, 8), (3, 14)),
    )];
    *f.code_intel.hits.lock().unwrap() = vec![CodeHit {
        location: Location {
            path: "src/main.rs".into(),
            range: range((12, 5), (12, 11)),
        },
        preview: "    widget();\n".into(),
        is_definition: false,
        source: Source::Lsp,
    }];

    let hits = core.find_symbol(find(&pr)).await.unwrap();
    assert_eq!(hits.len(), 2, "{hits:?}");
    assert_eq!(
        (hits[0].line, hits[0].kind, hits[0].source),
        (3, SymbolKind::Definition, Some(SymbolSource::Syntax))
    );
    assert_eq!(
        (
            hits[1].path.as_str(),
            hits[1].preview.as_str(),
            hits[1].source
        ),
        ("src/main.rs", "widget();", Some(SymbolSource::Lsp))
    );
    assert!(
        f.code_intel
            .calls()
            .contains(&"references src/lib.rs:3:8".to_string())
    );

    // A click with a position asks at it.
    let at = FindSymbolRequest {
        line: Some(12),
        col: Some(5),
        ..find(&pr)
    };
    core.find_symbol(at).await.unwrap();
    assert!(
        f.code_intel
            .calls()
            .contains(&"definition src/lib.rs:12:5".to_string())
    );

    // Nothing from the port, then a failing port: the grep answers.
    f.code_intel.symbols.lock().unwrap().clear();
    let grep = core.find_symbol(find(&pr)).await.unwrap();
    assert_eq!(grep.len(), 1);
    assert_eq!(
        (grep[0].line, grep[0].source),
        (9, Some(SymbolSource::Text))
    );
    *f.code_intel.fail.lock().unwrap() = Some(CodeIntelError::Other("boom".into()));
    let grep = core.find_symbol(find(&pr)).await.unwrap();
    assert_eq!(grep[0].line, 9);
}
