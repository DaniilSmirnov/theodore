//! Per-agent model and effort defaults (`agents.<id>.model`,
//! `agents.<id>.effort`, `user` scope), filled into every run that doesn't
//! name its own. Wraps the `AgentPort` once, at the composition root, so
//! grouping, skills, chat, fixes and space turns all pick them up.

use std::sync::Arc;

use async_trait::async_trait;
use theodore_contract::models;
use theodore_domain::AgentId;
use theodore_ports::Store;
use theodore_ports::agent::{AgentError, AgentInfo, AgentPort, AgentRequest, AgentRun, ModelInfo};

use crate::settings::{SCOPE_USER, pick_setting};

pub fn model_key(agent: AgentId) -> String {
    format!("agents.{}.model", agent.as_str())
}

pub fn effort_key(agent: AgentId) -> String {
    format!("agents.{}.effort", agent.as_str())
}

/// `AgentPort` that fills `model` / `effort` from settings when a request
/// leaves them unset. An explicit value (a space's pinned model) wins.
pub struct AgentDefaults {
    inner: Arc<dyn AgentPort>,
    store: Arc<dyn Store>,
}

impl AgentDefaults {
    pub fn new(inner: Arc<dyn AgentPort>, store: Arc<dyn Store>) -> Self {
        Self { inner, store }
    }

    /// A non-empty string setting, `None` when unset, `null` or blank.
    async fn text(&self, key: String) -> Option<String> {
        let store = self.store.clone();
        let rows = tokio::task::spawn_blocking(move || store.query(models::SETTING, "key", &key))
            .await
            .ok()?
            .ok()?;
        // User scope only: like `agents.<id>.enabled`, a machine preference.
        let rows = rows
            .into_iter()
            .filter(|r| r.data.get("scope").and_then(|s| s.as_str()) == Some(SCOPE_USER))
            .collect();
        pick_setting(rows, None)?
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    }
}

#[async_trait]
impl AgentPort for AgentDefaults {
    async fn detect(&self) -> Vec<AgentInfo> {
        self.inner.detect().await
    }

    async fn run(&self, mut req: AgentRequest) -> Result<AgentRun, AgentError> {
        if req.model.is_none() {
            req.model = self.text(model_key(req.agent)).await;
        }
        if req.effort.is_none() {
            req.effort = self.text(effort_key(req.agent)).await;
        }
        self.inner.run(req).await
    }

    async fn models(&self, agent: AgentId) -> Vec<ModelInfo> {
        self.inner.models(agent).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{FakeAgent, FakeStore};
    use theodore_ports::agent::AgentMode;
    use theodore_ports::{NewAction, SyncActionCode};

    fn set(store: &FakeStore, scope: &str, key: &str, value: serde_json::Value) {
        store
            .apply(vec![NewAction {
                model: models::SETTING.into(),
                model_id: format!("{scope}:{key}"),
                action: SyncActionCode::I,
                data: Some(serde_json::json!({ "key": key, "scope": scope, "value": value })),
            }])
            .unwrap();
    }

    fn req(agent: AgentId, model: Option<&str>) -> AgentRequest {
        AgentRequest {
            agent,
            prompt: "p".into(),
            system: None,
            cwd: "/tmp".into(),
            mode: AgentMode::ReadOnly,
            mcp: None,
            model: model.map(str::to_string),
            effort: None,
            stall_after_ms: 1000,
            env: Vec::new(),
            user_mcp: false,
            attachments: vec![],
            resume: None,
            // `true`: so `FakeAgent::last_request` (reserved for turn-like
            // calls, not one-shot utility ones) sees it.
            stream_text: true,
        }
    }

    async fn sent(store: Arc<FakeStore>, r: AgentRequest) -> AgentRequest {
        let agent = FakeAgent::new();
        let port = AgentDefaults::new(agent.clone(), store);
        let _ = port.run(r).await;
        agent.last_request().expect("ran")
    }

    #[tokio::test]
    async fn settings_fill_an_unset_model_and_effort() {
        let store = FakeStore::new();
        set(&store, "user", "agents.claude.model", "opus".into());
        set(&store, "user", "agents.claude.effort", "high".into());
        let r = sent(store, req(AgentId::Claude, None)).await;
        assert_eq!(r.model.as_deref(), Some("opus"));
        assert_eq!(r.effort.as_deref(), Some("high"));
    }

    #[tokio::test]
    async fn an_explicit_model_wins() {
        let store = FakeStore::new();
        set(&store, "user", "agents.claude.model", "opus".into());
        let r = sent(store, req(AgentId::Claude, Some("haiku"))).await;
        assert_eq!(r.model.as_deref(), Some("haiku"));
    }

    #[tokio::test]
    async fn unset_null_or_blank_keeps_the_cli_default() {
        let store = FakeStore::new();
        set(
            &store,
            "user",
            "agents.claude.model",
            serde_json::Value::Null,
        );
        set(&store, "user", "agents.claude.effort", " ".into());
        let r = sent(store, req(AgentId::Claude, None)).await;
        assert_eq!(r.model, None);
        assert_eq!(r.effort, None);
    }

    #[tokio::test]
    async fn each_agent_reads_its_own_keys() {
        let store = FakeStore::new();
        set(&store, "user", "agents.claude.model", "opus".into());
        set(&store, "user", "agents.codex.model", "gpt-5".into());
        let r = sent(store, req(AgentId::Codex, None)).await;
        assert_eq!(r.model.as_deref(), Some("gpt-5"));
    }

    #[tokio::test]
    async fn codex_gets_its_effort_too() {
        let store = FakeStore::new();
        set(&store, "user", "agents.claude.effort", "max".into());
        set(&store, "user", "agents.codex.effort", "xhigh".into());
        let r = sent(store, req(AgentId::Codex, None)).await;
        assert_eq!(r.effort.as_deref(), Some("xhigh"));
    }

    #[tokio::test]
    async fn an_explicit_effort_wins() {
        let store = FakeStore::new();
        set(&store, "user", "agents.codex.effort", "xhigh".into());
        let mut r = req(AgentId::Codex, None);
        r.effort = Some("low".into());
        let r = sent(store, r).await;
        assert_eq!(r.effort.as_deref(), Some("low"));
    }

    #[tokio::test]
    async fn a_repo_scope_row_is_ignored() {
        let store = FakeStore::new();
        set(&store, "repo:o/r", "agents.claude.model", "opus".into());
        let r = sent(store, req(AgentId::Claude, None)).await;
        assert_eq!(r.model, None);
    }
}
