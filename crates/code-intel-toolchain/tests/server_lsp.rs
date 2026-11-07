//! Whether each language server of the manifest answers: drives it through
//! the real client (`theodore-code-intel-lsp`) on a tiny project, by the
//! manifest's own entry, and asks for documentSymbol, definition and
//! cross-file references. Where a server may run the project's code, a
//! marker file in the build says whether it did.
//!
//! Network (downloads; Gradle and Maven fetch too), slow, ignored by default:
//!
//! ```text
//! cargo test -p theodore-code-intel-toolchain --test server_lsp -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `THEODORE_TEST_SERVERS=<dir>` keeps downloads between runs (kotlin-lsp is
//! 360 MB). ruby-lsp and intelephense are never downloaded: those two tests
//! need them on `PATH` (`gem install --install-dir <dir> ruby-lsp` with
//! `GEM_HOME`/`GEM_PATH` set does; nothing has to be installed globally).

use std::path::{Path, PathBuf};
use std::time::Duration;

use theodore_code_intel_lsp::{LaunchConfig, LspSession, SessionOptions};
use theodore_code_intel_toolchain::{ServerOrigin, Toolchain};
use theodore_ports::{InstallState, LanguageId, Position, ServerState, TrustMode};

/// 1-based line and char column of the first `needle` in `text`.
fn find(text: &str, needle: &str) -> Position {
    let byte = text
        .find(needle)
        .unwrap_or_else(|| panic!("{needle} not in {text}"));
    let before = &text[..byte];
    let line = before.matches('\n').count() as u32 + 1;
    let col = before.rsplit('\n').next().unwrap().chars().count() as u32 + 1;
    Position { line, col }
}

