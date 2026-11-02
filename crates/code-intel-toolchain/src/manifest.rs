//! The manifest: which files a language owns, which grammar parses it, which
//! servers serve it and how to get each one (docs/code-intel.md §3).
//!
//! ```text
//! manifest/languages.yaml (hand-written) ─┐
//! manifest/grammars.json  (CI-generated) ─┴─▶ parse ─▶ validate ─▶ Manifest
//!                                                       └─ bad entry: dropped + warning
//! ```
//!
//! # `grammars.json` schema (version 1)
//!
//! Written by the grammar CI, never by hand. One entry per grammar package;
//! a package may hold several languages (`tree-sitter-typescript`:
//! `typescript` + `tsx`).
//!
//! ```json
//! {
//!   "schema": 1,
//!   "grammars": [{
//!     "package": "tree-sitter-typescript",
//!     "version": "0.23.2",
//!     "languages": [{
//!       "id": "typescript",
//!       "symbol": "tree_sitter_typescript",
//!       "tags_query": { "url": "https://…/typescript.tags.scm", "sha256": "…" }
//!     }],
//!     "downloads": {
//!       "darwin-arm64": { "url": "https://…/tree-sitter-typescript-0.23.2.dylib",
//!                         "sha256": "…", "size": 1234567 }
//!     }
//!   }]
//! }
//! ```
//!
//! - `languages[].id` must be a language of `languages.yaml`; others are
//!   ignored with a warning.
//! - `downloads` is keyed by [`crate::current_platform`] names. A dylib may be
//!   universal (arm64 + x64); it is still listed under each platform.
//! - Every artifact (dylib, tags query) needs `url` (https) and `sha256` (64
//!   hex chars). `size` and `redirect_hosts` are optional, as for servers.

use std::collections::{BTreeMap, HashSet};
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use theodore_ports::code_intel::{CodeIntelError, CodeIntelResult, LanguageId};

use crate::detect::glob_match;

const BUILTIN_LANGUAGES: &str = include_str!("../manifest/languages.yaml");
const BUILTIN_GRAMMARS: &str = include_str!("../manifest/grammars.json");

/// The `grammars.json` schema version this build reads.
pub const GRAMMARS_SCHEMA: u32 = 1;

/// Placeholder in a server's `launch` argv for the resolved binary.
pub const BIN_PLACEHOLDER: &str = "{bin}";

/// A downloadable file, pinned by hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub url: String,
    pub sha256: String,
    #[serde(default)]
    pub size: Option<u64>,
    /// Hosts the URL may redirect to besides its own (GitHub release assets
    /// live on `release-assets.githubusercontent.com`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redirect_hosts: Vec<String>,
}

/// How a server download is packed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ArchiveFormat {
    #[serde(rename = "tar.gz")]
    TarGz,
    #[serde(rename = "zip")]
    Zip,
    /// One gzipped file.
    #[serde(rename = "gz")]
    Gz,
    /// The file itself.
    #[serde(rename = "binary")]
    Binary,
    /// An npm tarball: tar.gz with everything under `package/`.
    #[serde(rename = "npm-tgz")]
    NpmTgz,
    /// A Python wheel (a zip).
    #[serde(rename = "wheel")]
    Wheel,
}

/// One platform's download of a server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerDownload {
    pub url: String,
    pub sha256: String,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redirect_hosts: Vec<String>,
    pub format: ArchiveFormat,
    /// The binary's path inside the unpacked archive (`/` separators).
    pub bin: String,
}

impl ServerDownload {
    pub fn artifact(&self) -> Artifact {
        Artifact {
            url: self.url.clone(),
            sha256: self.sha256.clone(),
            size: self.size,
            redirect_hosts: self.redirect_hosts.clone(),
        }
    }
}

