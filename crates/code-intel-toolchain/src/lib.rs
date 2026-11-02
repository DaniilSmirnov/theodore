//! Code-intel toolchain (docs/code-intel.md §3–§5): the manifest of
//! languages, which ones a worktree uses, and getting their grammars and
//! language servers onto disk — downloaded, checked by sha256, unpacked
//! safely — then loaded (grammars) or resolved to a launch command (servers).
//!
//! ```text
//! paths ─▶ detect_languages ─▶ [(rust, 120), (typescript, 40)]
//!
//! ensure_grammar(lang) ─▶ memory? ─▶ disk + sha ok? ─▶ download ─▶ dlopen ─▶ ABI ─▶ LoadedGrammar
//!
//! server_for(lang) ─▶ resolve_server: PATH · rustup · xcrun · node_modules (Trusted) · downloaded
//!                  └─ none ─▶ (spec, Missing) ─▶ consent ─▶ install_server ─▶ launch_config
//! ```
//!
//! On disk, under `<data dir>/code-intel/`:
//!
//! ```text
//! grammars/<package>-<version>/   grammar.dylib, <language>.tags.scm
//! servers/<name>/<version>/       the unpacked server
//! servers/<name>/<version>.partial  an install in progress (renamed when done)
//! tmp/                            downloads in flight
//! ```

mod detect;
mod download;
mod grammar;
mod manifest;
mod server;
mod unpack;

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use theodore_ports::code_intel::{
    CodeIntelError, CodeIntelResult, InstallState, LanguageId, TrustMode,
};

pub use detect::git_ls_files;
pub use download::http_client;
pub use grammar::{LoadedGrammar, check_abi, supported_abi};
pub use manifest::{
    ArchiveFormat, Artifact, BIN_PLACEHOLDER, GRAMMARS_SCHEMA, GrammarLanguage, GrammarSpec,
    LanguageSpec, Manifest, ServerDownload, ServerSpec,
};
pub use server::{LaunchConfig, PROBE_TIMEOUT, ResolvedServer, ServerOrigin, launch_config};

use crate::download::{fetch_verified, io_err};
use crate::server::ProbeEnv;

/// This machine's manifest platform key: `darwin-arm64`, `darwin-x64`,
/// `linux-x64`, `linux-arm64`, `windows-x64`.
pub fn current_platform() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "darwin-arm64",
        ("macos", "x86_64") => "darwin-x64",
        ("linux", "x86_64") => "linux-x64",
        ("linux", "aarch64") => "linux-arm64",
        ("windows", "x86_64") => "windows-x64",
        _ => "unknown",
    }
}

/// Grammars and servers for code intelligence. Cheap to clone; clones share
/// caches and in-flight downloads.
#[derive(Clone)]
pub struct Toolchain {
    inner: Arc<Inner>,
}

struct Inner {
    root: PathBuf,
    manifest: Manifest,
    platform: String,
    http: reqwest::Client,
    probe: ProbeEnv,
    grammars: Mutex<HashMap<LanguageId, LoadedGrammar>>,
    /// Single flight per install key (`grammar:…`, `server:…`).
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Install keys downloading now.
    busy: Mutex<HashSet<String>>,
    /// Install key → last error, until a retry succeeds.
    failed: Mutex<HashMap<String, String>>,
}

/// Options for [`Toolchain`]; defaults fit the app.
pub struct ToolchainBuilder {
    root: PathBuf,
    manifest: Option<Manifest>,
    platform: Option<String>,
    http: Option<reqwest::Client>,
    path: Option<OsString>,
    probe_timeout: Duration,
}

impl ToolchainBuilder {
    /// Default: [`Manifest::builtin`].
    pub fn manifest(mut self, manifest: Manifest) -> Self {
        self.manifest = Some(manifest);
        self
    }

    /// Default: [`current_platform`].
    pub fn platform(mut self, platform: impl Into<String>) -> Self {
        self.platform = Some(platform.into());
        self
    }

