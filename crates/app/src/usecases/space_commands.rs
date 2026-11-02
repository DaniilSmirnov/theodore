//! A space's commands (docs/config.md): `copy` and `ports` on create,
//! `setup` before the first turn, `run` on demand, `check` after a turn,
//! `archive` before the worktree goes; repo-provided ones only once
//! allowed.
//!
//! ```text
//! create ─▶ copy · ports ─▶ setup? ──ok──▶ goal turn ─▶ check? ─▶ …
//!                            │  └─fail─▶ Retry / Continue without setup
//!                            └─ repo, not allowed ─▶ waiting ─▶ Allow / Not now
//! archive ─▶ stop run ─▶ checkpoint ─▶ archive? ─▶ remove worktree ─▶ release ports
//! ```

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Duration;

use theodore_contract::{
    ConfigLayer, ConfiguredCommand, Ephemeral, Project, RepoCommands, Setting, Space,
    SpaceCommandKind, SpaceLogLine, ToastLevel, models,
};
use theodore_domain::Sha;
use theodore_ports::{
    CommandEvent, CommandExit, CommandRun, CommandSpec, NewAction, SyncActionCode,
};
use tokio_util::sync::CancellationToken;

use crate::config::vars::{SpaceVars, space_env};
use crate::config::{self, LayerText, Merged, trust};
use crate::core::Core;
use crate::error::{AppError, AppResult};

/// Log lines kept per space, all commands together.
const LOG_LINES: usize = 2_000;
/// Lines of a `check`'s output kept on its turn.
pub(crate) const CHECK_TAIL: usize = 60;
/// How long archive waits for a stopped `run` to exit.
pub(crate) const STOP_WAIT: Duration = Duration::from_secs(10);

/// The setting row holding a project's approved repo-command hash.
const TRUST_KEY: &str = "spaces.trust";

fn trust_row_id(project: &str) -> String {
    format!("trust:{project}")
}

/// In-memory per space: its commands' last lines and those in flight.
#[derive(Default)]
pub(crate) struct SpaceRuntime {
    pub logs: VecDeque<SpaceLogLine>,
    /// The `run` command: cancel, and cancelled itself once it has exited.
    pub run: Option<(CancellationToken, CancellationToken)>,
    pub setup: Option<CancellationToken>,
}

/// A space's effective config plus its trust state.
pub(crate) struct SpaceConfig {
    pub merged: Merged,
    pub repo: Option<RepoCommands>,
    pub denied: bool,
}

impl SpaceConfig {
    /// Whether the effective value of `key` may be used: anything not from
    /// the repo, or the repo's once allowed (and not "Not now").
    pub fn allowed(&self, key: &str) -> bool {
        if self.merged.layer_of(key) != Some(ConfigLayer::Repo) || !trust::is_gated(key) {
            return true;
        }
        !self.denied && self.repo.as_ref().is_none_or(|r| r.trusted)
    }

    pub fn command(&self, kind: SpaceCommandKind) -> Option<ConfiguredCommand> {
        let script = self.merged.config.spaces.command(kind)?.to_string();
        let key = command_key(kind);
        Some(ConfiguredCommand {
            kind,
            script,
            layer: self.merged.layer_of(&key).unwrap_or(ConfigLayer::Global),
            allowed: self.allowed(&key),
        })
    }

    /// The command to run, when configured and allowed.
    pub fn runnable(&self, kind: SpaceCommandKind) -> Option<String> {
        self.command(kind).filter(|c| c.allowed).map(|c| c.script)
    }

    /// `spaces.env` without the repo's values that aren't allowed.
    pub fn env(&self) -> BTreeMap<String, String> {
        self.merged
            .config
            .spaces
            .env
            .iter()
            .filter(|(k, _)| self.allowed(&format!("spaces.env.{k}")))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    pub fn timeout(&self, kind: SpaceCommandKind) -> Option<Duration> {
        self.merged.config.spaces.timeout(kind)
    }
}

pub(crate) fn command_key(kind: SpaceCommandKind) -> String {
    format!("spaces.{}", kind_name(kind))
}

pub(crate) fn kind_name(kind: SpaceCommandKind) -> &'static str {
    match kind {
        SpaceCommandKind::Setup => "setup",
        SpaceCommandKind::Run => "run",
        SpaceCommandKind::Check => "check",
        SpaceCommandKind::Archive => "archive",
    }
}