/// A language server and every way to get it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerSpec {
    pub name: String,
    /// The pinned version of `download`; required when `download` is set.
    #[serde(default)]
    pub version: Option<String>,
    /// Alternatives, each an argv: `[["gopls", "version"]]`. A one-element
    /// argv only checks that the program exists.
    #[serde(default)]
    pub detect: Vec<Vec<String>>,
    /// Keyed by platform (`darwin-arm64`, …).
    #[serde(default)]
    pub download: Option<BTreeMap<String, ServerDownload>>,
    /// argv; [`BIN_PLACEHOLDER`] is replaced by the resolved binary.
    pub launch: Vec<String>,
    #[serde(default)]
    pub init_options: serde_json::Value,
    /// Merged over `init_options` in `TrustMode::Review` (docs §8).
    #[serde(default)]
    pub review_init_options: serde_json::Value,
    /// One server per repo with worktrees as folders (docs §6).
    #[serde(default)]
    pub workspace_folders: bool,
    /// A runtime the server needs and Theodore does not ship (`"node"`).
    #[serde(default)]
    pub requires: Option<String>,
}

impl ServerSpec {
    /// This platform's download, if any.
    pub fn download_for(&self, platform: &str) -> Option<&ServerDownload> {
        self.download.as_ref()?.get(platform)
    }
}

/// One language of a grammar package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrammarLanguage {
    pub id: LanguageId,
    /// The C function returning the `TSLanguage` (`tree_sitter_typescript`).
    pub symbol: String,
    pub tags_query: Artifact,
}

/// A grammar package: one dylib, one or more languages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrammarSpec {
    pub package: String,
    pub version: String,
    pub languages: Vec<GrammarLanguage>,
    /// Keyed by platform.
    pub downloads: BTreeMap<String, Artifact>,
}

impl GrammarSpec {
    /// `<package>-<version>`: its directory under `grammars/`.
    pub fn dir_name(&self) -> String {
        format!("{}-{}", self.package, self.version)
    }

    pub fn language(&self, id: &LanguageId) -> Option<&GrammarLanguage> {
        self.languages.iter().find(|l| &l.id == id)
    }
}

/// A language as the rest of the app sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct LanguageSpec {
    pub id: LanguageId,
    pub globs: Vec<String>,
    /// LSP `languageId` when it differs from `id` (`tsx` → `typescriptreact`).
    pub lsp_id: Option<String>,
    /// A language whose tags query goes in front of this one's: upstream
    /// TypeScript's `tags.scm` only adds to JavaScript's (it has no classes
    /// or functions of its own). [`Toolchain::ensure_grammar`] hands over
    /// each language's own query; the code-intel facade joins the two.
    ///
    /// [`Toolchain::ensure_grammar`]: crate::Toolchain::ensure_grammar
    pub tags_extends: Option<LanguageId>,
    /// The package that parses it, if CI built one.
    pub grammar: Option<GrammarSpec>,
    /// In preference order.
    pub servers: Vec<ServerSpec>,
}

impl LanguageSpec {
    pub fn lsp_language_id(&self) -> &str {
        self.lsp_id.as_deref().unwrap_or(self.id.as_str())
    }

    /// Whether `path` (repo-relative, `/` separators) belongs to it.
    pub fn matches(&self, path: &str) -> bool {
        let name = path.rsplit('/').next().unwrap_or(path);
        self.globs.iter().any(|g| {
            if g.contains('/') {
                glob_match(g, path)
            } else {
                glob_match(g, name)
            }
        })
    }
}

