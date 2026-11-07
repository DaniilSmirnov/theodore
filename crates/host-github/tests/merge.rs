mod support;

use theodore_domain::{HostKind, PrRef, RepoRef};
use theodore_ports::{HostAdapter, HostError, MergeMethod};
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
        host_id: Some("PR_node_42".into()),
    }
}

#[tokio::test]
async fn merge_sends_method_title_and_body_and_returns_the_merge_sha() {
    let s = support::servers().await;

    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains("mergePullRequest"))
        .and(body_string_contains("SQUASH"))
        .and(body_string_contains("final title"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": {
                "mergePullRequest": {
                    "pullRequest": { "mergeCommit": { "oid": "deadbeef" } }
                }
            }
        })))
        .mount(&s.graphql)
        .await;

    let sha = s
        .adapter
        .merge(
            &pr(),
            MergeMethod::Squash,
            Some("final title".into()),
            Some("final body".into()),
        )
        .await
        .unwrap();
    assert_eq!(sha.as_str(), "deadbeef");
}

#[tokio::test]
async fn merge_maps_a_graphql_validation_error() {
    let s = support::servers().await;

    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains("mergePullRequest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "errors": [{ "message": "Pull Request is not mergeable", "type": "UNPROCESSABLE" }]
        })))
        .mount(&s.graphql)
        .await;

    let err = s
        .adapter
        .merge(&pr(), MergeMethod::Merge, None, None)
        .await
        .unwrap_err();
    assert!(matches!(err, HostError::Validation(_)), "got {err:?}");
}

#[tokio::test]
async fn enable_auto_merge_sends_the_method() {
    let s = support::servers().await;

    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains("enablePullRequestAutoMerge"))
        .and(body_string_contains("REBASE"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": { "enablePullRequestAutoMerge": { "pullRequest": { "id": "PR_node_42" } } }
        })))
        .mount(&s.graphql)
        .await;

    s.adapter
        .enable_auto_merge(&pr(), MergeMethod::Rebase)
        .await
        .unwrap();
}

#[tokio::test]
async fn disable_auto_merge_calls_the_mutation() {
    let s = support::servers().await;

    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains("disablePullRequestAutoMerge"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": { "disablePullRequestAutoMerge": { "pullRequest": { "id": "PR_node_42" } } }
        })))
        .mount(&s.graphql)
        .await;

    s.adapter.disable_auto_merge(&pr()).await.unwrap();
}

#[tokio::test]
async fn enqueue_calls_the_mutation() {
    let s = support::servers().await;

    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains("enqueuePullRequest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": { "enqueuePullRequest": { "mergeQueueEntry": { "id": "mqe_1" } } }
        })))
        .mount(&s.graphql)
        .await;

    s.adapter.enqueue(&pr()).await.unwrap();
}

#[tokio::test]
async fn update_branch_succeeds_when_the_mutation_returns_the_pull_request() {
    let s = support::servers().await;

    // GitHub's `UpdatePullRequestBranchPayload` has only `clientMutationId`
    // and `pullRequest`: a query selecting `success` is rejected outright.
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains("updatePullRequestBranch"))
        .and(body_string_contains("success"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "errors": [{
                "message": "Field 'success' doesn't exist on type 'UpdatePullRequestBranchPayload'",
                "extensions": { "code": "undefinedField" }
            }]
        })))
        .with_priority(1)
        .mount(&s.graphql)
        .await;
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains("updatePullRequestBranch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": { "updatePullRequestBranch": { "pullRequest": { "id": "PR_node_42" } } }
        })))
        .mount(&s.graphql)
        .await;

    s.adapter.update_branch(&pr()).await.unwrap();
}

#[tokio::test]
async fn update_branch_is_a_conflict_when_the_mutation_returns_no_pull_request() {
    let s = support::servers().await;

    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains("updatePullRequestBranch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": { "updatePullRequestBranch": { "pullRequest": null } }
        })))
        .mount(&s.graphql)
        .await;

    let err = s.adapter.update_branch(&pr()).await.unwrap_err();
    assert!(matches!(err, HostError::Conflict(_)), "got {err:?}");
}

#[tokio::test]
async fn mark_ready_and_convert_to_draft_call_their_mutations() {
    let s = support::servers().await;

    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains("markPullRequestReadyForReview"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": { "markPullRequestReadyForReview": { "pullRequest": { "id": "PR_node_42" } } }
        })))
        .mount(&s.graphql)
        .await;
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains("convertPullRequestToDraft"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": { "convertPullRequestToDraft": { "pullRequest": { "id": "PR_node_42" } } }
        })))
        .mount(&s.graphql)
        .await;

    s.adapter.mark_ready(&pr()).await.unwrap();
    s.adapter.convert_to_draft(&pr()).await.unwrap();
}
