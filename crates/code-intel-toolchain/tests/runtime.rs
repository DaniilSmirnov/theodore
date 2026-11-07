//! `requires:` for real, with fake runtimes on a private `$PATH`: missing,
//! too old, a stub that fails, fall-through to the next server, nothing
//! downloaded for a server that can't run, probes outside the worktree in a
//! review run, and what reaches the launch.

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use pretty_assertions::assert_eq;
use theodore_code_intel_toolchain::{JavaLookup, Manifest, ServerSpec, Toolchain};
use theodore_ports::code_intel::{CodeIntelError, InstallState, LanguageId, TrustMode};

const PLATFORM: &str = "test-plat";
const SHA: &str = "8fb90cb58f5910d8d8b24ac1a93cca49f053365cbf0eb3c682144ad0a0027dcf";

fn script(path: &Path, body: &str) -> PathBuf {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.to_path_buf()
}

struct Env {
    _tmp: tempfile::TempDir,
    base: PathBuf,
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
            base,
            _tmp: tmp,
        };
        std::fs::create_dir_all(&env.bin).unwrap();
        std::fs::create_dir_all(&env.worktree).unwrap();
        env
    }

    fn toolchain(&self, yaml: &str) -> Toolchain {
        self.toolchain_java(yaml, JavaLookup::none())
    }

    fn toolchain_java(&self, yaml: &str, java: JavaLookup) -> Toolchain {
        let (manifest, warnings) = Manifest::parse(yaml, r#"{"schema":1,"grammars":[]}"#).unwrap();
        assert_eq!(warnings, Vec::<String>::new());
        Toolchain::builder(&self.root)
            .manifest(manifest)
            .platform(PLATFORM)
            .search_path(self.bin.clone().into_os_string())
            .java_lookup(java)
            .build()
    }

    fn node(&self, version: &str) -> PathBuf {
        script(
            &self.bin.join("node"),
            &format!(r#"[ "$1" = --version ] && echo v{version} && exit 0; exit 1"#),
        )
    }

    /// A download of `name`, on disk as `install_server` would leave it.
    fn downloaded(&self, name: &str, version: &str, bin: &str) {
        script(
            &self.root.join("servers").join(name).join(version).join(bin),
            "exit 0",
        );
    }
}

fn spec(tc: &Toolchain, name: &str) -> ServerSpec {
    tc.manifest().server(name).unwrap().clone()
}

fn x() -> LanguageId {
    LanguageId::new("x")
}

/// `a`: downloadable, needs Node 20. `b`: detect-only, needs nothing.
fn a_then_b() -> String {
    format!(
        r#"
servers:
  - name: a
    version: "1.0"
    download:
      {PLATFORM}: {{ url: "https://example.invalid/a.gz", sha256: "{SHA}", format: gz, bin: a.js }}
    launch: ["{{runtime}}", "{{bin}}", "--stdio"]
    requires: {{ runtime: node, min: 20 }}
  - name: b
    detect: [["b-ls"]]
    launch: ["{{bin}}"]
    hint: "install b"
languages:
  - id: x
    globs: ["*.x"]
    servers: [a, b]
"#
    )
}

#[tokio::test]
async fn runtime_missing_a_server_is_skipped_and_nothing_downloads() {
    let env = Env::new();
    let tc = env.toolchain(&a_then_b());
    let got = tc
        .server_for(&x(), None, &env.worktree, TrustMode::Review)
        .await
        .unwrap();
    assert_eq!(got.0.name, "a");
    assert_eq!(
        got.1,
        InstallState::NeedsRuntime {
            runtime: "node".into(),
            min: "20".into(),
            found: None,
        }
    );

    // On the machine or not, a download without its runtime can't run.
    let err = tc.install_server(&spec(&tc, "a")).await.unwrap_err();
    assert!(
        matches!(&err, CodeIntelError::Unsupported(m) if m.contains("needs node 20")),
        "{err}"
    );
    assert!(!env.root.join("tmp").exists(), "no download started");

    // Asked about for a yes: the same answer, and it is no download.
    let (_, state) = tc.server_to_install(&x(), None).await.unwrap();
    assert!(
        matches!(state, InstallState::NeedsRuntime { .. }),
        "{state:?}"
    );

    // Later, a server that needs nothing is found: that one.
    script(&env.bin.join("b-ls"), "exit 0");
    let (s, state) = tc
        .server_for(&x(), None, &env.worktree, TrustMode::Review)
        .await
        .unwrap();
    assert_eq!(s.name, "b");
    assert!(matches!(state, InstallState::System { .. }), "{state:?}");
}

#[tokio::test]
async fn runtime_too_old_says_what_was_found() {
    let env = Env::new();
    env.node("18.19.1");
    let tc = env.toolchain(&a_then_b());
    let (s, state) = tc
        .server_for(&x(), None, &env.worktree, TrustMode::Review)
        .await
        .unwrap();
    assert_eq!(s.name, "a");
    assert_eq!(
        state,
        InstallState::NeedsRuntime {
            runtime: "node".into(),
            min: "20".into(),
            found: Some("18.19.1".into()),
        }
    );
    assert!(tc.install_server(&spec(&tc, "a")).await.is_err());
}

#[tokio::test]
async fn runtime_ok_the_download_is_offered_and_reaches_the_launch() {
    let env = Env::new();
    let node = env.node("22.14.0");
    let tc = env.toolchain(&a_then_b());
    let (s, state) = tc
        .server_for(&x(), None, &env.worktree, TrustMode::Review)
        .await
        .unwrap();
    assert_eq!((s.name.as_str(), state), ("a", InstallState::Missing));

    env.downloaded("a", "1.0", "a.js");
    let (s, state) = tc
        .server_for(&x(), None, &env.worktree, TrustMode::Review)
        .await
        .unwrap();
    assert_eq!(
        state,
        InstallState::Installed {
            version: "1.0".into()
        }
    );
    let r = tc
        .resolve_server(&s, &env.worktree, TrustMode::Review)
        .await
        .unwrap();
    let want: Vec<OsString> = vec![
        node.clone().into_os_string(),
        r.bin.clone().into_os_string(),
        "--stdio".into(),
    ];
    assert_eq!(r.argv, want);
    let cfg = tc.launch_config(&r, &s, TrustMode::Review);
    assert_eq!(
        cfg.env,
        vec![(OsString::from("PATH"), env.bin.clone().into_os_string())]
    );
}

#[tokio::test]
async fn a_found_binary_needs_its_runtime_too() {
    let yaml = r#"
servers:
  - name: p
    detect: [["p-ls"]]
    launch: ["{bin}", "--stdio"]
    requires: { runtime: node, min: 14 }
languages:
  - id: x
    globs: ["*.x"]
    servers: [p]
"#;
    let env = Env::new();
    script(&env.bin.join("p-ls"), "exit 0");
    let tc = env.toolchain(yaml);
    let p = spec(&tc, "p");
    assert!(
        tc.resolve_server(&p, &env.worktree, TrustMode::Trusted)
            .await
            .is_none(),
        "found, but it can't run"
    );
    let (_, state) = tc
        .server_for(&x(), None, &env.worktree, TrustMode::Trusted)
        .await
        .unwrap();
    assert!(
        matches!(state, InstallState::NeedsRuntime { .. }),
        "{state:?}"
    );

    // The script's `env node` finds the runtime that passed the check.
    env.node("22.14.0");
    let r = tc
        .resolve_server(&p, &env.worktree, TrustMode::Trusted)
        .await
        .unwrap();
    let cfg = tc.launch_config(&r, &p, TrustMode::Trusted);
    assert_eq!(cfg.argv[0], env.bin.join("p-ls").into_os_string());
    assert_eq!(cfg.env[0].0, OsString::from("PATH"));
    assert_eq!(cfg.env[0].1, env.bin.clone().into_os_string());
}

/// `prefer` is tried first, and it is skipped like any other when its
/// runtime is missing.
#[tokio::test]
async fn fall_through_order_with_a_preference() {
    let env = Env::new();
    script(&env.bin.join("b-ls"), "exit 0");
    let tc = env.toolchain(&a_then_b());
    env.node("22.0.0");
    env.downloaded("a", "1.0", "a.js");
    let name = |want: Option<&'static str>| {
        let tc = tc.clone();
        let wt = env.worktree.clone();
        async move {
            tc.server_for(&x(), want, &wt, TrustMode::Review)
                .await
                .unwrap()
                .0
                .name
        }
    };
    assert_eq!(name(None).await, "a");
    assert_eq!(name(Some("b")).await, "b");
    assert_eq!(name(Some("nope")).await, "a");
    std::fs::remove_file(env.bin.join("node")).unwrap();
    assert_eq!(name(Some("a")).await, "b", "a's runtime is gone");
}

