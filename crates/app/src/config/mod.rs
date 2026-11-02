//! Project config (docs/config.md): one YAML file, three layers, merged key
//! by key (later wins; lists replace).
//!
//! ```text
//! ~/.theodore/config.yaml                global ─┐
//! <repo>/.theodore/config.yaml           repo   ─┼─▶ merge ─▶ ProjectConfig
//!   (or legacy <repo>/.review/config.yaml)       │      └──▶ provenance per key
//! <repo>/.theodore/config.local.yaml     local  ─┘
//! ```
//!
//! Pure: reading the files is the caller's job (`load`), so layering,
//! the legacy flat keys and unknown-key detection are testable on strings.

pub mod editor;
pub mod ports;
pub mod trust;
pub mod vars;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Map, Value};
use theodore_contract::{ConfigEntry, ConfigFile, ConfigLayer, SpaceCommandKind};

pub const GLOBAL_FILE: &str = "config.yaml";
pub const REPO_FILE: &str = ".theodore/config.yaml";
pub const LOCAL_FILE: &str = ".theodore/config.local.yaml";
/// Deprecated: read as the repo layer when `.theodore/config.yaml` is absent.
pub const LEGACY_REPO_FILE: &str = ".review/config.yaml";

pub const DEFAULT_SETUP_TIMEOUT_S: u64 = 600;
pub const DEFAULT_CHECK_TIMEOUT_S: u64 = 600;
pub const DEFAULT_ARCHIVE_TIMEOUT_S: u64 = 120;
/// More would be a typo, not a plan.
pub const MAX_PORTS: u16 = 16;
/// `spaces.ports` unset: a space gets this many free ports so `run` has
/// one to bind without asking (explicit `ports: 0` turns it off).
pub const DEFAULT_PORTS: u16 = 2;
/// `spaces.copy` unset: a space's worktree gets the project's env files
/// (explicit `copy: []` turns it off).
pub const DEFAULT_COPY: &str = ".env*";

/// The typed, merged config. Unknown keys are dropped here (and reported
/// by `Merged::unknown`).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct ProjectConfig {
    pub review: ReviewConfig,
    pub spaces: SpacesConfig,
    pub code_intel: CodeIntelConfig,
}

/// `review:` (docs/config.md §1); applied through
/// `usecases::review_config`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct ReviewConfig {
    /// `agents.grouping`, `agents.chat`, `agents.skills`, `agents.fix`, …
    /// as setting values, keyed without the `agents.` prefix.
    pub agents: Map<String, Value>,
    /// Text fed to every agent run on this repo.
    pub instructions: Option<String>,
    /// Repo files an agent reads first.
    pub context: Vec<String>,
    /// Globs collapsed in the diff.
    pub hide: Vec<String>,
    /// Globs shown but marked as skimmable.
    pub skim: Vec<String>,
    /// Glob → checklist items shown when a matching file changes.
    pub checklist: BTreeMap<String, OneOrMany>,
    /// Skills run when a PR is opened.
    pub auto_skills: Vec<String>,
    /// Findings on matching paths are dropped.
    pub ignore_findings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    pub fn items(&self) -> Vec<String> {
        match self {
            OneOrMany::One(s) => vec![s.clone()],
            OneOrMany::Many(v) => v.clone(),
        }
    }
}

/// `spaces:`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct SpacesConfig {
    /// Paths (a `*` / `?` glob in the last segment) copied from the
    /// project's local checkout into a new space. `None`: unset by any
    /// layer, defaults to `[DEFAULT_COPY]`; `Some(vec![])` is an explicit
    /// `copy: []`, turning it off.
    pub copy: Option<Vec<String>>,
    /// Free TCP ports reserved per space. `None`: unset, defaults to
    /// `DEFAULT_PORTS`; `Some(0)` is explicit, turning it off.
    pub ports: Option<u16>,
    pub env: BTreeMap<String, String>,
    pub setup: Option<String>,
    pub run: Option<String>,
    pub check: Option<String>,
    pub archive: Option<String>,
    pub timeout_s: Timeouts,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Timeouts {
    pub setup: Option<u64>,
    pub check: Option<u64>,
    pub archive: Option<u64>,
}

