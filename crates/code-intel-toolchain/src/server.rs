//! Finding a language server binary and how to launch it (docs/code-intel.md
//! §4.3, §8).
//!
//! ```text
//! for each source, each `detect` argv:
//!   PATH (which) ─▶ rustup which ─▶ xcrun --find ─▶ <worktree>/node_modules/.bin
//!                                                   └─ Trusted only
//!   candidate ─▶ inside the worktree and Review? ─▶ skip
//!             ─▶ run detect argv (timeout) ─▶ exit 0 ─▶ System { path, version }
//! none ─▶ <root>/servers/<name>/<version>/<bin> ─▶ Downloaded { version }
//! ```
//! In a review run probes run outside the worktree, so a PR's
//! `rust-toolchain.toml` cannot pick (or install) the toolchain they hit.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use theodore_ports::code_intel::{InstallState, TrustMode};

use crate::manifest::{BIN_PLACEHOLDER, ServerSpec};

/// How long a `--version` / `rustup which` / `xcrun --find` may take.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Where a server binary came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "origin", rename_all = "snake_case")]
pub enum ServerOrigin {
    /// Found on the machine; `version` is the first line of its `detect`
    /// output, when it printed one.
    System {
        path: PathBuf,
        version: Option<String>,
    },
    /// Downloaded by Theodore, at the manifest's version.
    Downloaded { version: String },
}

/// A server binary ready to launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedServer {
    pub name: String,
    pub bin: PathBuf,
    /// The manifest's `launch` with `{bin}` replaced.
    pub argv: Vec<OsString>,
    pub origin: ServerOrigin,
}

impl ResolvedServer {
    pub fn install_state(&self) -> InstallState {
        match &self.origin {
            ServerOrigin::System { path, version } => InstallState::System {
                path: path.clone(),
                version: version.clone(),
            },
            ServerOrigin::Downloaded { version } => InstallState::Installed {
                version: version.clone(),
            },
        }
    }
}

/// What the LSP client needs to start a server.
#[derive(Debug, Clone, PartialEq)]
pub struct LaunchConfig {
    pub argv: Vec<OsString>,
    /// `initializationOptions`: the manifest's, with `review_init_options`
    /// merged over them in a review run.
    pub init_options: serde_json::Value,
    pub workspace_folders: bool,
}

pub fn launch_config(
    resolved: &ResolvedServer,
    spec: &ServerSpec,
    trust: TrustMode,
) -> LaunchConfig {
    let init_options = match trust {
        TrustMode::Trusted => spec.init_options.clone(),
        TrustMode::Review => merge_json(spec.init_options.clone(), &spec.review_init_options),
    };
    LaunchConfig {
        argv: resolved.argv.clone(),
        init_options,
        workspace_folders: spec.workspace_folders,
    }
}

pub(crate) fn launch_argv(spec: &ServerSpec, bin: &Path) -> Vec<OsString> {
    spec.launch
        .iter()
        .map(|a| {
            if a == BIN_PLACEHOLDER {
                bin.as_os_str().to_owned()
            } else {
                OsString::from(a)
            }
        })
        .collect()
}

/// Objects merge key by key; anything else in `overlay` (except `null`)
/// replaces `base`.
pub(crate) fn merge_json(
    base: serde_json::Value,
    overlay: &serde_json::Value,
) -> serde_json::Value {
    use serde_json::Value;
    match (base, overlay) {
        (base, Value::Null) => base,
        (Value::Object(mut b), Value::Object(o)) => {
            for (k, v) in o {
                let merged = merge_json(b.remove(k).unwrap_or(Value::Null), v);
                b.insert(k.clone(), merged);
            }
            Value::Object(b)
        }
        (_, o) => o.clone(),
    }
}

/// Process environment for probes; `path` overrides `$PATH` (tests).
#[derive(Debug, Clone)]
pub(crate) struct ProbeEnv {
    pub path: Option<OsString>,
    pub timeout: Duration,
    /// Where probes run in a review run: not the worktree.
    pub neutral_dir: PathBuf,
}

impl ProbeEnv {
    fn path_var(&self) -> Option<OsString> {
        self.path.clone().or_else(|| std::env::var_os("PATH"))
    }
}

/// The server on the machine, by the manifest's `detect` rules.
pub(crate) async fn find_system(
    spec: &ServerSpec,
    worktree: &Path,
    trust: TrustMode,
    env: &ProbeEnv,
) -> Option<ResolvedServer> {
    let cwd = match trust {
        TrustMode::Trusted => worktree,
        TrustMode::Review => env.neutral_dir.as_path(),
    };
    let path_var = env.path_var();
    let detects = &spec.detect;

    let mut candidates: Vec<(PathBuf, &[String])> = Vec::new();
    for argv in detects {
        if let Ok(p) = which::which_in(&argv[0], path_var.as_ref(), cwd) {
            candidates.push((p, &argv[1..]));
        }
    }
    if let Some(found) = try_candidates(spec, candidates, worktree, trust, cwd, env).await {
        return Some(found);
    }
    for (tool, flag) in [("rustup", "which"), ("xcrun", "--find")] {
        let mut candidates = Vec::new();
        for argv in detects {
            if let Some(p) = tool_lookup(tool, &[flag, &argv[0]], cwd, env).await {
                candidates.push((p, &argv[1..]));
            }
        }
        if let Some(found) = try_candidates(spec, candidates, worktree, trust, cwd, env).await {
            return Some(found);
        }
    }
    if trust == TrustMode::Trusted {
        let candidates = detects
            .iter()
            .map(|argv| {
                (
                    worktree.join("node_modules/.bin").join(&argv[0]),
                    &argv[1..],
                )
            })
            .filter(|(p, _)| p.is_file())
            .collect();
        return try_candidates(spec, candidates, worktree, trust, cwd, env).await;
    }
    None
}

