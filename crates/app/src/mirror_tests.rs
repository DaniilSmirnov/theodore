//! Mirroring a space into a local folder (docs/spaces.md "Mirror into a
//! local folder") against `Core` + `testkit` fakes: turning it on syncs
//! from the space's base, a turn re-syncs automatically, Mirror off
//! refuses on an edited folder unless discarded, switching moves it, and
//! a mirror left `On` comes back as `Paused`.

use std::time::Duration;

use serde_json::json;
use theodore_contract::{
    AddProjectRequest, CreateSpaceRequest, MirrorStatus, Project, Space, SpaceSendRequest,
    SpaceStatus, UnmirrorSpaceRequest, models,
};
use theodore_domain::{Sha, Usage};
use theodore_ports::{AgentEvent, Store};

use crate::testkit::{Fixtures, fixtures};
use crate::{AppError, Core};

async fn wait_until(mut cond: impl FnMut() -> bool) {
    for _ in 0..400 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("condition not met in time");
}

async fn folder_project(f: &Fixtures, path: &str) -> (Core, Project) {
    let (core, _recv) = Core::start(f.deps());
    tokio::time::sleep(Duration::from_millis(30)).await;
    let project = core
        .add_project(AddProjectRequest {
            repo: None,
            path: Some(path.into()),
        })
        .await
        .unwrap();
    wait_until(|| {
        f.store
            .get(models::PROJECT, &project.id)
            .unwrap()
            .is_some_and(|r| r.data["status"] == json!("ready"))
    })
    .await;
    f.git.set_resolve("main", Sha::new("base000"));
    f.git.set_working_state(theodore_ports::WorkingState {
        current_branch: Some("main".into()),
        dirty_files: 0,
    });
    f.git.set_resolve("HEAD", Sha::new("base000"));
    (core, project)
}

fn space(f: &Fixtures, id: &str) -> Space {
    serde_json::from_value(f.store.get(models::SPACE, id).unwrap().unwrap().data).unwrap()
}

async fn idle(f: &Fixtures, id: &str) -> Space {
    wait_until(|| space(f, id).status != SpaceStatus::Running).await;
    space(f, id)
}

fn done(text: &str) -> Vec<AgentEvent> {
    vec![AgentEvent::Done {
        final_text: text.into(),
        usage: Usage {
            input_tokens: 1,
            output_tokens: 1,
            usd: 0.0,
        },
    }]
}

/// A space with one finished turn at checkpoint `turn111`, base `base000`.
async fn one_turn(f: &Fixtures, core: &Core, project: &Project, goal: &str) -> Space {
    f.agents.push_script(done("done"));
    f.git.push_checkpoint(Sha::new("turn111"));
    let s = core
        .create_space(CreateSpaceRequest {
            project: project.id.clone(),
            goal: goal.into(),
            name: None,
            branch: None,
            base: None,
            agent: None,
            model: None,
            start_from: None,
        })
        .await
        .unwrap();
    idle(f, &s.id).await
}

async fn turn(f: &Fixtures, core: &Core, s: &Space, text: &str, head: &str) -> Space {
    f.agents.push_script(done("ok"));
    f.git.push_checkpoint(Sha::new(head));
    core.space_send(SpaceSendRequest {
        space: s.id.clone(),
        text: text.into(),
        agent: None,
        model: None,
        mode: None,
        attachments: None,
    })
    .await
    .unwrap();
    idle(f, &s.id).await
}

#[tokio::test]
async fn mirror_on_remembers_the_folder_and_syncs_from_base() {
    let f = fixtures();
    let (core, project) = folder_project(&f, "/tmp/mirror-a").await;
    let s = one_turn(&f, &core, &project, "add a csv export").await;

    let mirrored = core.mirror_space(&s.id).await.unwrap();
    let m = mirrored.mirror.expect("mirror on");
    assert_eq!(m.status, MirrorStatus::On);
    assert_eq!(m.remembered_head, Sha::new("base000"));
    assert_eq!(m.remembered_branch, Some("main".into()));
    assert_eq!(m.mirrored_head, Some(Sha::new("turn111")));

    let calls = f.git.mirror_syncs();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1, s.id);
    assert_eq!(calls[0].2, Sha::new("turn111"));
    assert_eq!(
        calls[0].3,
        Sha::new("base000"),
        "first sync is from base, not None"
    );
}