impl SpacesConfig {
    pub fn command(&self, kind: SpaceCommandKind) -> Option<&str> {
        let c = match kind {
            SpaceCommandKind::Setup => &self.setup,
            SpaceCommandKind::Run => &self.run,
            SpaceCommandKind::Check => &self.check,
            SpaceCommandKind::Archive => &self.archive,
        };
        c.as_deref().map(str::trim).filter(|s| !s.is_empty())
    }

    /// `None` for `run`, which lives until stopped.
    pub fn timeout(&self, kind: SpaceCommandKind) -> Option<std::time::Duration> {
        let s = match kind {
            SpaceCommandKind::Setup => self.timeout_s.setup.unwrap_or(DEFAULT_SETUP_TIMEOUT_S),
            SpaceCommandKind::Check => self.timeout_s.check.unwrap_or(DEFAULT_CHECK_TIMEOUT_S),
            SpaceCommandKind::Archive => {
                self.timeout_s.archive.unwrap_or(DEFAULT_ARCHIVE_TIMEOUT_S)
            }
            SpaceCommandKind::Run => return None,
        };
        Some(std::time::Duration::from_secs(s.max(1)))
    }

    pub fn port_count(&self) -> u16 {
        self.ports.unwrap_or(DEFAULT_PORTS).min(MAX_PORTS)
    }

    /// Paths copied into a new space's worktree (§3): `copy` as set, or
    /// `[DEFAULT_COPY]` when no layer sets it at all.
    pub fn effective_copy(&self) -> Vec<String> {
        self.copy
            .clone()
            .unwrap_or_else(|| vec![DEFAULT_COPY.to_string()])
    }
}

/// `code_intel:` (docs/code-intel.md §3.1, §9): which languages get
/// symbols, references and diagnostics, and whether their servers are
/// downloaded. Read from the base like the rest of the repo layer, so a
/// PR can't change what gets downloaded or run.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct CodeIntelConfig {
    /// Off: no index, no servers, no tools.
    pub enabled: bool,
    pub install: InstallPolicy,
    /// Manifest language ids; empty = detected from the repo's files.
    pub languages: Vec<String>,
    /// Language → the server preferred over the manifest's order.
    pub servers: BTreeMap<String, String>,
    pub max_servers: u32,
    pub idle_minutes: u32,
    /// Larger files are not indexed.
    pub max_file_kb: u32,
    /// Where language servers may run. Only your global and local files
    /// set it (`repo_only_ignored`); unset falls back to Settings →
    /// Languages, then to the defaults.
    pub lsp: LspConfig,
}

/// `code_intel.lsp`: language servers, which can run a project's code
/// (build scripts, proc macros, plugins; docs/code-intel.md §8).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct LspConfig {
    /// In review runs on PRs (default off): **a PR's code may run** —
    /// whoever wrote the PR decides what a server executes.
    pub review: Option<bool>,
    /// In spaces (default on).
    pub spaces: Option<bool>,
}

pub const DEFAULT_LSP_REVIEW: bool = false;
pub const DEFAULT_LSP_SPACES: bool = true;

/// Keys a repo's file may not set: they decide whether code from the repo
/// (a PR author's included) runs on your machine. Ignored in the repo
/// layer and reported (`Merged::ignored`).
pub fn repo_may_not_set(key: &str) -> bool {
    key == "code_intel.lsp" || key.starts_with("code_intel.lsp.")
}

