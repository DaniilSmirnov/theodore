//! Settings → Projects → Config (docs/config.md "Creating and saving the
//! config"): start a project's config from what is already set, check it
//! as you type, and save it to one of the three layers.
//!
//! ```text
//! config_draft ─▶ each layer's file + YAML generated from the settings
//! check_config ─▶ problems (lines) + the effective config with the text in place
//! save_config
//!   global ─▶ <data dir>/config.yaml
//!   local  ─▶ <folder>/.theodore/config.local.yaml (+ .gitignore)   local folder
//!             <data dir>/projects/<host>/<owner>/<name>/config.local.yaml   host
//!   repo   ─▶ <folder>/.theodore/config.yaml (+ commit, if asked)   local folder
//!             commit onto <login>/theodore-config ─▶ push ─▶ outbox create_config_pr   host
//!   then: repo commands you wrote yourself are marked allowed
//! ```

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use theodore_contract::{
    CheckConfigRequest, ConfigCheck, ConfigDraft, ConfigDraftFile, ConfigLayer, ConfigProblemLevel,
    EffectiveConfig, Ephemeral, Project, RepoCommands, SaveConfigRequest, SaveConfigResult,
    Setting, models,
};
use theodore_domain::{PrRef, PrSummary, RepoRef, Sha};
use theodore_ports::NewPr;

use crate::config::{self, LayerText, Merged, editor, trust};
use crate::core::Core;
use crate::error::{AppError, AppResult};

/// The per-task agents of Settings → Agents, as `review.agents.<task>`.
const AGENT_TASKS: &[&str] = &["grouping", "chat", "skills", "fix"];
const ADD_MESSAGE: &str = "Add Theodore config";
const UPDATE_MESSAGE: &str = "Update Theodore config";
const PR_BODY: &str = "The team's Theodore config, `.theodore/config.yaml` (review rules, \
space commands; docs/config.md). Review rules come from the base branch, so they apply to \
other PRs once this one is merged.";

/// Outbox payload of `create_config_pr`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateConfigPrPayload {
    pub project: String,
    pub repo: RepoRef,
    pub pr: NewPr,
}

/// A host project's repo layer: read from the cache at the default
/// branch's tip, saved through a PR.
struct HostRepo {
    repo_path: PathBuf,
    default_branch: String,
    tip: Option<Sha>,
}

/// One project's three layers as the editor reads them.
struct Layers {
    /// Global, repo, local: always all three (`text: None` when absent).
    layers: Vec<LayerText>,
    host: Option<HostRepo>,
}

impl Layers {
    fn get(&self, layer: ConfigLayer) -> &LayerText {
        self.layers
            .iter()
            .find(|l| l.layer == layer)
            .expect("all three layers")
    }

    /// The layers with `text` in place of `layer`'s.
    fn with(&self, layer: ConfigLayer, text: &str) -> Vec<LayerText> {
        self.layers
            .iter()
            .map(|l| {
                if l.layer == layer {
                    LayerText {
                        layer,
                        path: l.path.clone(),
                        legacy: false,
                        text: Some(text.to_string()),
                    }
                } else {
                    l.clone()
                }
            })
            .collect()
    }
}

impl Core {
    /// The config editor's starting point for `project`.
    pub async fn config_draft(&self, project: &str) -> AppResult<ConfigDraft> {
        let project = self.config_project(project).await?;
        let ed = self.editor_layers(&project, true).await;
        let generated = self.generated_config(&project, &ed).await;
        let mut files = Vec::with_capacity(3);
        for l in &ed.layers {
            files.push(ConfigDraftFile {
                layer: l.layer,
                path: l.path.display().to_string(),
                save_to: self.save_target(&project, &ed, l.layer).await,
                text: l.text.clone(),
            });
        }
        let initial = [ConfigLayer::Repo, ConfigLayer::Local]
            .into_iter()
            .find(|layer| ed.get(*layer).text.is_some());
        let (can_commit, commit_note) = match &project.path {
            Some(folder) => match self.folder_commit_refusal(Path::new(folder)).await {
                None => (true, None),
                Some(why) => (false, Some(why)),
            },
            None => (false, None),
        };
        Ok(ConfigDraft {
            project: project.id,
            generated,
            files,
            initial,
            can_commit,
            commit_note,
        })
    }