struct Case<'a> {
    server: &'a str,
    language: &'a str,
    /// `Trusted`, or `Review` with its options.
    trust: TrustMode,
    /// Repo-relative path, text.
    files: &'a [(&'a str, &'a str)],
    /// documentSymbol of this file has this symbol.
    symbols_in: (&'a str, &'a str),
    /// Definition at `needle` in `.0` is in `.2`.
    definition: (&'a str, &'a str, &'a str),
    /// References at `needle` in `.0` include `.2` and `.0`.
    references: (&'a str, &'a str, &'a str),
}

struct Toolbox {
    tc: Toolchain,
    /// Keeps a temporary root alive.
    _tmp: Option<tempfile::TempDir>,
}

fn toolbox() -> Toolbox {
    match std::env::var_os("THEODORE_TEST_SERVERS") {
        Some(dir) => Toolbox {
            tc: Toolchain::new(PathBuf::from(dir)),
            _tmp: None,
        },
        None => {
            let tmp = tempfile::tempdir().unwrap();
            Toolbox {
                tc: Toolchain::new(tmp.path()),
                _tmp: Some(tmp),
            }
        }
    }
}

/// A project of `files`, canonical (macOS's temp dir is a symlink).
fn project(files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    for (rel, text) in files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    (dir, root)
}

/// Run `case` on an existing project: downloaded if the machine lacks it.
async fn drive(tc: &Toolchain, root: &Path, case: &Case<'_>) {
    let name = case.server;
    let spec = tc
        .manifest()
        .server(name)
        .unwrap_or_else(|| panic!("{name}: not in the manifest"))
        .clone();
    let mut resolved = tc.resolve_server(&spec, root, case.trust).await;
    if resolved.is_none() {
        assert!(
            spec.download_for(tc.platform()).is_some(),
            "{name}: not on PATH and Theodore does not download it"
        );
        tc.install_server(&spec)
            .await
            .unwrap_or_else(|e| panic!("{name}: install: {e}"));
        resolved = tc.resolve_server(&spec, root, case.trust).await;
    }
    let resolved = resolved.unwrap_or_else(|| panic!("{name}: installed but not resolved"));
    match &resolved.origin {
        ServerOrigin::System { path, version } => {
            eprintln!("{name}: found at {} ({version:?})", path.display());
        }
        ServerOrigin::Downloaded { version } => eprintln!("{name}: downloaded {version}"),
    }
    let cfg = tc.launch_config(&resolved, &spec, case.trust);
    let lang = tc
        .manifest()
        .language(&LanguageId::new(case.language))
        .unwrap_or_else(|| panic!("no language {}", case.language));
    let exts = lang
        .globs
        .iter()
        .filter_map(|g| g.strip_prefix("*.").map(str::to_string))
        .collect();
    let config = LaunchConfig {
        server: name.into(),
        argv: cfg
            .argv
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect(),
        env: cfg
            .env
            .iter()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.to_string_lossy().into_owned(),
                )
            })
            .collect(),
        init_options: cfg.init_options,
        workspace_folders: cfg.workspace_folders,
        language_ids: vec![(lang.id.clone(), lang.lsp_language_id().to_string(), exts)],
        start_timeout_secs: cfg.start_timeout_secs,
        ready_settle_secs: cfg.ready_settle_secs,
    };
    eprintln!("{name}: argv {:?}", config.argv);
    let options = SessionOptions {
        request_timeout: Duration::from_secs(120),
        ..SessionOptions::default()
    };

    let started = std::time::Instant::now();
    let s = LspSession::start(root, config, options).unwrap();
    let state = s.wait_ready(Duration::from_secs(900)).await;
    eprintln!("{name}: {state:?} after {:?}", started.elapsed());
    assert_eq!(state, ServerState::Ready, "{name} did not warm up");

    let read = |rel: &str| std::fs::read_to_string(root.join(rel)).unwrap();

    let (file, want) = case.symbols_in;
    let syms = s.document_symbols(root, file).await;
    eprintln!("{name}: documentSymbol({file}): {syms:?}");

    let (file, needle, def_in) = case.definition;
    let at = find(&read(file), needle);
    let defs = s.definition(root, file, at).await;
    eprintln!("{name}: definition({file} {at:?}): {defs:?}");

    let (ref_from, needle, ref_in) = case.references;
    let at = find(&read(ref_from), needle);
    let refs = s.references(root, ref_from, at, true).await;
    eprintln!("{name}: references({ref_from} {at:?}): {refs:?}");

    s.shutdown().await;

    let syms = syms.unwrap_or_else(|e| panic!("{name}: documentSymbol: {e}"));
    assert!(
        syms.iter().any(|sym| sym.name.contains(want)),
        "{name}: no {want} in documentSymbol: {syms:?}"
    );
    let defs = defs.unwrap_or_else(|e| panic!("{name}: definition: {e}"));
    assert!(
        defs.items.iter().any(|l| l.path == def_in),
        "{name}: definition not in {def_in}: {defs:?}"
    );
    let refs = refs.unwrap_or_else(|e| panic!("{name}: references: {e}"));
    for rel in [ref_in, ref_from] {
        assert!(
            refs.items.iter().any(|l| l.path == rel),
            "{name}: no reference in {rel}: {refs:?}"
        );
    }
}

// ── JavaScript ───────────────────────────────────────────────────────────

/// Plain `.js`, no tsconfig or jsconfig: does tsgo serve it? It goes first
/// for `javascript` only because it does.
#[tokio::test]
#[ignore = "network: downloads tsgo"]
async fn tsgo_serves_plain_javascript_without_a_tsconfig() {
    let bx = toolbox();
    let (_dir, root) = project(&[
        (
            "a.js",
            "export function greet(name) {\n  return `hi ${name}`;\n}\n",
        ),
        (
            "b.js",
            "import { greet } from \"./a.js\";\n\nconsole.log(greet(\"world\"));\n",
        ),
    ]);
    drive(
        &bx.tc,
        &root,
        &Case {
            server: "tsgo",
            language: "javascript",
            trust: TrustMode::Trusted,
            files: &[],
            symbols_in: ("a.js", "greet"),
            definition: ("b.js", "greet(\"world", "a.js"),
            references: ("a.js", "greet(name", "b.js"),
        },
    )
    .await;
}

// ── PHP ──────────────────────────────────────────────────────────────────

