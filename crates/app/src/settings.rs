//! Settings (plan §8.5): `agents.grouping`, `agents.chat`, `agents.skills`,
//! `agents.fix`, `agents.user_mcp`, `updates.channel`, each with a default.
//! Layered scopes:
//! `user` is the baseline; a project's config files (docs/config.md, read
//! by `load_repo_settings`) set `review.agents.*`, stored as `repo:<slug>`
//! rows that win over `user` for that repo.
//!
//! `agents.<id>.model` / `agents.<id>.effort` (user scope, unset = the
//! CLI's own default) are read per run by `AgentDefaults`.

use theodore_contract::{Setting, models};
use theodore_domain::{AgentId, PrId, RepoRef};
use theodore_ports::{NewAction, SyncActionCode};

use crate::config;

use crate::core::Core;
use crate::error::AppResult;

pub const SCOPE_USER: &str = "user";
/// The setting key behind [`Core::user_mcp_allowed`].
pub const USER_MCP: &str = "agents.user_mcp";

pub fn defaults() -> Vec<Setting> {
    vec![
        Setting {
            key: "agents.grouping".into(),
            scope: SCOPE_USER.into(),
            value: serde_json::json!(AgentId::Claude),
        },
        Setting {
            key: "agents.chat".into(),
            scope: SCOPE_USER.into(),
            value: serde_json::json!(AgentId::Claude),
        },
        Setting {
            key: "agents.skills".into(),
            scope: SCOPE_USER.into(),
            value: serde_json::json!([AgentId::Claude]),
        },
        Setting {
            key: "agents.fix".into(),
            scope: SCOPE_USER.into(),
            value: serde_json::json!(AgentId::Claude),
        },
        Setting {
            key: "updates.channel".into(),
            scope: SCOPE_USER.into(),
            value: serde_json::json!("stable"),
        },
        Setting {
            key: "agents.stall_after_ms".into(),
            scope: SCOPE_USER.into(),
            value: serde_json::json!(DEFAULT_STALL_AFTER_MS),
        },
    ]
}

/// No output for this long while an agent run is alive → `AgentEvent::Stalled`
/// (plan "failed agent runs"). 120s: long enough that a slow tool call
/// doesn't look stuck, short enough that a genuinely hung CLI is caught
/// well before a human gives up and force-quits the app.
pub const DEFAULT_STALL_AFTER_MS: u64 = 120_000;

impl Core {
    /// Seeds any default setting not already present. Idempotent; the shell
    /// calls this once after `Core::start` (not done automatically there,
    /// so bootstrapping stays deterministic for tests).
    pub async fn ensure_default_settings(&self) -> AppResult<()> {
        let mut actions = Vec::new();
        for setting in defaults() {
            let store = self.0.deps.store.clone();
            let key = setting.key.clone();
            let existing = tokio::task::spawn_blocking(move || store.get(models::SETTING, &key))
                .await
                .ok()
                .and_then(|r| r.ok())
                .flatten();
            if existing.is_none() {
                let data = serde_json::to_value(&setting).expect("serializable");
                actions.push(NewAction {
                    model: models::SETTING.into(),
                    model_id: setting.key.clone(),
                    action: theodore_ports::SyncActionCode::I,
                    data: Some(data),
                });
            }
        }
        self.apply(actions).await?;
        Ok(())
    }

    pub async fn setting_agent(&self, key: &str, default: AgentId) -> AgentId {
        self.setting_agent_scoped(key, None, default).await
    }

    pub async fn setting_agents(&self, key: &str, default: Vec<AgentId>) -> Vec<AgentId> {
        self.setting_agents_scoped(key, None, default).await
    }

