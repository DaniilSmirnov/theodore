//! Which models each CLI can run, and the effort levels each takes, for the
//! model picker (`AgentPort::models`).
//!
//! ```text
//! claude ─▶ Claude Code's own model catalog cache
//!            (`~/.claude/cache/model-catalog/*-cc.json`), else its
//!            `--model` aliases × `--effort` levels
//! codex  ─▶ `codex debug models` (JSON) ─▶ visibility "list", by priority
//! gemini ─▶ []
//! ```

use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use theodore_ports::agent::ModelInfo;

use crate::claude::EFFORT_LEVELS;

/// `codex debug models` is a few hundred ms; a hung one must not hold the
/// picker forever.
const CODEX_MODELS_TIMEOUT: Duration = Duration::from_secs(15);

/// Claude Code's models: the catalog it caches from Anthropic (every version
/// it runs, each with its own effort levels), else its aliases.
pub fn claude_models() -> Vec<ModelInfo> {
    claude_config_dir()
        .and_then(|dir| claude_catalog(&dir.join("cache/model-catalog")))
        .unwrap_or_else(claude_aliases)
}

/// `CLAUDE_CONFIG_DIR`, else `~/.claude`.
fn claude_config_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".claude")))
}

/// The newest `*-cc.json` (Claude Code's surface) in `dir` that parses to at
/// least one model. Its format is Claude Code's own and may change: any
/// surprise is `None`, and the aliases stand in.
pub fn claude_catalog(dir: &Path) -> Option<Vec<ModelInfo>> {
    let mut files: Vec<(std::time::SystemTime, std::path::PathBuf)> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with("-cc.json"))
        })
        .filter_map(|p| Some((std::fs::metadata(&p).ok()?.modified().ok()?, p)))
        .collect();
    files.sort_by_key(|a| std::cmp::Reverse(a.0));
    files.into_iter().find_map(|(_, p)| {
        let text = std::fs::read_to_string(p).ok()?;
        parse_claude_catalog(&text).filter(|m| !m.is_empty())
    })
}

#[derive(Deserialize)]
struct ClaudeCatalogFile {
    catalog: ClaudeCatalog,
}

#[derive(Deserialize)]
struct ClaudeCatalog {
    config: ClaudeCatalogConfig,
}

#[derive(Deserialize)]
struct ClaudeCatalogConfig {
    models: Vec<ClaudeCatalogModel>,
}

#[derive(Deserialize)]
struct ClaudeCatalogModel {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    section: Option<String>,
    #[serde(default)]
    runtime: Option<ClaudeRuntime>,
}

#[derive(Deserialize, Default)]
struct ClaudeRuntime {
    #[serde(default)]
    effort_levels: Vec<String>,
    #[serde(default)]
    default_effort: Option<String>,
}

/// Pure: a catalog file → models, in its order. `section: "main"` are the
/// current ones; the rest are earlier versions (`older`).
pub fn parse_claude_catalog(json: &str) -> Option<Vec<ModelInfo>> {
    let file: ClaudeCatalogFile = serde_json::from_str(json).ok()?;
    Some(
        file.catalog
            .config
            .models
            .into_iter()
            .map(|m| {
                let runtime = m.runtime.unwrap_or_default();
                ModelInfo {
                    label: m
                        .name
                        .filter(|s| !s.trim().is_empty())
                        .unwrap_or_else(|| m.id.clone()),
                    id: m.id,
                    description: m.description.filter(|s| !s.trim().is_empty()),
                    efforts: runtime.effort_levels,
                    default_effort: runtime.default_effort,
                    older: m.section.as_deref() != Some("main"),
                }
            })
            .collect(),
    )
}

/// Claude Code's `--model` aliases (each the latest of its family), newest
/// family first.
pub fn claude_aliases() -> Vec<ModelInfo> {
    [
        ("fable", "Fable", "Newest"),
        ("opus", "Opus", "Deep reasoning"),
        ("sonnet", "Sonnet", "Balanced"),
        ("haiku", "Haiku", "Fastest, cheapest"),
    ]
    .into_iter()
    .map(|(id, label, description)| ModelInfo {
        id: id.to_string(),
        label: label.to_string(),
        description: Some(description.to_string()),
        efforts: EFFORT_LEVELS.iter().map(|e| e.to_string()).collect(),
        default_effort: None,
        older: false,
    })
    .collect()
}

