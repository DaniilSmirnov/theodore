//! Projects (plan "projects"): connected repositories — a host repo cloned
//! bare under the cache, or a local folder mirrored into it without ever
//! writing to the folder's own `.git` — and reviewing one of their branches
//! without a host PR.

use std::path::{Path, PathBuf};

use theodore_contract::{
    AddProjectRequest, Ephemeral, LocalBranch, OpenBranchRequest, Project, ProjectStatus,
    ProjectWorkingState, ToastLevel, models,
};
use theodore_domain::{HostKind, HostRepo, PrDetail, PrId, PrRef, PrSummary, RepoRef};
use theodore_ports::{InboxSection, NewAction, SyncActionCode};

use crate::core::Core;
use crate::error::{AppError, AppResult};

impl Core {
    pub async fn list_host_repos(&self) -> AppResult<Vec<HostRepo>> {
        Ok(self.0.deps.host.list_repos().await?)
    }

    pub async fn add_project(&self, req: AddProjectRequest) -> AppResult<Project> {
        let (id, repo, path) = match (req.repo, req.path) {
            (Some(_), Some(_)) | (None, None) => {
                return Err(AppError::Invalid(
                    "add_project needs exactly one of repo or path".into(),
                ));
            }
            (Some(repo), None) => (project_id_for_repo(&repo), repo, None),
            (None, Some(path)) => {
                let folder = Path::new(&path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("project")
                    .to_string();
                if !self.0.deps.git.is_repo(Path::new(&path)).await? {
                    return Err(AppError::Invalid(format!("{path} is not a git repository")));
                }
                let name = self.unique_local_name(&folder).await?;
                let repo = RepoRef {
                    host: HostKind::Local,
                    owner: "local".into(),
                    name: name.clone(),
                };
                (format!("local:{name}"), repo, Some(path))
            }
        };

        if self.project_row(&id).await?.is_some() {
            return Err(AppError::Invalid(format!("project {id} already added")));
        }

        let project = Project {
            id: id.clone(),
            repo: repo.clone(),
            path: path.clone(),
            status: ProjectStatus::Cloning,
            error: None,
            default_branch: None,
            added_at: self.0.deps.clock.now(),
            description: None,
            private: false,
            pushed_at: None,
            last_fetched_at: None,
        };
        self.apply(vec![NewAction {
            model: models::PROJECT.into(),
            model_id: id.clone(),
            action: SyncActionCode::I,
            data: Some(
                serde_json::to_value(&project).map_err(|e| AppError::Invalid(e.to_string()))?,
            ),
        }])
        .await?;

        self.spawn_project_clone(id, repo, path);
        Ok(project)
    }

    pub async fn remove_project(&self, id: &str) -> AppResult<()> {
        let project = self
            .project_row(id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("project {id}")))?;
        // Best-effort: the project row is still removed even if the cache
        // was already gone. `path` (a local folder) is never touched.
        let _ = self.0.deps.git.remove_repo(&project.repo).await;
        self.apply(vec![NewAction {
            model: models::PROJECT.into(),
            model_id: id.into(),
            action: SyncActionCode::D,
            data: None,
        }])
        .await?;
        Ok(())
    }

    /// Fetches/refreshes the project's cache, keeps `default_branch` and the
    /// host's own repo metadata fresh, and — for a host project — writes its
    /// open, recently merged and recently closed PRs as `pr` rows. Stamps
    /// `last_fetched_at` unconditionally (local projects included), for the
    /// header's "fetched Nm ago".
    pub async fn refresh_project(&self, id: &str) -> AppResult<()> {
        self.refresh_project_with(id, false).await
    }

    /// The poller's background refresh: same work, no toasts — a failed
    /// refresh nobody asked for shouldn't interrupt anyone every 10 minutes.
    pub(crate) async fn refresh_project_quietly(&self, id: &str) -> AppResult<()> {
        self.refresh_project_with(id, true).await
    }

    /// The host project the poller should refresh next: ready, and last
    /// fetched before `cutoff` (never-fetched first, then the oldest).
    /// Local folders aren't polled — opening their page refreshes them.
    pub(crate) async fn next_stale_host_project(&self, cutoff: i64) -> Option<String> {
        let store = self.0.deps.store.clone();
        let rows = tokio::task::spawn_blocking(move || store.all(models::PROJECT))
            .await
            .ok()?
            .ok()?;
        rows.into_iter()
            .filter_map(|r| serde_json::from_value::<Project>(r.data).ok())
            .filter(|p| p.repo.host != HostKind::Local && p.status == ProjectStatus::Ready)
            .filter(|p| p.last_fetched_at.is_none_or(|at| at < cutoff))
            .min_by_key(|p| p.last_fetched_at.unwrap_or(i64::MIN))
            .map(|p| p.id)
    }

