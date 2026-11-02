//! Downloads, installs and grammar caching against a local HTTP server.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use sha2::{Digest, Sha256};
use theodore_code_intel_toolchain::{Manifest, ServerOrigin, Toolchain};
use theodore_ports::code_intel::{CodeIntelError, InstallState, LanguageId, TrustMode};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PLATFORM: &str = "test-plat";

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(data).unwrap();
    gz.finish().unwrap()
}

/// tar.gz of `(raw name, contents, mode)`; names are written raw so `..`
/// survives.
fn tar_gz(files: &[(&str, &[u8], u32)]) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    for (name, data, mode) in files {
        let mut h = tar::Header::new_gnu();
        h.as_gnu_mut().unwrap().name[..name.len()].copy_from_slice(name.as_bytes());
        h.set_entry_type(tar::EntryType::Regular);
        h.set_mode(*mode);
        h.set_size(data.len() as u64);
        h.set_cksum();
        b.append(&h, *data).unwrap();
    }
    gzip(&b.into_inner().unwrap())
}

fn zip_of(files: &[(&str, &[u8], u32)]) -> Vec<u8> {
    use zip::write::SimpleFileOptions;
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, data, mode) in files {
        w.start_file(*name, SimpleFileOptions::default().unix_permissions(*mode))
            .unwrap();
        w.write_all(data).unwrap();
    }
    w.finish().unwrap().into_inner()
}

/// One language `x` served by one server `srv` with a download for
/// [`PLATFORM`].
fn server_manifest(url: &str, sha256: &str, format: &str, bin: &str, extra: &str) -> Manifest {
    let yaml = format!(
        r#"
servers:
  - name: srv
    version: "1.0"
    detect: [["srv-not-on-path", "--version"]]
    download:
      {PLATFORM}: {{ url: "{url}", sha256: "{sha256}", format: {format}, bin: "{bin}" {extra} }}
    launch: ["{{bin}}", "--stdio"]
languages:
  - id: x
    globs: ["*.x"]
    servers: [srv]
"#
    );
    let (m, warnings) = Manifest::parse(&yaml, r#"{"schema":1,"grammars":[]}"#).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    m
}

fn toolchain(root: &Path, manifest: Manifest) -> Toolchain {
    let empty_path = root.join("no-path");
    std::fs::create_dir_all(&empty_path).unwrap();
    Toolchain::builder(root)
        .manifest(manifest)
        .platform(PLATFORM)
        .search_path(empty_path)
        .build()
}

async fn serve(server: &MockServer, at: &str, body: Vec<u8>) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
        .mount(server)
        .await;
}