const PHP: &[(&str, &str)] = &[
    (
        "src/Greeter.php",
        "<?php\n\nnamespace Demo;\n\nclass Greeter\n{\n    public function greet(string $name): string\n    {\n        return \"hi \" . $name;\n    }\n}\n",
    ),
    (
        "src/App.php",
        "<?php\n\nnamespace Demo;\n\nclass App\n{\n    public function run(): string\n    {\n        $g = new Greeter();\n        return $g->greet(\"world\");\n    }\n}\n",
    ),
    (
        "composer.json",
        r#"{"name": "demo/app", "autoload": {"psr-4": {"Demo\\": "src/"}}}"#,
    ),
];

const PHP_CASE: Case = Case {
    server: "phpactor",
    language: "php",
    trust: TrustMode::Trusted,
    files: PHP,
    symbols_in: ("src/Greeter.php", "Greeter"),
    definition: ("src/App.php", "greet(\"world", "src/Greeter.php"),
    references: ("src/Greeter.php", "greet(string", "src/App.php"),
};

#[tokio::test]
#[ignore = "network: downloads phpactor; needs php"]
async fn phpactor_answers_and_keeps_its_index_under_data() {
    let bx = toolbox();
    let (_dir, root) = project(PHP_CASE.files);
    drive(&bx.tc, &root, &PHP_CASE).await;
    let data = bx.tc.root().join("server-data/phpactor");
    let indexed = std::fs::read_dir(&data)
        .unwrap()
        .flatten()
        .any(|d| d.path().join("index").is_dir());
    assert!(indexed, "no index under {}", data.display());
}

/// Why phpactor is `trusted_only`: a PR's `vendor/composer` runs.
#[tokio::test]
#[ignore = "network: downloads phpactor; needs php"]
async fn phpactor_runs_the_projects_composer_files() {
    let bx = toolbox();
    let (_dir, root) = project(PHP_CASE.files);
    let marker = root.join("marker");
    let classmap = format!(
        "<?php\nfile_put_contents({:?}, 'ran');\nreturn [];\n",
        marker.display().to_string()
    );
    std::fs::create_dir_all(root.join("vendor/composer")).unwrap();
    std::fs::write(root.join("vendor/autoload.php"), "<?php\n").unwrap();
    std::fs::write(root.join("vendor/composer/autoload_classmap.php"), classmap).unwrap();
    drive(&bx.tc, &root, &PHP_CASE).await;
    assert!(
        marker.exists(),
        "phpactor no longer runs them: revisit trusted_only"
    );
}

#[tokio::test]
#[ignore = "needs intelephense on PATH; never downloaded"]
async fn intelephense_answers_and_runs_nothing() {
    let bx = toolbox();
    let (_dir, root) = project(PHP_CASE.files);
    let marker = root.join("marker");
    let classmap = format!(
        "<?php\nfile_put_contents({:?}, 'ran');\nreturn [];\n",
        marker.display().to_string()
    );
    std::fs::create_dir_all(root.join("vendor/composer")).unwrap();
    std::fs::write(root.join("vendor/autoload.php"), "<?php\n").unwrap();
    std::fs::write(root.join("vendor/composer/autoload_classmap.php"), classmap).unwrap();
    drive(
        &bx.tc,
        &root,
        &Case {
            server: "intelephense",
            trust: TrustMode::Review,
            ..PHP_CASE
        },
    )
    .await;
    assert!(!marker.exists(), "intelephense ran the project's PHP");
}

// ── Ruby ─────────────────────────────────────────────────────────────────

const RUBY: &[(&str, &str)] = &[
    (
        "lib/greeter.rb",
        "class Greeter\n  def greet(name)\n    \"hi #{name}\"\n  end\nend\n",
    ),
    (
        "lib/app.rb",
        "require_relative \"greeter\"\n\nclass App\n  def run\n    Greeter.new.greet(\"world\")\n  end\nend\n",
    ),
];

#[tokio::test]
#[ignore = "needs ruby-lsp on PATH; never downloaded; composes a bundle (network)"]
async fn ruby_lsp_answers() {
    let bx = toolbox();
    let (_dir, root) = project(RUBY);
    drive(
        &bx.tc,
        &root,
        &Case {
            server: "ruby-lsp",
            language: "ruby",
            trust: TrustMode::Trusted,
            files: RUBY,
            symbols_in: ("lib/greeter.rb", "Greeter"),
            definition: ("lib/app.rb", "greet(\"world", "lib/greeter.rb"),
            references: ("lib/greeter.rb", "greet(name", "lib/app.rb"),
        },
    )
    .await;
}