#[derive(Deserialize)]
struct CodexModels {
    models: Vec<CodexModel>,
}

#[derive(Deserialize)]
struct CodexModel {
    slug: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    default_reasoning_level: Option<String>,
    #[serde(default)]
    supported_reasoning_levels: Vec<CodexReasoningLevel>,
    #[serde(default)]
    visibility: Option<String>,
    #[serde(default)]
    priority: i64,
}

#[derive(Deserialize)]
struct CodexReasoningLevel {
    effort: String,
}

/// Pure: `codex debug models` stdout → the models the picker lists. Only
/// `visibility: "list"` (the ones Codex's own picker shows), by `priority`
/// ascending; ties keep Codex's order.
pub fn parse_codex_models(json: &str) -> Result<Vec<ModelInfo>, serde_json::Error> {
    let parsed: CodexModels = serde_json::from_str(json)?;
    let mut listed: Vec<CodexModel> = parsed
        .models
        .into_iter()
        .filter(|m| m.visibility.as_deref() == Some("list"))
        .collect();
    listed.sort_by_key(|m| m.priority);
    Ok(listed
        .into_iter()
        .map(|m| ModelInfo {
            label: m
                .display_name
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| m.slug.clone()),
            id: m.slug,
            description: m.description.filter(|s| !s.trim().is_empty()),
            efforts: m
                .supported_reasoning_levels
                .into_iter()
                .map(|l| l.effort)
                .collect(),
            default_effort: m.default_reasoning_level,
            older: false,
        })
        .collect())
}