    /// `text` checked on its own (problems, with lines), and the project's
    /// effective config if it were saved to `layer`. Never toasts.
    pub async fn check_config(&self, req: CheckConfigRequest) -> AppResult<ConfigCheck> {
        let project = self.config_project(&req.project).await?;
        let ed = self.editor_layers(&project, false).await;
        let merged = config::merge(&ed.with(req.layer, &req.text));
        let repo = self.repo_commands_of(&merged, &project.id).await;
        Ok(ConfigCheck {
            problems: editor::validate_layer(&req.text, req.layer),
            preview: effective(&merged, repo),
        })
    }

    /// Writes `text` to `layer` (see the module doc). Refused, with nothing
    /// written, while the text has errors.
    pub async fn save_config(&self, req: SaveConfigRequest) -> AppResult<SaveConfigResult> {
        let r#where = crate::usecases::telemetry::wire_str(&req.layer);
        let result = self.save_config_inner(req).await;
        if result.is_ok() {
            theodore_telemetry::product(theodore_telemetry::ProductEvent::ConfigSaved { r#where });
        }
        result
    }

    async fn save_config_inner(&self, req: SaveConfigRequest) -> AppResult<SaveConfigResult> {
        let _guard = self.0.ship_lock.lock().await;
        let project = self.config_project(&req.project).await?;
        if let Some(p) = editor::validate(&req.text)
            .into_iter()
            .find(|p| p.level == ConfigProblemLevel::Error)
        {
            let at = p.line.map(|l| format!("line {l}: ")).unwrap_or_default();
            return Err(AppError::Invalid(format!(
                "the config has an error, nothing was saved: {at}{}",
                p.message
            )));
        }
        if req.commit && !(req.layer == ConfigLayer::Repo && project.path.is_some()) {
            return Err(AppError::Invalid(
                "only a local folder's repo file can be committed from here".into(),
            ));
        }
        let ed = self.editor_layers(&project, true).await;
        let before = config::merge(&ed.layers);
        let old = self.repo_commands_of(&before, &project.id).await;

        let mut result = match (req.layer, project.path.as_deref().map(Path::new)) {
            (ConfigLayer::Global, _) => {
                let path = config::global_path(&self.0.deps.data_dir);
                write_file(&path, &req.text).await?;
                saved(
                    ConfigLayer::Global,
                    &path,
                    "Saved your global config".into(),
                )
            }
            (ConfigLayer::Local, Some(folder)) => {
                let path = folder.join(config::LOCAL_FILE);
                write_file(&path, &req.text).await?;
                let ignored = self.0.deps.git.is_repo(folder).await.unwrap_or(false)
                    && ensure_gitignored(folder).await?;
                let note = if ignored {
                    " and added it to .gitignore"
                } else {
                    ""
                };
                saved(
                    ConfigLayer::Local,
                    &path,
                    format!("Saved {}{note}", config::LOCAL_FILE),
                )
            }
            (ConfigLayer::Local, None) => {
                let path = config::host_local_path(&self.0.deps.data_dir, &project.repo);
                write_file(&path, &req.text).await?;
                saved(
                    ConfigLayer::Local,
                    &path,
                    "Saved your local config for this repo".into(),
                )
            }
            (ConfigLayer::Repo, Some(folder)) => {
                self.save_repo_in_folder(folder, &req.text, req.commit)
                    .await?
            }
            (ConfigLayer::Repo, None) => self.save_repo_on_host(&project, &ed, &req.text).await?,
        };

        // Repo commands you just wrote are yours: allowed. A local or global
        // save only keeps an allowed set allowed (it may have overridden
        // some of it); it never allows commands someone else wrote.
        let after = config::merge(&ed.with(req.layer, &req.text));
        if let Some(new) = trust::repo_commands(&after, None) {
            let mine = req.layer == ConfigLayer::Repo;
            let kept = old
                .as_ref()
                .is_some_and(|o| o.trusted && new.items.iter().all(|i| o.items.contains(i)));
            if mine || kept {
                self.add_trust(&project.id, &new.hash).await?;
                result.trusted = true;
            }
        }
        Ok(result)
    }

    /// The outbox opened (or found) a config PR: fetch it and tell the UI.
    pub(crate) async fn config_pr_opened(&self, project: &str, pr_ref: &PrRef) {
        let pr = pr_ref.id();
        let url = match self.refresh_pr(&pr, pr_ref).await {
            Some(detail) => detail.summary.url,
            None => self
                .get_row(models::PR, &pr.0)
                .await
                .ok()
                .flatten()
                .and_then(|v| serde_json::from_value::<PrSummary>(v).ok())
                .map(|s| s.url)
                .unwrap_or_default(),
        };
        self.emit(Ephemeral::ConfigPr {
            project: project.to_string(),
            number: pr_ref.number,
            url,
        });
    }

    async fn config_project(&self, id: &str) -> AppResult<Project> {
        self.project_row(id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("project {id}")))
    }

    /// Global, repo, local for `project`: a local folder's own files; for a
    /// host project, the repo file at the default branch's tip in the cache
    /// (absent when the cache can't be read) and the local file in the
    /// data directory. `fetch: false` reads a host repo from the cache as
    /// it is when it was fetched before (the check while typing).
    async fn editor_layers(&self, project: &Project, fetch: bool) -> Layers {
        let data_dir = &self.0.deps.data_dir;
        let global = config::global_path(data_dir);
        let mut layers = vec![LayerText {
            layer: ConfigLayer::Global,
            text: tokio::fs::read_to_string(&global).await.ok(),
            path: global,
            legacy: false,
        }];
        if let Some(folder) = &project.path {
            let folder = Path::new(folder);
            layers.push(config::read_repo_layer(folder).await);
            layers.push(config::read_local_layer(&folder.join(config::LOCAL_FILE)).await);
            return Layers { layers, host: None };
        }
        let host = self.host_repo(project, fetch).await;
        let slug = project.repo.slug();
        layers.push(match &host {
            Some(HostRepo {
                repo_path,
                tip: Some(tip),
                ..
            }) => self.repo_layer_at(repo_path, tip, &slug).await,
            _ => LayerText {
                layer: ConfigLayer::Repo,
                path: PathBuf::from(format!("{slug}:{}", config::REPO_FILE)),
                legacy: false,
                text: None,
            },
        });
        layers.push(
            config::read_local_layer(&config::host_local_path(data_dir, &project.repo)).await,
        );
        Layers { layers, host }
    }

    async fn host_repo(&self, project: &Project, fetch: bool) -> Option<HostRepo> {
        let known = (!fetch)
            .then(|| {
                self.0
                    .config_repos
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .get(&project.id)
                    .cloned()
            })
            .flatten();
        let repo_path = match known {
            Some(p) => p,
            None => {
                let p = self.ensure_project_cache(project).await.ok()?;
                self.0
                    .config_repos
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(project.id.clone(), p.clone());
                p
            }
        };
        let git = &self.0.deps.git;
        let default_branch = match &project.default_branch {
            Some(b) => b.clone(),
            None => git.default_branch(&repo_path).await.ok().flatten()?,
        };
        let tip = git.resolve_ref(&repo_path, &default_branch).await.ok();
        Some(HostRepo {
            repo_path,
            default_branch,
            tip,
        })
    }

    /// Where saving to `layer` writes, as shown in the Save menu.
    async fn save_target(&self, project: &Project, ed: &Layers, layer: ConfigLayer) -> String {
        match (layer, &project.path, &ed.host) {
            (ConfigLayer::Repo, None, Some(host)) => format!(
                "a PR from {} into {}",
                self.config_branch(project).await,
                host.default_branch
            ),
            (ConfigLayer::Repo, None, None) => "a PR (the repo can't be read right now)".into(),
            (ConfigLayer::Repo, Some(folder), _) => Path::new(folder)
                .join(config::REPO_FILE)
                .display()
                .to_string(),
            _ => ed.get(layer).path.display().to_string(),
        }
    }

    /// `<login>/theodore-config`, or `theodore/config` without a login.
    async fn config_branch(&self, project: &Project) -> String {
        let login = self
            .0
            .deps
            .host
            .viewer_for(project.repo.host)
            .await
            .ok()
            .map(|p| p.login);
        let branch = crate::usecases::ship::login_segment(login.as_deref())
            .map(|l| format!("{l}/theodore-config"))
            .filter(|b| crate::usecases::spaces::is_valid_branch(b));
        branch.unwrap_or_else(|| "theodore/config".into())
    }

    /// What's already set for `project` as a config file: the repo and
    /// local files' values, the Settings → Agents choice for each task the
    /// files don't set, the rest of `review:` and `spaces:` as comments.
    async fn generated_config(&self, project: &Project, ed: &Layers) -> String {
        let own: Vec<LayerText> = ed
            .layers
            .iter()
            .filter(|l| l.layer != ConfigLayer::Global)
            .cloned()
            .collect();
        let mut value = config::merge(&own).value;
        if !value.is_object() {
            value = Value::Object(Map::new());
        }
        let root = value.as_object_mut().expect("object");
        let review = root
            .entry("review")
            .or_insert_with(|| Value::Object(Map::new()));
        if !review.is_object() {
            *review = Value::Object(Map::new());
        }
        let agents = review
            .as_object_mut()
            .expect("object")
            .entry("agents")
            .or_insert_with(|| Value::Object(Map::new()));
        if !agents.is_object() {
            *agents = Value::Object(Map::new());
        }
        for task in AGENT_TASKS {
            let set = agents.get(*task).is_some();
            if !set && let Some(v) = self.user_setting(&format!("agents.{task}")).await {
                agents
                    .as_object_mut()
                    .expect("object")
                    .insert((*task).to_string(), v);
            }
        }
        let label = match &project.path {
            Some(folder) => Path::new(folder)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| project.repo.name.clone()),
            None => project.repo.slug(),
        };
        editor::render(&editor::header(&label), &value)
    }

