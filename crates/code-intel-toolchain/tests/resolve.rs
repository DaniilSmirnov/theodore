//! Finding servers on the machine: PATH, rustup, xcrun, node_modules/.bin,
//! then the downloaded copy — with fake tools on a private `$PATH`.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use theodore_code_intel_toolchain::{Manifest, ServerOrigin, Toolchain};
use theodore_ports::code_intel::{InstallState, LanguageId, TrustMode};

const PLATFORM: &str = "test-plat";
const SHA: &str = "8fb90cb58f5910d8d8b24ac1a93cca49f053365cbf0eb3c682144ad0a0027dcf";

fn script(path: &Path, body: &str) -> PathBuf {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.to_path_buf()
}

/// Language `x` with servers `a` (detect `a-ls --version`) and `b`
/// (downloadable, detect `b-ls`).
fn manifest() -> Manifest {
    let yaml = format!(
        r#"
servers:
  - name: a
    detect: [["a-ls", "--version"]]
    launch: ["{{bin}}"]
  - name: b
    version: "2.0"
    detect: [["b-ls"]]
    download:
      {PLATFORM}: {{ url: "https://example.com/b.gz", sha256: "{SHA}", format: gz, bin: b-ls }}
    launch: ["{{bin}}", "--stdio"]
languages:
  - id: x
    globs: ["*.x"]
    servers: [a, b]
"#
    );
    Manifest::parse(&yaml, r#"{"schema":1,"grammars":[]}"#)
        .unwrap()
        .0
}

struct Env {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    bin: PathBuf,
    worktree: PathBuf,
}

impl Env {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let env = Self {
            root: base.join("code-intel"),
            bin: base.join("bin"),
            worktree: base.join("wt"),
            _tmp: tmp,
        };
        std::fs::create_dir_all(&env.bin).unwrap();
        std::fs::create_dir_all(&env.worktree).unwrap();
        env
    }

    fn toolchain(&self, extra_path: Option<&Path>) -> Toolchain {
        let mut path = self.bin.clone().into_os_string();
        if let Some(p) = extra_path {
            path.push(":");
            path.push(p);
        }
        Toolchain::builder(&self.root)
            .manifest(manifest())
            .platform(PLATFORM)
            .search_path(path)
            .build()
    }

    fn spec(&self, tc: &Toolchain, name: &str) -> theodore_code_intel_toolchain::ServerSpec {
        tc.manifest().server(name).unwrap().clone()
    }
}

#[tokio::test]
async fn path_hit_reports_its_version() {
    let env = Env::new();
    let ls = script(&env.bin.join("a-ls"), "echo 'a-ls 1.2.3 (abc)'");
    let tc = env.toolchain(None);
    let r = tc
        .resolve_server(&env.spec(&tc, "a"), &env.worktree, TrustMode::Review)
        .await
        .unwrap();
    assert_eq!(
        r.origin,
        ServerOrigin::System {
            path: ls.clone(),
            version: Some("a-ls 1.2.3 (abc)".into())
        }
    );
    assert_eq!(r.argv, vec![ls.into_os_string()]);
}

#[tokio::test]
async fn failing_detect_is_skipped_for_rustup_then_xcrun() {
    let env = Env::new();
    // A rustup proxy without the component: on PATH, exits 1.
    script(
        &env.bin.join("a-ls"),
        "echo 'error: unknown binary' >&2; exit 1",
    );
    let toolchain_bin = script(
        &env.bin.parent().unwrap().join("toolchain/a-ls"),
        "echo 'a-ls 9.9'",
    );
    let toolchain_bin = toolchain_bin.canonicalize().unwrap();
    script(
        &env.bin.join("rustup"),
        &format!(
            r#"[ "$1" = which ] && [ "$2" = a-ls ] && [ "$RUSTUP_AUTO_INSTALL" = 0 ] && echo {} && exit 0; exit 1"#,
            toolchain_bin.display()
        ),
    );
    let tc = env.toolchain(None);
    let spec = env.spec(&tc, "a");
    let r = tc
        .resolve_server(&spec, &env.worktree, TrustMode::Review)
        .await
        .unwrap();
    assert_eq!(r.bin, toolchain_bin);

    // No rustup answer: xcrun --find.
    std::fs::remove_file(env.bin.join("rustup")).unwrap();
    let xcode_bin = script(
        &env.bin.parent().unwrap().join("xcode/a-ls"),
        "echo 'a-ls from xcode'",
    )
    .canonicalize()
    .unwrap();
    script(
        &env.bin.join("xcrun"),
        &format!(
            r#"[ "$1" = --find ] && [ "$2" = a-ls ] && echo {} && exit 0; exit 1"#,
            xcode_bin.display()
        ),
    );
    let r = tc
        .resolve_server(&spec, &env.worktree, TrustMode::Review)
        .await
        .unwrap();
    assert_eq!(r.bin, xcode_bin);
}

