use std::path::PathBuf;

use pretty_assertions::assert_eq;
use theodore_contract::{ConfigLayer, SpaceCommandKind};

use super::*;

fn layer(layer: ConfigLayer, text: &str) -> LayerText {
    LayerText {
        layer,
        path: PathBuf::from(format!("/{layer:?}.yaml")),
        legacy: false,
        text: Some(text.into()),
    }
}

#[test]
fn later_layers_win_key_by_key_and_lists_replace() {
    let m = merge(&[
        layer(
            ConfigLayer::Global,
            "spaces:\n  copy: [.env, .env.local]\n  ports: 1\n  env: {A: global, B: global}\n  check: make check\n",
        ),
        layer(
            ConfigLayer::Repo,
            "spaces:\n  copy: [.env.team]\n  env: {B: repo}\n  run: pnpm dev\n",
        ),
        layer(
            ConfigLayer::Local,
            "spaces:\n  ports: 3\n  env: {C: local}\n",
        ),
    ]);
    let s = &m.config.spaces;
    assert_eq!(
        s.effective_copy(),
        vec![".env.team".to_string()],
        "lists replace"
    );
    assert_eq!(s.port_count(), 3);
    assert_eq!(s.env["A"], "global");
    assert_eq!(s.env["B"], "repo");
    assert_eq!(s.env["C"], "local");
    assert_eq!(s.command(SpaceCommandKind::Run), Some("pnpm dev"));
    assert_eq!(s.command(SpaceCommandKind::Check), Some("make check"));
    assert_eq!(m.layer_of("spaces.copy"), Some(ConfigLayer::Repo));
    assert_eq!(m.layer_of("spaces.ports"), Some(ConfigLayer::Local));
    assert_eq!(m.layer_of("spaces.env.A"), Some(ConfigLayer::Global));
    assert_eq!(m.layer_of("spaces.check"), Some(ConfigLayer::Global));
    assert!(m.unknown.is_empty(), "{:?}", m.unknown);
}

#[test]
fn defaults_when_nothing_is_set() {
    let m = merge(&[]);
    let s = &m.config.spaces;
    // Nothing in any layer: a space still copies `.env*` and reserves 2
    // ports, so it works with zero config.
    assert_eq!(s.port_count(), 2);
    assert_eq!(s.effective_copy(), vec![".env*".to_string()]);
    assert_eq!(s.command(SpaceCommandKind::Setup), None);
    assert_eq!(
        s.timeout(SpaceCommandKind::Setup),
        Some(std::time::Duration::from_secs(600))
    );
    assert_eq!(
        s.timeout(SpaceCommandKind::Archive),
        Some(std::time::Duration::from_secs(120))
    );
    assert_eq!(s.timeout(SpaceCommandKind::Run), None);
}

#[test]
fn explicit_zero_and_empty_turn_the_defaults_off() {
    let m = merge(&[layer(
        ConfigLayer::Repo,
        "spaces:\n  copy: []\n  ports: 0\n",
    )]);
    let s = &m.config.spaces;
    assert_eq!(s.effective_copy(), Vec::<String>::new());
    assert_eq!(s.port_count(), 0);
}

#[test]
fn a_set_copy_or_ports_replaces_the_default_without_adding_to_it() {
    let m = merge(&[layer(
        ConfigLayer::Repo,
        "spaces:\n  copy: [.env.team]\n  ports: 1\n",
    )]);
    let s = &m.config.spaces;
    assert_eq!(s.effective_copy(), vec![".env.team".to_string()]);
    assert_eq!(s.port_count(), 1);
}

#[test]
fn timeouts_and_blank_commands() {
    let m = merge(&[layer(
        ConfigLayer::Repo,
        "spaces:\n  setup: '  '\n  timeout_s: {check: 30}\n  ports: 999\n",
    )]);
    let s = &m.config.spaces;
    assert_eq!(s.command(SpaceCommandKind::Setup), None);
    assert_eq!(
        s.timeout(SpaceCommandKind::Check),
        Some(std::time::Duration::from_secs(30))
    );
    assert_eq!(s.port_count(), MAX_PORTS);
}

#[test]
fn legacy_flat_and_top_level_agents_map_under_review() {
    let m = merge(&[
        layer(ConfigLayer::Global, "agents:\n  chat: codex\n"),
        layer(
            ConfigLayer::Repo,
            "agents.grouping: gemini\nreview:\n  agents: {fix: codex}\n",
        ),
    ]);
    let a = &m.config.review.agents;
    assert_eq!(a["chat"], serde_json::json!("codex"));
    assert_eq!(a["grouping"], serde_json::json!("gemini"));
    assert_eq!(a["fix"], serde_json::json!("codex"));
    assert_eq!(m.layer_of("review.agents.chat"), Some(ConfigLayer::Global));
    assert_eq!(
        m.layer_of("review.agents.grouping"),
        Some(ConfigLayer::Repo)
    );
    assert!(m.unknown.is_empty(), "{:?}", m.unknown);
}

