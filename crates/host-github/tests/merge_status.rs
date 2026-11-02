mod support;

use theodore_domain::{HostKind, PrRef, RepoRef};
use theodore_ports::{HostAdapter, MergeMethod, MergeState};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, ResponseTemplate};

fn pr() -> PrRef {
    PrRef {
        repo: RepoRef {
            host: HostKind::Github,
            owner: "acme".into(),
            name: "widgets".into(),
        },
        number: 42,
        host_id: None,
    }
}

#[tokio::test]
async fn merge_status_maps_a_clean_mergeable_pr() {
    let s = support::servers().await;

    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains("mergeStateStatus"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": {
                "repository": {
                    "mergeCommitAllowed": true,
                    "squashMergeAllowed": true,
                    "rebaseMergeAllowed": false,
                    "autoMergeAllowed": true,
                    "viewerDefaultMergeMethod": "SQUASH",
                    "pullRequest": {
                        "mergeable": "MERGEABLE",
                        "mergeStateStatus": "CLEAN",
                        "isInMergeQueue": false,
                        "isMergeQueueEnabled": false,
                        "autoMergeRequest": null
                    }
                }
            }
        })))
        .mount(&s.graphql)
        .await;

    let status = s.adapter.merge_status(&pr()).await.unwrap();
    assert_eq!(status.mergeable, Some(true));
    assert_eq!(status.state, MergeState::Clean);
    assert_eq!(
        status.allowed_methods,
        vec![MergeMethod::Merge, MergeMethod::Squash]
    );
    assert!(!status.auto_merge_enabled);
    assert!(!status.in_queue);
    assert!(status.auto_merge_allowed);
    assert!(!status.merge_queue);
    assert_eq!(status.default_method, Some(MergeMethod::Squash));
}

#[tokio::test]
async fn merge_status_reports_conflicts_and_auto_merge() {
    let s = support::servers().await;

    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains("mergeStateStatus"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": {
                "repository": {
                    "mergeCommitAllowed": true,
                    "squashMergeAllowed": true,
                    "rebaseMergeAllowed": true,
                    "pullRequest": {
                        "mergeable": "CONFLICTING",
                        "mergeStateStatus": "DIRTY",
                        "isInMergeQueue": true,
                        "autoMergeRequest": { "enabledAt": "2024-01-01T00:00:00Z" }
                    }
                }
            }
        })))
        .mount(&s.graphql)
        .await;

    let status = s.adapter.merge_status(&pr()).await.unwrap();
    assert_eq!(status.mergeable, Some(false));
    assert_eq!(status.state, MergeState::Dirty);
    assert!(status.auto_merge_enabled);
    assert!(status.in_queue);
}

#[tokio::test]
async fn assignable_users_collects_every_page() {
    let s = support::servers().await;

    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains("assignableUsers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": {
                "repository": {
                    "assignableUsers": {
                        "pageInfo": { "hasNextPage": false, "endCursor": null },
                        "nodes": [
                            { "login": "alice", "name": "Alice", "avatarUrl": null },
                            { "login": "bob", "name": null, "avatarUrl": null }
                        ]
                    }
                }
            }
        })))
        .mount(&s.graphql)
        .await;

    let people = s.adapter.assignable_users(&pr().repo).await.unwrap();
    assert_eq!(people.len(), 2);
    assert_eq!(people[0].login, "alice");
    assert_eq!(people[1].login, "bob");
}