/// One line describing how a command ended, or `None` on success.
pub(crate) fn describe_failure(
    kind: SpaceCommandKind,
    exit: &CommandExit,
    timeout: Option<Duration>,
) -> Option<String> {
    if exit.success() {
        return None;
    }
    let what = kind_name(kind);
    Some(if let Some(e) = &exit.error {
        format!("{what}: {e}")
    } else if exit.timed_out {
        match timeout {
            Some(t) => format!("{what} timed out after {} s", t.as_secs()),
            None => format!("{what} timed out"),
        }
    } else if exit.cancelled {
        format!("{what} was stopped")
    } else {
        match exit.code {
            Some(c) => format!("{what} exited with {c}"),
            None => format!("{what} was killed"),
        }
    })
}

/// The parent environment without Theodore's own `THEODORE_*` secrets, for
/// expanding `$VAR` in `spaces.env` values.
fn inherited_env() -> Vec<(String, String)> {
    std::env::vars()
        .filter(|(k, _)| !k.starts_with("THEODORE_"))
        .collect()
}

impl Core {
    /// The layers of `project`'s config. A local folder's own files (its
    /// working copy, and the personal `config.local.yaml`); a host project
    /// has no checkout, so its repo file is read from the cache at `rev`
    /// and its personal file from Theodore's data directory.
    pub(crate) async fn project_layers(
        &self,
        project: &Project,
        rev: Option<&Sha>,
    ) -> Vec<LayerText> {
        let data_dir = self.0.deps.data_dir.clone();
        if let Some(folder) = &project.path {
            let folder = Path::new(folder);
            return config::read_layers(&data_dir, Some(folder), Some(folder)).await;
        }
        let mut layers = config::read_layers(&data_dir, None, None).await;
        if let Some(rev) = rev
            && let Ok(repo_path) = self.ensure_project_cache(project).await
        {
            layers.push(
                self.repo_layer_at(&repo_path, rev, &project.repo.slug())
                    .await,
            );
        }
        let local = config::host_local_path(&data_dir, &project.repo);
        layers.push(config::read_local_layer(&local).await);
        layers
    }

    /// The config a space runs with: its project's layers (a host
    /// project's repo file at the space's base) and the trust state.
    pub(crate) async fn space_config_of(&self, space: &Space, project: &Project) -> SpaceConfig {
        let layers = self.project_layers(project, Some(&space.base_sha)).await;
        self.config_with_trust(&layers, project, space.commands_denied)
            .await
    }

    pub(crate) async fn config_with_trust(
        &self,
        layers: &[LayerText],
        project: &Project,
        denied: bool,
    ) -> SpaceConfig {
        let merged = config::merge(layers);
        self.warn_config(&merged);
        let repo = self.repo_commands_of(&merged, &project.id).await;
        SpaceConfig {
            merged,
            repo,
            denied,
        }
    }

    /// The repo layer's commands in `merged`, `trusted` when you allowed
    /// that exact set for `project`.
    pub(crate) async fn repo_commands_of(
        &self,
        merged: &Merged,
        project: &str,
    ) -> Option<theodore_contract::RepoCommands> {
        let set = trust::repo_commands(merged, None)?;
        let allowed = self.trusted_hashes(project).await;
        let hit = allowed.contains(&set.hash).then_some(set.hash.as_str());
        trust::repo_commands(merged, hit)
    }

    /// Unknown keys, unreadable files and badly typed sections: a toast
    /// each, once per session.
    pub(crate) fn warn_config(&self, merged: &Merged) {
        let mut problems: Vec<(String, String)> = Vec::new();
        for f in &merged.files {
            if let Some(e) = &f.error {
                problems.push((
                    format!("{}\0error", f.path),
                    format!("{}: ignored ({e})", f.path),
                ));
            }
            if f.legacy && f.exists {
                problems.push((
                    format!("{}\0legacy", f.path),
                    format!("{}: deprecated, move it to .theodore/config.yaml", f.path),
                ));
            }
        }
        let mut by_file: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (file, key) in &merged.unknown {
            by_file.entry(file).or_default().push(key);
        }
        for (file, keys) in by_file {
            problems.push((
                format!("{file}\0unknown\0{}", keys.join(",")),
                format!("{file}: unknown keys ignored: {}", keys.join(", ")),
            ));
        }
        let mut ignored: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (file, key) in &merged.ignored {
            ignored.entry(file).or_default().push(key);
        }
        for (file, keys) in ignored {
            problems.push((
                format!("{file}\0ignored\0{}", keys.join(",")),
                format!(
                    "{file}: ignored, a repo's file can't set {} (set it in your own config)",
                    keys.join(", ")
                ),
            ));
        }
        for (section, e) in &merged.errors {
            problems.push((
                format!("{section}\0type\0{e}"),
                format!("config `{section}:` ignored: {e}"),
            ));
        }
        let fresh: Vec<String> = {
            let mut warned = self
                .0
                .config_warned
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            problems
                .into_iter()
                .filter(|(key, _)| warned.insert(key.clone()))
                .map(|(_, text)| text)
                .collect()
        };
        for text in fresh {
            self.emit(Ephemeral::Toast {
                level: ToastLevel::Warn,
                text,
            });
        }
    }