fn dir_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|r| {
            r.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[tokio::test]
async fn installs_tar_gz_atomically_and_resolves_it() {
    let http = MockServer::start().await;
    let archive = tar_gz(&[
        ("srv-1.0/bin/srv", b"#!/bin/sh\necho srv\n", 0o644),
        ("srv-1.0/LICENSE", b"MIT", 0o644),
    ]);
    serve(&http, "/srv.tar.gz", archive.clone()).await;
    let root = tempfile::tempdir().unwrap();
    let m = server_manifest(
        &format!("{}/srv.tar.gz", http.uri()),
        &sha(&archive),
        "tar.gz",
        "srv-1.0/bin/srv",
        "",
    );
    let tc = toolchain(root.path(), m);
    let spec = tc.manifest().server("srv").unwrap().clone();
    let lang = LanguageId::new("x");
    let wt = root.path();

    let (_, state) = tc
        .server_for(&lang, None, wt, TrustMode::Review)
        .await
        .unwrap();
    assert_eq!(state, InstallState::Missing);

    let r = tc.install_server(&spec).await.unwrap();
    assert_eq!(
        r.origin,
        ServerOrigin::Downloaded {
            version: "1.0".into()
        }
    );
    assert_eq!(r.bin, root.path().join("servers/srv/1.0/srv-1.0/bin/srv"));
    let mode = std::fs::metadata(&r.bin).unwrap().permissions().mode();
    assert_ne!(mode & 0o111, 0, "bin is executable");
    assert_eq!(dir_names(&root.path().join("servers/srv")), vec!["1.0"]);
    assert_eq!(dir_names(&root.path().join("tmp")), Vec::<String>::new());

    let (_, state) = tc
        .server_for(&lang, None, wt, TrustMode::Review)
        .await
        .unwrap();
    assert_eq!(
        state,
        InstallState::Installed {
            version: "1.0".into()
        }
    );
    let launch = tc.launch_config(&r, &spec, TrustMode::Review);
    assert_eq!(
        launch.argv,
        vec![r.bin.clone().into_os_string(), "--stdio".into()]
    );

    // Installed already: no second download.
    tc.install_server(&spec).await.unwrap();
    assert_eq!(http.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn checksum_mismatch_fails_and_leaves_nothing() {
    let http = MockServer::start().await;
    serve(&http, "/srv.gz", gzip(b"payload")).await;
    let root = tempfile::tempdir().unwrap();
    let m = server_manifest(
        &format!("{}/srv.gz", http.uri()),
        &sha(b"something else"),
        "gz",
        "srv",
        "",
    );
    let tc = toolchain(root.path(), m);
    let spec = tc.manifest().server("srv").unwrap().clone();

    let err = tc.install_server(&spec).await.unwrap_err();
    assert!(matches!(err, CodeIntelError::Checksum(_)), "{err}");
    assert!(!root.path().join("servers/srv/1.0").exists());
    assert_eq!(dir_names(&root.path().join("tmp")), Vec::<String>::new());

    let (_, state) = tc
        .server_for(&LanguageId::new("x"), None, root.path(), TrustMode::Trusted)
        .await
        .unwrap();
    assert!(matches!(state, InstallState::Failed { .. }), "{state:?}");
}

#[tokio::test]
async fn redirects_stay_on_allowed_hosts() {
    let http = MockServer::start().await;
    let body = gzip(b"payload");
    serve(&http, "/real.gz", body.clone()).await;
    Mock::given(path("/same-host.gz"))
        .respond_with(ResponseTemplate::new(302).insert_header("Location", "/real.gz"))
        .mount(&http)
        .await;
    // Same server, other host name: not the manifest's host.
    let port = http.address().port();
    Mock::given(path("/other-host.gz"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", format!("http://localhost:{port}/real.gz")),
        )
        .mount(&http)
        .await;

    let install = |url_path: &'static str, extra: &'static str| {
        let (uri, body_sha) = (http.uri(), sha(&body));
        async move {
            let root = tempfile::tempdir().unwrap();
            let m = server_manifest(&format!("{uri}{url_path}"), &body_sha, "gz", "srv", extra);
            let tc = toolchain(root.path(), m);
            let spec = tc.manifest().server("srv").unwrap().clone();
            tc.install_server(&spec).await.map(|_| ())
        }
    };

    install("/same-host.gz", "").await.unwrap();
    let err = install("/other-host.gz", "").await.unwrap_err();
    assert!(
        matches!(&err, CodeIntelError::Download(m) if m.contains("redirect to http://localhost")),
        "{err}"
    );
    install("/other-host.gz", ", redirect_hosts: [localhost]")
        .await
        .unwrap();
}

#[tokio::test]
async fn every_archive_format_installs() {
    let http = MockServer::start().await;
    let exe: &[u8] = b"#!/bin/sh\n";
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        ("binary", exe.to_vec(), "srv"),
        ("gz", gzip(exe), "bin/srv"),
        ("tar.gz", tar_gz(&[("d/srv", exe, 0o755)]), "d/srv"),
        ("zip", zip_of(&[("d/srv", exe, 0o755)]), "d/srv"),
        (
            "npm-tgz",
            tar_gz(&[("package/lib/cli.js", exe, 0o644)]),
            "lib/cli.js",
        ),
        (
            "wheel",
            zip_of(&[("srv-1.0.data/scripts/srv", exe, 0o755)]),
            "srv-1.0.data/scripts/srv",
        ),
    ];
    for (i, (format, bytes, bin)) in cases.into_iter().enumerate() {
        let at = format!("/a{i}");
        serve(&http, &at, bytes.clone()).await;
        let root = tempfile::tempdir().unwrap();
        let m = server_manifest(
            &format!("{}{at}", http.uri()),
            &sha(&bytes),
            format,
            bin,
            "",
        );
        let tc = toolchain(root.path(), m);
        let spec = tc.manifest().server("srv").unwrap().clone();
        let r = tc
            .install_server(&spec)
            .await
            .unwrap_or_else(|e| panic!("{format}: {e}"));
        assert_eq!(std::fs::read(&r.bin).unwrap(), exe, "{format}");
        let mode = std::fs::metadata(&r.bin).unwrap().permissions().mode();
        assert_ne!(mode & 0o111, 0, "{format}: executable");
    }
}

#[tokio::test]
async fn traversal_entry_fails_the_install() {
    let http = MockServer::start().await;
    let archive = tar_gz(&[("srv", b"x", 0o755), ("../../escaped", b"x", 0o644)]);
    serve(&http, "/evil.tar.gz", archive.clone()).await;
    let root = tempfile::tempdir().unwrap();
    let m = server_manifest(
        &format!("{}/evil.tar.gz", http.uri()),
        &sha(&archive),
        "tar.gz",
        "srv",
        "",
    );
    let tc = toolchain(root.path(), m);
    let spec = tc.manifest().server("srv").unwrap().clone();

    let err = tc.install_server(&spec).await.unwrap_err();
    assert!(err.to_string().contains("unsafe archive entry"), "{err}");
    assert_eq!(
        dir_names(&root.path().join("servers/srv")),
        Vec::<String>::new()
    );
    assert!(!root.path().join("escaped").exists());
    assert!(!root.path().join("servers/escaped").exists());
}

#[tokio::test]
async fn concurrent_installs_download_once() {
    let http = MockServer::start().await;
    let body = gzip(b"payload");
    Mock::given(path("/srv.gz"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(body.clone())
                .set_delay(std::time::Duration::from_millis(100)),
        )
        .expect(1)
        .mount(&http)
        .await;
    let root = tempfile::tempdir().unwrap();
    let m = server_manifest(
        &format!("{}/srv.gz", http.uri()),
        &sha(&body),
        "gz",
        "srv",
        "",
    );
    let tc = toolchain(root.path(), m);
    let spec = tc.manifest().server("srv").unwrap().clone();
    let other = tc.clone();
    let (a, b) = tokio::join!(tc.install_server(&spec), other.install_server(&spec));
    assert_eq!(a.unwrap().bin, b.unwrap().bin);
}

#[tokio::test]
async fn gc_keeps_named_versions_and_unrelated_servers() {
    let root = tempfile::tempdir().unwrap();
    for d in [
        "servers/x/1",
        "servers/x/2",
        "servers/x/3.partial",
        "servers/y/1",
        "grammars/tree-sitter-old-1.0",
        "grammars/tree-sitter-new-2.0.partial",
    ] {
        std::fs::create_dir_all(root.path().join(d)).unwrap();
    }
    let tc = toolchain(root.path(), Manifest::default());
    tc.gc(&[("x", "2")]);
    assert_eq!(dir_names(&root.path().join("servers/x")), vec!["2"]);
    assert_eq!(dir_names(&root.path().join("servers/y")), vec!["1"]);
    assert_eq!(
        dir_names(&root.path().join("grammars")),
        Vec::<String>::new()
    );
}

fn grammar_manifest(uri: &str, lib_sha: &str, tags_sha: &str) -> Manifest {
    let yaml = "languages:\n  - id: json\n    globs: ['*.json']\n";
    let json = format!(
        r#"{{"schema":1,"grammars":[{{
            "package":"tree-sitter-json","version":"0.24.8",
            "languages":[{{"id":"json","symbol":"tree_sitter_json",
              "tags_query":{{"url":"{uri}/json.tags.scm","sha256":"{tags_sha}"}}}}],
            "downloads":{{"{PLATFORM}":{{"url":"{uri}/json.dylib","sha256":"{lib_sha}"}}}}
        }}]}}"#
    );
    let (m, warnings) = Manifest::parse(yaml, &json).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    m
}

