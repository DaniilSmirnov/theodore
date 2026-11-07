mod support;

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use theodore_host_http::{HttpClient, StaticToken};
use theodore_ports::{Clock, Fetched, HostError};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use support::FakeClock;

#[derive(Debug, Deserialize, Serialize, PartialEq)]
struct Pr {
    number: u64,
    title: String,
}

fn client(server: &MockServer, token: Option<&str>, clock: Arc<FakeClock>) -> HttpClient {
    let base = url::Url::parse(&format!("{}/", server.uri())).unwrap();
    let token: Arc<dyn theodore_host_http::TokenSource> =
        Arc::new(StaticToken(token.map(str::to_string)));
    HttpClient::new(base, token, clock)
}

#[tokio::test]
async fn no_etag_is_a_fresh_get_even_after_one_was_seen() {
    let server = MockServer::start().await;
    let clock = FakeClock::new(0);

    // Any request carrying the ETag gets a 304 — mounted first so it would
    // win if the client sent it.
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls/1"))
        .and(header("if-none-match", "\"abc\""))
        .respond_with(ResponseTemplate::new(304))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls/1"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("etag", "\"abc\"")
                .set_body_json(Pr {
                    number: 1,
                    title: "first".into(),
                }),
        )
        .mount(&server)
        .await;

    let http = client(&server, Some("secret-token"), clock);

    for _ in 0..2 {
        let fetched = http
            .get_conditional::<Pr>("repos/o/r/pulls/1", None)
            .await
            .unwrap();
        match fetched {
            Fetched::Modified { etag, value } => {
                assert_eq!(etag, Some("\"abc\"".to_string()));
                assert_eq!(value.value.title, "first");
            }
            Fetched::NotModified => panic!("a fetch with no etag must never be not-modified"),
        }
    }
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .all(|r| !r.headers.contains_key("if-none-match")),
        "no If-None-Match may be sent when the caller passes none"
    );

    // The caller's own ETag still makes it conditional.
    let third = http
        .get_conditional::<Pr>("repos/o/r/pulls/1", Some("\"abc\""))
        .await
        .unwrap();
    assert!(matches!(third, Fetched::NotModified));
}

#[tokio::test]
async fn explicit_etag_makes_the_get_conditional() {
    let server = MockServer::start().await;
    let clock = FakeClock::new(0);

    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls/1"))
        .and(header("if-none-match", "\"caller\""))
        .respond_with(ResponseTemplate::new(304))
        .mount(&server)
        .await;

    let http = client(&server, None, clock);
    let res = http
        .get_conditional::<Pr>("repos/o/r/pulls/1", Some("\"caller\""))
        .await
        .unwrap();
    assert!(matches!(res, Fetched::NotModified));
}

#[tokio::test]
async fn rate_limited_429_maps_to_host_error_with_reset_at() {
    let server = MockServer::start().await;
    let clock = FakeClock::new(1_000);

    Mock::given(method("GET"))
        .and(path("/notifications"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "30"))
        .mount(&server)
        .await;

    let http = client(&server, None, clock);
    let err = http
        .get::<serde_json::Value>("notifications")
        .await
        .unwrap_err();
    match err {
        HostError::RateLimited { reset_at } => assert_eq!(reset_at, 1_000 + 30_000),
        other => panic!("expected RateLimited, got {other:?}"),
    }
}