impl Default for CodeIntelConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            install: InstallPolicy::Ask,
            languages: Vec::new(),
            servers: BTreeMap::new(),
            max_servers: DEFAULT_MAX_SERVERS,
            idle_minutes: DEFAULT_IDLE_MINUTES,
            max_file_kb: DEFAULT_MAX_FILE_KB,
            lsp: LspConfig::default(),
        }
    }
}

impl CodeIntelConfig {
    /// `languages`, trimmed, without blanks and repeats.
    pub fn language_ids(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for l in &self.languages {
            let l = l.trim();
            if !l.is_empty() && !out.iter().any(|o| o == l) {
                out.push(l.to_string());
            }
        }
        out
    }
}

/// `code_intel.install`: whether a language server is downloaded
/// (grammars always are, docs/code-intel.md §5.1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstallPolicy {
    /// Asked once per server; the answer is remembered.
    #[default]
    Ask,
    Always,
    /// Only servers already on the machine.
    Never,
}

pub const DEFAULT_MAX_SERVERS: u32 = 4;
pub const DEFAULT_IDLE_MINUTES: u32 = 10;
pub const DEFAULT_MAX_FILE_KB: u32 = 1024;

/// One layer's text, as read by the caller.
#[derive(Debug, Clone)]
pub struct LayerText {
    pub layer: ConfigLayer,
    pub path: PathBuf,
    pub legacy: bool,
    /// `None`: the file doesn't exist.
    pub text: Option<String>,
}

/// The merged layers.
#[derive(Debug, Clone, Default)]
pub struct Merged {
    pub config: ProjectConfig,
    /// The merged tree, before typing.
    pub value: Value,
    /// Every leaf (dotted key) and the layer it came from.
    pub sources: BTreeMap<String, (Value, ConfigLayer)>,
    pub files: Vec<ConfigFile>,
    /// `(file, dotted key)` for keys outside the schema.
    pub unknown: Vec<(String, String)>,
    /// `(file, dotted key)` for keys the repo layer set but may not
    /// (`repo_may_not_set`): dropped before merging.
    pub ignored: Vec<(String, String)>,
    /// `(file, message)` for a value of the wrong type: the whole section
    /// is dropped rather than half-applied.
    pub errors: Vec<(String, String)>,
}

impl Merged {
    pub fn layer_of(&self, key: &str) -> Option<ConfigLayer> {
        self.sources.get(key).map(|(_, l)| *l)
    }

    pub fn entries(&self) -> Vec<ConfigEntry> {
        self.sources
            .iter()
            .map(|(key, (value, layer))| ConfigEntry {
                key: key.clone(),
                value: display_value(value),
                layer: *layer,
            })
            .collect()
    }
}

pub fn display_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Merges `layers` in order (global, repo, local).
pub fn merge(layers: &[LayerText]) -> Merged {
    let mut out = Merged {
        value: Value::Object(Map::new()),
        ..Merged::default()
    };
    for l in layers {
        let path = l.path.display().to_string();
        let mut file = ConfigFile {
            layer: l.layer,
            path: path.clone(),
            exists: l.text.is_some(),
            legacy: l.legacy,
            error: None,
        };
        let Some(text) = &l.text else {
            out.files.push(file);
            continue;
        };
        let parsed = if text.trim().is_empty() {
            Ok(Value::Object(Map::new()))
        } else {
            serde_yaml::from_str::<Value>(text)
        };
        let value = match parsed {
            Ok(Value::Null) => Value::Object(Map::new()),
            Ok(v @ Value::Object(_)) => v,
            Ok(_) => {
                file.error = Some("the top level must be a mapping".into());
                out.files.push(file);
                continue;
            }
            Err(e) => {
                file.error = Some(e.to_string());
                out.files.push(file);
                continue;
            }
        };
        let mut value = normalize(value);
        if l.layer == ConfigLayer::Repo {
            for key in strip_repo_only(&mut value) {
                out.ignored.push((path.clone(), key));
            }
        }
        let mut leaves = Vec::new();
        flatten(String::new(), &value, &mut leaves);
        for (key, _) in &leaves {
            if !is_known(key) {
                out.unknown.push((path.clone(), key.clone()));
            }
        }
        // A list replaces the earlier one: drop the earlier layer's
        // leaves under a key this layer sets.
        for (key, v) in leaves {
            if v.is_object() {
                // An empty map merges into the earlier one: nothing to set.
                continue;
            }
            let prefix = format!("{key}.");
            out.sources.retain(|k, _| !k.starts_with(&prefix));
            out.sources.insert(key, (v, l.layer));
        }
        deep_merge(&mut out.value, value);
        out.files.push(file);
    }
    // Leaves that a later scalar overwrote at a parent key are gone from
    // the tree; keep `sources` in step with it.
    let mut live = Vec::new();
    flatten(String::new(), &out.value, &mut live);
    let live: std::collections::BTreeSet<String> = live.into_iter().map(|(k, _)| k).collect();
    out.sources.retain(|k, _| live.contains(k));

    out.config = typed(&out.value, &mut out.errors);
    out
}