    async fn refresh_project_with(&self, id: &str, quiet: bool) -> AppResult<()> {
        let warn = |text: String| {
            if !quiet {
                self.emit(Ephemeral::Toast {
                    level: ToastLevel::Warn,
                    text,
                });
            }
        };
        let project = self
            .project_row(id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("project {id}")))?;
        let cache = self
            .project_cache(&project.repo, project.path.as_deref())
            .await?;
        if !cache.fetched {
            // Offline: nothing new to read, and the host's PR queries would
            // only fail. `last_fetched_at` stays put, so the header's
            // "fetched Nm ago" stays true and the poller retries it.
            return Ok(());
        }
        let repo_path = cache.path;

        let mut patch = serde_json::Map::new();
        patch.insert(
            "last_fetched_at".into(),
            serde_json::json!(self.0.deps.clock.now()),
        );

        if let Ok(Some(default_branch)) = self.0.deps.git.default_branch(&repo_path).await
            && project.default_branch.as_deref() != Some(default_branch.as_str())
        {
            patch.insert("default_branch".into(), serde_json::json!(default_branch));
        }

        if project.repo.host != HostKind::Local {
            match self.0.deps.host.repo_info(&project.repo).await {
                Ok(info) => {
                    patch.insert("description".into(), serde_json::json!(info.description));
                    patch.insert("private".into(), serde_json::json!(info.private));
                    patch.insert("pushed_at".into(), serde_json::json!(info.pushed_at));
                }
                // No per-repo metadata endpoint on this host — the header
                // just keeps whatever it already had.
                Err(theodore_ports::HostError::Unsupported(_)) => {}
                Err(e) => warn(format!("project refresh failed: {e}")),
            }

            let slug = project.repo.slug();
            let now = self.0.deps.clock.now();
            // Open PRs aren't capped (there's rarely many); merged/closed
            // are capped to the last 14 days or 30 items, whichever bites
            // first — both queries sort newest-first, so a plain `take`
            // after the age cutoff is enough.
            let queries = [
                (format!("is:pr is:open repo:{slug}"), false),
                (
                    format!("is:pr repo:{slug} is:merged sort:updated-desc"),
                    true,
                ),
                (
                    format!("is:pr repo:{slug} is:closed is:unmerged sort:updated-desc"),
                    true,
                ),
            ];
            let mut by_id = std::collections::HashMap::new();
            for (query, cap) in queries {
                match self
                    .0
                    .deps
                    .host
                    .search_repo(&project.repo, &InboxSection::Query(query))
                    .await
                {
                    Ok(summaries) => {
                        let summaries = if cap {
                            recent(summaries, now, FOURTEEN_DAYS_MS, 30)
                        } else {
                            summaries
                        };
                        for summary in summaries {
                            by_id.entry(summary.pr.id().0).or_insert(summary);
                        }
                    }
                    Err(e) => warn(format!("project refresh failed: {e}")),
                }
            }
            let mut actions = Vec::with_capacity(by_id.len());
            for summary in by_id.into_values() {
                let id = summary.pr.id().0;
                if self.is_stale_summary(&id, summary.updated_at).await {
                    continue;
                }
                let mut data =
                    serde_json::to_value(&summary).map_err(|e| AppError::Invalid(e.to_string()))?;
                self.keep_inbox_reasons(&id, &mut data).await;
                actions.push(NewAction {
                    model: models::PR.into(),
                    model_id: id,
                    action: SyncActionCode::U,
                    data: Some(data),
                });
            }
            self.apply(actions).await?;
        }