async fn try_candidates(
    spec: &ServerSpec,
    candidates: Vec<(PathBuf, &[String])>,
    worktree: &Path,
    trust: TrustMode,
    cwd: &Path,
    env: &ProbeEnv,
) -> Option<ResolvedServer> {
    for (path, args) in candidates {
        if trust == TrustMode::Review && is_inside(&path, worktree) {
            tracing::info!(
                target: "code_intel", server = %spec.name, path = %path.display(),
                "binary inside the worktree ignored in a review run"
            );
            continue;
        }
        let Some(version) = probe(&path, args, cwd, env).await else {
            continue;
        };
        return Some(ResolvedServer {
            name: spec.name.clone(),
            argv: launch_argv(spec, &path),
            bin: path.clone(),
            origin: ServerOrigin::System { path, version },
        });
    }
    None
}

/// `Some(version)` if `program args…` exits 0 (or times out: slow, but
/// there). No args: only checks that it exists.
async fn probe(
    program: &Path,
    args: &[String],
    cwd: &Path,
    env: &ProbeEnv,
) -> Option<Option<String>> {
    if args.is_empty() {
        return program.is_file().then_some(None);
    }
    match run(program, args, cwd, env).await {
        Run::Ok(out) => Some(first_line(&out)),
        Run::Timeout => Some(None),
        Run::Failed => None,
    }
}

/// `rustup which <bin>` / `xcrun --find <bin>`: an absolute path that exists.
async fn tool_lookup(tool: &str, args: &[&str], cwd: &Path, env: &ProbeEnv) -> Option<PathBuf> {
    let tool = which::which_in(tool, env.path_var().as_ref(), cwd).ok()?;
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    let Run::Ok(out) = run(&tool, &args, cwd, env).await else {
        return None;
    };
    let path = PathBuf::from(String::from_utf8_lossy(&out.stdout).lines().next()?.trim());
    (path.is_absolute() && path.is_file()).then_some(path)
}

enum Run {
    Ok(std::process::Output),
    Timeout,
    Failed,
}

async fn run(program: &Path, args: &[String], cwd: &Path, env: &ProbeEnv) -> Run {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // rustup must not download a toolchain to answer a probe.
        .env("RUSTUP_AUTO_INSTALL", "0")
        .kill_on_drop(true);
    if let Some(path) = &env.path {
        cmd.env("PATH", path);
    }
    let Ok(child) = cmd.spawn() else {
        return Run::Failed;
    };
    match tokio::time::timeout(env.timeout, child.wait_with_output()).await {
        Ok(Ok(out)) if out.status.success() => Run::Ok(out),
        Ok(_) => Run::Failed,
        Err(_) => Run::Timeout,
    }
}

fn first_line(out: &std::process::Output) -> Option<String> {
    [&out.stdout, &out.stderr].into_iter().find_map(|bytes| {
        let text = String::from_utf8_lossy(bytes);
        let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
        Some(line.chars().take(200).collect())
    })
}

fn is_inside(path: &Path, dir: &Path) -> bool {
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    canon(path).starts_with(canon(dir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn review_options_merge_over_init_options() {
        let base = json!({"cargo": {"features": "all", "buildScripts": {"enable": true}}, "x": 1});
        let overlay =
            json!({"cargo": {"buildScripts": {"enable": false}}, "procMacro": {"enable": false}});
        assert_eq!(
            merge_json(base.clone(), &overlay),
            json!({
                "cargo": {"features": "all", "buildScripts": {"enable": false}},
                "procMacro": {"enable": false},
                "x": 1
            })
        );
        assert_eq!(merge_json(base.clone(), &serde_json::Value::Null), base);
        assert_eq!(merge_json(serde_json::Value::Null, &overlay), overlay);
    }

    #[test]
    fn launch_config_per_trust() {
        let spec: ServerSpec = serde_yaml::from_str(
            r#"
name: s
launch: ["node", "{bin}", "--stdio"]
init_options: { a: 1 }
review_init_options: { plugins: [] }
workspace_folders: true
"#,
        )
        .unwrap();
        let bin = PathBuf::from("/opt/s/server.js");
        let resolved = ResolvedServer {
            name: "s".into(),
            argv: launch_argv(&spec, &bin),
            bin,
            origin: ServerOrigin::Downloaded {
                version: "1".into(),
            },
        };
        let review = launch_config(&resolved, &spec, TrustMode::Review);
        assert_eq!(
            review.argv,
            vec![
                OsString::from("node"),
                OsString::from("/opt/s/server.js"),
                OsString::from("--stdio")
            ]
        );
        assert_eq!(review.init_options, json!({"a": 1, "plugins": []}));
        assert!(review.workspace_folders);
        let trusted = launch_config(&resolved, &spec, TrustMode::Trusted);
        assert_eq!(trusted.init_options, json!({"a": 1}));
    }
}