/// Removes what a repo's file may not set (`repo_may_not_set`) from
/// `value`, returning the dotted leaves removed.
fn strip_repo_only(value: &mut Value) -> Vec<String> {
    let Some(code) = value.get_mut("code_intel").and_then(Value::as_object_mut) else {
        return Vec::new();
    };
    let Some(lsp) = code.remove("lsp") else {
        return Vec::new();
    };
    let mut leaves = Vec::new();
    flatten("code_intel.lsp".into(), &lsp, &mut leaves);
    leaves.into_iter().map(|(k, _)| k).collect()
}

/// Each section on its own, so a bad `review.hide` doesn't also drop
/// `spaces` or `code_intel`.
fn typed(value: &Value, errors: &mut Vec<(String, String)>) -> ProjectConfig {
    let mut cfg = ProjectConfig::default();
    if let Some(review) = value.get("review") {
        match serde_json::from_value::<ReviewConfig>(review.clone()) {
            Ok(r) => cfg.review = r,
            Err(e) => errors.push(("review".into(), e.to_string())),
        }
    }
    if let Some(spaces) = value.get("spaces") {
        match serde_json::from_value::<SpacesConfig>(spaces.clone()) {
            Ok(s) => cfg.spaces = s,
            Err(e) => errors.push(("spaces".into(), e.to_string())),
        }
    }
    if let Some(code) = value.get("code_intel") {
        match serde_json::from_value::<CodeIntelConfig>(code.clone()) {
            Ok(c) => cfg.code_intel = c,
            Err(e) => errors.push(("code_intel".into(), e.to_string())),
        }
    }
    cfg
}

/// Dotted keys become nested maps (`agents.grouping: x` →
/// `agents: {grouping: x}`), and the legacy top-level `agents:` moves
/// under `review.agents` (where `review.agents` in the same file wins).
fn normalize(value: Value) -> Value {
    let mut nested = undot(value);
    if let Value::Object(map) = &mut nested
        && let Some(agents) = map.remove("agents")
    {
        let mut moved = serde_json::json!({ "review": { "agents": agents } });
        if let Some(review) = map.remove("review") {
            deep_merge(&mut moved["review"], review);
        }
        map.insert("review".into(), moved["review"].take());
    }
    nested
}

fn undot(value: Value) -> Value {
    undot_at("", value)
}

/// Maps whose keys are globs or names, not config paths: a `.` in them
/// (`"**/*.sql"`) is part of the key.
const LITERAL_KEYS: &[&str] = &["review.checklist", "spaces.env", "code_intel.servers"];