    /// The allowed sets' hashes: the one you allowed (`hash`), and the sets
    /// your own config saves were allowed for (`also`, most recent first).
    async fn trusted_hashes(&self, project: &str) -> Vec<String> {
        let Ok(Some(row)) = self.get_row(models::SETTING, &trust_row_id(project)).await else {
            return Vec::new();
        };
        let Some(value) = row.get("value") else {
            return Vec::new();
        };
        let mut out: Vec<String> = Vec::new();
        if let Some(h) = value.get("hash").and_then(|h| h.as_str()) {
            out.push(h.to_string());
        }
        for h in value
            .get("also")
            .and_then(|a| a.as_array())
            .into_iter()
            .flatten()
        {
            if let Some(h) = h.as_str() {
                out.push(h.to_string());
            }
        }
        out
    }

    /// "Allow": exactly this set is allowed for the project from now on.
    pub(crate) async fn store_trust(&self, project: &str, hash: &str) -> AppResult<()> {
        self.write_trust(
            project,
            serde_json::json!({ "hash": hash, "at": self.0.deps.clock.now() }),
        )
        .await
    }

    /// A config you saved yourself: `hash` is allowed too, and the sets
    /// allowed before stay allowed (a host repo's saved file only takes
    /// effect once its PR merges). Keeps the last few.
    pub(crate) async fn add_trust(&self, project: &str, hash: &str) -> AppResult<()> {
        const KEEP: usize = 4;
        let mut also: Vec<String> = self
            .trusted_hashes(project)
            .await
            .into_iter()
            .filter(|h| h != hash)
            .collect();
        also.truncate(KEEP);
        self.write_trust(
            project,
            serde_json::json!({ "hash": hash, "also": also, "at": self.0.deps.clock.now() }),
        )
        .await
    }

    async fn write_trust(&self, project: &str, value: serde_json::Value) -> AppResult<()> {
        let setting = Setting {
            key: TRUST_KEY.into(),
            scope: format!("project:{project}"),
            value,
        };
        self.apply(vec![NewAction {
            model: models::SETTING.into(),
            model_id: trust_row_id(project),
            action: SyncActionCode::U,
            data: Some(
                serde_json::to_value(&setting).map_err(|e| AppError::Invalid(e.to_string()))?,
            ),
        }])
        .await?;
        Ok(())
    }

    /// The environment added to a space's commands and agent runs.
    pub(crate) fn space_command_env(
        &self,
        space: &Space,
        project: &Project,
        cfg: &SpaceConfig,
    ) -> Vec<(String, String)> {
        let vars = SpaceVars {
            space: crate::usecases::spaces::slug(&space.name),
            id: space.id.clone(),
            ports: space.ports.clone(),
            root: project.path.clone(),
            worktree: space.worktree.clone(),
            branch: space.branch.clone(),
        };
        space_env(&vars, &cfg.env(), inherited_env())
    }

    /// `n` free ports no other live space holds. The caller holds
    /// `ports_lock` until the row carrying them is written.
    pub(crate) async fn reserve_ports(&self, n: u16, except: &str) -> AppResult<Vec<u16>> {
        if n == 0 {
            return Ok(Vec::new());
        }
        let taken: std::collections::HashSet<u16> = self
            .list_spaces(None)
            .await?
            .into_iter()
            .filter(|s| s.id != except && s.status != theodore_contract::SpaceStatus::Archived)
            .flat_map(|s| s.ports)
            .collect();
        config::ports::allocate(n, &taken)
            .map_err(|e| AppError::Invalid(format!("couldn't reserve {n} ports: {e}")))
    }