        self.apply(vec![NewAction {
            model: models::PROJECT.into(),
            model_id: id.into(),
            action: SyncActionCode::C,
            data: Some(serde_json::Value::Object(patch)),
        }])
        .await?;
        Ok(())
    }

    pub async fn list_branches(&self, project: &str) -> AppResult<Vec<LocalBranch>> {
        let project = self
            .project_row(project)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("project {project}")))?;
        let repo_path = self.ensure_project_cache(&project).await?;
        let default_branch = project.default_branch.clone().unwrap_or_default();
        let branches = self
            .0
            .deps
            .git
            .list_branches(&repo_path, &default_branch)
            .await?;
        Ok(branches
            .into_iter()
            .map(|b| LocalBranch {
                name: b.name,
                head: b.head,
                subject: b.subject,
                author: b.author,
                ahead: b.ahead,
                behind: b.behind,
                updated_at: b.updated_at,
            })
            .collect())
    }

    /// A local folder project's own checkout (current branch, uncommitted
    /// paths), read straight from the folder — never from the mirror, which
    /// has no working tree. Read-only: the folder is never written to.
    pub async fn project_working_state(&self, project: &str) -> AppResult<ProjectWorkingState> {
        let project = self
            .project_row(project)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("project {project}")))?;
        let path = project.path.ok_or_else(|| {
            AppError::Invalid(format!("project {} is not a local folder", project.id))
        })?;
        let state = self.0.deps.git.working_state(Path::new(&path)).await?;
        Ok(ProjectWorkingState {
            path,
            current_branch: state.current_branch,
            dirty_files: state.dirty_files,
        })
    }

    pub async fn open_branch(
        &self,
        req: OpenBranchRequest,
    ) -> AppResult<theodore_contract::OpenPrResponse> {
        let project = self
            .project_row(&req.project)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("project {}", req.project)))?;
        let base = req
            .base
            .clone()
            .or_else(|| project.default_branch.clone())
            .ok_or_else(|| {
                AppError::Invalid(format!("project {} has no default branch yet", project.id))
            })?;
        let number = stable_hash_u32(&req.branch) as u64;
        let pr_ref = PrRef {
            repo: project.repo.clone(),
            number,
            host_id: Some(req.branch.clone()),
        };

        let _guard = self.0.open_lock.lock().await;
        let id = pr_ref.id();
        if let Some(head) = self.0.current_head.read().await.get(&id).cloned() {
            return Ok(theodore_contract::OpenPrResponse { pr: id, head });
        }
        self.open_local(&project, pr_ref, &base).await
    }

    /// Closes a branch review (`open_branch`): drops its `pr`, `pr_detail`
    /// and `grouping` rows, its in-memory head/revision and its worktree.
    /// Only ever a branch review — a stored row with a host `url` is a real
    /// PR, which this never touches, locally or on the host.
    pub async fn close_branch_review(&self, pr: &PrId) -> AppResult<()> {
        let row = self
            .get_row(models::PR, &pr.0)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("pr {pr}")))?;
        let summary: PrSummary =
            serde_json::from_value(row).map_err(|e| AppError::Invalid(e.to_string()))?;
        if !summary.url.is_empty() {
            return Err(AppError::Invalid(format!(
                "{pr} is a host pull request, not a branch review"
            )));
        }

        // Same lock `open_branch` takes, so a re-open can't interleave.
        let _guard = self.0.open_lock.lock().await;
        self.0.current_head.write().await.remove(pr);
        let mut roots: Vec<PathBuf> = Vec::new();
        self.0.revisions.write().await.retain(|(id, _), e| {
            if id == pr && !roots.contains(&e.worktree_path) {
                roots.push(e.worktree_path.clone());
            }
            id != pr
        });
        self.0.etags.write().await.remove(pr);
        // Best-effort: the review is closed even if its worktree was
        // already gone (or never made it to disk).
        let _ = self.0.deps.git.remove_worktree(&summary.pr).await;
        for root in roots {
            self.close_code_root(&root).await;
        }

        let mut actions = Vec::new();
        for model in [models::PR, models::PR_DETAIL, models::GROUPING] {
            if model == models::PR || self.get_row(model, &pr.0).await?.is_some() {
                actions.push(NewAction {
                    model: model.into(),
                    model_id: pr.0.clone(),
                    action: SyncActionCode::D,
                    data: None,
                });
            }
        }
        self.apply(actions).await?;
        Ok(())
    }

    /// Recovers a `local:` PR's `PrRef` (branch, base) from its stored `pr`
    /// row and runs the same flow `open_branch` would.
    pub(crate) async fn ensure_open_local(&self, pr: &PrId) -> AppResult<()> {
        let row = self
            .get_row(models::PR, &pr.0)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("pr {pr}")))?;
        let summary: PrSummary =
            serde_json::from_value(row).map_err(|e| AppError::Invalid(e.to_string()))?;
        let pr_ref = summary.pr;
        // A space's review forks at a fixed commit, not at its base branch.
        if pr_ref
            .host_id
            .as_deref()
            .is_some_and(crate::usecases::spaces::is_space_ref)
        {
            return self.reopen_space_review(pr).await;
        }
        let project_id = project_id_for_repo(&pr_ref.repo);
        let project = self
            .project_row(&project_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("project {project_id}")))?;
        self.open_local(&project, pr_ref, &summary.base_ref)
            .await
            .map(|_| ())
    }

    async fn open_local(
        &self,
        project: &Project,
        pr_ref: PrRef,
        base: &str,
    ) -> AppResult<theodore_contract::OpenPrResponse> {
        let branch = pr_ref
            .host_id
            .clone()
            .ok_or_else(|| AppError::Invalid("local PR is missing its branch".into()))?;
        let repo_path = self.ensure_project_cache(project).await?;
        let (detail, revision): (PrDetail, theodore_domain::Revision) = self
            .0
            .deps
            .git
            .describe_branch(&repo_path, &pr_ref.repo, pr_ref.number, &branch, base)
            .await?;
        let head = detail.summary.head_sha.clone();
        let worktree = self.0.deps.git.worktree(&pr_ref, &head).await?;
        self.finish_open(pr_ref, detail, revision, repo_path, worktree.path)
            .await
    }

    /// Resumes a mirror/clone that was still `cloning` when Theodore last
    /// shut down.
    pub async fn resume_cloning_projects(&self) -> AppResult<()> {
        let store = self.0.deps.store.clone();
        let rows = tokio::task::spawn_blocking(move || store.all(models::PROJECT))
            .await
            .map_err(AppError::join)??;
        for row in rows {
            let Ok(project) = serde_json::from_value::<Project>(row.data) else {
                continue;
            };
            if project.status == ProjectStatus::Cloning {
                self.spawn_project_clone(project.id, project.repo, project.path);
            }
        }
        Ok(())
    }

    /// Clones (or mirrors) a new project in the background. Offline with no
    /// clone yet is a wait, not a failure: the project stays `cloning` with
    /// the reason in `error`, and the clone is retried with backoff until
    /// it lands, the project is removed, or Theodore quits.
    fn spawn_project_clone(&self, id: String, repo: RepoRef, path: Option<String>) {
        let core = self.clone();
        tokio::spawn(async move {
            let mut wait = OFFLINE_CLONE_RETRY_FIRST;
            let mut noted_offline = false;
            let result = loop {
                let result: AppResult<String> = async {
                    let cache = core.project_cache(&repo, path.as_deref()).await?;
                    let default_branch = core.0.deps.git.default_branch(&cache.path).await?;
                    Ok(default_branch.unwrap_or_else(|| "main".into()))
                }
                .await;
                match result {
                    Err(e) if e.is_offline() => {
                        tracing::info!(project = %id, error.code = e.code(), "clone waits for the network");
                        if !noted_offline {
                            noted_offline = true;
                            let _ = core
                                .apply(vec![NewAction {
                                    model: models::PROJECT.into(),
                                    model_id: id.clone(),
                                    action: SyncActionCode::C,
                                    data: Some(serde_json::json!({ "error": e.to_string() })),
                                }])
                                .await;
                        }
                        tokio::select! {
                            _ = core.0.shutdown.cancelled() => return,
                            _ = tokio::time::sleep(wait) => {}
                        }
                        wait = (wait * 2).min(OFFLINE_CLONE_RETRY_MAX);
                        match core.project_row(&id).await {
                            Ok(Some(p)) if p.status == ProjectStatus::Cloning => continue,
                            _ => return,
                        }
                    }
                    other => break other,
                }
            };

            match result {
                Ok(default_branch) => {
                    let _ = core
                        .apply(vec![NewAction {
                            model: models::PROJECT.into(),
                            model_id: id.clone(),
                            action: SyncActionCode::C,
                            data: Some(serde_json::json!({
                                "status": ProjectStatus::Ready,
                                "default_branch": default_branch,
                                "error": null,
                            })),
                        }])
                        .await;
                    if let Err(e) = core.refresh_project(&id).await {
                        core.emit(Ephemeral::Toast {
                            level: ToastLevel::Warn,
                            text: format!("project refresh failed: {e}"),
                        });
                    }
                }
                Err(e) => {
                    let _ = core
                        .apply(vec![NewAction {
                            model: models::PROJECT.into(),
                            model_id: id,
                            action: SyncActionCode::C,
                            data: Some(serde_json::json!({
                                "status": ProjectStatus::Error,
                                "error": e.to_string(),
                            })),
                        }])
                        .await;
                }
            }
        });
    }

    /// Recovery path for the poller (plan "projects"): resolves a `Local`
    /// `RepoRef` back to its project's cache when the revision cache has
    /// nothing for it (e.g. after a restart).
    pub(crate) async fn ensure_local_project_cache(
        &self,
        repo: &RepoRef,
    ) -> theodore_ports::GitResult<std::path::PathBuf> {
        let id = project_id_for_repo(repo);
        let project = self.project_row(&id).await.ok().flatten();
        let Some(project) = project else {
            return Err(theodore_ports::GitError::Unsupported(
                "unknown local project",
            ));
        };
        self.ensure_project_cache(&project)
            .await
            .map_err(|e| theodore_ports::GitError::Parse(e.to_string()))
    }

    /// The project's cache, refreshed when the remote can be reached.
    pub(crate) async fn ensure_project_cache(
        &self,
        project: &Project,
    ) -> AppResult<std::path::PathBuf> {
        Ok(self
            .project_cache(&project.repo, project.path.as_deref())
            .await?
            .path)
    }

    /// A host project's clone works offline (plan "offline"): the fetch is
    /// best-effort once a clone is on disk, and only a project never
    /// cloned needs the network.
    ///
    /// ```text
    /// local folder ──▶ mirror fetch (no network) ─────────────▶ fetched
    /// host repo ─┬─ clone on disk, host known offline ────────▶ stale copy
    ///            └─▶ fetch ─┬─ ok ─────────────────────────────▶ fetched
    ///                       └─ network ─┬─ clone on disk ─────▶ stale copy
    ///                                   └─ none ─▶ AppError::Offline
    /// ```
    pub(crate) async fn project_cache(
        &self,
        repo: &RepoRef,
        folder: Option<&str>,
    ) -> AppResult<ProjectCache> {
        let git = &self.0.deps.git;
        if let Some(folder) = folder {
            let path = git.ensure_local_mirror(Path::new(folder), repo).await?;
            return Ok(ProjectCache {
                path,
                fetched: true,
            });
        }
        let cached = git.cached_repo(repo).await?;
        // The host already failed to answer: a fetch would only wait out a
        // network timeout before falling back to the same copy.
        if let Some(path) = &cached
            && !self.host_online()
        {
            tracing::info!(repo = %repo.slug(), "You're offline — using the copy on this Mac");
            return Ok(ProjectCache {
                path: path.clone(),
                fetched: false,
            });
        }
        match git.ensure_repo(repo).await {
            Ok(path) => Ok(ProjectCache {
                path,
                fetched: true,
            }),
            Err(theodore_ports::GitError::Network(stderr)) => match cached {
                Some(path) => {
                    tracing::info!(
                        repo = %repo.slug(),
                        error.code = "git.network",
                        stderr = %stderr.trim(),
                        "You're offline — using the copy on this Mac"
                    );
                    Ok(ProjectCache {
                        path,
                        fetched: false,
                    })
                }
                None => Err(AppError::Offline),
            },
            Err(e) => Err(e.into()),
        }
    }

    pub(crate) async fn project_row(&self, id: &str) -> AppResult<Option<Project>> {
        match self.get_row(models::PROJECT, id).await? {
            Some(data) => Ok(Some(
                serde_json::from_value(data).map_err(|e| AppError::Invalid(e.to_string()))?,
            )),
            None => Ok(None),
        }
    }

    pub(crate) async fn get_row(
        &self,
        model: &str,
        id: &str,
    ) -> AppResult<Option<serde_json::Value>> {
        let store = self.0.deps.store.clone();
        let (model, id) = (model.to_string(), id.to_string());
        let row = tokio::task::spawn_blocking(move || store.get(&model, &id))
            .await
            .map_err(AppError::join)??;
        Ok(row.map(|r| r.data))
    }

    async fn unique_local_name(&self, folder: &str) -> AppResult<String> {
        let store = self.0.deps.store.clone();
        let rows = tokio::task::spawn_blocking(move || store.all(models::PROJECT))
            .await
            .map_err(AppError::join)??;
        let existing: std::collections::HashSet<String> = rows
            .into_iter()
            .filter_map(|r| serde_json::from_value::<Project>(r.data).ok())
            .filter(|p| p.repo.host == HostKind::Local)
            .map(|p| p.repo.name)
            .collect();
        if !existing.contains(folder) {
            return Ok(folder.to_string());
        }
        for n in 2.. {
            let candidate = format!("{folder}-{n}");
            if !existing.contains(&candidate) {
                return Ok(candidate);
            }
        }
        unreachable!()
    }
}