#[tokio::test]
async fn mirror_on_refuses_a_dirty_folder() {
    let f = fixtures();
    let (core, project) = folder_project(&f, "/tmp/mirror-dirty").await;
    let s = one_turn(&f, &core, &project, "add a csv export").await;
    f.git.set_working_state(theodore_ports::WorkingState {
        current_branch: Some("main".into()),
        dirty_files: 2,
    });

    let err = core.mirror_space(&s.id).await.unwrap_err();
    assert!(
        matches!(&err, AppError::Invalid(m) if m.contains("2 uncommitted changes")),
        "{err}"
    );
    assert!(space(&f, &s.id).mirror.is_none());
}

#[tokio::test]
async fn a_turn_resyncs_automatically_and_the_touched_set_accumulates() {
    let f = fixtures();
    let (core, project) = folder_project(&f, "/tmp/mirror-resync").await;
    let s = one_turn(&f, &core, &project, "add a csv export").await;
    f.git
        .push_mirror_sync(Ok(vec!["a.txt".into(), "c.txt".into()]));
    core.mirror_space(&s.id).await.unwrap();

    f.git.push_mirror_sync(Ok(vec!["a.txt".into()]));
    let s2 = turn(&f, &core, &s, "edit a.txt more", "turn222").await;
    // The re-sync follows the turn's end (idle comes first).
    wait_until(|| {
        space(&f, &s2.id)
            .mirror
            .is_some_and(|m| m.mirrored_head == Some(Sha::new("turn222")))
    })
    .await;
    let s2 = space(&f, &s2.id);

    let m = s2.mirror.expect("still mirroring");
    assert_eq!(m.mirrored_head, Some(Sha::new("turn222")));
    let mut touched = m.touched;
    touched.sort();
    assert_eq!(
        touched,
        vec!["a.txt", "c.txt"],
        "union, not just the latest sync"
    );

    let calls = f.git.mirror_syncs();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[1].3,
        Sha::new("turn111"),
        "resync is from the last mirrored head"
    );
    assert_eq!(calls[1].2, Sha::new("turn222"));
}

#[tokio::test]
async fn sync_mirror_is_a_silent_no_op_when_not_mirroring() {
    let f = fixtures();
    let (core, project) = folder_project(&f, "/tmp/mirror-off").await;
    let s = one_turn(&f, &core, &project, "add a csv export").await;

    let result = core.sync_mirror(&s.id).await.unwrap();
    assert!(result.mirror.is_none());
    assert!(f.git.mirror_syncs().is_empty());
}

#[tokio::test]
async fn mirror_off_refuses_when_the_folder_was_edited_unless_discarded() {
    let f = fixtures();
    let (core, project) = folder_project(&f, "/tmp/mirror-edited").await;
    let s = one_turn(&f, &core, &project, "add a csv export").await;
    f.git.push_mirror_sync(Ok(vec!["c.txt".into()]));
    core.mirror_space(&s.id).await.unwrap();

    f.git.set_mirror_edited(Ok(vec!["c.txt".into()]));
    let err = core
        .unmirror_space(UnmirrorSpaceRequest {
            space: s.id.clone(),
            discard: false,
        })
        .await
        .unwrap_err();
    assert!(
        matches!(&err, AppError::Invalid(m) if m.contains("c.txt")),
        "{err}"
    );
    assert!(
        space(&f, &s.id).mirror.is_some(),
        "still on: nothing restored"
    );
    assert!(f.git.mirror_restores.lock().unwrap().is_empty());

    let restored = core
        .unmirror_space(UnmirrorSpaceRequest {
            space: s.id.clone(),
            discard: true,
        })
        .await
        .unwrap();
    assert!(restored.mirror.is_none());
    let restores = f.git.mirror_restores.lock().unwrap().clone();
    assert_eq!(restores.len(), 1);
    assert_eq!(restores[0].1, Sha::new("base000"));
}

#[tokio::test]
async fn mirror_off_restores_cleanly_when_nothing_was_edited() {
    let f = fixtures();
    let (core, project) = folder_project(&f, "/tmp/mirror-clean-off").await;
    let s = one_turn(&f, &core, &project, "add a csv export").await;
    f.git.push_mirror_sync(Ok(vec!["c.txt".into()]));
    core.mirror_space(&s.id).await.unwrap();

    let result = core
        .unmirror_space(UnmirrorSpaceRequest {
            space: s.id.clone(),
            discard: false,
        })
        .await
        .unwrap();
    assert!(result.mirror.is_none());
    let restores = f.git.mirror_restores.lock().unwrap().clone();
    assert_eq!(restores.len(), 1);
    assert_eq!(restores[0].2, vec!["c.txt".to_string()]);
}