    /// A `user`-scoped setting's value (the row the UI writes,
    /// `user:<key>`, before the seeded default).
    async fn user_setting(&self, key: &str) -> Option<Value> {
        let store = self.0.deps.store.clone();
        let k = key.to_string();
        let rows = tokio::task::spawn_blocking(move || store.query(models::SETTING, "key", &k))
            .await
            .ok()?
            .ok()?;
        let mut user: Vec<(String, Setting)> = rows
            .into_iter()
            .filter_map(|r| Some((r.id, serde_json::from_value::<Setting>(r.data).ok()?)))
            .filter(|(_, s)| s.scope == crate::settings::SCOPE_USER)
            .collect();
        user.sort_by_key(|(id, _)| *id != format!("user:{key}"));
        user.into_iter().next().map(|(_, s)| s.value)
    }

    /// Why the folder can't take a commit of just the repo file, if it can't.
    async fn folder_commit_refusal(&self, folder: &Path) -> Option<String> {
        let git = &self.0.deps.git;
        if !git.is_repo(folder).await.unwrap_or(false) {
            return Some("the folder isn't a git repository".into());
        }
        match git.working_state(folder).await {
            Ok(w) if w.current_branch.is_none() => {
                return Some("the folder has a detached HEAD".into());
            }
            Ok(_) => {}
            Err(_) => return Some("the folder's git status can't be read".into()),
        }
        let others = match git.dirty_paths(folder).await {
            Ok(paths) => paths.into_iter().filter(|p| p != config::REPO_FILE).count(),
            Err(_) => return Some("the folder's git status can't be read".into()),
        };
        (others > 0).then(|| {
            format!(
                "the folder has {others} other uncommitted change{}",
                if others == 1 { "" } else { "s" }
            )
        })
    }