fn project_id_for_repo(repo: &RepoRef) -> String {
    match repo.host {
        // A local project's id is `local:<name>`, not `local:local/<name>`
        // — `owner` is always the literal "local", it isn't part of the id.
        HostKind::Local => format!("local:{}", repo.name),
        HostKind::Github => format!("github:{}", repo.slug()),
        HostKind::Gitlab => format!("gitlab:{}", repo.slug()),
        HostKind::Bitbucket => format!("bitbucket:{}", repo.slug()),
    }
}

const FOURTEEN_DAYS_MS: i64 = 14 * 24 * 60 * 60 * 1000;

/// A clone that couldn't start offline is retried after this, doubling up
/// to `OFFLINE_CLONE_RETRY_MAX`.
const OFFLINE_CLONE_RETRY_FIRST: std::time::Duration = std::time::Duration::from_secs(15);
const OFFLINE_CLONE_RETRY_MAX: std::time::Duration = std::time::Duration::from_secs(300);

/// Where a project's cache is, and whether it was just refreshed from its
/// remote (`false`: offline, the copy already on disk as is).
pub(crate) struct ProjectCache {
    pub path: std::path::PathBuf,
    pub fetched: bool,
}

/// `summaries` sorted newest-first (as a `sort:updated-desc` search query
/// returns them): keeps a prefix no older than `max_age_ms` and no longer
/// than `max_items` — whichever bound is hit first.
fn recent(
    summaries: Vec<PrSummary>,
    now: i64,
    max_age_ms: i64,
    max_items: usize,
) -> Vec<PrSummary> {
    summaries
        .into_iter()
        .take(max_items)
        .take_while(|p| now - p.updated_at <= max_age_ms)
        .collect()
}