/// The validated manifest.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Manifest {
    languages: Vec<LanguageSpec>,
    servers: Vec<ServerSpec>,
    grammars: Vec<GrammarSpec>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLanguages {
    #[serde(default)]
    servers: Vec<ServerSpec>,
    languages: Vec<RawLanguage>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLanguage {
    id: LanguageId,
    globs: Vec<String>,
    #[serde(default)]
    lsp_id: Option<String>,
    #[serde(default)]
    tags_extends: Option<LanguageId>,
    #[serde(default)]
    servers: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGrammars {
    schema: u32,
    /// The release the URLs point at; informational.
    #[serde(default)]
    #[allow(dead_code)]
    release_tag: Option<String>,
    grammars: Vec<GrammarSpec>,
}

impl Manifest {
    /// The manifest compiled into this build. Warnings are logged; a builtin
    /// that does not parse at all (a bug, caught by tests) yields an empty
    /// manifest rather than a crash.
    pub fn builtin() -> Self {
        match Self::parse(BUILTIN_LANGUAGES, BUILTIN_GRAMMARS) {
            Ok((m, warnings)) => {
                for w in warnings {
                    tracing::warn!(target: "code_intel", "manifest: {w}");
                }
                m
            }
            Err(e) => {
                tracing::error!(target: "code_intel", "builtin manifest: {e}");
                Self::default()
            }
        }
    }

    /// Parse and validate. Invalid entries are dropped and described in the
    /// returned warnings; only a file that does not parse is an error.
    pub fn parse(
        languages_yaml: &str,
        grammars_json: &str,
    ) -> CodeIntelResult<(Self, Vec<String>)> {
        let raw: RawLanguages = serde_yaml::from_str(languages_yaml)
            .map_err(|e| CodeIntelError::Other(format!("languages manifest: {e}")))?;
        let grammars: RawGrammars = serde_json::from_str(grammars_json)
            .map_err(|e| CodeIntelError::Other(format!("grammars manifest: {e}")))?;
        if grammars.schema != GRAMMARS_SCHEMA {
            return Err(CodeIntelError::Other(format!(
                "grammars manifest: schema {} (this build reads {GRAMMARS_SCHEMA})",
                grammars.schema
            )));
        }

        let mut warnings = Vec::new();
        let servers = validate_servers(raw.servers, &mut warnings);
        let grammars = validate_grammars(grammars.grammars, &mut warnings);

        let mut seen = HashSet::new();
        let mut languages = Vec::new();
        for lang in raw.languages {
            if !seen.insert(lang.id.clone()) {
                warnings.push(format!("language {}: duplicate, dropped", lang.id));
                continue;
            }
            if lang.globs.is_empty() {
                warnings.push(format!("language {}: no globs, dropped", lang.id));
                continue;
            }
            let mut specs = Vec::new();
            for name in &lang.servers {
                match servers.iter().find(|s| &s.name == name) {
                    Some(s) => specs.push(s.clone()),
                    None => warnings.push(format!("language {}: unknown server {name}", lang.id)),
                }
            }
            let grammar = grammars
                .iter()
                .find(|g| g.language(&lang.id).is_some())
                .cloned();
            languages.push(LanguageSpec {
                id: lang.id,
                globs: lang.globs,
                lsp_id: lang.lsp_id,
                tags_extends: lang.tags_extends,
                grammar,
                servers: specs,
            });
        }
        // Checked once every id is known. One level only: a base that
        // extends another is refused rather than chained.
        let ids: HashSet<LanguageId> = languages.iter().map(|l| l.id.clone()).collect();
        let extending: HashSet<LanguageId> = languages
            .iter()
            .filter(|l| l.tags_extends.is_some())
            .map(|l| l.id.clone())
            .collect();
        for l in &mut languages {
            if let Some(base) = &l.tags_extends
                && (!ids.contains(base) || extending.contains(base))
            {
                warnings.push(format!(
                    "language {}: tags_extends {base} is unknown or extends another, ignored",
                    l.id
                ));
                l.tags_extends = None;
            }
        }
        for g in &grammars {
            for l in &g.languages {
                if !seen.contains(&l.id) {
                    warnings.push(format!("grammar {}: unknown language {}", g.package, l.id));
                }
            }
        }

        Ok((
            Self {
                languages,
                servers,
                grammars,
            },
            warnings,
        ))
    }

    pub fn languages(&self) -> &[LanguageSpec] {
        &self.languages
    }

    pub fn language(&self, id: &LanguageId) -> Option<&LanguageSpec> {
        self.languages.iter().find(|l| &l.id == id)
    }

    /// The first language whose globs match `path`.
    pub fn language_for_path(&self, path: &str) -> Option<&LanguageSpec> {
        self.languages.iter().find(|l| l.matches(path))
    }

    pub fn servers(&self) -> &[ServerSpec] {
        &self.servers
    }

    pub fn server(&self, name: &str) -> Option<&ServerSpec> {
        self.servers.iter().find(|s| s.name == name)
    }

    pub fn grammars(&self) -> &[GrammarSpec] {
        &self.grammars
    }
}

fn validate_servers(raw: Vec<ServerSpec>, warnings: &mut Vec<String>) -> Vec<ServerSpec> {
    let mut out: Vec<ServerSpec> = Vec::new();
    for mut s in raw {
        let name = s.name.clone();
        if out.iter().any(|o| o.name == name) {
            warnings.push(format!("server {name}: duplicate, dropped"));
            continue;
        }
        if s.launch.is_empty() || !s.launch.iter().any(|a| a == BIN_PLACEHOLDER) {
            warnings.push(format!(
                "server {name}: launch has no {BIN_PLACEHOLDER}, dropped"
            ));
            continue;
        }
        s.detect.retain(|argv| {
            let ok = argv
                .first()
                .is_some_and(|p| !p.is_empty() && !p.contains('/'));
            if !ok {
                warnings.push(format!("server {name}: bad detect {argv:?}, ignored"));
            }
            ok
        });
        if let Some(downloads) = s.download.take() {
            let mut kept = BTreeMap::new();
            for (platform, mut d) in downloads {
                let what = format!("server {name} [{platform}]");
                if s.version.as_deref().is_none_or(|v| !is_safe_segment(v)) {
                    warnings.push(format!("{what}: download without a valid version, skipped"));
                    continue;
                }
                if !is_safe_relative(&d.bin) {
                    warnings.push(format!("{what}: bad bin path {:?}, skipped", d.bin));
                    continue;
                }
                match check_artifact(&d.url, &d.sha256) {
                    Ok(sha) => d.sha256 = sha,
                    Err(e) => {
                        warnings.push(format!("{what}: {e}, skipped"));
                        continue;
                    }
                }
                kept.insert(platform, d);
            }
            s.download = (!kept.is_empty()).then_some(kept);
        }
        if s.detect.is_empty() && s.download.is_none() {
            warnings.push(format!("server {name}: no way to find or download it"));
        }
        out.push(s);
    }
    out
}

fn validate_grammars(raw: Vec<GrammarSpec>, warnings: &mut Vec<String>) -> Vec<GrammarSpec> {
    let mut out: Vec<GrammarSpec> = Vec::new();
    for mut g in raw {
        let what = format!("grammar {}", g.package);
        if !is_safe_segment(&g.package) || !is_safe_segment(&g.version) {
            warnings.push(format!("{what}: bad package or version, dropped"));
            continue;
        }
        g.languages.retain_mut(|l| {
            if !is_safe_segment(l.id.as_str()) || !is_c_identifier(&l.symbol) {
                warnings.push(format!(
                    "{what}: bad language {} / {}, dropped",
                    l.id, l.symbol
                ));
                return false;
            }
            match check_artifact(&l.tags_query.url, &l.tags_query.sha256) {
                Ok(sha) => {
                    l.tags_query.sha256 = sha;
                    true
                }
                Err(e) => {
                    warnings.push(format!("{what}: tags query of {}: {e}, dropped", l.id));
                    false
                }
            }
        });
        g.downloads
            .retain(|platform, a| match check_artifact(&a.url, &a.sha256) {
                Ok(sha) => {
                    a.sha256 = sha;
                    true
                }
                Err(e) => {
                    warnings.push(format!("{what} [{platform}]: {e}, skipped"));
                    false
                }
            });
        if g.languages.is_empty() {
            warnings.push(format!("{what}: no valid language, dropped"));
            continue;
        }
        let taken = g.languages.iter().find(|l| {
            out.iter()
                .any(|o: &GrammarSpec| o.language(&l.id).is_some())
        });
        if let Some(l) = taken {
            warnings.push(format!(
                "{what}: language {} already has a grammar, dropped",
                l.id
            ));
            continue;
        }
        out.push(g);
    }
    out
}

/// A valid artifact has an allowed URL and a sha256; returns the hash in
/// lowercase.
fn check_artifact(url: &str, sha256: &str) -> Result<String, String> {
    let parsed = url::Url::parse(url).map_err(|e| format!("bad url {url:?}: {e}"))?;
    if !url_allowed(&parsed) {
        return Err(format!("url must be https: {url}"));
    }
    let sha = sha256.trim().to_ascii_lowercase();
    if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("missing or malformed sha256".into());
    }
    Ok(sha)
}

/// https, or plain http to the loopback interface (local mirrors, tests);
/// the sha256 guards content either way.
pub(crate) fn url_allowed(url: &url::Url) -> bool {
    match url.scheme() {
        "https" => url.host_str().is_some(),
        "http" => {
            matches!(
                url.host(),
                Some(url::Host::Ipv4(ip)) if ip.is_loopback()
            ) || matches!(url.host(), Some(url::Host::Ipv6(ip)) if ip.is_loopback())
                || url.host_str() == Some("localhost")
        }
        _ => false,
    }
}

/// One path segment usable as a directory name.
fn is_safe_segment(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && !s.ends_with(".partial")
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+'))
}

/// A relative path that stays where it is put: no root, no `..`.
pub(crate) fn is_safe_relative(p: &str) -> bool {
    !p.is_empty()
        && !p.contains('\\')
        && Path::new(p)
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

fn is_c_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const SHA: &str = "8fb90cb58f5910d8d8b24ac1a93cca49f053365cbf0eb3c682144ad0a0027dcf";

    #[test]
    fn builtin_parses_without_warnings() {
        let (m, warnings) = Manifest::parse(BUILTIN_LANGUAGES, BUILTIN_GRAMMARS).unwrap();
        assert_eq!(warnings, Vec::<String>::new());
        for id in [
            "rust",
            "typescript",
            "tsx",
            "javascript",
            "python",
            "go",
            "c",
            "cpp",
            "swift",
            "json",
            "yaml",
            "toml",
            "markdown",
        ] {
            assert!(m.language(&LanguageId::new(id)).is_some(), "{id}");
        }
        let ra = m.server("rust-analyzer").unwrap();
        assert!(ra.download_for("darwin-arm64").is_some());
        assert_eq!(
            m.language(&LanguageId::new("python")).unwrap().servers[0].name,
            "ty"
        );
        assert_eq!(
            m.language(&LanguageId::new("tsx"))
                .unwrap()
                .lsp_language_id(),
            "typescriptreact"
        );
        for id in ["typescript", "tsx"] {
            assert_eq!(
                m.language(&LanguageId::new(id)).unwrap().tags_extends,
                Some(LanguageId::new("javascript")),
                "{id}"
            );
        }
    }

    #[test]
    fn tags_extends_must_name_a_plain_language() {
        let yaml = r#"
servers:
  - name: s
    detect: [["s"]]
    launch: ["{bin}"]
languages:
  - { id: base, globs: ["*.b"] }
  - { id: ok, globs: ["*.o"], tags_extends: base }
  - { id: unknown, globs: ["*.u"], tags_extends: nope }
  - { id: chained, globs: ["*.c"], tags_extends: ok }
  - { id: self, globs: ["*.s"], tags_extends: self, servers: [s] }
"#;
        let (m, warnings) = Manifest::parse(yaml, r#"{"schema":1,"grammars":[]}"#).unwrap();
        let ext = |id: &str| {
            m.language(&LanguageId::new(id))
                .unwrap()
                .tags_extends
                .clone()
        };
        assert_eq!(ext("ok"), Some(LanguageId::new("base")));
        assert_eq!(ext("unknown"), None);
        assert_eq!(ext("chained"), None);
        assert_eq!(ext("self"), None);
        assert_eq!(warnings.len(), 3, "{warnings:?}");
    }

    #[test]
    fn download_without_sha_is_skipped_with_warning() {
        let yaml = format!(
            r#"
servers:
  - name: good
    version: "1.0"
    detect: [["good", "--version"]]
    download:
      darwin-arm64: {{ url: "https://example.com/a.gz", sha256: "{SHA}", format: gz, bin: good }}
      darwin-x64:   {{ url: "https://example.com/b.gz", sha256: "", format: gz, bin: good }}
      linux-x64:    {{ url: "http://example.com/c.gz", sha256: "{SHA}", format: gz, bin: good }}
    launch: ["{{bin}}"]
  - name: nobin
    launch: ["nobin"]
languages:
  - id: x
    globs: ["*.x"]
    servers: [good, nobin]
"#
        );
        let (m, warnings) = Manifest::parse(&yaml, r#"{"schema":1,"grammars":[]}"#).unwrap();
        let good = m.server("good").unwrap();
        let platforms: Vec<_> = good.download.as_ref().unwrap().keys().cloned().collect();
        assert_eq!(platforms, vec!["darwin-arm64".to_string()]);
        assert!(m.server("nobin").is_none());
        assert_eq!(m.language(&LanguageId::new("x")).unwrap().servers.len(), 1);
        assert_eq!(warnings.len(), 4, "{warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("nobin: launch")));
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("darwin-x64") && w.contains("sha256"))
        );
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("linux-x64") && w.contains("https"))
        );
        assert!(warnings.iter().any(|w| w.contains("unknown server nobin")));
    }

    #[test]
    fn grammar_validation() {
        let yaml = "languages:\n  - id: json\n    globs: ['*.json']\n";
        let json = format!(
            r#"{{"schema":1,"grammars":[
              {{"package":"tree-sitter-json","version":"0.24.8",
                "languages":[{{"id":"json","symbol":"tree_sitter_json",
                  "tags_query":{{"url":"https://e.com/json.scm","sha256":"{SHA}"}}}}],
                "downloads":{{"darwin-arm64":{{"url":"https://e.com/j.dylib","sha256":"{}"}},
                              "darwin-x64":{{"url":"https://e.com/j.dylib","sha256":"nope"}}}}}},
              {{"package":"tree-sitter-bad","version":"1",
                "languages":[{{"id":"json","symbol":"tree_sitter_json; rm","tags_query":{{"url":"https://e.com/x","sha256":"{SHA}"}}}}],
                "downloads":{{}}}}
            ]}}"#,
            SHA.to_uppercase()
        );
        let (m, warnings) = Manifest::parse(yaml, &json).unwrap();
        let g = m
            .language(&LanguageId::new("json"))
            .unwrap()
            .grammar
            .clone()
            .unwrap();
        assert_eq!(g.dir_name(), "tree-sitter-json-0.24.8");
        assert_eq!(g.downloads.len(), 1);
        assert_eq!(g.downloads["darwin-arm64"].sha256, SHA);
        assert_eq!(m.grammars().len(), 1);
        assert_eq!(warnings.len(), 3, "{warnings:?}");
    }

    #[test]
    fn wrong_grammar_schema_is_an_error() {
        let err = Manifest::parse("languages: []", r#"{"schema":2,"grammars":[]}"#).unwrap_err();
        assert!(err.to_string().contains("schema 2"));
    }

    #[test]
    fn safe_paths() {
        assert!(is_safe_relative("a/b/c"));
        assert!(is_safe_relative("./a"));
        assert!(!is_safe_relative("../a"));
        assert!(!is_safe_relative("a/../../b"));
        assert!(!is_safe_relative("/abs"));
        assert!(!is_safe_relative("a\\b"));
        assert!(!is_safe_relative(""));
    }
}
