//! The project config's `review:` section (docs/config.md §1), resolved for
//! one PR or space and handed to whatever applies it.
//!
//! ```text
//! global ─────────────────┐
//! repo, at the base's tip ┼─▶ sha256(layers) ─▶ cached Merged ─▶ ReviewRules ─▶ grouping, prompts,
//! local (yours)          ─┘     (fresh read        (re-merged        │             findings, auto skills
//!                                every time)        on change)       └─ context: resolved against the
//!                                                                       PR's worktree
//! ```
//!
//! The repo layer is read at the base, never at the PR's head: a PR can't
//! change how itself is reviewed (hide its own files, drop findings on
//! them, steer the agents). A config change applies once it is merged.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use sha2::{Digest, Sha256};
use theodore_contract::{ConfigLayer, models};
use theodore_domain::glob::{glob_match, is_glob};
use theodore_domain::review_rules::{MAX_CONTEXT_FILES, ReviewRules};
use theodore_domain::{HostKind, PrDetail, PrId, PrRef, RepoRef, Sha};

use crate::config::{self, LayerText, Merged, ReviewConfig};
use crate::core::Core;

/// Merged configs kept, by layer hash; more than this and the cache starts
/// over (a config edit leaves its old entry behind).
const CACHE_MAX: usize = 64;

/// Files looked at while expanding a `context` glob, at most.
const WALK_MAX_ENTRIES: usize = 20_000;

/// The commit a review's repo layer is read at.
///
/// | review | read at |
/// | --- | --- |
/// | host PR, branch review | tip of `refs/heads/<base>` in the cache, else the PR's base commit |
/// | a space's review | the space's base commit |
/// | local folder project | not used: the folder's own files |
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ConfigBase {
    /// The base branch; `None` reads at `sha`.
    pub branch: Option<String>,
    /// The PR's base commit: read at when the branch doesn't resolve.
    pub sha: Sha,
}

impl ConfigBase {
    pub fn of(pr: &PrRef, detail: &PrDetail) -> Self {
        // A space's review forks at a fixed commit, not at its base branch.
        let space = pr
            .host_id
            .as_deref()
            .is_some_and(crate::usecases::spaces::is_space_ref);
        let branch = detail.summary.base_ref.trim();
        Self {
            branch: (!space && !branch.is_empty()).then(|| branch.to_string()),
            sha: detail.base_sha.clone(),
        }
    }
}

/// Where a review's config is read: the repo layer from `repo_path` at
/// `base`, `context` against `worktree`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ReviewSource<'a> {
    pub repo: &'a RepoRef,
    /// The bare cache (or a local folder's mirror).
    pub repo_path: &'a Path,
    /// The PR's checkout at its head.
    pub worktree: &'a Path,
    pub base: &'a ConfigBase,
}

