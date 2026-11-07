//! The sandbox host on the default scenario, through the `HostAdapter`
//! port only: inbox reasons, ETags, review rules, merges that move git,
//! pushes, faults and the control spool.

use std::path::Path;
use std::time::Duration;

use theodore_domain::{
    Anchor, CheckRollup, HostKind, InboxReason, PrRef, PrState, RepoRef, ReviewDecision,
    ReviewEvent, Side,
};
use theodore_ports::{
    Fetched, HostAdapter, HostError, InboxSection, MergeMethod, MergeState, NewPr, ReviewComment,
    ReviewSubmission,
};
use theodore_sandbox::{Action, PrSel, Sandbox, control, default_scenario};

fn open() -> (Sandbox, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let scenario = tmp.path().join("scenario");
    default_scenario::write(&scenario).unwrap();
    let sandbox = Sandbox::open(&scenario, &tmp.path().join("data")).unwrap();
    (sandbox, tmp)
}

fn pr(name: &str, number: u64) -> PrRef {
    PrRef {
        repo: RepoRef {
            host: HostKind::Github,
            owner: "acme".into(),
            name: name.into(),
        },
        number,
        host_id: None,
    }
}

async fn detail(host: &dyn HostAdapter, p: &PrRef) -> theodore_domain::PrDetail {
    match host.get_pr(p, None).await.unwrap() {
        Fetched::Modified { value, .. } => value,
        Fetched::NotModified => panic!("not modified without an etag"),
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[tokio::test]
async fn inbox_sections_cover_every_reason() {
    let (sb, _tmp) = open();
    let host = sb.host();
    let mut reasons = std::collections::BTreeSet::new();
    for section in [
        InboxSection::ReviewRequested,
        InboxSection::Assigned,
        InboxSection::Authored,
        InboxSection::Mentioned,
    ] {
        for s in host.search(&section).await.unwrap() {
            assert!(matches!(s.state, PrState::Open | PrState::Draft));
            reasons.extend(s.reasons.iter().map(|r| format!("{r:?}")));
        }
    }
    for r in [
        InboxReason::ReviewRequested,
        InboxReason::TeamReviewRequested,
        InboxReason::Assigned,
        InboxReason::Authored,
        InboxReason::Mentioned,
        InboxReason::UpdatedAfterReview,
    ] {
        assert!(
            reasons.contains(&format!("{r:?}")),
            "missing {r:?} in {reasons:?}"
        );
    }
    let merged = host
        .search(&InboxSection::Query(
            "is:pr repo:acme/storefront is:merged".into(),
        ))
        .await
        .unwrap();
    assert_eq!(
        merged.iter().map(|s| s.pr.number).collect::<Vec<_>>(),
        vec![115]
    );
}

#[tokio::test]
async fn pr_shapes_match_the_scenario() {
    let (sb, _tmp) = open();
    let host = sb.host();
    let conflict = detail(&*host, &pr("storefront", 107)).await;
    assert!(conflict.summary.conflicts);
    let big = detail(&*host, &pr("storefront", 102)).await;
    assert!(
        big.summary.changed_files >= 40,
        "{}",
        big.summary.changed_files
    );
    let failing = detail(&*host, &pr("storefront", 109)).await;
    assert_eq!(failing.summary.checks, CheckRollup::Fail);
    let approved = detail(&*host, &pr("storefront", 110)).await;
    assert_eq!(
        approved.summary.review_decision,
        Some(ReviewDecision::Approved)
    );
    let changes = detail(&*host, &pr("storefront", 111)).await;
    assert_eq!(
        changes.summary.review_decision,
        Some(ReviewDecision::ChangesRequested)
    );
    assert!(changes.threads.iter().any(|t| t.resolved));
    assert!(
        changes
            .threads
            .iter()
            .any(|t| t.outdated && t.anchor.is_none())
    );
    assert!(
        changes
            .threads
            .iter()
            .any(|t| t.comments[0].body.contains("```suggestion")
                && t.comments[0].reactions.iter().any(|r| r.viewer_reacted))
    );
    let stacked = detail(&*host, &pr("storefront", 113)).await;
    assert_eq!(stacked.summary.base_ref, "priya/ABC-190-1-payment-client");
    assert!(stacked.commits.iter().any(|c| c.after_my_review));
    let draft = detail(&*host, &pr("storefront", 108)).await;
    assert_eq!(draft.summary.state, PrState::Draft);
}

#[tokio::test]
async fn etags_change_only_when_the_pr_does() {
    let (sb, _tmp) = open();
    let host = sb.host();
    let p = pr("storefront", 101);
    let Fetched::Modified { etag, .. } = host.get_pr(&p, None).await.unwrap() else {
        panic!()
    };
    let etag = etag.unwrap();
    assert!(matches!(
        host.get_pr(&p, Some(&etag)).await.unwrap(),
        Fetched::NotModified
    ));
    let d = detail(&*host, &p).await;
    host.reply(&p, &d.threads[0].id, "on it").await.unwrap();
    assert!(matches!(
        host.get_pr(&p, Some(&etag)).await.unwrap(),
        Fetched::Modified { .. }
    ));
}

#[tokio::test]
async fn reviews_follow_github_rules_and_are_idempotent() {
    let (sb, _tmp) = open();
    let host = sb.host();
    // Own PR: no approving.
    let own = pr("storefront", 110);
    let d = detail(&*host, &own).await;
    let err = host
        .submit_review(
            &own,
            &ReviewSubmission {
                event: ReviewEvent::Approve,
                body: String::new(),
                comments: vec![],
                commit: d.summary.head_sha.clone(),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, HostError::Validation(_)), "{err}");

    let p = pr("storefront", 101);
    let d = detail(&*host, &p).await;
    let review = ReviewSubmission {
        event: ReviewEvent::Approve,
        body: "nice".into(),
        comments: vec![ReviewComment {
            anchor: Anchor {
                rev: d.summary.head_sha.clone(),
                path: "src/lib/cart.ts".into(),
                side: Side::New,
                line: 13,
                start_line: None,
                start_side: None,
            },
            body: "consider a helper".into(),
            marker: "m-1".into(),
        }],
        commit: d.summary.head_sha.clone(),
    };
    let first = host.submit_review(&p, &review).await.unwrap();
    let again = host.submit_review(&p, &review).await.unwrap();
    assert_eq!(first.threads, again.threads, "same marker → same thread");
    assert!(host.find_marker(&p, "m-1").await.unwrap().is_some());
    let d = detail(&*host, &p).await;
    assert_eq!(d.summary.approvals, 1);
    assert!(!d.summary.reasons.contains(&InboxReason::ReviewRequested));
}

#[tokio::test]
async fn merging_moves_the_base_branch_in_git() {
    let (sb, _tmp) = open();
    let host = sb.host();
    let p = pr("storefront", 110);
    let status = host.merge_status(&p).await.unwrap();
    assert_eq!(status.state, MergeState::Clean);
    let head = detail(&*host, &p).await.summary.head_sha;
    let sha = host
        .merge(&p, MergeMethod::Squash, None, None)
        .await
        .unwrap();
    let bare = sb.world().bare("acme", "storefront");
    assert_eq!(bare.branch_tip("main").unwrap(), sha.0);
    assert_eq!(
        bare.read_file(&sha.0, "src/lib/orders.ts"),
        bare.read_file(&head.0, "src/lib/orders.ts")
    );
    assert_eq!(detail(&*host, &p).await.summary.state, PrState::Merged);
    // A conflicting PR can't merge.
    let err = host
        .merge(&pr("storefront", 107), MergeMethod::Merge, None, None)
        .await
        .unwrap_err();
    assert!(matches!(err, HostError::Validation(_)), "{err}");
}

#[tokio::test]
async fn a_push_to_the_head_branch_is_seen_on_the_next_get_pr() {
    let (sb, tmp) = open();
    let host = sb.host();
    let p = pr("storefront", 101);
    let before = detail(&*host, &p).await;
    // Push straight into the bare repo, like `commit_and_push` does.
    let bare = sb.world().bare("acme", "storefront");
    let work = tmp.path().join("work");
    git(
        tmp.path(),
        &["clone", "-q", &bare.url(), work.to_str().unwrap()],
    );
    git(&work, &["checkout", "-q", "mira/ABC-101-cart-rounding"]);
    std::fs::write(work.join("NOTES.md"), "pushed\n").unwrap();
    git(&work, &["add", "-A"]);
    git(
        &work,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "more",
        ],
    );
    git(&work, &["push", "-q", "origin", "HEAD"]);
    let after = detail(&*host, &p).await;
    assert_ne!(after.summary.head_sha, before.summary.head_sha);
    assert_eq!(after.commits.len(), before.commits.len() + 1);
    assert_eq!(
        after.summary.checks,
        CheckRollup::Pending,
        "CI restarts on push"
    );
    assert_eq!(
        bare.rev_parse("refs/pull/101/head").unwrap(),
        after.summary.head_sha.0
    );
}

#[tokio::test]
async fn faults_map_to_host_errors() {
    let (sb, _tmp) = open();
    let host = sb.host();
    let p = pr("storefront", 101);
    sb.set_fault("auth", true, None).unwrap();
    assert!(matches!(host.viewer().await, Err(HostError::Auth)));
    sb.set_fault("auth", false, None).unwrap();
    sb.set_fault("rate_limit", true, Some(60.0)).unwrap();
    match host.get_pr(&p, None).await {
        Err(HostError::RateLimited { reset_at }) => assert!(reset_at > 0),
        other => panic!("expected a rate limit, got {other:?}"),
    }
    sb.set_fault("rate_limit", false, None).unwrap();
    sb.set_fault("offline", true, None).unwrap();
    assert!(matches!(
        host.get_pr(&p, None).await,
        Err(HostError::Network(_))
    ));
    let url = (sb.remote_urls())(&p.repo).unwrap();
    assert!(url.contains("/offline/"));
    sb.set_fault("offline", false, None).unwrap();
    sb.set_fault("not_found", true, Some(101.0)).unwrap();
    assert!(matches!(
        host.get_pr(&p, None).await,
        Err(HostError::NotFound(_))
    ));
    sb.set_fault("not_found", false, None).unwrap();
    sb.set_fault("server_errors", true, Some(1.0)).unwrap();
    assert!(matches!(
        host.get_pr(&p, None).await,
        Err(HostError::Server(_))
    ));
    sb.set_fault("server_errors", false, None).unwrap();
    assert!(sb.set_fault("gremlins", true, None).is_err());
    assert!(host.get_pr(&p, None).await.is_ok());
}

#[tokio::test]
async fn timeline_actions_and_ci_transitions_apply() {
    let (sb, _tmp) = open();
    let host = sb.host();
    let msg = sb.fire("push-101").unwrap();
    assert!(msg.starts_with("pushed"), "{msg}");
    let d = detail(&*host, &pr("storefront", 101)).await;
    // The thread's line was edited by the push → outdated.
    assert!(d.threads[0].outdated);
    sb.apply(&Action::RequestReview {
        pr: PrSel::Number(117),
        user: None,
        team: None,
    })
    .unwrap();
    let requested = host.search(&InboxSection::ReviewRequested).await.unwrap();
    assert!(requested.iter().any(|s| s.pr.number == 117));
    sb.apply(&Action::Merge {
        pr: PrSel::Ref("acme/storefront#106".into()),
        author: Some("lee".into()),
    })
    .unwrap();
    assert_eq!(
        detail(&*host, &pr("storefront", 106)).await.summary.state,
        PrState::Merged
    );
    // Re-run → pending → pass on the next tick after the delay.
    let p = pr("storefront", 109);
    let check = detail(&*host, &p).await.checks[1].clone();
    host.rerun_check(&p, &check).await.unwrap();
    assert_eq!(
        detail(&*host, &p).await.checks[1].status,
        theodore_domain::CheckStatus::Pending
    );
}

#[test]
fn due_timeline_events_fire_once_on_tick() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("scenario");
    default_scenario::write(&dir).unwrap();
    let mut s = theodore_sandbox::Scenario::load(&dir).unwrap();
    s.timeline[0].at = Some("0s".into());
    s.save(&dir).unwrap();
    let sb = Sandbox::open(&dir, &tmp.path().join("data")).unwrap();
    let bare = sb.world().bare("acme", "storefront");
    let before = bare.branch_tip("mira/ABC-101-cart-rounding").unwrap();
    sb.tick();
    let after = bare.branch_tip("mira/ABC-101-cart-rounding").unwrap();
    assert_ne!(before, after, "push-101 fired");
    sb.tick();
    assert_eq!(
        bare.branch_tip("mira/ABC-101-cart-rounding").unwrap(),
        after,
        "and only once"
    );
    let status = sb.status();
    assert!(status.events.iter().any(|e| e.id == "push-101" && e.fired));
}