/// Nothing can serve, for two reasons: the runtime is the one to name.
#[tokio::test]
async fn needs_runtime_outranks_a_detect_only_server() {
    let yaml = r#"
servers:
  - name: detect-only
    detect: [["d-ls"]]
    launch: ["{bin}"]
    hint: "install d"
  - name: needs-php
    detect: [["p-ls"]]
    launch: ["{bin}"]
    requires: { runtime: php, min: "8.2" }
languages:
  - id: x
    globs: ["*.x"]
    servers: [detect-only, needs-php]
  - id: y
    globs: ["*.y"]
    servers: [detect-only]
"#;
    let env = Env::new();
    let tc = env.toolchain(yaml);
    let (s, state) = tc
        .server_for(&x(), None, &env.worktree, TrustMode::Trusted)
        .await
        .unwrap();
    assert_eq!(s.name, "needs-php");
    assert_eq!(
        state,
        InstallState::NeedsRuntime {
            runtime: "php".into(),
            min: "8.2".into(),
            found: None
        }
    );
    let (s, state) = tc
        .server_for(
            &LanguageId::new("y"),
            None,
            &env.worktree,
            TrustMode::Trusted,
        )
        .await
        .unwrap();
    assert_eq!(state, InstallState::Unavailable);
    assert_eq!(s.hint.as_deref(), Some("install d"));

    // 8.1 is older than 8.2, however the major reads.
    script(
        &env.bin.join("php"),
        r#"[ "$1" = --version ] && echo "PHP 8.1.30 (cli)" && exit 0; exit 1"#,
    );
    let (_, state) = tc
        .server_for(&x(), None, &env.worktree, TrustMode::Trusted)
        .await
        .unwrap();
    assert_eq!(
        state,
        InstallState::NeedsRuntime {
            runtime: "php".into(),
            min: "8.2".into(),
            found: Some("8.1.30".into())
        }
    );
    script(
        &env.bin.join("php"),
        r#"[ "$1" = --version ] && echo "PHP 8.4.3 (cli)" && exit 0; exit 1"#,
    );
    let (_, state) = tc
        .server_for(&x(), None, &env.worktree, TrustMode::Trusted)
        .await
        .unwrap();
    assert_eq!(state, InstallState::Unavailable, "php is fine now");
}