// ── Java ─────────────────────────────────────────────────────────────────

const GREETER_JAVA: &str = "package demo;\n\npublic class Greeter {\n    public String greet(String name) {\n        return \"hi \" + name;\n    }\n}\n";
const APP_JAVA: &str = "package demo;\n\npublic class App {\n    public static void main(String[] args) {\n        System.out.println(new Greeter().greet(\"world\"));\n    }\n}\n";
const POM: &str = "<project xmlns=\"http://maven.apache.org/POM/4.0.0\">\n  <modelVersion>4.0.0</modelVersion>\n  <groupId>demo</groupId>\n  <artifactId>demo</artifactId>\n  <version>1</version>\n  <properties>\n    <maven.compiler.source>17</maven.compiler.source>\n    <maven.compiler.target>17</maven.compiler.target>\n  </properties>\n</project>\n";

const JAVA_CASE: Case = Case {
    server: "jdtls",
    language: "java",
    trust: TrustMode::Trusted,
    files: &[
        ("src/main/java/demo/Greeter.java", GREETER_JAVA),
        ("src/main/java/demo/App.java", APP_JAVA),
    ],
    symbols_in: ("src/main/java/demo/Greeter.java", "Greeter"),
    definition: (
        "src/main/java/demo/App.java",
        "greet(\"world",
        "src/main/java/demo/Greeter.java",
    ),
    references: (
        "src/main/java/demo/Greeter.java",
        "greet(String",
        "src/main/java/demo/App.java",
    ),
};

#[tokio::test]
#[ignore = "network: downloads jdtls, Maven fetches plugins; needs Java 21+"]
async fn jdtls_answers_on_a_maven_project_and_leaves_no_metadata() {
    let bx = toolbox();
    let (_dir, root) = project(JAVA_CASE.files);
    std::fs::write(root.join("pom.xml"), POM).unwrap();
    drive(&bx.tc, &root, &JAVA_CASE).await;
    for metadata in [".project", ".classpath", ".settings"] {
        assert!(
            !root.join(metadata).exists(),
            "jdtls wrote {metadata} into the worktree"
        );
    }
}

/// A build that would leave a marker. `review_init_options`: it doesn't, the
/// answers still come, and nothing lands in the worktree. Defaults: it does,
/// so the fixture does trip.
#[tokio::test]
#[ignore = "network: downloads jdtls, Gradle fetches itself; needs Java 21+"]
async fn jdtls_review_runs_no_build_and_defaults_do() {
    let bx = toolbox();
    let (_dir, root) = project(JAVA_CASE.files);
    let marker = root.join("marker");
    let script = format!(
        "new File({:?}).text = 'ran'\n",
        marker.display().to_string()
    );
    std::fs::write(
        root.join("settings.gradle"),
        format!("{script}rootProject.name = 'demo'\n"),
    )
    .unwrap();
    std::fs::write(
        root.join("build.gradle"),
        format!("{script}apply plugin: 'java'\n"),
    )
    .unwrap();

    drive(
        &bx.tc,
        &root,
        &Case {
            trust: TrustMode::Review,
            ..JAVA_CASE
        },
    )
    .await;
    assert!(!marker.exists(), "a review session ran the Gradle build");
    for left in [
        ".project",
        ".classpath",
        ".settings",
        ".gradle",
        "build",
        "bin",
    ] {
        assert!(
            !root.join(left).exists(),
            "review left {left} in the worktree"
        );
    }

    drive(&bx.tc, &root, &JAVA_CASE).await;
    assert!(marker.exists(), "the trusted session did not run the build");
}

// ── Kotlin ───────────────────────────────────────────────────────────────