#[test]
fn control_spool_round_trips_through_a_ticking_sandbox() {
    let (sb, _tmp) = open();
    let root = sb.root().to_path_buf();
    assert!(control::send(&root, Action::Status, Duration::from_secs(1)).is_err());
    let ticker = {
        let sb = sb.clone();
        std::thread::spawn(move || {
            for _ in 0..40 {
                sb.tick();
                std::thread::sleep(Duration::from_millis(50));
            }
        })
    };
    for _ in 0..100 {
        if control::alive(&root).is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let r = control::send(
        &root,
        Action::Check {
            pr: PrSel::Number(101),
            name: "ci/test".into(),
            status: theodore_domain::CheckStatus::Fail,
        },
        Duration::from_secs(5),
    )
    .unwrap();
    assert!(r.ok, "{}", r.message);
    assert_eq!(r.status.unwrap().info.scenario, "default");
    ticker.join().unwrap();
}

/// Clones `acme/storefront`, branches `branch` off `main` (with one new
/// commit when `commit`) and pushes it, like the app does before opening a
/// PR from a space.
fn push_branch(sb: &Sandbox, tmp: &Path, branch: &str, commit: bool) {
    let bare = sb.world().bare("acme", "storefront");
    let work = tmp.join(format!("work-{}", branch.replace('/', "-")));
    git(tmp, &["clone", "-q", &bare.url(), work.to_str().unwrap()]);
    git(&work, &["checkout", "-q", "-b", branch, "origin/main"]);
    if commit {
        std::fs::write(work.join("SPACE.md"), "from a space\n").unwrap();
        git(&work, &["add", "-A"]);
        git(
            &work,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "space work",
            ],
        );
    }
    git(&work, &["push", "-q", "origin", branch]);
}