    async fn save_repo_in_folder(
        &self,
        folder: &Path,
        text: &str,
        commit: bool,
    ) -> AppResult<SaveConfigResult> {
        // Checked before anything is written: a refused commit leaves the
        // folder as it was.
        if commit && let Some(why) = self.folder_commit_refusal(folder).await {
            return Err(AppError::Invalid(format!(
                "{why}; save without committing, or commit those first"
            )));
        }
        let path = folder.join(config::REPO_FILE);
        let existed = tokio::fs::try_exists(&path).await.unwrap_or(false);
        write_file(&path, text).await?;
        let mut result = saved(
            ConfigLayer::Repo,
            &path,
            format!("Saved {}", config::REPO_FILE),
        );
        if commit {
            let message = if existed { UPDATE_MESSAGE } else { ADD_MESSAGE };
            let sha = self
                .0
                .deps
                .git
                .commit_paths(folder, &[config::REPO_FILE.to_string()], message)
                .await
                .map_err(|e| match e {
                    theodore_ports::GitError::Refused(m) => {
                        AppError::Invalid(format!("saved, but not committed: {m}"))
                    }
                    other => AppError::Git(other),
                })?;
            result.message = format!(
                "Saved and committed {} ({})",
                config::REPO_FILE,
                short(&sha)
            );
            result.commit = Some(sha.0);
        }
        Ok(result)
    }