/// Run `<codex> debug models` and parse it. `Err` carries a one-line reason
/// for the log.
pub async fn codex_models(program: &Path) -> Result<Vec<ModelInfo>, String> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(["debug", "models"])
        .env_clear()
        .envs(crate::env::filtered_env())
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let fut = crate::exit_output::output_after_exit(cmd);
    let output = tokio::time::timeout(CODEX_MODELS_TIMEOUT, fut)
        .await
        .map_err(|_| format!("timed out after {}s", CODEX_MODELS_TIMEOUT.as_secs()))?
        .map_err(|e| format!("spawn failed: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: String = stderr.trim().chars().rev().take(300).collect();
        let tail: String = tail.chars().rev().collect();
        return Err(format!("exited with {}: {tail}", output.status));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_codex_models(&stdout).map_err(|e| format!("bad JSON: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const SAMPLE: &str = r#"{
      "models": [
        {
          "slug": "gpt-b",
          "display_name": "GPT-B",
          "description": "Second.",
          "default_reasoning_level": "low",
          "supported_reasoning_levels": [
            {"effort": "low", "description": "Fast"},
            {"effort": "high", "description": "Deep"}
          ],
          "visibility": "list",
          "priority": 5,
          "context_window": 272000
        },
        {
          "slug": "gpt-hidden",
          "display_name": "Hidden",
          "description": "Not in the picker.",
          "default_reasoning_level": "medium",
          "supported_reasoning_levels": [{"effort": "medium", "description": ""}],
          "visibility": "hide",
          "priority": 0
        },
        {
          "slug": "gpt-a",
          "display_name": "GPT-A",
          "description": "First.",
          "default_reasoning_level": "medium",
          "supported_reasoning_levels": [
            {"effort": "low", "description": ""},
            {"effort": "medium", "description": ""},
            {"effort": "xhigh", "description": ""}
          ],
          "visibility": "list",
          "priority": 1
        }
      ]
    }"#;

    #[test]
    fn codex_lists_visible_models_by_priority() {
        let models = parse_codex_models(SAMPLE).unwrap();
        assert_eq!(
            models,
            vec![
                ModelInfo {
                    id: "gpt-a".into(),
                    label: "GPT-A".into(),
                    description: Some("First.".into()),
                    efforts: vec!["low".into(), "medium".into(), "xhigh".into()],
                    default_effort: Some("medium".into()),
                    older: false,
                },
                ModelInfo {
                    id: "gpt-b".into(),
                    label: "GPT-B".into(),
                    description: Some("Second.".into()),
                    efforts: vec!["low".into(), "high".into()],
                    default_effort: Some("low".into()),
                    older: false,
                },
            ]
        );
    }

    #[test]
    fn codex_missing_fields_fall_back() {
        let models =
            parse_codex_models(r#"{"models":[{"slug":"bare","visibility":"list"}]}"#).unwrap();
        assert_eq!(
            models,
            vec![ModelInfo {
                id: "bare".into(),
                label: "bare".into(),
                description: None,
                efforts: vec![],
                default_effort: None,
                older: false,
            }]
        );
    }

    #[test]
    fn codex_bad_json_is_an_error() {
        assert!(parse_codex_models("not json").is_err());
        assert!(parse_codex_models(r#"{"other":[]}"#).is_err());
    }

    const CLAUDE_CATALOG: &str = r#"{
      "version": 2,
      "catalog": {"surface": "cc", "config": {"id": "cc", "models": [
        {"id": "claude-opus-5-5", "name": "Opus 5.5", "description": "For complex work",
         "section": "main",
         "runtime": {"effort_levels": ["low", "medium", "high"], "default_effort": "medium"}},
        {"id": "claude-haiku-4-5-20251001", "name": "Haiku 4.5", "section": "overflow"},
        {"id": "claude-opus-4-6", "name": "", "section": "overflow",
         "runtime": {"effort_levels": ["low", "max"], "default_effort": "high"}}
      ]}}
    }"#;

    #[test]
    fn claude_catalog_lists_every_version_older_ones_marked() {
        let models = parse_claude_catalog(CLAUDE_CATALOG).unwrap();
        assert_eq!(
            models,
            vec![
                ModelInfo {
                    id: "claude-opus-5-5".into(),
                    label: "Opus 5.5".into(),
                    description: Some("For complex work".into()),
                    efforts: vec!["low".into(), "medium".into(), "high".into()],
                    default_effort: Some("medium".into()),
                    older: false,
                },
                ModelInfo {
                    id: "claude-haiku-4-5-20251001".into(),
                    label: "Haiku 4.5".into(),
                    description: None,
                    efforts: vec![],
                    default_effort: None,
                    older: true,
                },
                ModelInfo {
                    id: "claude-opus-4-6".into(),
                    label: "claude-opus-4-6".into(),
                    description: None,
                    efforts: vec!["low".into(), "max".into()],
                    default_effort: Some("high".into()),
                    older: true,
                },
            ]
        );
    }

    #[test]
    fn claude_catalog_takes_the_newest_cc_file_that_parses() {
        let dir = tempfile::tempdir().unwrap();
        // Not Claude Code's surface: ignored.
        std::fs::write(dir.path().join("published-x.json"), CLAUDE_CATALOG).unwrap();
        assert_eq!(claude_catalog(dir.path()), None);
        std::fs::write(dir.path().join("a-cc.json"), CLAUDE_CATALOG).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        // Newer but unreadable: the older good one still counts.
        std::fs::write(dir.path().join("b-cc.json"), "{not json").unwrap();
        let models = claude_catalog(dir.path()).unwrap();
        assert_eq!(models[0].id, "claude-opus-5-5");
        assert_eq!(claude_catalog(&dir.path().join("missing")), None);
    }

    #[test]
    fn claude_aliases_in_order_with_every_effort() {
        let models = claude_aliases();
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["fable", "opus", "sonnet", "haiku"]);
        for m in &models {
            assert_eq!(m.efforts, ["low", "medium", "high", "xhigh", "max"]);
            assert_eq!(m.default_effort, None);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn codex_models_runs_the_binary() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let ok = dir.path().join("codex");
        std::fs::write(
            &ok,
            "#!/bin/sh\n[ \"$1 $2\" = \"debug models\" ] || exit 2\n\
             echo '{\"models\":[{\"slug\":\"m\",\"visibility\":\"list\"}]}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&ok, std::fs::Permissions::from_mode(0o755)).unwrap();
        let models = codex_models(&ok).await.unwrap();
        assert_eq!(models.len(), 1);

        let bad = dir.path().join("codex-fails");
        std::fs::write(&bad, "#!/bin/sh\necho nope >&2\nexit 1\n").unwrap();
        std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = codex_models(&bad).await.unwrap_err();
        assert!(err.contains("nope"), "{err}");
    }
}