#[tokio::test]
async fn preemptive_rate_limit_skips_the_network_call() {
    let server = MockServer::start().await;
    let clock = FakeClock::new(1_000);

    Mock::given(method("GET"))
        .and(path("/x"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "30"))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let http = client(&server, None, clock);
    let _ = http.get::<serde_json::Value>("x").await.unwrap_err();

    // Second call must fail fast (no second request hits the mock, which
    // would panic since it's limited to one match).
    let err = http.get::<serde_json::Value>("x").await.unwrap_err();
    assert!(matches!(err, HostError::RateLimited { .. }));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn preemptive_block_clears_once_the_clock_passes_reset_at() {
    let server = MockServer::start().await;
    let clock = FakeClock::new(1_000);

    Mock::given(method("GET"))
        .and(path("/x"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "1"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/x"))
        .respond_with(ResponseTemplate::new(200).set_body_json(Pr {
            number: 1,
            title: "ok".into(),
        }))
        .mount(&server)
        .await;

    let http = client(&server, None, clock.clone());
    let _ = http.get::<serde_json::Value>("x").await.unwrap_err();

    clock.set(1_000 + 1_000);
    let got = http.get::<Pr>("x").await.unwrap();
    assert_eq!(
        got.value,
        Pr {
            number: 1,
            title: "ok".into()
        }
    );
}

/// A keychain-like token that the user can set after the client exists.
struct SwappableToken(std::sync::Mutex<Option<String>>);

impl theodore_host_http::TokenSource for SwappableToken {
    fn token(&self) -> Option<String> {
        self.0.lock().unwrap().clone()
    }
}

#[tokio::test]
async fn a_rate_limit_hit_without_a_token_does_not_block_the_token_connected_next() {
    let server = MockServer::start().await;
    let clock = FakeClock::new(1_000);

    // Polling before sign-in uses up GitHub's 60/h unauthenticated quota.
    Mock::given(method("GET"))
        .and(path("/user"))
        .respond_with(
            ResponseTemplate::new(403)
                .insert_header("x-ratelimit-remaining", "0")
                .insert_header("x-ratelimit-reset", "3600"),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/user"))
        .and(header("authorization", "Bearer fresh"))
        .respond_with(ResponseTemplate::new(200).set_body_json(Pr {
            number: 1,
            title: "ok".into(),
        }))
        .mount(&server)
        .await;

    let token = Arc::new(SwappableToken(std::sync::Mutex::new(None)));
    let base = url::Url::parse(&format!("{}/", server.uri())).unwrap();
    let http = HttpClient::new(base, token.clone(), clock);

    let err = http.get::<serde_json::Value>("user").await.unwrap_err();
    assert!(matches!(
        err,
        HostError::RateLimited {
            reset_at: 3_600_000
        }
    ));
    // Still blocked for the same (absent) token, without a round trip.
    let err = http.get::<serde_json::Value>("user").await.unwrap_err();
    assert!(matches!(err, HostError::RateLimited { .. }));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);

    // The user connects GitHub: the new token has its own quota.
    *token.0.lock().unwrap() = Some("fresh".into());
    let got = http.get::<Pr>("user").await.unwrap();
    assert_eq!(got.value.title, "ok");
}

/// Same scenario as above, but over `post_json` — GitHub's GraphQL root
/// (`viewer` query) answers an unauthenticated POST with the same 403 +
/// zeroed `x-ratelimit-*` shape REST does, and the block must still clear
/// for the token the user connects next.
#[tokio::test]
async fn an_unauthenticated_graphql_post_is_blocked_then_a_fresh_token_reaches_the_server() {
    let server = MockServer::start().await;
    let clock = FakeClock::new(1_000);

    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(403)
                .insert_header("x-ratelimit-limit", "0")
                .insert_header("x-ratelimit-remaining", "0")
                .insert_header("x-ratelimit-reset", "3600"),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/"))
        .and(header("authorization", "Bearer fresh"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": { "viewer": { "login": "octocat" } }
        })))
        .mount(&server)
        .await;

    let token = Arc::new(SwappableToken(std::sync::Mutex::new(None)));
    let base = url::Url::parse(&format!("{}/", server.uri())).unwrap();
    let http = HttpClient::new(base, token.clone(), clock);
    let query = serde_json::json!({ "query": "{ viewer { login } }" });

    let err = http
        .post_json::<_, serde_json::Value>("", &query)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        HostError::RateLimited {
            reset_at: 3_600_000
        }
    ));
    // Still blocked for the same (absent) token, without a round trip.
    let err = http
        .post_json::<_, serde_json::Value>("", &query)
        .await
        .unwrap_err();
    assert!(matches!(err, HostError::RateLimited { .. }));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);

    // The user connects GitHub: the new token has its own quota, so this
    // request isn't blocked locally and actually hits the server.
    *token.0.lock().unwrap() = Some("fresh".into());
    let got = http
        .post_json::<_, serde_json::Value>("", &query)
        .await
        .unwrap();
    assert_eq!(got.value["data"]["viewer"]["login"], "octocat");
}

#[tokio::test]
async fn forbidden_without_rate_limit_headers_is_auth_not_rate_limited() {
    let server = MockServer::start().await;
    let clock = FakeClock::new(0);

    Mock::given(method("GET"))
        .and(path("/notifications"))
        .respond_with(ResponseTemplate::new(403).set_body_string("Resource not accessible"))
        .mount(&server)
        .await;

    let http = client(&server, None, clock);
    let err = http
        .get::<serde_json::Value>("notifications")
        .await
        .unwrap_err();
    assert!(matches!(err, HostError::Auth), "expected Auth, got {err:?}");
}