    pub async fn setting_string(&self, key: &str, default: &str) -> String {
        self.setting_value_scoped(key, None)
            .await
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| default.to_string())
    }

    /// `repo_scope` is a repo slug (`owner/name`), not the full `repo:<slug>`
    /// setting scope string — the repo value wins over `user` when present.
    pub async fn setting_agent_scoped(
        &self,
        key: &str,
        repo_scope: Option<&str>,
        default: AgentId,
    ) -> AgentId {
        self.setting_value_scoped(key, repo_scope)
            .await
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or(default)
    }

    pub async fn setting_agents_scoped(
        &self,
        key: &str,
        repo_scope: Option<&str>,
        default: Vec<AgentId>,
    ) -> Vec<AgentId> {
        self.setting_value_scoped(key, repo_scope)
            .await
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or(default)
    }

    /// A numeric setting; `None` when unset or not a number.
    pub async fn setting_f64_scoped(&self, key: &str, repo_scope: Option<&str>) -> Option<f64> {
        self.setting_value_scoped(key, repo_scope)
            .await
            .and_then(|v| v.as_f64())
    }

    /// `agents.stall_after_ms`: no scope override today (a stall timeout is
    /// a machine/CLI property, not a per-repo one), kept as `None` repo scope
    /// so it's a one-line change if that ever needs to differ.
    pub async fn stall_after_ms(&self) -> u64 {
        self.setting_value_scoped("agents.stall_after_ms", None)
            .await
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_STALL_AFTER_MS)
    }

    /// `agents.<id>.enabled` (plan "enable/disable agents"), `user` scope
    /// only — a machine-level preference, not something a repo's
    /// `.review/config.yaml` overrides. Unset = enabled: installing
    /// Theodore must not silently turn every agent off.
    pub async fn agent_enabled(&self, agent: AgentId) -> bool {
        let key = format!("agents.{}.enabled", agent.as_str());
        self.setting_value_scoped(&key, None)
            .await
            .and_then(|v| v.as_bool())
            .unwrap_or(true)
    }

    /// `agents.user_mcp`: agent runs may use the user's own MCP servers
    /// (Claude Code's `~/.claude.json`, `.mcp.json`, claude.ai connectors)
    /// next to Theodore's tools. `user` scope only — what a run may reach
    /// beyond the repo is the user's call, not a repo config's. Unset = on.
    pub async fn user_mcp_allowed(&self) -> bool {
        let store = self.0.deps.store.clone();
        let rows =
            tokio::task::spawn_blocking(move || store.query(models::SETTING, "key", USER_MCP))
                .await
                .ok()
                .and_then(|r| r.ok())
                .unwrap_or_default();
        // Not `pick_setting`: with no user row it falls back to any scope,
        // and a repo's `review.agents.user_mcp` must not count.
        rows.into_iter()
            .filter_map(|r| serde_json::from_value::<Setting>(r.data).ok())
            .find(|s| s.scope == SCOPE_USER)
            .and_then(|s| s.value.as_bool())
            .unwrap_or(true)
    }

    /// The first of `installed` (in order) that isn't disabled. Used
    /// wherever a configured agent setting (e.g. `agents.grouping`) names an
    /// agent the user has since disabled — falls back instead of running a
    /// disabled CLI.
    pub async fn first_enabled(&self, installed: &[AgentId]) -> Option<AgentId> {
        for &a in installed {
            if self.agent_enabled(a).await {
                return Some(a);
            }
        }
        None
    }

    /// The repo slug of an open PR, for scoped setting lookups. `None` for a
    /// PR not open in this session (falls back to the `user` scope).
    pub(crate) async fn repo_scope_of(&self, pr: &PrId) -> Option<String> {
        self.pr_ref(pr).await.map(|r| r.repo.slug())
    }

    /// No agent run in flight and no host write still queued — the shell
    /// may restart for an update.
    pub async fn is_idle(&self) -> bool {
        if !self.0.runs.read().await.is_empty() {
            return false;
        }
        let store = self.0.deps.store.clone();
        let pending = tokio::task::spawn_blocking(move || store.outbox_pending_count())
            .await
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or(0);
        pending == 0
    }

    pub(crate) async fn setting_value_scoped(
        &self,
        key: &str,
        repo_scope: Option<&str>,
    ) -> Option<serde_json::Value> {
        let store = self.0.deps.store.clone();
        let key_owned = key.to_string();
        let rows =
            tokio::task::spawn_blocking(move || store.query(models::SETTING, "key", &key_owned))
                .await
                .ok()?
                .ok()?;
        pick_setting(rows, repo_scope)
    }

    /// Reads a review's config (docs/config.md: the global file, then the
    /// repo's `.theodore/config.yaml` or legacy `.review/config.yaml` at
    /// the base) and applies its `review.agents` to the repo. Best-effort:
    /// a missing file is normal; a broken one is a toast, once.
    pub(crate) async fn load_repo_settings(
        &self,
        src: crate::usecases::review_config::ReviewSource<'_>,
    ) {
        // A local folder's config is its own files (working copy and
        // personal layer), whichever review of it is opened: one source per
        // repo, so a review's checkout never undoes the folder's values.
        let layers = self.pr_config_layers(src).await;
        let merged = self.merge_cached(&layers);
        self.warn_config(&merged);
        self.apply_review_agents(&merged, src.repo).await;
    }

    /// `review.agents.<k>` → `setting` rows `agents.<k>` scoped to `repo`,
    /// which win over the `user` scope for it. Rows of keys no longer set
    /// are removed, so deleting a line from the file takes effect.
    pub(crate) async fn apply_review_agents(&self, merged: &config::Merged, repo: &RepoRef) {
        let scope = format!("repo:{}", repo.slug());
        let wanted: Vec<(String, serde_json::Value)> = merged
            .config
            .review
            .agents
            .iter()
            .map(|(k, v)| (format!("agents.{k}"), v.clone()))
            .collect();
        let store = self.0.deps.store.clone();
        let scope_q = scope.clone();
        let existing: Vec<Setting> =
            tokio::task::spawn_blocking(move || store.query(models::SETTING, "scope", &scope_q))
                .await
                .ok()
                .and_then(|r| r.ok())
                .unwrap_or_default()
                .into_iter()
                .filter_map(|r| serde_json::from_value(r.data).ok())
                .collect();
        let mut actions = Vec::new();
        for s in &existing {
            if s.key.starts_with("agents.") && !wanted.iter().any(|(k, _)| *k == s.key) {
                actions.push(NewAction {
                    model: models::SETTING.into(),
                    model_id: format!("{scope}:{}", s.key),
                    action: SyncActionCode::D,
                    data: None,
                });
            }
        }
        for (key, value) in wanted {
            if existing.iter().any(|s| s.key == key && s.value == value) {
                continue;
            }
            let setting = Setting {
                key: key.clone(),
                scope: scope.clone(),
                value,
            };
            let Ok(data) = serde_json::to_value(&setting) else {
                continue;
            };
            actions.push(NewAction {
                model: models::SETTING.into(),
                model_id: format!("{scope}:{key}"),
                action: SyncActionCode::U,
                data: Some(data),
            });
        }
        let _ = self.apply(actions).await;
    }
}

/// One setting's value among its rows: the repo scope's when asked for and
/// present, else the user scope's, else any.
pub(crate) fn pick_setting(
    rows: Vec<theodore_ports::ModelRow>,
    repo_scope: Option<&str>,
) -> Option<serde_json::Value> {
    let settings: Vec<Setting> = rows
        .into_iter()
        .filter_map(|r| serde_json::from_value(r.data).ok())
        .collect();
    if let Some(scope) = repo_scope {
        let repo_scope_key = format!("repo:{scope}");
        if let Some(s) = settings.iter().find(|s| s.scope == repo_scope_key) {
            return Some(s.value.clone());
        }
    }
    settings
        .iter()
        .find(|s| s.scope == SCOPE_USER)
        .or_else(|| settings.first())
        .map(|s| s.value.clone())
}