/// `undot` of the map at dotted path `at`.
fn undot_at(at: &str, value: Value) -> Value {
    let mut nested = Value::Object(Map::new());
    if let Value::Object(map) = value {
        for (k, v) in map {
            let path = if at.is_empty() {
                k.clone()
            } else {
                format!("{at}.{k}")
            };
            let v = match v {
                Value::Object(_) if LITERAL_KEYS.contains(&path.as_str()) => v,
                Value::Object(_) => undot_at(&path, v),
                other => other,
            };
            insert_dotted(&mut nested, &k, v);
        }
    }
    nested
}

fn insert_dotted(into: &mut Value, key: &str, v: Value) {
    let mut parts = key.split('.').filter(|p| !p.is_empty()).peekable();
    let mut cur = into;
    while let Some(part) = parts.next() {
        if !cur.is_object() {
            *cur = Value::Object(Map::new());
        }
        let map = cur.as_object_mut().expect("object");
        if parts.peek().is_none() {
            match map.get_mut(part) {
                Some(existing) if existing.is_object() && v.is_object() => {
                    deep_merge(existing, v);
                }
                _ => {
                    map.insert(part.to_string(), v);
                }
            }
            return;
        }
        cur = map
            .entry(part.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
    }
}

/// Maps merge key by key; anything else (lists included) replaces.
fn deep_merge(into: &mut Value, from: Value) {
    match (into, from) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, v) in b {
                match a.get_mut(&k) {
                    Some(existing) if existing.is_object() && v.is_object() => {
                        deep_merge(existing, v)
                    }
                    _ => {
                        a.insert(k, v);
                    }
                }
            }
        }
        (slot, v) => *slot = v,
    }
}

/// Dotted leaves: a scalar, a list or an empty map stops the recursion.
fn flatten(prefix: String, value: &Value, out: &mut Vec<(String, Value)>) {
    match value {
        Value::Object(map) if !map.is_empty() => {
            for (k, v) in map {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten(key, v, out);
            }
        }
        _ => {
            if !prefix.is_empty() {
                out.push((prefix, value.clone()));
            }
        }
    }
}

/// Whether a dotted leaf is part of the schema.
pub fn is_known(key: &str) -> bool {
    const EXACT: &[&str] = &[
        "review.instructions",
        "review.context",
        "review.hide",
        "review.skim",
        "review.auto_skills",
        "review.ignore_findings",
        "spaces.copy",
        "spaces.ports",
        "spaces.setup",
        "spaces.run",
        "spaces.check",
        "spaces.archive",
        "spaces.timeout_s.setup",
        "spaces.timeout_s.check",
        "spaces.timeout_s.archive",
        "code_intel.enabled",
        "code_intel.install",
        "code_intel.languages",
        "code_intel.max_servers",
        "code_intel.idle_minutes",
        "code_intel.max_file_kb",
        "code_intel.lsp.review",
        "code_intel.lsp.spaces",
    ];
    const UNDER: &[&str] = &[
        "review.agents.",
        "review.checklist.",
        "spaces.env.",
        "code_intel.servers.",
    ];
    EXACT.contains(&key) || UNDER.iter().any(|p| key.starts_with(p))
}

/// The global layer's file in `data_dir`.
pub fn global_path(data_dir: &Path) -> PathBuf {
    data_dir.join(GLOBAL_FILE)
}

/// Reads one optional file; unreadable counts as absent.
async fn read(path: &Path) -> Option<String> {
    tokio::fs::read_to_string(path).await.ok()
}

/// The three layers from disk. `repo_root` is where the team's file is
/// read (a PR worktree, a local folder, a space's worktree); `local_root`
/// the main checkout holding the personal file, if there is one. The
/// local layer is never read from a space's worktree, where the agent
/// could write it.
pub async fn read_layers(
    data_dir: &Path,
    repo_root: Option<&Path>,
    local_root: Option<&Path>,
) -> Vec<LayerText> {
    let mut layers = Vec::with_capacity(3);
    let global = global_path(data_dir);
    layers.push(LayerText {
        layer: ConfigLayer::Global,
        text: read(&global).await,
        path: global,
        legacy: false,
    });
    if let Some(root) = repo_root {
        layers.push(read_repo_layer(root).await);
    }
    if let Some(root) = local_root {
        layers.push(read_local_layer(&root.join(LOCAL_FILE)).await);
    }
    layers
}

