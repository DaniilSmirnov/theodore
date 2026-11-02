//! Installs every server newly pinned in `manifest/languages.yaml`
//! (zls excluded: detect-only, no download yet — see its comment in the
//! manifest) through the real `Toolchain::install_server`, against their
//! real upstream hosts, then runs each with `--version`.
//!
//! `gopls` is excluded too: its manifest entry today points at a release
//! tag `.github/workflows/servers.yml` has not published yet (its sha256
//! is from a local build, not the published asset — see the comment on
//! the manifest's `gopls` entry); it 404s until that workflow has run on
//! staging at least once.
//!
//! Network, slow, and ignored by default:
//! `cargo test -p theodore-code-intel-toolchain --test new_servers_install -- --ignored --nocapture`.

use theodore_code_intel_toolchain::Toolchain;
use theodore_ports::code_intel::LanguageId;

/// No network: the builtin manifest (`Manifest::parse` is already covered
/// by `crates/code-intel-toolchain/src/manifest.rs`'s
/// `builtin_parses_without_warnings`) has every language and server this
/// PR adds, and tsgo is ordered ahead of typescript-language-server.
#[test]
fn builtin_manifest_has_the_new_entries() {
    let tc = Toolchain::new(std::env::temp_dir().join("theodore-new-servers-test"));
    let m = tc.manifest();
    for id in ["zig", "lua", "terraform", "bash"] {
        assert!(m.language(&LanguageId::new(id)).is_some(), "language {id}");
    }
    for name in [
        "zls",
        "lua-language-server",
        "taplo",
        "marksman",
        "terraform-ls",
        "tsgo",
        "gopls",
    ] {
        assert!(m.server(name).is_some(), "server {name}");
    }
    let ts = m.language(&LanguageId::new("typescript")).unwrap();
    assert_eq!(
        ts.servers.first().map(|s| s.name.as_str()),
        Some("tsgo"),
        "tsgo should be tried before typescript-language-server: {:?}",
        ts.servers.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
    let toml = m.language(&LanguageId::new("toml")).unwrap();
    assert_eq!(toml.servers.first().map(|s| s.name.as_str()), Some("taplo"));
    let md = m.language(&LanguageId::new("markdown")).unwrap();
    assert_eq!(
        md.servers.first().map(|s| s.name.as_str()),
        Some("marksman")
    );
}

async fn install_and_run_version(name: &str) {
    let root = tempfile::tempdir().unwrap();
    let tc = Toolchain::new(root.path());
    let spec = tc
        .manifest()
        .server(name)
        .unwrap_or_else(|| panic!("{name}: not in the builtin manifest"))
        .clone();
    let resolved = tc
        .install_server(&spec)
        .await
        .unwrap_or_else(|e| panic!("{name}: install failed: {e}"));
    eprintln!("{name}: installed at {}", resolved.bin.display());
    let out = std::process::Command::new(&resolved.bin)
        .arg("--version")
        .output()
        .unwrap_or_else(|e| panic!("{name}: spawn {}: {e}", resolved.bin.display()));
    eprintln!(
        "{name} --version -> status {:?}\nstdout: {}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(out.status.success(), "{name}: --version failed");
}

#[tokio::test]
#[ignore = "network: downloads real servers from their upstream hosts"]
async fn lua_language_server_installs_and_runs() {
    install_and_run_version("lua-language-server").await;
}

#[tokio::test]
#[ignore = "network: downloads real servers from their upstream hosts"]
async fn taplo_installs_and_runs() {
    install_and_run_version("taplo").await;
}

#[tokio::test]
#[ignore = "network: downloads real servers from their upstream hosts"]
async fn marksman_installs_and_runs() {
    install_and_run_version("marksman").await;
}

#[tokio::test]
#[ignore = "network: downloads real servers from their upstream hosts"]
async fn terraform_ls_installs_and_runs() {
    install_and_run_version("terraform-ls").await;
}

#[tokio::test]
#[ignore = "network: downloads real servers from their upstream hosts"]
async fn tsgo_installs_and_runs() {
    install_and_run_version("tsgo").await;
}