/// A PR's `.tool-versions` or `.ruby-version` must not steer a review probe.
#[tokio::test]
async fn review_probes_run_in_the_neutral_dir_and_trusted_ones_in_the_worktree() {
    let env = Env::new();
    let seen = env.base.join("seen");
    script(
        &env.bin.join("ruby"),
        &format!(
            r#"[ "$1" = --version ] && echo "$PWD" >> '{}' && echo "ruby 3.4.1 (x)" && exit 0; exit 1"#,
            seen.display()
        ),
    );
    let yaml = r#"
servers:
  - name: r
    detect: [["r-ls"]]
    launch: ["{bin}"]
    requires: { runtime: ruby, min: 3 }
languages:
  - id: x
    globs: ["*.x"]
    servers: [r]
"#;
    let tc = env.toolchain(yaml);
    tc.server_for(&x(), None, &env.worktree, TrustMode::Review)
        .await
        .unwrap();
    let neutral = std::env::temp_dir().canonicalize().unwrap();
    assert_eq!(
        std::fs::read_to_string(&seen).unwrap().trim(),
        neutral.display().to_string()
    );

    std::fs::remove_file(&seen).unwrap();
    tc.server_for(&x(), None, &env.worktree, TrustMode::Trusted)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&seen).unwrap().trim(),
        env.worktree.display().to_string()
    );
}