    pub(crate) fn push_log(&self, space: &str, kind: SpaceCommandKind, line: String, stderr: bool) {
        let entry = SpaceLogLine {
            kind,
            line,
            stderr,
            at: self.0.deps.clock.now(),
        };
        {
            let mut rt = self.0.space_rt.lock().unwrap_or_else(|p| p.into_inner());
            let logs = &mut rt.entry(space.to_string()).or_default().logs;
            logs.push_back(entry.clone());
            while logs.len() > LOG_LINES {
                logs.pop_front();
            }
        }
        self.emit(Ephemeral::SpaceLog {
            space: space.to_string(),
            line: entry,
        });
    }

    pub(crate) fn with_runtime<T>(&self, space: &str, f: impl FnOnce(&mut SpaceRuntime) -> T) -> T {
        let mut rt = self.0.space_rt.lock().unwrap_or_else(|p| p.into_inner());
        f(rt.entry(space.to_string()).or_default())
    }

    /// Runs one command to its end in `cwd`, logging every line. `cancel`
    /// (or Theodore shutting down) stops it. Returns how it ended and its
    /// last `CHECK_TAIL` lines.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn run_space_command(
        &self,
        space: &str,
        kind: SpaceCommandKind,
        script: &str,
        cwd: &Path,
        env: Vec<(String, String)>,
        timeout: Option<Duration>,
        cancel: CancellationToken,
    ) -> (CommandExit, Vec<String>) {
        let run = self
            .spawn_space_command(space, kind, script, cwd, env, timeout)
            .await;
        self.drive_space_command(space, kind, run, timeout, cancel)
            .await
    }

    /// Logs `$ script` and spawns it; the caller drives it with
    /// `drive_space_command`. Split out of `run_space_command` so a caller
    /// that needs the pid before the command's output (a space's `run`,
    /// to record its process group) can see it between the two steps.
    pub(crate) async fn spawn_space_command(
        &self,
        space: &str,
        kind: SpaceCommandKind,
        script: &str,
        cwd: &Path,
        env: Vec<(String, String)>,
        timeout: Option<Duration>,
    ) -> CommandRun {
        self.push_log(space, kind, format!("$ {script}"), false);
        self.0
            .deps
            .commands
            .spawn(CommandSpec {
                script: script.to_string(),
                cwd: cwd.to_path_buf(),
                env,
                timeout,
            })
            .await
    }

    /// Streams `run`'s output to the space's log and waits for it to end;
    /// `cancel` (or Theodore shutting down) stops it early. Returns how it
    /// ended and its last `CHECK_TAIL` lines.
    pub(crate) async fn drive_space_command(
        &self,
        space: &str,
        kind: SpaceCommandKind,
        run: CommandRun,
        timeout: Option<Duration>,
        cancel: CancellationToken,
    ) -> (CommandExit, Vec<String>) {
        let mut events = run.events;
        let mut tail: VecDeque<String> = VecDeque::new();
        let mut stopping = false;
        let exit = loop {
            let event = if stopping {
                events.recv().await
            } else {
                tokio::select! {
                    _ = cancel.cancelled() => { stopping = true; run.cancel.cancel(); continue; }
                    _ = self.0.shutdown.cancelled() => { stopping = true; run.cancel.cancel(); continue; }
                    e = events.recv() => e,
                }
            };
            match event {
                Some(CommandEvent::Line { stderr, text }) => {
                    tail.push_back(text.clone());
                    if tail.len() > CHECK_TAIL {
                        tail.pop_front();
                    }
                    self.push_log(space, kind, text, stderr);
                }
                Some(CommandEvent::Exit(exit)) => break exit,
                None => {
                    break CommandExit {
                        error: Some("the command ended without an exit status".into()),
                        ..CommandExit::default()
                    };
                }
            }
        };
        let failure = describe_failure(kind, &exit, timeout);
        if let Some(why) = &failure
            && !exit.cancelled
        {
            // Not the output: that's the user's program, and stays in the
            // space's own log.
            tracing::warn!(
                error.code = if exit.timed_out {
                    "space.command_timed_out"
                } else {
                    "space.command_failed"
                },
                space,
                command = kind_name(kind),
                exit_code = exit.code,
                error = %why,
                "space command failed"
            );
        }
        let summary = failure.unwrap_or_else(|| format!("{} finished", kind_name(kind)));
        self.push_log(space, kind, format!("── {summary}"), !exit.success());
        (exit, tail.into())
    }
}