impl Core {
    /// The layers of the config a review runs with: a local folder
    /// project's own files (working copy and personal layer), whichever
    /// review of it is open; otherwise the global file, the repo's file at
    /// the base (never the PR's head) and, for a host repo, your personal
    /// file in Theodore's data directory.
    pub(crate) async fn pr_config_layers(&self, src: ReviewSource<'_>) -> Vec<LayerText> {
        let store = self.0.deps.store.clone();
        let repo = src.repo.clone();
        let folder = tokio::task::spawn_blocking(move || store.all(models::PROJECT))
            .await
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|r| serde_json::from_value::<theodore_contract::Project>(r.data).ok())
            .find(|p| p.repo == repo && p.path.is_some());
        if let Some(project) = folder {
            return self.project_layers(&project, None).await;
        }
        let data_dir = &self.0.deps.data_dir;
        let mut layers = config::read_layers(data_dir, None, None).await;
        let rev = self.config_base_rev(src.repo_path, src.base).await;
        layers.push(
            self.repo_layer_at(src.repo_path, &rev, &src.repo.slug())
                .await,
        );
        if src.repo.host != HostKind::Local {
            let path = config::host_local_path(data_dir, src.repo);
            layers.push(config::read_local_layer(&path).await);
        }
        layers
    }

    /// The tip of `base.branch` in the cache, else `base.sha`.
    pub(crate) async fn config_base_rev(&self, repo_path: &Path, base: &ConfigBase) -> Sha {
        if let Some(branch) = &base.branch
            && let Ok(sha) = self
                .0
                .deps
                .git
                .resolve_ref(repo_path, &format!("refs/heads/{branch}"))
                .await
        {
            return sha;
        }
        base.sha.clone()
    }

    /// The repo layer as committed at `rev` in `repo_path`, labelled
    /// `<slug>@<short sha>:<file>`.
    pub(crate) async fn repo_layer_at(&self, repo_path: &Path, rev: &Sha, slug: &str) -> LayerText {
        let git = &self.0.deps.git;
        let short: String = rev.as_str().chars().take(8).collect();
        let label = |file: &str| PathBuf::from(format!("{slug}@{short}:{file}"));
        for (file, legacy) in [(config::REPO_FILE, false), (config::LEGACY_REPO_FILE, true)] {
            if let Ok(Some(bytes)) = git.show(repo_path, rev, file).await {
                return LayerText {
                    layer: ConfigLayer::Repo,
                    path: label(file),
                    legacy,
                    text: Some(String::from_utf8_lossy(&bytes).into_owned()),
                };
            }
        }
        LayerText {
            layer: ConfigLayer::Repo,
            path: label(config::REPO_FILE),
            legacy: false,
            text: None,
        }
    }

    /// `config::merge`, reused while the layers' text is unchanged.
    pub(crate) fn merge_cached(&self, layers: &[LayerText]) -> Arc<Merged> {
        let key = layers_hash(layers);
        let mut cache = self
            .0
            .review_config
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(m) = cache.get(&key) {
            return m.clone();
        }
        let merged = Arc::new(config::merge(layers));
        if cache.len() >= CACHE_MAX {
            cache.clear();
        }
        cache.insert(key, merged.clone());
        merged
    }

    /// The review rules for a review read from `src`.
    pub(crate) async fn review_rules_at(&self, src: ReviewSource<'_>) -> ReviewRules {
        let layers = self.pr_config_layers(src).await;
        let merged = self.merge_cached(&layers);
        self.warn_config(&merged);
        rules_from(&merged.config.review, src.worktree).await
    }

    /// The review rules for an open PR, at its current head; the default
    /// (nothing set) when it isn't open.
    pub(crate) async fn review_rules_of(&self, pr: &PrId) -> ReviewRules {
        match self.open_entry(pr).await {
            Some(e) => self.review_rules_at(e.config_source()).await,
            None => ReviewRules::default(),
        }
    }

    /// Whether a finding on `path` of an open PR is dropped by
    /// `review.ignore_findings` (without resolving `context`).
    pub(crate) async fn finding_ignored(&self, pr: &PrId, path: &str) -> bool {
        let Some(e) = self.open_entry(pr).await else {
            return false;
        };
        let layers = self.pr_config_layers(e.config_source()).await;
        let merged = self.merge_cached(&layers);
        clean(&merged.config.review.ignore_findings)
            .iter()
            .any(|g| glob_match(g, path))
    }

    pub(crate) async fn open_entry(&self, pr: &PrId) -> Option<Arc<crate::core::RevisionEntry>> {
        let head = self.0.current_head.read().await.get(pr).cloned()?;
        self.0
            .revisions
            .read()
            .await
            .get(&(pr.clone(), head))
            .cloned()
    }

    /// The review rules a space's agent turns run with: its project's
    /// config (a host project's repo file at the space's base), `context`
    /// resolved against the space's worktree.
    pub(crate) async fn space_review_rules(
        &self,
        space: &theodore_contract::Space,
        project: &theodore_contract::Project,
        worktree: &Path,
    ) -> ReviewRules {
        let layers = self.project_layers(project, Some(&space.base_sha)).await;
        let merged = self.merge_cached(&layers);
        rules_from(&merged.config.review, worktree).await
    }

    /// The configured `review.auto_skills` for an open PR.
    pub(crate) async fn auto_skills_of(&self, src: ReviewSource<'_>) -> Vec<String> {
        let layers = self.pr_config_layers(src).await;
        let merged = self.merge_cached(&layers);
        let mut out: Vec<String> = Vec::new();
        for s in &merged.config.review.auto_skills {
            let s = s.trim();
            if !s.is_empty() && !out.iter().any(|o| o == s) {
                out.push(s.to_string());
            }
        }
        out
    }

    /// Whether `review.auto_skills` runs at all (off in a sandbox replaying
    /// agents, unless its scenario turns it on).
    pub fn set_auto_skills(&self, on: bool) {
        self.0
            .auto_skills
            .store(on, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn auto_skills_enabled(&self) -> bool {
        self.0.auto_skills.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// A new cache; `Inner` holds one.
pub(crate) fn new_cache() -> std::sync::Mutex<HashMap<String, Arc<Merged>>> {
    std::sync::Mutex::new(HashMap::new())
}

fn layers_hash(layers: &[LayerText]) -> String {
    let mut h = Sha256::new();
    for l in layers {
        h.update(format!("{:?}\0{}\0{}\0", l.layer, l.path.display(), l.legacy).as_bytes());
        match &l.text {
            Some(t) => {
                h.update(b"1");
                h.update(t.as_bytes());
            }
            None => h.update(b"0"),
        }
        h.update(b"\0");
    }
    hex::encode(h.finalize())
}

/// The typed `review:` section as rules, `context` resolved against `root`.
pub(crate) async fn rules_from(cfg: &ReviewConfig, root: &Path) -> ReviewRules {
    let root = root.to_path_buf();
    let entries = cfg.context.clone();
    let context = tokio::task::spawn_blocking(move || resolve_context(&root, &entries))
        .await
        .unwrap_or_default();
    ReviewRules {
        instructions: cfg
            .instructions
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        context,
        hide: clean(&cfg.hide),
        skim: clean(&cfg.skim),
        checklist: cfg
            .checklist
            .iter()
            .flat_map(|(glob, items)| {
                items
                    .items()
                    .into_iter()
                    .map(|i| i.trim().to_string())
                    .filter(|i| !i.is_empty())
                    .map(move |i| (glob.clone(), i))
            })
            .collect(),
        ignore_findings: clean(&cfg.ignore_findings),
    }
}

fn clean(globs: &[String]) -> Vec<String> {
    globs
        .iter()
        .map(|g| g.trim().to_string())
        .filter(|g| !g.is_empty())
        .collect()
}

/// `context` entries as existing repo paths: a plain path if it exists, a
/// glob expanded over the checkout (sorted); missing ones and paths
/// leaving the repo dropped with a debug note; at most
/// [`MAX_CONTEXT_FILES`].
pub(crate) fn resolve_context(root: &Path, entries: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut walked: Option<Vec<String>> = None;
    for entry in entries {
        if out.len() >= MAX_CONTEXT_FILES {
            break;
        }
        let entry = entry.trim().trim_start_matches("./");
        if entry.is_empty() {
            continue;
        }
        if !is_glob(entry) {
            let rel = entry.trim_end_matches('/');
            let inside = Path::new(rel)
                .components()
                .all(|c| matches!(c, Component::Normal(_)));
            if inside && root.join(rel).exists() {
                if !out.iter().any(|o| o == rel) {
                    out.push(rel.to_string());
                }
            } else {
                tracing::debug!("review.context: {entry} not found in {}", root.display());
            }
            continue;
        }
        let files = walked.get_or_insert_with(|| walk(root));
        let before = out.len();
        for f in files.iter() {
            if out.len() >= MAX_CONTEXT_FILES {
                break;
            }
            if glob_match(entry, f) && !out.contains(f) {
                out.push(f.clone());
            }
        }
        if out.len() == before {
            tracing::debug!(
                "review.context: {entry} matches nothing in {}",
                root.display()
            );
        }
    }
    out
}

/// Repo-relative files under `root`, sorted, without `.git`.
fn walk(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    let mut seen = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            seen += 1;
            if seen > WALK_MAX_ENTRIES {
                out.sort();
                return out;
            }
            let path = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                if e.file_name() != ".git" {
                    stack.push(path);
                }
            } else if let Ok(rel) = path.strip_prefix(root) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_keeps_existing_paths_expands_globs_and_drops_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("docs/adr")).unwrap();
        std::fs::write(root.join("docs/architecture.md"), "a").unwrap();
        std::fs::write(root.join("docs/adr/001.md"), "a").unwrap();
        std::fs::write(root.join("docs/adr/002.md"), "a").unwrap();
        std::fs::write(root.join("AGENTS.md"), "a").unwrap();
        let got = resolve_context(
            root,
            &[
                "docs/architecture.md".into(),
                "missing.md".into(),
                "../outside.md".into(),
                "docs/adr/*.md".into(),
                "AGENTS.md".into(),
                "AGENTS.md".into(),
            ],
        );
        assert_eq!(
            got,
            vec![
                "docs/architecture.md",
                "docs/adr/001.md",
                "docs/adr/002.md",
                "AGENTS.md"
            ]
        );
    }

    #[test]
    fn context_is_capped() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..(MAX_CONTEXT_FILES + 5) {
            std::fs::write(tmp.path().join(format!("f{i:02}.md")), "a").unwrap();
        }
        let got = resolve_context(tmp.path(), &["*.md".into()]);
        assert_eq!(got.len(), MAX_CONTEXT_FILES);
        assert_eq!(got[0], "f00.md");
    }
}