/// A runtime on `PATH` inside the worktree is a PR's, in a review run.
#[tokio::test]
async fn a_runtime_inside_the_worktree_counts_only_when_trusted() {
    let env = Env::new();
    let tools = env.worktree.join("tools");
    script(
        &tools.join("node"),
        r#"[ "$1" = --version ] && echo v22.0.0 && exit 0; exit 1"#,
    );
    let (manifest, _) = Manifest::parse(&a_then_b(), r#"{"schema":1,"grammars":[]}"#).unwrap();
    let tc = Toolchain::builder(&env.root)
        .manifest(manifest)
        .platform(PLATFORM)
        .search_path(tools.clone().into_os_string())
        .java_lookup(JavaLookup::none())
        .build();
    let (_, state) = tc
        .server_for(&x(), None, &env.worktree, TrustMode::Review)
        .await
        .unwrap();
    assert!(
        matches!(state, InstallState::NeedsRuntime { .. }),
        "{state:?}"
    );
    let (_, state) = tc
        .server_for(&x(), None, &env.worktree, TrustMode::Trusted)
        .await
        .unwrap();
    assert_eq!(state, InstallState::Missing);
}

const JAVA_YAML: &str = r#"
servers:
  - name: j
    detect: [["j-ls"]]
    launch: ["{runtime}", "-jar", "{bin}"]
    requires: { runtime: java, min: 21 }
languages:
  - id: x
    globs: ["*.x"]
    servers: [j]
"#;

/// `java` that prints like the real one, to stderr, with `java.home`.
fn fake_java(home: &Path, version: &str) -> PathBuf {
    script(
        &home.join("bin/java"),
        &format!(
            r#"echo "    java.home = {}" >&2; echo 'openjdk version "{version}" 2025-01-01' >&2; exit 0"#,
            home.display()
        ),
    )
}

#[tokio::test]
async fn java_is_looked_for_in_order_and_a_stub_does_not_count() {
    let env = Env::new();
    script(&env.bin.join("j-ls"), "exit 0");
    // macOS's stub: on PATH, runs, fails.
    script(
        &env.bin.join("java"),
        "echo 'Unable to locate a Java Runtime.' >&2; exit 1",
    );
    let tc = env.toolchain(JAVA_YAML);
    let j = spec(&tc, "j");
    let (_, state) = tc
        .server_for(&x(), None, &env.worktree, TrustMode::Review)
        .await
        .unwrap();
    assert_eq!(
        state,
        InstallState::NeedsRuntime {
            runtime: "java".into(),
            min: "21".into(),
            found: None
        }
    );

    // JAVA_HOME too old, java_home has nothing, Homebrew's keg has one.
    let old_home = env.base.join("jdk17");
    fake_java(&old_home, "17.0.2");
    let kegs = env.base.join("opt");
    let keg_home = kegs.join("openjdk@21");
    let keg_java = fake_java(&keg_home, "21.0.5");
    let lookup = JavaLookup {
        java_home_env: Some(old_home.clone()),
        java_home_tool: Some(script(&env.base.join("java_home"), "exit 1")),
        keg_roots: vec![kegs.clone()],
    };
    let tc = env.toolchain_java(JAVA_YAML, lookup.clone());
    let r = tc
        .resolve_server(&j, &env.worktree, TrustMode::Review)
        .await
        .unwrap();
    assert_eq!(r.argv[0], keg_java.clone().into_os_string());
    let cfg = tc.launch_config(&r, &j, TrustMode::Review);
    assert!(
        cfg.env.contains(&(
            OsString::from("JAVA_HOME"),
            keg_home.clone().into_os_string()
        )),
        "the server gets the JDK that passed: {:?}",
        cfg.env
    );
    assert_eq!(
        cfg.env.iter().find(|(k, _)| k == "PATH").unwrap().1,
        std::env::join_paths([keg_home.join("bin"), env.bin.clone()]).unwrap()
    );

    // No keg either: the old JAVA_HOME is what was found.
    let tc = env.toolchain_java(
        JAVA_YAML,
        JavaLookup {
            keg_roots: vec![],
            ..lookup.clone()
        },
    );
    let (_, state) = tc
        .server_for(&x(), None, &env.worktree, TrustMode::Review)
        .await
        .unwrap();
    assert_eq!(
        state,
        InstallState::NeedsRuntime {
            runtime: "java".into(),
            min: "21".into(),
            found: Some("17.0.2".into())
        }
    );

    // java_home answers for `-v 21+`: ahead of PATH and the keg.
    let tool_home = env.base.join("jdk25");
    let tool_java = fake_java(&tool_home, "25.0.1");
    let tool = script(
        &env.base.join("java_home2"),
        &format!(
            r#"[ "$1" = -v ] && [ "$2" = "21+" ] && echo '{}' && exit 0; exit 1"#,
            tool_home.display()
        ),
    );
    let tc = env.toolchain_java(
        JAVA_YAML,
        JavaLookup {
            java_home_tool: Some(tool),
            ..lookup
        },
    );
    let r = tc
        .resolve_server(&j, &env.worktree, TrustMode::Review)
        .await
        .unwrap();
    assert_eq!(r.argv[0], tool_java.into_os_string());

    // A new enough JAVA_HOME goes first.
    let tc = env.toolchain_java(
        JAVA_YAML,
        JavaLookup {
            java_home_env: Some(keg_home),
            java_home_tool: None,
            keg_roots: vec![],
        },
    );
    let r = tc
        .resolve_server(&j, &env.worktree, TrustMode::Review)
        .await
        .unwrap();
    assert_eq!(r.argv[0], keg_java.into_os_string());
}

#[tokio::test]
async fn dotnet_reads_runtimes_and_an_sdk_is_separate() {
    let yaml = r#"
servers:
  - name: rt
    detect: [["rt-ls"]]
    launch: ["{bin}"]
    requires: { runtime: dotnet, min: 10 }
  - name: sdk
    detect: [["sdk-ls"]]
    launch: ["{bin}"]
    requires: { runtime: dotnet-sdk, min: 10 }
languages:
  - id: rt
    globs: ["*.rt"]
    servers: [rt]
  - id: sdk
    globs: ["*.sdk"]
    servers: [sdk]
"#;
    let env = Env::new();
    script(&env.bin.join("rt-ls"), "exit 0");
    script(&env.bin.join("sdk-ls"), "exit 0");
    let dotnet = |runtimes: &str, sdks: &str| {
        script(
            &env.bin.join("dotnet"),
            &format!(
                r#"case "$1" in
--list-runtimes) printf '%s\n' {runtimes} ;;
--list-sdks) printf '%s\n' {sdks} ;;
*) exit 1 ;;
esac"#
            ),
        )
    };
    let aspnet = "'Microsoft.AspNetCore.App 11.0.0 [/d/shared/Microsoft.AspNetCore.App]'";
    let core8 = "'Microsoft.NETCore.App 8.0.5 [/d/shared/Microsoft.NETCore.App]'";
    let core10 = "'Microsoft.NETCore.App 10.0.3 [/d/shared/Microsoft.NETCore.App]'";
    let tc = env.toolchain(yaml);
    let state = |lang: &'static str| {
        let tc = tc.clone();
        let wt = env.worktree.clone();
        async move {
            tc.server_for(&LanguageId::new(lang), None, &wt, TrustMode::Review)
                .await
                .unwrap()
                .1
        }
    };

    // Newest runtime 8, and ASP.NET's 11 doesn't count.
    dotnet(&format!("{aspnet} {core8}"), "'8.0.100 [/d/sdk]'");
    assert_eq!(
        state("rt").await,
        InstallState::NeedsRuntime {
            runtime: "dotnet".into(),
            min: "10".into(),
            found: Some("8.0.5".into())
        }
    );

    // The runtime without an SDK serves one and not the other.
    dotnet(&format!("{core8} {core10}"), "''");
    assert!(matches!(state("rt").await, InstallState::System { .. }));
    assert_eq!(
        state("sdk").await,
        InstallState::NeedsRuntime {
            runtime: "dotnet-sdk".into(),
            min: "10".into(),
            found: Some("10.0.3 without an SDK".into())
        }
    );

    dotnet(core10, "'10.0.103 [/d/sdk]'");
    assert!(matches!(state("sdk").await, InstallState::System { .. }));
    let s = spec(&tc, "sdk");
    let r = tc
        .resolve_server(&s, &env.worktree, TrustMode::Review)
        .await
        .unwrap();
    let cfg = tc.launch_config(&r, &s, TrustMode::Review);
    assert!(
        cfg.env
            .contains(&(OsString::from("DOTNET_ROOT"), OsString::from("/d"))),
        "{:?}",
        cfg.env
    );
}