/// Deterministic across restarts (unlike `DefaultHasher`, whose seed is
/// randomized per process) — FNV-1a over the branch name.
pub(crate) fn stable_hash_u32(s: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for b in s.as_bytes() {
        hash ^= *b as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_hash_is_deterministic() {
        assert_eq!(stable_hash_u32("feature/x"), stable_hash_u32("feature/x"));
        assert_ne!(stable_hash_u32("feature/x"), stable_hash_u32("feature/y"));
    }

    #[test]
    fn project_id_matches_the_contract_doc_comment() {
        let repo = RepoRef {
            host: HostKind::Github,
            owner: "acme".into(),
            name: "widgets".into(),
        };
        assert_eq!(project_id_for_repo(&repo), "github:acme/widgets");
    }

    fn summary_at(updated_at: i64) -> PrSummary {
        PrSummary {
            pr: PrRef {
                repo: RepoRef {
                    host: HostKind::Github,
                    owner: "acme".into(),
                    name: "widgets".into(),
                },
                number: 1,
                host_id: None,
            },
            title: "t".into(),
            author: theodore_domain::Person {
                login: "a".into(),
                name: None,
                avatar_url: None,
            },
            state: theodore_domain::PrState::Merged,
            base_ref: "main".into(),
            head_ref: "f".into(),
            head_sha: theodore_domain::Sha::new("s"),
            additions: 0,
            deletions: 0,
            changed_files: 0,
            reasons: vec![],
            checks: theodore_domain::CheckRollup::None,
            approvals: 0,
            required_approvals: None,
            updated_at,
            url: "https://github.com/acme/widgets/pull/1".into(),
            review_decision: None,
            conflicts: false,
            created_at: None,
            stack: None,
        }
    }

    #[test]
    fn recent_drops_items_older_than_the_age_cutoff() {
        let now = 1_000_000_000;
        let items = vec![
            summary_at(now),
            summary_at(now - 1_000),
            summary_at(now - FOURTEEN_DAYS_MS - 1),
        ];
        let kept = recent(items, now, FOURTEEN_DAYS_MS, 30);
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn recent_caps_item_count_even_within_the_age_window() {
        let items: Vec<_> = (0..40).map(|_| summary_at(0)).collect();
        let kept = recent(items, 0, FOURTEEN_DAYS_MS, 30);
        assert_eq!(kept.len(), 30);
    }
}