fn storefront() -> RepoRef {
    RepoRef {
        host: HostKind::Github,
        owner: "acme".into(),
        name: "storefront".into(),
    }
}

#[tokio::test]
async fn create_pr_opens_a_pr_from_a_pushed_branch() {
    let (sb, tmp) = open();
    let host = sb.host();
    let repo = storefront();
    push_branch(&sb, tmp.path(), "space/checkout-copy", true);

    assert_eq!(
        host.find_open_pr(&repo, "space/checkout-copy")
            .await
            .unwrap(),
        None
    );
    let highest = sb
        .world()
        .snapshot()
        .prs
        .iter()
        .filter(|p| p.repo == "storefront")
        .map(|p| p.number)
        .max()
        .unwrap();
    let cursor = host.changed_since(None).await.unwrap().cursor;

    let new_pr = NewPr {
        head: "space/checkout-copy".into(),
        base: "main".into(),
        title: "Tighten checkout copy".into(),
        body: "Opened from a space.".into(),
        draft: true,
    };
    let created = host.create_pr(&repo, &new_pr).await.unwrap();
    assert_eq!(created.repo, repo);
    assert_eq!(created.number, highest + 1);
    assert_eq!(
        host.find_open_pr(&repo, "space/checkout-copy")
            .await
            .unwrap(),
        Some(created.clone())
    );

    let changed = host.changed_since(Some(&cursor)).await.unwrap();
    assert!(changed.prs.contains(&created));

    let authored = host.search(&InboxSection::Authored).await.unwrap();
    let row = authored
        .iter()
        .find(|s| s.pr == created)
        .expect("created PR shows under Authored");
    assert_eq!(row.state, PrState::Draft);
    assert_eq!(row.head_ref, "space/checkout-copy");
    assert_eq!(row.base_ref, "main");
    assert_eq!(row.title, "Tighten checkout copy");
    assert_eq!(row.author.login, sb.status().viewer);
    assert!(row.reasons.contains(&InboxReason::Authored));

    // By number alone (no host_id), the way the app polls it.
    let d = detail(&*host, &pr("storefront", created.number)).await;
    assert_eq!(d.summary.pr, created);
    assert_eq!(d.body, "Opened from a space.");
    assert_eq!(d.commits.len(), 1);
    assert_eq!(d.summary.changed_files, 1);
    let bare = sb.world().bare("acme", "storefront");
    assert_eq!(
        bare.rev_parse(&format!("refs/pull/{}/head", created.number))
            .unwrap(),
        d.summary.head_sha.0
    );

    let err = host.create_pr(&repo, &new_pr).await.unwrap_err();
    assert!(matches!(err, HostError::Conflict(_)), "{err}");
}

#[tokio::test]
async fn create_pr_refuses_a_branch_that_is_missing_or_has_no_commits() {
    let (sb, tmp) = open();
    let host = sb.host();
    let repo = storefront();
    let mut new_pr = NewPr {
        head: "space/nothing-yet".into(),
        base: "main".into(),
        title: "Nothing".into(),
        body: String::new(),
        draft: false,
    };
    let err = host.create_pr(&repo, &new_pr).await.unwrap_err();
    assert!(matches!(err, HostError::Validation(_)), "{err}");

    push_branch(&sb, tmp.path(), "space/nothing-yet", false);
    let err = host.create_pr(&repo, &new_pr).await.unwrap_err();
    assert!(matches!(err, HostError::Validation(_)), "{err}");

    new_pr.base = "no-such-base".into();
    let err = host.create_pr(&repo, &new_pr).await.unwrap_err();
    assert!(matches!(err, HostError::Validation(_)), "{err}");
}