#[tokio::test]
async fn trusted_only_serves_spaces_and_can_still_be_installed_for_them() {
    let yaml = format!(
        r#"
servers:
  - name: t
    version: "1.0"
    download:
      {PLATFORM}: {{ url: "https://example.invalid/t.gz", sha256: "{SHA}", format: gz, bin: t }}
    launch: ["{{bin}}"]
    trusted_only: true
languages:
  - id: x
    globs: ["*.x"]
    servers: [t]
"#
    );
    let env = Env::new();
    let tc = env.toolchain(&yaml);
    let review = tc
        .server_for(&x(), None, &env.worktree, TrustMode::Review)
        .await
        .unwrap();
    assert_eq!(review.1, InstallState::TrustedOnly);
    let trusted = tc
        .server_for(&x(), None, &env.worktree, TrustMode::Trusted)
        .await
        .unwrap();
    assert_eq!(trusted.1, InstallState::Missing);
    let install = tc.server_to_install(&x(), None).await.unwrap();
    assert_eq!(install.1, InstallState::Missing);

    // On disk it stays out of reviews too.
    env.downloaded("t", "1.0", "t");
    let review = tc
        .server_for(&x(), None, &env.worktree, TrustMode::Review)
        .await
        .unwrap();
    assert_eq!(review.1, InstallState::TrustedOnly);
}