    /// Default: [`http_client`]. A custom client must not follow redirects
    /// itself, or the host check is bypassed.
    pub fn http(mut self, client: reqwest::Client) -> Self {
        self.http = Some(client);
        self
    }

    /// `$PATH` for finding servers. Default: the process's.
    pub fn search_path(mut self, path: impl Into<OsString>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// Default: [`PROBE_TIMEOUT`].
    pub fn probe_timeout(mut self, timeout: Duration) -> Self {
        self.probe_timeout = timeout;
        self
    }

    pub fn build(self) -> Toolchain {
        Toolchain {
            inner: Arc::new(Inner {
                root: self.root,
                manifest: self.manifest.unwrap_or_else(Manifest::builtin),
                platform: self.platform.unwrap_or_else(|| current_platform().into()),
                http: self.http.unwrap_or_else(http_client),
                probe: ProbeEnv {
                    path: self.path,
                    timeout: self.probe_timeout,
                    neutral_dir: std::env::temp_dir(),
                },
                grammars: Mutex::default(),
                locks: Mutex::default(),
                busy: Mutex::default(),
                failed: Mutex::default(),
            }),
        }
    }
}

impl Toolchain {
    /// `root` is `<data dir>/code-intel`; created on first install.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::builder(root).build()
    }

    pub fn builder(root: impl Into<PathBuf>) -> ToolchainBuilder {
        ToolchainBuilder {
            root: root.into(),
            manifest: None,
            platform: None,
            http: None,
            path: None,
            probe_timeout: PROBE_TIMEOUT,
        }
    }

    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    pub fn manifest(&self) -> &Manifest {
        &self.inner.manifest
    }

    pub fn platform(&self) -> &str {
        &self.inner.platform
    }

    /// Files per language among repo-relative `paths`, most first.
    pub fn detect_languages<S: AsRef<str>>(&self, paths: &[S]) -> Vec<(LanguageId, usize)> {
        self.inner.manifest.detect(paths)
    }

    // ── grammars ──────────────────────────────────────────────────────────

    pub fn grammar_state(&self, language: &LanguageId) -> InstallState {
        let Some(spec) = self.grammar_spec(language) else {
            return InstallState::Unavailable;
        };
        if !spec.downloads.contains_key(&self.inner.platform) {
            return InstallState::Unavailable;
        }
        let key = grammar_key(spec);
        if lock(&self.inner.busy).contains(&key) {
            return InstallState::Downloading;
        }
        if lock(&self.inner.grammars).contains_key(language) {
            return InstallState::Installed {
                version: spec.version.clone(),
            };
        }
        // Before the disk check: a grammar on disk that fails to load (ABI)
        // is failed, not installed.
        if let Some(error) = lock(&self.inner.failed).get(&key) {
            return InstallState::Failed {
                error: error.clone(),
            };
        }
        if self.grammar_dir(spec).is_dir() {
            InstallState::Installed {
                version: spec.version.clone(),
            }
        } else {
            InstallState::Missing
        }
    }

    /// The language's grammar, downloaded if needed and loaded once per
    /// process. Concurrent calls share one download.
    pub async fn ensure_grammar(&self, language: &LanguageId) -> CodeIntelResult<LoadedGrammar> {
        if let Some(g) = lock(&self.inner.grammars).get(language) {
            return Ok(g.clone());
        }
        let spec = self.grammar_spec(language).cloned().ok_or_else(|| {
            CodeIntelError::Unsupported(format!("{language}: no grammar in the manifest"))
        })?;
        let platform = self.inner.platform.clone();
        if !spec.downloads.contains_key(&platform) {
            return Err(CodeIntelError::Unsupported(format!(
                "{}: no build for {platform}",
                spec.package
            )));
        }
        let key = grammar_key(&spec);
        let flight = self.flight(&key);
        let _flight = flight.lock().await;
        if let Some(g) = lock(&self.inner.grammars).get(language) {
            return Ok(g.clone());
        }

        let result = self.load_or_install_grammar(&spec, language, &key).await;
        self.record(&key, &result);
        let loaded = result?;
        lock(&self.inner.grammars).insert(language.clone(), loaded.clone());
        Ok(loaded)
    }