    /// One commit with the file on `<login>/theodore-config` (from the
    /// default branch's tip, or on top of the branch when it exists), pushed
    /// without force, then the PR through the outbox.
    async fn save_repo_on_host(
        &self,
        project: &Project,
        ed: &Layers,
        text: &str,
    ) -> AppResult<SaveConfigResult> {
        let Some(host) = &ed.host else {
            return Err(AppError::Invalid(
                "the repo can't be read right now; nothing was pushed".into(),
            ));
        };
        let Some(tip) = &host.tip else {
            return Err(AppError::Invalid(format!(
                "the default branch {} isn't in Theodore's copy of the repo; nothing was pushed",
                host.default_branch
            )));
        };
        let git = &self.0.deps.git;
        let branch = self.config_branch(project).await;
        let remote = git
            .remote_branch(&host.repo_path, &project.repo, &branch)
            .await?;
        let parent = remote.clone().unwrap_or_else(|| tip.clone());
        let current = git
            .show(&host.repo_path, &parent, config::REPO_FILE)
            .await?;
        if current.as_deref() == Some(text.as_bytes()) && remote.is_none() {
            return Err(AppError::Invalid(format!(
                "{} already has this config; nothing to propose",
                host.default_branch
            )));
        }
        let mut result = SaveConfigResult {
            layer: ConfigLayer::Repo,
            path: branch.clone(),
            message: String::new(),
            branch: Some(branch.clone()),
            commit: None,
            trusted: false,
        };
        if current.as_deref() == Some(text.as_bytes()) {
            result.message = format!("{branch} already has this config; checking its PR");
        } else {
            let author = self.ship_author(&host.repo_path, project).await?;
            let message = if current.is_some() {
                UPDATE_MESSAGE
            } else {
                ADD_MESSAGE
            };
            let sha = git
                .commit_file(
                    &host.repo_path,
                    &parent,
                    config::REPO_FILE,
                    text.as_bytes(),
                    message,
                    &author,
                )
                .await?;
            git.push_branch(&host.repo_path, &project.repo, &sha, &branch)
                .await
                .map_err(|e| match e {
                    theodore_ports::GitError::Refused(m) => AppError::Invalid(m),
                    other => AppError::Git(other),
                })?;
            result.message = format!("Pushed {branch}; opening the PR…");
            result.commit = Some(sha.0);
        }
        let payload = CreateConfigPrPayload {
            project: project.id.clone(),
            repo: project.repo.clone(),
            pr: NewPr {
                head: branch,
                base: host.default_branch.clone(),
                title: ADD_MESSAGE.into(),
                body: PR_BODY.into(),
                draft: false,
            },
        };
        let payload =
            serde_json::to_value(&payload).map_err(|e| AppError::Invalid(e.to_string()))?;
        self.apply_with_outbox(Vec::new(), "create_config_pr", payload)
            .await?;
        Ok(result)
    }
}