#[tokio::test]
async fn worktree_binaries_only_when_trusted() {
    let env = Env::new();
    let nm = script(
        &env.worktree.join("node_modules/.bin/a-ls"),
        "echo 'a-ls 0.1'",
    );
    let tc = env.toolchain(None);
    let spec = env.spec(&tc, "a");
    assert!(
        tc.resolve_server(&spec, &env.worktree, TrustMode::Review)
            .await
            .is_none()
    );
    let r = tc
        .resolve_server(&spec, &env.worktree, TrustMode::Trusted)
        .await
        .unwrap();
    assert_eq!(r.bin, nm);

    // A PATH entry inside the worktree is refused in review too.
    let wt_bin = env.worktree.join("tools");
    script(&wt_bin.join("a-ls"), "echo 'a-ls evil'");
    let tc = env.toolchain(Some(&wt_bin));
    assert!(
        tc.resolve_server(&spec, &env.worktree, TrustMode::Review)
            .await
            .is_none()
    );
    let r = tc
        .resolve_server(&spec, &env.worktree, TrustMode::Trusted)
        .await
        .unwrap();
    assert_eq!(r.bin, wt_bin.join("a-ls"));
}

#[tokio::test]
async fn server_for_order_and_states() {
    let env = Env::new();
    let tc = env.toolchain(None);
    let x = LanguageId::new("x");

    // Nothing anywhere: the first downloadable one, missing.
    let (spec, state) = tc
        .server_for(&x, None, &env.worktree, TrustMode::Review)
        .await
        .unwrap();
    assert_eq!((spec.name.as_str(), state), ("b", InstallState::Missing));

    // A downloaded copy of b counts as installed.
    script(&env.root.join("servers/b/2.0/b-ls"), "exit 0");
    let (spec, state) = tc
        .server_for(&x, None, &env.worktree, TrustMode::Review)
        .await
        .unwrap();
    assert_eq!(spec.name, "b");
    assert_eq!(
        state,
        InstallState::Installed {
            version: "2.0".into()
        }
    );

    // a on PATH wins (manifest order) over b's download…
    let a = script(&env.bin.join("a-ls"), "echo 'a 1'");
    let (spec, state) = tc
        .server_for(&x, None, &env.worktree, TrustMode::Review)
        .await
        .unwrap();
    assert_eq!(spec.name, "a");
    assert_eq!(
        state,
        InstallState::System {
            path: a,
            version: Some("a 1".into())
        }
    );

    // …unless the user prefers b.
    let (spec, _) = tc
        .server_for(&x, Some("b"), &env.worktree, TrustMode::Review)
        .await
        .unwrap();
    assert_eq!(spec.name, "b");

    // b found on PATH (presence-only detect) beats its download.
    let b = script(&env.bin.join("b-ls"), "exit 1");
    let r = tc
        .resolve_server(&env.spec(&tc, "b"), &env.worktree, TrustMode::Review)
        .await
        .unwrap();
    assert_eq!(
        r.origin,
        ServerOrigin::System {
            path: b,
            version: None
        }
    );

    assert!(
        tc.server_for(
            &LanguageId::new("nope"),
            None,
            &env.worktree,
            TrustMode::Review
        )
        .await
        .is_none()
    );
}

#[tokio::test]
async fn slow_detect_counts_as_found() {
    let env = Env::new();
    let ls = script(&env.bin.join("a-ls"), "exec /bin/sleep 5");
    let tc = Toolchain::builder(&env.root)
        .manifest(manifest())
        .platform(PLATFORM)
        .search_path(env.bin.clone().into_os_string())
        .probe_timeout(std::time::Duration::from_millis(200))
        .build();
    let r = tc
        .resolve_server(&env.spec(&tc, "a"), &env.worktree, TrustMode::Review)
        .await
        .unwrap();
    assert_eq!(
        r.origin,
        ServerOrigin::System {
            path: ls,
            version: None
        }
    );
}