    async fn load_or_install_grammar(
        &self,
        spec: &GrammarSpec,
        language: &LanguageId,
        key: &str,
    ) -> CodeIntelResult<LoadedGrammar> {
        let dir = self.grammar_dir(spec);
        let platform = self.inner.platform.clone();
        let verified = {
            let (dir, spec, platform) = (dir.clone(), spec.clone(), platform.clone());
            tokio::task::spawn_blocking(move || {
                dir.is_dir() && grammar::verify(&dir, &spec, &platform)
            })
            .await
            .unwrap_or(false)
        };
        if !verified {
            if dir.is_dir() {
                tracing::warn!(target: "code_intel", dir = %dir.display(), "grammar cache does not match the manifest; downloading again");
            }
            let _busy = Busy::mark(&self.inner, key);
            grammar::install(&self.inner.http, &self.inner.root, spec, &platform, &dir).await?;
        }
        let (spec, language) = (spec.clone(), language.clone());
        tokio::task::spawn_blocking(move || grammar::load(&dir, &spec, &language))
            .await
            .map_err(|e| CodeIntelError::Other(format!("grammar load: {e}")))?
    }

    fn grammar_spec(&self, language: &LanguageId) -> Option<&GrammarSpec> {
        self.inner.manifest.language(language)?.grammar.as_ref()
    }

    fn grammar_dir(&self, spec: &GrammarSpec) -> PathBuf {
        self.inner.root.join("grammars").join(spec.dir_name())
    }

    // ── servers ───────────────────────────────────────────────────────────

    /// The server that would serve `language`, and its state: the first
    /// (preferred, then manifest order) found on the machine or downloaded;
    /// else the first with a download for this platform; else the first,
    /// `Unavailable`. `None` when the manifest lists no server.
    pub async fn server_for(
        &self,
        language: &LanguageId,
        prefer: Option<&str>,
        worktree: &Path,
        trust: TrustMode,
    ) -> Option<(ServerSpec, InstallState)> {
        let lang = self.inner.manifest.language(language)?;
        let mut order: Vec<&ServerSpec> = lang.servers.iter().collect();
        if let Some(i) = prefer.and_then(|p| order.iter().position(|s| s.name == p)) {
            let preferred = order.remove(i);
            order.insert(0, preferred);
        }
        for spec in &order {
            if let Some(r) = self.resolve_server(spec, worktree, trust).await {
                return Some(((*spec).clone(), r.install_state()));
            }
        }
        if let Some(spec) = order
            .iter()
            .find(|s| s.download_for(&self.inner.platform).is_some())
        {
            return Some(((*spec).clone(), self.download_state(spec)));
        }
        order
            .first()
            .map(|s| ((*s).clone(), InstallState::Unavailable))
    }

    /// The binary to run for `spec`: on the machine first (docs §4.3), then
    /// Theodore's download. `worktree`'s own binaries count only when
    /// `trust` is `Trusted`.
    pub async fn resolve_server(
        &self,
        spec: &ServerSpec,
        worktree: &Path,
        trust: TrustMode,
    ) -> Option<ResolvedServer> {
        if let Some(found) = server::find_system(spec, worktree, trust, &self.inner.probe).await {
            return Some(found);
        }
        self.downloaded(spec)
    }