#[tokio::test]
async fn grammar_that_does_not_load_is_failed_not_a_crash() {
    let http = MockServer::start().await;
    let lib = b"not a dylib".to_vec();
    let tags = b"(pair key: (string) @name) @definition.field".to_vec();
    Mock::given(path("/json.dylib"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(lib.clone()))
        .expect(1)
        .mount(&http)
        .await;
    serve(&http, "/json.tags.scm", tags.clone()).await;
    let root = tempfile::tempdir().unwrap();
    let tc = toolchain(
        root.path(),
        grammar_manifest(&http.uri(), &sha(&lib), &sha(&tags)),
    );
    let json = LanguageId::new("json");

    assert_eq!(tc.grammar_state(&json), InstallState::Missing);
    assert!(tc.ensure_grammar(&json).await.is_err());
    let dir = root.path().join("grammars/tree-sitter-json-0.24.8");
    assert_eq!(dir_names(&dir), vec!["grammar.dylib", "json.tags.scm"]);
    assert!(matches!(
        tc.grammar_state(&json),
        InstallState::Failed { .. }
    ));
    // The cache verifies, so a retry loads from disk without downloading.
    assert!(tc.ensure_grammar(&json).await.is_err());
    assert_eq!(
        tc.grammar_state(&LanguageId::new("nope")),
        InstallState::Unavailable
    );
}

#[tokio::test]
async fn grammar_checksum_mismatch_installs_nothing() {
    let http = MockServer::start().await;
    serve(&http, "/json.dylib", b"lib".to_vec()).await;
    serve(&http, "/json.tags.scm", b"tags".to_vec()).await;
    let root = tempfile::tempdir().unwrap();
    let tc = toolchain(
        root.path(),
        grammar_manifest(&http.uri(), &sha(b"lib"), &sha(b"other")),
    );
    let err = tc
        .ensure_grammar(&LanguageId::new("json"))
        .await
        .unwrap_err();
    assert!(matches!(err, CodeIntelError::Checksum(_)), "{err}");
    assert_eq!(
        dir_names(&root.path().join("grammars")),
        Vec::<String>::new()
    );
}

/// Compiles a real grammar and parses with it. Needs a C compiler and a
/// tree-sitter grammar checkout:
///
/// ```sh
/// THEODORE_TEST_GRAMMAR_DIR=~/src/tree-sitter-json \
///   cargo test -p theodore-code-intel-toolchain -- --ignored real_grammar
/// ```
/// `THEODORE_TEST_GRAMMAR_SYMBOL` defaults to `tree_sitter_json`.
#[tokio::test]
#[ignore = "needs cc and THEODORE_TEST_GRAMMAR_DIR"]
async fn real_grammar_loads_and_parses() {
    let src = std::path::PathBuf::from(
        std::env::var("THEODORE_TEST_GRAMMAR_DIR").expect("THEODORE_TEST_GRAMMAR_DIR"),
    )
    .join("src");
    let symbol =
        std::env::var("THEODORE_TEST_GRAMMAR_SYMBOL").unwrap_or_else(|_| "tree_sitter_json".into());
    let build = tempfile::tempdir().unwrap();
    let out = build.path().join("grammar.dylib");
    let mut cc = std::process::Command::new("cc");
    cc.args(["-shared", "-fPIC", "-O1", "-I"])
        .arg(&src)
        .arg(src.join("parser.c"));
    if src.join("scanner.c").exists() {
        cc.arg(src.join("scanner.c"));
    }
    assert!(
        cc.arg("-o").arg(&out).status().unwrap().success(),
        "cc failed"
    );
    let lib = std::fs::read(&out).unwrap();
    let tags = b"(pair key: (string) @name) @definition.field".to_vec();

    let http = MockServer::start().await;
    serve(&http, "/json.dylib", lib.clone()).await;
    serve(&http, "/json.tags.scm", tags.clone()).await;
    let yaml = "languages:\n  - id: json\n    globs: ['*.json']\n";
    let json = format!(
        r#"{{"schema":1,"grammars":[{{
            "package":"tree-sitter-json","version":"0.0.0",
            "languages":[{{"id":"json","symbol":"{symbol}",
              "tags_query":{{"url":"{uri}/json.tags.scm","sha256":"{tags_sha}"}}}}],
            "downloads":{{"{PLATFORM}":{{"url":"{uri}/json.dylib","sha256":"{lib_sha}"}}}}
        }}]}}"#,
        uri = http.uri(),
        tags_sha = sha(&tags),
        lib_sha = sha(&lib),
    );
    let (m, _) = Manifest::parse(yaml, &json).unwrap();
    let root = tempfile::tempdir().unwrap();
    let tc = toolchain(root.path(), m);

    let id = LanguageId::new("json");
    let (a, b) = tokio::join!(tc.ensure_grammar(&id), tc.ensure_grammar(&id));
    let g = a.unwrap();
    b.unwrap();
    assert_eq!(g.tags_query.as_bytes(), tags.as_slice());
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&g.language).unwrap();
    let tree = parser.parse(r#"{"a": [1, 2]}"#, None).unwrap();
    assert!(!tree.root_node().has_error());
    assert_eq!(
        tc.grammar_state(&id),
        InstallState::Installed {
            version: "0.0.0".into()
        }
    );
}
