//! `space_fix_ci` (docs/spaces.md "Fix CI"): a space's agent gets its PR's
//! failing checks — their logs when the host has them, a bare link
//! otherwise — as one turn, against `Core` + `testkit` fakes. Reuses the
//! local-folder `Env` harness from `space_commands_tests` (real temp
//! folder, so a `spaces.check` command from its config can be read).

use serde_json::json;
use theodore_contract::{SpaceFixCiRequest, SpaceStatus, models};
use theodore_domain::{Author, Check, CheckStatus, HostKind, PrId, PrRef, RepoRef, Sha};
use theodore_ports::{Fetched, HostError, NewAction, Store, SyncActionCode};

use crate::AppError;
use crate::space_commands_tests::{Env, done, start_env};
use crate::testkit::{self, Fixtures};
use crate::usecases::spaces::space_chat_key;

fn transcript(f: &Fixtures, id: &str) -> Vec<theodore_contract::ChatMessage> {
    let mut out: Vec<theodore_contract::ChatMessage> = f
        .store
        .query(models::CHAT_MESSAGE, "pr", &space_chat_key(id).0)
        .unwrap()
        .into_iter()
        .map(|r| serde_json::from_value(r.data).unwrap())
        .collect();
    out.sort_by_key(|m| (m.at, matches!(m.author, Author::Agent(_))));
    out
}

fn put(f: &Fixtures, model: &str, id: &str, data: serde_json::Value) {
    f.store
        .apply(vec![NewAction {
            model: model.into(),
            model_id: id.into(),
            action: SyncActionCode::I,
            data: Some(data),
        }])
        .unwrap();
}

fn patch_space(f: &Fixtures, id: &str, data: serde_json::Value) {
    f.store
        .apply(vec![NewAction {
            model: models::SPACE.into(),
            model_id: id.into(),
            action: SyncActionCode::C,
            data: Some(data),
        }])
        .unwrap();
}

fn invalid(err: AppError) -> String {
    match err {
        AppError::Invalid(m) => m,
        other => panic!("expected Invalid, got {other:?}"),
    }
}

fn fixture_pr_ref() -> PrRef {
    PrRef {
        repo: RepoRef {
            host: HostKind::Github,
            owner: "acme".into(),
            name: "widgets".into(),
        },
        number: 7,
        host_id: Some("node-7".into()),
    }
}

/// Writes `models::PR` and `models::PR_DETAIL` rows directly (no `open_pr`
/// round trip needed): `pr_ref` and the fallback `load_pr_detail` read
/// straight from the store.
fn seed_pr(f: &Fixtures, checks: Vec<Check>) -> PrId {
    let pr_ref = fixture_pr_ref();
    let mut detail = testkit::sample_pr_detail(&pr_ref, &Sha::new("base000"), &Sha::new("head111"));
    detail.checks = checks;
    let id = pr_ref.id();
    put(
        f,
        models::PR,
        &id.0,
        serde_json::to_value(&detail.summary).unwrap(),
    );
    put(
        f,
        models::PR_DETAIL,
        &id.0,
        serde_json::to_value(&detail).unwrap(),
    );
    id
}

fn check(id: &str, name: &str, status: CheckStatus, url: Option<&str>) -> Check {
    Check {
        id: id.into(),
        name: name.into(),
        status,
        url: url.map(str::to_string),
        duration_ms: None,
    }
}

async fn idle(env: &Env, id: &str) -> theodore_contract::Space {
    env.until(id, |s| s.status != SpaceStatus::Running).await
}

#[tokio::test]
async fn fix_ci_sends_the_failing_checks_log_as_one_turn() {
    let env = start_env(false).await;
    let s = env.create("").await;

    let pr = seed_pr(
        &env.f,
        vec![
            check("900001", "build", CheckStatus::Fail, None),
            check("900002", "lint", CheckStatus::Pass, None),
        ],
    );
    patch_space(&env.f, &s.id, json!({ "pr": pr }));
    // Nothing new since the stored copy: `load_pr_detail` is used.
    env.f.host.push_get_pr(Ok(Fetched::NotModified));
    env.f.host.push_check_log(Ok(Some(
        "2024-01-01T00:00:00.0000000Z build failed\n".into(),
    )));

    env.f.agents.push_script(done("Fixed the build."));
    env.core
        .space_fix_ci(SpaceFixCiRequest {
            space: s.id.clone(),
        })
        .await
        .expect("space_fix_ci");
    idle(&env, &s.id).await;

    assert_eq!(env.f.host.check_log_calls(), vec!["900001".to_string()]);

    let prompt = env.f.agents.last_request().unwrap().prompt;
    assert!(
        prompt.contains("This pull request's check is failing:\n"),
        "{prompt}"
    );
    assert!(
        prompt.contains("- build — its log is attached (build.log)"),
        "{prompt}"
    );
    assert!(!prompt.contains("lint"), "{prompt}");

    let mine = transcript(&env.f, &s.id)
        .into_iter()
        .find(|m| m.author == Author::Me)
        .unwrap();
    assert_eq!(mine.attachments.len(), 1);
    assert_eq!(mine.attachments[0].name, "build.log");
}