    /// Download, verify and unpack `spec` for this platform. Already
    /// installed → returns it. Concurrent calls share one download.
    pub async fn install_server(&self, spec: &ServerSpec) -> CodeIntelResult<ResolvedServer> {
        let platform = &self.inner.platform;
        let (Some(dl), Some(version)) = (spec.download_for(platform), spec.version.as_deref())
        else {
            return Err(CodeIntelError::Unsupported(format!(
                "{}: no download for {platform}",
                spec.name
            )));
        };
        let key = server_key(&spec.name, version);
        let flight = self.flight(&key);
        let _flight = flight.lock().await;
        if let Some(found) = self.downloaded(spec) {
            return Ok(found);
        }

        let result = async {
            let _busy = Busy::mark(&self.inner, &key);
            let tmp = fetch_verified(
                &self.inner.http,
                &dl.artifact(),
                &self.inner.root.join("tmp"),
            )
            .await?;
            let dir = self.server_dir(&spec.name, version);
            let (format, bin) = (dl.format, dl.bin.clone());
            tokio::task::spawn_blocking(move || install_unpacked(tmp, format, &dir, &bin))
                .await
                .map_err(|e| CodeIntelError::Other(format!("server install: {e}")))??;
            self.downloaded(spec).ok_or_else(|| {
                CodeIntelError::Other(format!("{}: installed but not found", spec.name))
            })
        }
        .await;
        self.record(&key, &result);
        if let Ok(r) = &result {
            tracing::info!(target: "code_intel", server = %spec.name, version, bin = %r.bin.display(), "server installed");
        }
        result
    }

    /// Command line and init options for a resolved server under `trust`.
    pub fn launch_config(
        &self,
        resolved: &ResolvedServer,
        spec: &ServerSpec,
        trust: TrustMode,
    ) -> LaunchConfig {
        launch_config(resolved, spec, trust)
    }

    fn downloaded(&self, spec: &ServerSpec) -> Option<ResolvedServer> {
        let dl = spec.download_for(&self.inner.platform)?;
        let version = spec.version.as_deref()?;
        let bin = self.server_dir(&spec.name, version).join(&dl.bin);
        bin.is_file().then(|| ResolvedServer {
            name: spec.name.clone(),
            argv: server::launch_argv(spec, &bin),
            bin,
            origin: ServerOrigin::Downloaded {
                version: version.to_string(),
            },
        })
    }

    fn download_state(&self, spec: &ServerSpec) -> InstallState {
        let key = server_key(&spec.name, spec.version.as_deref().unwrap_or_default());
        if lock(&self.inner.busy).contains(&key) {
            return InstallState::Downloading;
        }
        match lock(&self.inner.failed).get(&key) {
            Some(error) => InstallState::Failed {
                error: error.clone(),
            },
            None => InstallState::Missing,
        }
    }

    fn server_dir(&self, name: &str, version: &str) -> PathBuf {
        self.inner.root.join("servers").join(name).join(version)
    }

    // ── cleanup ───────────────────────────────────────────────────────────