#[tokio::test]
async fn switching_restores_the_first_space_then_mirrors_the_second() {
    let f = fixtures();
    let (core, project) = folder_project(&f, "/tmp/mirror-switch").await;
    let s1 = one_turn(&f, &core, &project, "first change").await;
    f.git.push_mirror_sync(Ok(vec!["a.txt".into()]));
    core.mirror_space(&s1.id).await.unwrap();

    f.agents.push_script(done("done"));
    f.git.push_checkpoint(Sha::new("turn999"));
    let s2 = core
        .create_space(CreateSpaceRequest {
            project: project.id.clone(),
            goal: "second change".into(),
            name: None,
            branch: None,
            base: None,
            agent: None,
            model: None,
            start_from: None,
        })
        .await
        .unwrap();
    let s2 = idle(&f, &s2.id).await;

    f.git.push_mirror_sync(Ok(vec!["d.txt".into()]));
    let mirrored2 = core.mirror_space(&s2.id).await.unwrap();

    assert!(space(&f, &s1.id).mirror.is_none(), "s1 was turned off");
    let m2 = mirrored2.mirror.expect("s2 now mirrors");
    assert_eq!(m2.status, MirrorStatus::On);
    let restores = f.git.mirror_restores.lock().unwrap().clone();
    assert_eq!(restores.len(), 1, "exactly s1's restore");
}

#[tokio::test]
async fn switching_is_refused_while_the_other_folder_was_edited() {
    let f = fixtures();
    let (core, project) = folder_project(&f, "/tmp/mirror-switch-refused").await;
    let s1 = one_turn(&f, &core, &project, "first change").await;
    f.git.push_mirror_sync(Ok(vec!["a.txt".into()]));
    core.mirror_space(&s1.id).await.unwrap();
    f.git.set_mirror_edited(Ok(vec!["a.txt".into()]));

    f.agents.push_script(done("done"));
    f.git.push_checkpoint(Sha::new("turn999"));
    let s2 = core
        .create_space(CreateSpaceRequest {
            project: project.id.clone(),
            goal: "second change".into(),
            name: None,
            branch: None,
            base: None,
            agent: None,
            model: None,
            start_from: None,
        })
        .await
        .unwrap();
    let s2 = idle(&f, &s2.id).await;

    let err = core.mirror_space(&s2.id).await.unwrap_err();
    assert!(
        matches!(&err, AppError::Invalid(m) if m.contains("a.txt")),
        "{err}"
    );
    assert!(space(&f, &s1.id).mirror.is_some(), "s1 stays on");
    assert!(space(&f, &s2.id).mirror.is_none(), "s2 never turned on");
}

#[tokio::test]
async fn a_mirror_left_on_comes_back_paused_and_resuming_keeps_its_remembered_state() {
    let f = fixtures();
    let (core, project) = folder_project(&f, "/tmp/mirror-pause").await;
    let s = one_turn(&f, &core, &project, "add a csv export").await;
    f.git.push_mirror_sync(Ok(vec!["a.txt".into()]));
    core.mirror_space(&s.id).await.unwrap();

    core.pause_resumed_mirrors().await;
    let paused = space(&f, &s.id);
    let m = paused.mirror.clone().expect("still remembered");
    assert_eq!(m.status, MirrorStatus::Paused);
    assert_eq!(m.remembered_head, Sha::new("base000"));
    assert_eq!(m.touched, vec!["a.txt".to_string()]);

    // Resuming while nothing changed in the folder in the meantime.
    f.git.push_mirror_sync(Ok(Vec::new()));
    let resumed = core.mirror_space(&s.id).await.unwrap();
    let m2 = resumed.mirror.expect("resumed");
    assert_eq!(m2.status, MirrorStatus::On);
    assert_eq!(
        m2.remembered_head,
        Sha::new("base000"),
        "the restore target never moved just because it paused"
    );
}

#[tokio::test]
async fn resuming_refuses_if_the_folder_is_dirty_now() {
    let f = fixtures();
    let (core, project) = folder_project(&f, "/tmp/mirror-pause-dirty").await;
    let s = one_turn(&f, &core, &project, "add a csv export").await;
    f.git.push_mirror_sync(Ok(vec!["a.txt".into()]));
    core.mirror_space(&s.id).await.unwrap();
    core.pause_resumed_mirrors().await;

    f.git.set_working_state(theodore_ports::WorkingState {
        current_branch: Some("main".into()),
        dirty_files: 1,
    });
    let err = core.mirror_space(&s.id).await.unwrap_err();
    assert!(matches!(err, AppError::Invalid(_)), "{err}");
    assert_eq!(
        space(&f, &s.id).mirror.unwrap().status,
        MirrorStatus::Paused,
        "stays paused, not silently resumed"
    );
}