/// The personal layer at `path`.
pub async fn read_local_layer(path: &Path) -> LayerText {
    LayerText {
        layer: ConfigLayer::Local,
        text: read(path).await,
        path: path.to_path_buf(),
        legacy: false,
    }
}

/// A host project's personal layer: it has no checkout of its own, so the
/// file lives in Theodore's data directory,
/// `<data dir>/projects/<host>/<owner>/<name>/config.local.yaml`.
/// Path segments that could leave that directory (`..`, empty) are dropped.
pub fn host_local_path(data_dir: &Path, repo: &theodore_domain::RepoRef) -> PathBuf {
    let host = serde_json::to_value(repo.host)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "host".into());
    let mut path = data_dir.join("projects").join(host);
    for part in repo.owner.split('/').chain(repo.name.split('/')) {
        if !part.is_empty() && part != "." && part != ".." && !part.contains('\\') {
            path.push(part);
        }
    }
    path.join("config.local.yaml")
}

/// `.theodore/config.yaml`, or the legacy `.review/config.yaml` when it is
/// absent.
pub async fn read_repo_layer(root: &Path) -> LayerText {
    let path = root.join(REPO_FILE);
    if let Some(text) = read(&path).await {
        return LayerText {
            layer: ConfigLayer::Repo,
            path,
            legacy: false,
            text: Some(text),
        };
    }
    let legacy = root.join(LEGACY_REPO_FILE);
    match read(&legacy).await {
        Some(text) => LayerText {
            layer: ConfigLayer::Repo,
            path: legacy,
            legacy: true,
            text: Some(text),
        },
        None => LayerText {
            layer: ConfigLayer::Repo,
            path,
            legacy: false,
            text: None,
        },
    }
}

/// What "Open in editor" creates when `~/.theodore/config.yaml` is missing.
pub const GLOBAL_TEMPLATE: &str = r#"# Theodore global config (docs/config.md).
# Layers, later wins key by key (lists replace):
#   ~/.theodore/config.yaml               this file: your defaults
#   <repo>/.theodore/config.yaml          in the repo, shared with the team
#   <repo>/.theodore/config.local.yaml    personal, gitignored
# Commands set here or in config.local.yaml run without asking; commands
# from a repo's file run only after you allow them.

# review:
#   agents:
#     chat: claude          # claude | codex | gemini
#     grouping: claude
#   instructions: Prefer small functions. Flag any new dependency.
#   hide: ["gen/**"]        # never sent to the agent
#   ignore_findings: ["vendor/**"]

# spaces:
#   copy: [.env, .env.local]  # default: [".env*"]; [] turns it off
#   ports: 1                # $THEODORE_PORT, $THEODORE_PORT_1, … (default: 2; 0 turns it off)
#   env:
#     APP_URL: "http://localhost:$THEODORE_PORT"
#   setup: pnpm install
#   run: pnpm dev --port $THEODORE_PORT
#   check: pnpm lint && pnpm test
#   archive: docker compose -p $THEODORE_SPACE down
#   timeout_s: { setup: 600, check: 600, archive: 120 }

# code_intel:               # symbols, references, diagnostics for agents
#   enabled: true
#   install: ask            # ask | always | never: download language servers
#   languages: [typescript, rust]   # empty: detected
#   servers: { python: pyright }    # preferred server per language
#   max_servers: 4
#   idle_minutes: 10
#   max_file_kb: 1024
#   lsp:                    # language servers; only here or in config.local.yaml
#     spaces: true          # in your spaces
#     review: false         # in PR reviews. true: a PR's code may run (build
#                           # scripts, macros), written by whoever opened it
"#;

#[cfg(test)]
mod tests;