    /// Remove what is no longer needed. Blocking (file system only).
    ///
    /// - servers: for each name in `keep`, every other version of it (call
    ///   once the new version has started, so a broken update can still
    ///   fall back). Servers not named in `keep` are left alone.
    /// - grammars: every version the manifest no longer lists.
    /// - interrupted installs and downloads older than a day.
    pub fn gc(&self, keep: &[(&str, &str)]) {
        let busy = lock(&self.inner.busy).clone();
        let servers = self.inner.root.join("servers");
        for name_dir in read_dirs(&servers) {
            let name = file_name(&name_dir);
            let kept: Vec<&str> = keep
                .iter()
                .filter(|(n, _)| *n == name)
                .map(|(_, v)| *v)
                .collect();
            for ver_dir in read_dirs(&name_dir) {
                let ver = file_name(&ver_dir);
                let remove = match ver.strip_suffix(".partial") {
                    Some(v) => !busy.contains(&server_key(&name, v)),
                    None => !kept.is_empty() && !kept.contains(&ver.as_str()),
                };
                if remove {
                    remove_logged(&ver_dir);
                }
            }
        }

        let current: HashSet<String> = self
            .inner
            .manifest
            .grammars()
            .iter()
            .map(GrammarSpec::dir_name)
            .collect();
        for dir in read_dirs(&self.inner.root.join("grammars")) {
            let name = file_name(&dir);
            let remove = match name.strip_suffix(".partial") {
                Some(n) => !busy.contains(&format!("grammar:{n}")),
                None => !current.contains(&name),
            };
            if remove {
                remove_logged(&dir);
            }
        }

        let day_ago = SystemTime::now() - Duration::from_secs(24 * 3600);
        if let Ok(entries) = std::fs::read_dir(self.inner.root.join("tmp")) {
            for e in entries.flatten() {
                let old = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .is_ok_and(|t| t < day_ago);
                if old {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }

    // ── bookkeeping ───────────────────────────────────────────────────────

    fn flight(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        lock(&self.inner.locks)
            .entry(key.to_string())
            .or_default()
            .clone()
    }

    fn record<T>(&self, key: &str, result: &CodeIntelResult<T>) {
        let mut failed = lock(&self.inner.failed);
        match result {
            Ok(_) => {
                failed.remove(key);
            }
            Err(e) => {
                tracing::warn!(target: "code_intel", key, error = %e, "install failed");
                failed.insert(key.to_string(), e.to_string());
            }
        }
    }
}

/// Unpack into `<dir>.partial`, sign the binary, rename to `dir`.
fn install_unpacked(
    archive: tempfile::NamedTempFile,
    format: ArchiveFormat,
    dir: &Path,
    bin: &str,
) -> CodeIntelResult<()> {
    let partial = grammar::partial_of(dir);
    if partial.exists() {
        std::fs::remove_dir_all(&partial).map_err(|e| io_err(&partial, e))?;
    }
    std::fs::create_dir_all(&partial).map_err(|e| io_err(&partial, e))?;
    let result = unpack::unpack(format, archive.path(), &partial, bin).and_then(|bin_path| {
        sign_adhoc(&bin_path);
        if dir.exists() {
            std::fs::remove_dir_all(dir).map_err(|e| io_err(dir, e))?;
        }
        std::fs::rename(&partial, dir).map_err(|e| io_err(dir, e))
    });
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&partial);
    }
    result
}

/// An unsigned arm64 binary does not start on macOS: give it an ad-hoc
/// signature. Best effort; elsewhere a no-op.
fn sign_adhoc(bin: &Path) {
    if !cfg!(target_os = "macos") || !is_mach_o(bin) {
        return;
    }
    let codesign = |args: &[&str]| {
        std::process::Command::new("codesign")
            .args(args)
            .arg(bin)
            .stdin(std::process::Stdio::null())
            .output()
    };
    if codesign(&["-v"]).is_ok_and(|o| o.status.success()) {
        return;
    }
    match codesign(&["-s", "-"]) {
        Ok(o) if o.status.success() => {
            tracing::info!(target: "code_intel", bin = %bin.display(), "signed ad-hoc");
        }
        Ok(o) => tracing::warn!(
            target: "code_intel", bin = %bin.display(),
            stderr = %String::from_utf8_lossy(&o.stderr).trim(), "codesign failed"
        ),
        Err(e) => {
            tracing::warn!(target: "code_intel", bin = %bin.display(), error = %e, "codesign failed")
        }
    }
}

fn is_mach_o(path: &Path) -> bool {
    use std::io::Read;
    let mut magic = [0u8; 4];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut magic))
        .is_ok()
        && matches!(
            magic,
            [0xcf, 0xfa, 0xed, 0xfe] | [0xce, 0xfa, 0xed, 0xfe] | [0xca, 0xfe, 0xba, 0xbe]
        )
}

/// Marks an install key busy while alive.
struct Busy<'a> {
    inner: &'a Inner,
    key: String,
}

impl<'a> Busy<'a> {
    fn mark(inner: &'a Inner, key: &str) -> Self {
        lock(&inner.busy).insert(key.to_string());
        Self {
            inner,
            key: key.to_string(),
        }
    }
}

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        lock(&self.inner.busy).remove(&self.key);
    }
}

fn grammar_key(spec: &GrammarSpec) -> String {
    format!("grammar:{}", spec.dir_name())
}

fn server_key(name: &str, version: &str) -> String {
    format!("server:{name}@{version}")
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn read_dirs(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default()
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn remove_logged(dir: &Path) {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => tracing::info!(target: "code_intel", dir = %dir.display(), "removed"),
        Err(e) => {
            tracing::warn!(target: "code_intel", dir = %dir.display(), error = %e, "remove failed")
        }
    }
}