const KOTLIN_CASE: Case = Case {
    server: "kotlin-lsp",
    language: "kotlin",
    trust: TrustMode::Trusted,
    files: &[
        (
            "src/main/kotlin/demo/Greeter.kt",
            "package demo\n\nclass Greeter {\n    fun greet(name: String): String {\n        return \"hi $name\"\n    }\n}\n",
        ),
        (
            "src/main/kotlin/demo/App.kt",
            "package demo\n\nfun main() {\n    println(Greeter().greet(\"world\"))\n}\n",
        ),
        (
            "build.gradle.kts",
            "plugins { kotlin(\"jvm\") version \"2.3.0\" }\nrepositories { mavenCentral() }\n",
        ),
    ],
    symbols_in: ("src/main/kotlin/demo/Greeter.kt", "Greeter"),
    definition: (
        "src/main/kotlin/demo/App.kt",
        "greet(\"world",
        "src/main/kotlin/demo/Greeter.kt",
    ),
    references: (
        "src/main/kotlin/demo/Greeter.kt",
        "greet(name",
        "src/main/kotlin/demo/App.kt",
    ),
};

/// The import takes as long as Gradle does; `Ready` has to wait for it, or
/// definition answers nothing. And it runs the build: the marker is why it
/// is `trusted_only`.
#[tokio::test]
#[ignore = "network: downloads kotlin-lsp (360 MB), Gradle fetches itself"]
async fn kotlin_lsp_answers_after_import_and_runs_the_build() {
    let bx = toolbox();
    let (_dir, root) = project(KOTLIN_CASE.files);
    let marker = root.join("marker");
    std::fs::write(
        root.join("settings.gradle.kts"),
        format!(
            "File({:?}).writeText(\"ran\")\nrootProject.name = \"demo\"\n",
            marker.display().to_string()
        ),
    )
    .unwrap();
    drive(&bx.tc, &root, &KOTLIN_CASE).await;
    assert!(
        marker.exists(),
        "kotlin-lsp no longer runs the build: revisit trusted_only"
    );
}

// ── C# ───────────────────────────────────────────────────────────────────

const CSPROJ: &str = "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <OutputType>Exe</OutputType>\n    <TargetFramework>net10.0</TargetFramework>\n  </PropertyGroup>\n  <Target Name=\"Marker\" BeforeTargets=\"ResolveAssemblyReferences;CoreCompile\">\n    <Exec Command=\"touch marker\" />\n  </Target>\n</Project>\n";

/// A project's MSBuild targets run: the marker is why it is `trusted_only`.
#[tokio::test]
#[ignore = "network: downloads csharp-ls; needs the .NET 10 SDK"]
async fn csharp_ls_answers_and_runs_the_projects_targets() {
    let bx = toolbox();
    let files: &[(&str, &str)] = &[
        (
            "Greeter.cs",
            "namespace Demo;\n\npublic class Greeter\n{\n    public string Greet(string name)\n    {\n        return \"hi \" + name;\n    }\n}\n",
        ),
        (
            "App.cs",
            "namespace Demo;\n\npublic static class App\n{\n    public static void Main()\n    {\n        System.Console.WriteLine(new Greeter().Greet(\"world\"));\n    }\n}\n",
        ),
        ("demo.csproj", CSPROJ),
    ];
    let (_dir, root) = project(files);
    drive(
        &bx.tc,
        &root,
        &Case {
            server: "csharp-ls",
            language: "csharp",
            trust: TrustMode::Trusted,
            files,
            symbols_in: ("Greeter.cs", "Greeter"),
            definition: ("App.cs", "Greet(\"world", "Greeter.cs"),
            references: ("Greeter.cs", "Greet(string", "App.cs"),
        },
    )
    .await;
    assert!(
        root.join("marker").exists(),
        "csharp-ls no longer runs targets: revisit trusted_only"
    );
}

// ── no download behind a review ──────────────────────────────────────────

/// No network: the servers without an off switch say so in a review run, and
/// in a space they stand as themselves.
#[tokio::test]
async fn servers_that_run_the_build_never_serve_a_review() {
    let bx = toolbox();
    let (_dir, root) = project(&[]);
    for (language, server) in [
        ("kotlin", "kotlin-lsp"),
        ("csharp", "csharp-ls"),
        ("ruby", "ruby-lsp"),
    ] {
        let (spec, state) = bx
            .tc
            .server_for(&LanguageId::new(language), None, &root, TrustMode::Review)
            .await
            .unwrap();
        assert_eq!(spec.name, server);
        assert!(spec.trusted_only, "{server}");
        assert_eq!(state, InstallState::TrustedOnly, "{server}");
    }
}
