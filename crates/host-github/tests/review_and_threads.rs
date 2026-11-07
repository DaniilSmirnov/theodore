mod support;

use theodore_domain::{Anchor, HostKind, PrRef, RepoRef, ReviewEvent, Sha, Side, ThreadId};
use theodore_ports::{HostAdapter, ReviewComment, ReviewSubmission};
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

fn comment(marker: &str, line: u32) -> ReviewComment {
    ReviewComment {
        anchor: Anchor {
            rev: Sha::new("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            path: "src/lib.rs".into(),
            side: Side::New,
            line,
            start_line: None,
            start_side: None,
        },
        body: "Please fix this.".into(),
        marker: marker.into(),
    }
}

#[tokio::test]
async fn submit_review_sends_one_mutation_with_marked_threads_then_resolves_thread_ids() {
    let s = support::servers().await;

    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains("addPullRequestReview"))
        .and(body_string_contains("<!-- theodore:m1 -->"))
        .and(body_string_contains("REQUEST_CHANGES"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": { "addPullRequestReview": { "pullRequestReview": { "id": "review_1" } } }
        })))
        .mount(&s.graphql)
        .await;

    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains("reviewThreads"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(support::fixture("review_threads_with_marker.json")),
        )
        .mount(&s.graphql)
        .await;

    let submission = ReviewSubmission {
        event: ReviewEvent::RequestChanges,
        body: "Looks mostly good.".into(),
        comments: vec![comment("m1", 10)],
        commit: Sha::new("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
    };

    let submitted = s.adapter.submit_review(&pr(), &submission).await.unwrap();
    assert_eq!(submitted.review_id, "review_1");
    assert_eq!(
        submitted.threads,
        vec![("m1".to_string(), ThreadId::new("thread_marked"))]
    );
}

#[tokio::test]
async fn find_marker_scans_threads_for_the_tagged_comment() {
    let s = support::servers().await;

    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains("reviewThreads"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(support::fixture("review_threads_with_marker.json")),
        )
        .mount(&s.graphql)
        .await;

    let found = s.adapter.find_marker(&pr(), "m1").await.unwrap();
    assert_eq!(found, Some(theodore_domain::CommentId::new("5001")));

    let missing = s
        .adapter
        .find_marker(&pr(), "no-such-marker")
        .await
        .unwrap();
    assert_eq!(missing, None);
}

#[tokio::test]
async fn find_markers_reads_the_threads_once_for_every_marker() {
    let s = support::servers().await;

    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains("reviewThreads"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(support::fixture("review_threads_with_marker.json")),
        )
        .expect(1)
        .mount(&s.graphql)
        .await;

    let found = s
        .adapter
        .find_markers(&pr(), &["m1".to_string(), "no-such-marker".to_string()])
        .await
        .unwrap();
    assert_eq!(found, std::collections::HashSet::from(["m1".to_string()]));
}

#[tokio::test]
async fn reply_posts_a_thread_reply_and_returns_its_comment_id() {
    let s = support::servers().await;

    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains("addPullRequestReviewThreadReply"))
        .and(body_string_contains("thread_1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": { "addPullRequestReviewThreadReply": { "comment": { "id": "node_x", "fullDatabaseId": "6001" } } }
        })))
        .mount(&s.graphql)
        .await;

    let id = s
        .adapter
        .reply(&pr(), &ThreadId::new("thread_1"), "Sounds good.")
        .await
        .unwrap();
    assert_eq!(id, theodore_domain::CommentId::new("6001"));
}

#[tokio::test]
async fn resolve_and_unresolve_hit_the_right_mutation() {
    let s = support::servers().await;

    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains("  resolveReviewThread(input"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": { "resolveReviewThread": { "thread": { "id": "thread_1" } } }
        })))
        .mount(&s.graphql)
        .await;
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains("unresolveReviewThread"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": { "unresolveReviewThread": { "thread": { "id": "thread_1" } } }
        })))
        .mount(&s.graphql)
        .await;

    s.adapter
        .resolve(&pr(), &ThreadId::new("thread_1"), true)
        .await
        .unwrap();
    s.adapter
        .resolve(&pr(), &ThreadId::new("thread_1"), false)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_refused_review_fails_with_githubs_message_not_a_null_payload() {
    let s = support::servers().await;

    // GitHub's answer to approving one's own PR: a null payload *and* an
    // error, in one 200.
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains("addPullRequestReview"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": { "addPullRequestReview": null },
            "errors": [{
                "type": "UNPROCESSABLE",
                "path": ["addPullRequestReview"],
                "locations": [{ "line": 3, "column": 3 }],
                "message": "Can not approve your own pull request"
            }]
        })))
        .mount(&s.graphql)
        .await;

    let submission = ReviewSubmission {
        event: ReviewEvent::Approve,
        body: String::new(),
        comments: vec![],
        commit: Sha::new("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
    };
    let err = s
        .adapter
        .submit_review(&pr(), &submission)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, theodore_ports::HostError::Validation(m) if m == "Can not approve your own pull request"),
        "got {err:?}"
    );
}

#[tokio::test]
async fn a_forbidden_mutation_is_forbidden_not_a_rejected_token() {
    let s = support::servers().await;

    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains("addPullRequestReviewThreadReply"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": { "addPullRequestReviewThreadReply": null },
            "errors": [{ "type": "FORBIDDEN", "message": "Resource not accessible by integration" }]
        })))
        .mount(&s.graphql)
        .await;

    let err = s
        .adapter
        .reply(&pr(), &ThreadId::new("thread_1"), "hi")
        .await
        .unwrap_err();
    assert!(
        matches!(&err, theodore_ports::HostError::Forbidden(m) if m == "Resource not accessible by integration"),
        "got {err:?}"
    );
}