#[tokio::test]
async fn retries_5xx_then_succeeds() {
    let server = MockServer::start().await;
    let clock = FakeClock::new(0);

    Mock::given(method("GET"))
        .and(path("/flaky"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(2)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/flaky"))
        .respond_with(ResponseTemplate::new(200).set_body_json(Pr {
            number: 9,
            title: "ok".into(),
        }))
        .mount(&server)
        .await;

    let http = client(&server, None, clock.clone());
    let got = http.get::<Pr>("flaky").await.unwrap();
    assert_eq!(
        got.value,
        Pr {
            number: 9,
            title: "ok".into()
        }
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
    // Backoff slept (on the fake clock) between attempts.
    assert!(clock.now() > 0);
}

#[tokio::test]
async fn does_not_retry_4xx() {
    let server = MockServer::start().await;
    let clock = FakeClock::new(0);

    Mock::given(method("GET"))
        .and(path("/missing"))
        .respond_with(ResponseTemplate::new(404).set_body_string("not here"))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let http = client(&server, None, clock);
    let err = http.get::<serde_json::Value>("missing").await.unwrap_err();
    assert!(matches!(err, HostError::NotFound(_)));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn gives_up_after_max_retries_on_persistent_5xx() {
    let server = MockServer::start().await;
    let clock = FakeClock::new(0);

    Mock::given(method("GET"))
        .and(path("/down"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let http = client(&server, None, clock);
    let err = http.get::<serde_json::Value>("down").await.unwrap_err();
    assert!(matches!(err, HostError::Server(_)));
    // Default retry config: 1 initial try + 3 retries.
    assert_eq!(server.received_requests().await.unwrap().len(), 4);
}

#[tokio::test]
async fn token_is_redacted_from_error_bodies() {
    let server = MockServer::start().await;
    let clock = FakeClock::new(0);
    let secret = "ghp_super_secret_value";

    // A server that (mis-)echoes the bearer token into an error body. Real
    // GitHub never does this; the test proves host-http scrubs it anyway.
    Mock::given(method("GET"))
        .and(path("/echo"))
        .respond_with(
            ResponseTemplate::new(404).set_body_string(format!("not found, saw token {secret}")),
        )
        .mount(&server)
        .await;

    let http = client(&server, Some(secret), clock);
    let err = http.get::<serde_json::Value>("echo").await.unwrap_err();
    let msg = err.to_string();
    assert!(!msg.contains(secret), "error leaked the token: {msg}");

    let HostError::NotFound(body) = &err else {
        panic!("expected NotFound, got {err:?}")
    };
    assert!(!body.contains(secret));
    assert!(body.contains("<redacted>"));
}

async fn error_for(status: u16, body: serde_json::Value) -> HostError {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x"))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(&server)
        .await;
    let http = client(&server, None, FakeClock::new(0));
    http.get::<serde_json::Value>("x").await.unwrap_err()
}

#[tokio::test]
async fn a_json_error_body_becomes_its_message() {
    let err = error_for(
        422,
        serde_json::json!({
            "message": "Review cannot be requested from pull request author.",
            "documentation_url": "https://docs.github.com/rest/pulls/review-requests",
            "status": "422"
        }),
    )
    .await;
    assert!(
        matches!(&err, HostError::Validation(m)
            if m == "Review cannot be requested from pull request author."),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_validation_failure_keeps_the_messages_of_its_errors() {
    let err = error_for(
        422,
        serde_json::json!({
            "message": "Validation Failed",
            "errors": [{
                "resource": "PullRequest",
                "code": "custom",
                "message": "A pull request already exists for acme:feature."
            }],
            "documentation_url": "https://docs.github.com/rest/pulls/pulls"
        }),
    )
    .await;
    assert!(
        matches!(&err, HostError::Validation(m)
            if m == "Validation Failed: A pull request already exists for acme:feature."),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_gitlab_error_body_becomes_its_message() {
    let err = error_for(
        409,
        serde_json::json!({ "message": ["Branch already exists"] }),
    )
    .await;
    assert!(
        matches!(&err, HostError::Conflict(m) if m == "Branch already exists"),
        "{err:?}"
    );
    let err = error_for(404, serde_json::json!({ "error": "404 Not Found" })).await;
    assert!(
        matches!(&err, HostError::NotFound(m) if m == "404 Not Found"),
        "{err:?}"
    );
}

#[tokio::test]
async fn get_raw_exposes_headers_on_not_modified_for_if_modified_since() {
    let server = MockServer::start().await;
    let clock = FakeClock::new(0);

    // wiremock's header matcher treats a comma-bearing value (an HTTP-date,
    // e.g.) as multiple header values and won't match it as one string, so
    // the test uses a comma-free stand-in; get_raw itself is header-agnostic.
    Mock::given(method("GET"))
        .and(path("/notifications"))
        .and(header("if-modified-since", "last-poll-cursor"))
        .respond_with(ResponseTemplate::new(304).insert_header("x-poll-interval", "60"))
        .mount(&server)
        .await;

    let http = client(&server, None, clock);
    let raw = http
        .get_raw(
            "notifications",
            &[("if-modified-since", "last-poll-cursor".to_string())],
        )
        .await
        .unwrap();

    assert!(raw.not_modified);
    assert_eq!(
        raw.headers
            .get("x-poll-interval")
            .and_then(|v| v.to_str().ok()),
        Some("60")
    );
}