#[test]
fn review_keys_parse() {
    let m = merge(&[layer(
        ConfigLayer::Repo,
        r#"
review:
  instructions: Prefer small functions.
  context: [docs/arch.md]
  hide: ["**/*.lock"]
  skim: ["gen/**"]
  checklist:
    "migrations/**": Has a down migration?
    "api/**": [Versioned?, Documented?]
    "**/*.sql": Reversible?
  auto_skills: [security]
  ignore_findings: ["vendor/**"]
"#,
    )]);
    let r = &m.config.review;
    assert_eq!(r.instructions.as_deref(), Some("Prefer small functions."));
    assert_eq!(r.context, vec!["docs/arch.md".to_string()]);
    assert_eq!(r.hide, vec!["**/*.lock".to_string()]);
    assert_eq!(r.skim, vec!["gen/**".to_string()]);
    assert_eq!(
        r.checklist["migrations/**"].items(),
        vec!["Has a down migration?".to_string()]
    );
    assert_eq!(r.checklist["api/**"].items().len(), 2);
    // A `.` in a glob key is part of the key, not a path.
    assert_eq!(
        r.checklist["**/*.sql"].items(),
        vec!["Reversible?".to_string()]
    );
    assert!(m.sources.contains_key("review.checklist.**/*.sql"));
    assert!(m.errors.is_empty(), "{:?}", m.errors);
    assert_eq!(r.auto_skills, vec!["security".to_string()]);
    assert_eq!(r.ignore_findings, vec!["vendor/**".to_string()]);
    assert!(m.unknown.is_empty(), "{:?}", m.unknown);
}

#[test]
fn unknown_keys_are_reported_with_their_file() {
    let m = merge(&[layer(
        ConfigLayer::Repo,
        "spaces:\n  setpu: x\n  ports: 1\nbudget:\n  usd: 5\n",
    )]);
    let keys: Vec<&str> = m.unknown.iter().map(|(_, k)| k.as_str()).collect();
    assert_eq!(keys, ["budget.usd", "spaces.setpu"]);
    assert_eq!(m.unknown[0].0, "/Repo.yaml");
    assert_eq!(m.config.spaces.port_count(), 1, "known keys still apply");
}

#[test]
fn a_bad_file_is_skipped_and_a_bad_section_is_dropped() {
    let m = merge(&[
        layer(ConfigLayer::Global, "spaces: {ports: 2}"),
        layer(ConfigLayer::Repo, "spaces: [not, a, map"),
        layer(ConfigLayer::Local, "review: {hide: 5}"),
    ]);
    assert!(m.files[1].error.is_some());
    assert_eq!(m.config.spaces.port_count(), 2);
    assert_eq!(m.errors.len(), 1);
    assert_eq!(m.errors[0].0, "review");
}

#[test]
fn entries_show_values_and_their_layer() {
    let m = merge(&[
        layer(ConfigLayer::Global, "spaces: {run: a, ports: 1}"),
        layer(ConfigLayer::Local, "spaces: {run: b}"),
    ]);
    let e = m.entries();
    assert_eq!(e.len(), 2);
    assert_eq!(e[0].key, "spaces.ports");
    assert_eq!(e[0].value, "1");
    assert_eq!(e[0].layer, ConfigLayer::Global);
    assert_eq!(e[1].key, "spaces.run");
    assert_eq!(e[1].value, "b");
    assert_eq!(e[1].layer, ConfigLayer::Local);
}

#[test]
fn an_empty_map_keeps_earlier_keys() {
    let m = merge(&[
        layer(ConfigLayer::Global, "spaces: {env: {A: x}}"),
        layer(ConfigLayer::Repo, "spaces: {env: {}}"),
    ]);
    // An empty map merges into the earlier one: A stays.
    assert_eq!(m.layer_of("spaces.env.A"), Some(ConfigLayer::Global));
}

#[tokio::test]
async fn the_legacy_repo_file_is_read_when_the_new_one_is_absent() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".review")).unwrap();
    std::fs::write(
        dir.path().join(LEGACY_REPO_FILE),
        "agents:\n  grouping: codex\n",
    )
    .unwrap();
    let l = read_repo_layer(dir.path()).await;
    assert!(l.legacy);
    assert_eq!(l.path, dir.path().join(LEGACY_REPO_FILE));

    std::fs::create_dir_all(dir.path().join(".theodore")).unwrap();
    std::fs::write(
        dir.path().join(REPO_FILE),
        "review: {agents: {chat: gemini}}",
    )
    .unwrap();
    let l = read_repo_layer(dir.path()).await;
    assert!(!l.legacy, "the new file wins; the legacy one is ignored");
    let m = merge(&[l]);
    assert_eq!(m.config.review.agents.get("grouping"), None);
    assert_eq!(m.config.review.agents["chat"], serde_json::json!("gemini"));
}

#[tokio::test]
async fn read_layers_takes_the_local_file_from_the_checkout_only() {
    let data = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let checkout = tempfile::tempdir().unwrap();
    std::fs::write(global_path(data.path()), "spaces: {ports: 1}").unwrap();
    for root in [repo.path(), checkout.path()] {
        std::fs::create_dir_all(root.join(".theodore")).unwrap();
    }
    std::fs::write(repo.path().join(LOCAL_FILE), "spaces: {ports: 9}").unwrap();
    std::fs::write(checkout.path().join(LOCAL_FILE), "spaces: {ports: 2}").unwrap();
    let layers = read_layers(data.path(), Some(repo.path()), Some(checkout.path())).await;
    let m = merge(&layers);
    assert_eq!(m.config.spaces.port_count(), 2);
    assert_eq!(m.layer_of("spaces.ports"), Some(ConfigLayer::Local));
    let without = merge(&read_layers(data.path(), Some(repo.path()), None).await);
    assert_eq!(
        without.config.spaces.port_count(),
        1,
        "no local layer without a checkout"
    );
}

#[test]
fn the_template_is_all_comments() {
    let m = merge(&[layer(ConfigLayer::Global, GLOBAL_TEMPLATE)]);
    assert!(m.files[0].error.is_none());
    assert!(m.sources.is_empty());
}