/// Copies `patterns` (paths relative to `root`; `*` / `?` allowed in the
/// last segment) into `dest`. Returns notes for what wasn't copied.
pub(crate) fn copy_into(root: &Path, dest: &Path, patterns: &[String]) -> Vec<String> {
    let mut notes = Vec::new();
    for pattern in patterns {
        let rel = Path::new(pattern);
        let safe = !pattern.is_empty()
            && rel.is_relative()
            && rel
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)))
            && rel
                .components()
                .next()
                .is_none_or(|c| c.as_os_str() != ".git");
        if !safe {
            notes.push(format!(
                "copy: skipped {pattern:?}: not a path inside the project"
            ));
            continue;
        }
        let name = rel.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let matches: Vec<PathBuf> = if name.contains(['*', '?']) {
            let parent = rel.parent().unwrap_or(Path::new(""));
            let mut found: Vec<PathBuf> = std::fs::read_dir(root.join(parent))
                .map(|rd| {
                    rd.flatten()
                        .filter_map(|e| e.file_name().to_str().map(str::to_string))
                        .filter(|n| glob_segment(name, n))
                        .map(|n| parent.join(n))
                        .collect()
                })
                .unwrap_or_default();
            found.sort();
            found
        } else {
            vec![rel.to_path_buf()]
        };
        if matches.is_empty() {
            notes.push(format!("copy: nothing matches {pattern}"));
        }
        for m in matches {
            if let Err(e) = copy_path(&root.join(&m), &dest.join(&m)) {
                notes.push(format!("copy: {}: {e}", m.display()));
            }
        }
    }
    notes
}

fn copy_path(from: &Path, to: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(from).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "not found in the project")
    })?;
    if meta.file_type().is_symlink() {
        return Err(std::io::Error::other("a symlink, not copied"));
    }
    if meta.is_dir() {
        std::fs::create_dir_all(to)?;
        for e in std::fs::read_dir(from)?.flatten() {
            if std::fs::symlink_metadata(e.path()).is_ok_and(|m| m.file_type().is_symlink()) {
                continue;
            }
            copy_path(&e.path(), &to.join(e.file_name()))?;
        }
        return Ok(());
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(from, to).map(|_| ())
}

/// `*` (any run) and `?` (one character) within one path segment.
fn glob_segment(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti, mut star, mut mark) = (0, 0, None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod copy_tests {
    use super::*;

    #[test]
    fn globs_match_within_a_segment() {
        assert!(glob_segment(".env*", ".env"));
        assert!(glob_segment(".env*", ".env.local"));
        assert!(!glob_segment(".env*", "env"));
        assert!(glob_segment("a?c", "abc"));
        assert!(!glob_segment("a?c", "ac"));
        assert!(glob_segment("*.yaml", "x.yaml"));
    }

    #[test]
    fn copies_files_dirs_and_globs_and_notes_the_rest() {
        let root = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".env"), "A=1").unwrap();
        std::fs::write(root.path().join(".env.local"), "B=2").unwrap();
        std::fs::create_dir_all(root.path().join("conf/sub")).unwrap();
        std::fs::write(root.path().join("conf/sub/k.pem"), "key").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc/hosts", root.path().join("link")).unwrap();
        let patterns: Vec<String> = [".env*", "conf", "missing.txt", "../up", "/abs", "link"]
            .into_iter()
            .map(String::from)
            .collect();
        let notes = copy_into(root.path(), dest.path(), &patterns);
        assert_eq!(
            std::fs::read_to_string(dest.path().join(".env")).unwrap(),
            "A=1"
        );
        assert_eq!(
            std::fs::read_to_string(dest.path().join(".env.local")).unwrap(),
            "B=2"
        );
        assert_eq!(
            std::fs::read_to_string(dest.path().join("conf/sub/k.pem")).unwrap(),
            "key"
        );
        assert!(!dest.path().join("link").exists());
        assert_eq!(notes.len(), 4, "{notes:?}");
        assert!(notes[0].contains("missing.txt"), "{notes:?}");
        assert!(notes[1].contains("../up"));
        assert!(notes[2].contains("/abs"));
        assert!(notes[3].contains("symlink"));
    }
}