/// The read-only view of `merged` (Settings → Labs, the editor's preview).
pub(crate) fn effective(merged: &Merged, repo: Option<RepoCommands>) -> EffectiveConfig {
    let mut unknown: Vec<String> = merged.unknown.iter().map(|(_, k)| k.clone()).collect();
    unknown.sort();
    unknown.dedup();
    EffectiveConfig {
        files: merged.files.clone(),
        entries: merged.entries(),
        unknown,
        repo_commands: repo,
    }
}

fn saved(layer: ConfigLayer, path: &Path, message: String) -> SaveConfigResult {
    SaveConfigResult {
        layer,
        path: path.display().to_string(),
        message,
        branch: None,
        commit: None,
        trusted: false,
    }
}

fn short(sha: &Sha) -> String {
    sha.as_str().chars().take(8).collect()
}

/// Writes `text` to `path` through a temporary file and a rename, so a
/// reader never sees half a file.
async fn write_file(path: &Path, text: &str) -> AppResult<()> {
    let io = |e: std::io::Error| AppError::Invalid(format!("{}: {e}", path.display()));
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await.map_err(io)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".tmp-{}", std::process::id()));
    let tmp = PathBuf::from(tmp);
    tokio::fs::write(&tmp, text).await.map_err(io)?;
    if let Err(e) = tokio::fs::rename(&tmp, path).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(io(e));
    }
    Ok(())
}

/// Appends `.theodore/config.local.yaml` to the folder's `.gitignore`
/// unless a line already names it. Whether it appended.
async fn ensure_gitignored(folder: &Path) -> AppResult<bool> {
    let path = folder.join(".gitignore");
    let text = match tokio::fs::read_to_string(&path).await {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(AppError::Invalid(format!("{}: {e}", path.display()))),
    };
    if gitignore_covers(&text) {
        return Ok(false);
    }
    let mut add = String::new();
    if !text.is_empty() && !text.ends_with('\n') {
        add.push('\n');
    }
    add.push_str(config::LOCAL_FILE);
    add.push('\n');
    use tokio::io::AsyncWriteExt;
    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await
        .map_err(|e| AppError::Invalid(format!("{}: {e}", path.display())))?;
    f.write_all(add.as_bytes())
        .await
        .map_err(|e| AppError::Invalid(format!("{}: {e}", path.display())))?;
    Ok(true)
}

/// Whether a `.gitignore` line names the local file itself.
fn gitignore_covers(text: &str) -> bool {
    text.lines().map(str::trim).any(|l| {
        let l = l.strip_prefix('/').unwrap_or(l);
        l == config::LOCAL_FILE
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_local_file_is_recognised_in_a_gitignore() {
        assert!(gitignore_covers(
            "node_modules\n.theodore/config.local.yaml\n"
        ));
        assert!(gitignore_covers("  /.theodore/config.local.yaml  "));
        assert!(!gitignore_covers("# .theodore/config.local.yaml\n"));
        assert!(!gitignore_covers(".theodore/config.yaml\n"));
    }
}