const DATA_YAML: &str = r#"
servers:
  - name: d
    version: "1.0"
    download:
      test-plat: { url: "https://example.invalid/d.gz", sha256: "8fb90cb58f5910d8d8b24ac1a93cca49f053365cbf0eb3c682144ad0a0027dcf", format: gz, bin: d/launcher.jar }
    launch: ["{bin}", "-cfg={dir}/config", "-data", "{data}/ws"]
languages:
  - id: x
    globs: ["*.x"]
    servers: [d]
"#;

#[tokio::test]
async fn data_is_per_worktree_and_trust_outside_the_worktree_and_remove_cleans_it() {
    let env = Env::new();
    env.downloaded("d", "1.0", "d/launcher.jar");
    let tc = env.toolchain(DATA_YAML);
    let d = spec(&tc, "d");
    let other = env.base.join("wt2");
    std::fs::create_dir_all(&other).unwrap();

    let trusted = tc
        .resolve_server(&d, &env.worktree, TrustMode::Trusted)
        .await
        .unwrap();
    let review = tc
        .resolve_server(&d, &env.worktree, TrustMode::Review)
        .await
        .unwrap();
    let second = tc
        .resolve_server(&d, &other, TrustMode::Trusted)
        .await
        .unwrap();
    let data = trusted.data_dir.clone().unwrap();
    assert!(data.starts_with(env.root.join("server-data/d")), "{data:?}");
    assert!(!data.starts_with(&env.worktree));
    assert_ne!(
        Some(&data),
        review.data_dir.as_ref(),
        "a review reads no trusted state"
    );
    assert_ne!(Some(&data), second.data_dir.as_ref());

    let dir = env.root.join("servers/d/1.0");
    assert_eq!(
        trusted.argv[1],
        OsString::from(format!("-cfg={}/config", dir.display()))
    );
    assert_eq!(
        trusted.argv[3],
        OsString::from(format!("{}/ws", data.display()))
    );

    // Resolving creates nothing; launching does.
    assert!(!data.exists());
    let cfg = tc.launch_config(&trusted, &d, TrustMode::Trusted);
    assert!(data.is_dir());
    assert_eq!(cfg.argv, trusted.argv);

    // Remove takes the versions and every worktree's data.
    tc.launch_config(&review, &d, TrustMode::Review);
    assert!(tc.remove_server("d").unwrap());
    assert!(!env.root.join("servers/d").exists());
    assert!(!env.root.join("server-data/d").exists());
    assert!(!tc.remove_server("d").unwrap(), "nothing left");
}

#[tokio::test]
async fn a_launch_drops_data_no_launch_touched_for_two_weeks() {
    let env = Env::new();
    env.downloaded("d", "1.0", "d/launcher.jar");
    let tc = env.toolchain(DATA_YAML);
    let d = spec(&tc, "d");
    let r = tc
        .resolve_server(&d, &env.worktree, TrustMode::Trusted)
        .await
        .unwrap();
    let root = env.root.join("server-data/d");

    let mark = |name: &str, age: Duration| {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let f = std::fs::File::create(dir.join(".last-used")).unwrap();
        f.set_modified(SystemTime::now() - age).unwrap();
        dir
    };
    let stale = mark("stale", Duration::from_secs(15 * 24 * 3600));
    let fresh = mark("fresh", Duration::from_secs(3600));

    tc.launch_config(&r, &d, TrustMode::Trusted);
    assert!(!stale.exists(), "two weeks unused");
    assert!(fresh.exists());
    assert!(r.data_dir.unwrap().is_dir());
}

/// A server that doesn't use `{data}` has no such directory.
#[tokio::test]
async fn no_data_placeholder_no_directory() {
    let env = Env::new();
    env.node("22.0.0");
    env.downloaded("a", "1.0", "a.js");
    let tc = env.toolchain(&a_then_b());
    let a = spec(&tc, "a");
    let r = tc
        .resolve_server(&a, &env.worktree, TrustMode::Trusted)
        .await
        .unwrap();
    assert_eq!(r.data_dir, None);
    tc.launch_config(&r, &a, TrustMode::Trusted);
    assert!(!env.root.join("server-data").exists());
}