#[tokio::test]
async fn fix_ci_mentions_the_space_check_command_when_configured() {
    let env = start_env(false).await;
    env.global("spaces: {check: pnpm test}");
    let s = env.create("").await;

    let pr = seed_pr(&env.f, vec![check("1", "test", CheckStatus::Fail, None)]);
    patch_space(&env.f, &s.id, json!({ "pr": pr }));
    env.f.host.push_get_pr(Ok(Fetched::NotModified));
    env.f.host.push_check_log(Ok(None));
    env.f.agents.push_script(done("ok"));

    env.core
        .space_fix_ci(SpaceFixCiRequest {
            space: s.id.clone(),
        })
        .await
        .expect("space_fix_ci");
    idle(&env, &s.id).await;

    let prompt = env.f.agents.last_request().unwrap().prompt;
    assert!(prompt.contains("`pnpm test`"), "{prompt}");
}

#[tokio::test]
async fn fix_ci_links_a_check_the_host_has_no_log_for() {
    let env = start_env(false).await;
    let s = env.create("").await;

    let pr = seed_pr(
        &env.f,
        vec![check(
            "sc1",
            "vercel",
            CheckStatus::Fail,
            Some("https://vercel.example/deployments/1"),
        )],
    );
    patch_space(&env.f, &s.id, json!({ "pr": pr }));
    env.f.host.push_get_pr(Ok(Fetched::NotModified));
    env.f
        .host
        .push_check_log(Err(HostError::Unsupported("check_log")));
    env.f.agents.push_script(done("ok"));

    env.core
        .space_fix_ci(SpaceFixCiRequest {
            space: s.id.clone(),
        })
        .await
        .expect("space_fix_ci");
    idle(&env, &s.id).await;

    let prompt = env.f.agents.last_request().unwrap().prompt;
    assert!(
        prompt.contains("- vercel — https://vercel.example/deployments/1 (no log to fetch)"),
        "{prompt}"
    );
    let mine = transcript(&env.f, &s.id)
        .into_iter()
        .find(|m| m.author == Author::Me)
        .unwrap();
    assert!(mine.attachments.is_empty());
}

#[tokio::test]
async fn fix_ci_is_refused_without_a_pull_request() {
    let env = start_env(false).await;
    let s = env.create("").await;

    let err = env
        .core
        .space_fix_ci(SpaceFixCiRequest {
            space: s.id.clone(),
        })
        .await
        .unwrap_err();
    assert!(invalid(err).contains("no pull request"));
}

#[tokio::test]
async fn fix_ci_is_refused_with_no_failing_checks() {
    let env = start_env(false).await;
    let s = env.create("").await;

    let pr = seed_pr(&env.f, vec![check("1", "build", CheckStatus::Pass, None)]);
    patch_space(&env.f, &s.id, json!({ "pr": pr }));
    env.f.host.push_get_pr(Ok(Fetched::NotModified));

    let err = env
        .core
        .space_fix_ci(SpaceFixCiRequest {
            space: s.id.clone(),
        })
        .await
        .unwrap_err();
    assert!(invalid(err).contains("no failing checks"));
    assert!(env.f.host.check_log_calls().is_empty());
}

#[tokio::test]
async fn fix_ci_is_refused_while_a_turn_is_running() {
    let env = start_env(false).await;
    let s = env.create("").await;
    patch_space(&env.f, &s.id, json!({ "status": "running" }));

    let err = env
        .core
        .space_fix_ci(SpaceFixCiRequest {
            space: s.id.clone(),
        })
        .await
        .unwrap_err();
    assert!(invalid(err).contains("still working"));
}
