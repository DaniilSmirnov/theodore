mod support;

use theodore_domain::{HostKind, RepoRef};
use theodore_ports::{HostAdapter, HostError};
use wiremock::matchers::{method, path, query_param, query_param_is_missing};
use wiremock::{Mock, ResponseTemplate};

fn repo() -> RepoRef {
    RepoRef {
        host: HostKind::Github,
        owner: "acme".into(),
        name: "widgets".into(),
    }
}

fn gh_issue(number: u64) -> serde_json::Value {
    serde_json::json!({
        "number": number,
        "title": "Fix the login redirect",
        "body": "It sends people to /undefined.",
        "html_url": format!("https://github.com/acme/widgets/issues/{number}"),
    })
}

#[tokio::test]
async fn get_issue_reads_number_title_body_and_url() {
    let s = support::servers().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/widgets/issues/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(gh_issue(42)))
        .mount(&s.rest)
        .await;

    let got = s.adapter.get_issue(&repo(), 42).await.unwrap();
    assert_eq!(got.number, 42);
    assert_eq!(got.title, "Fix the login redirect");
    assert_eq!(got.body, "It sends people to /undefined.");
    assert_eq!(got.url, "https://github.com/acme/widgets/issues/42");
    assert!(!got.closed, "no state reads as open");
}

#[tokio::test]
async fn get_issue_reads_a_closed_state() {
    let s = support::servers().await;
    let mut issue = gh_issue(12);
    issue["state"] = serde_json::json!("closed");
    Mock::given(method("GET"))
        .and(path("/repos/acme/widgets/issues/12"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue))
        .mount(&s.rest)
        .await;

    assert!(s.adapter.get_issue(&repo(), 12).await.unwrap().closed);
}

#[tokio::test]
async fn get_issue_on_a_pull_request_number_is_not_found() {
    let s = support::servers().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/widgets/issues/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "number": 7,
            "title": "Some PR",
            "body": null,
            "html_url": "https://github.com/acme/widgets/pull/7",
            "pull_request": { "url": "https://api.github.com/repos/acme/widgets/pulls/7" }
        })))
        .mount(&s.rest)
        .await;

    let err = s.adapter.get_issue(&repo(), 7).await.unwrap_err();
    assert!(matches!(err, HostError::NotFound(_)), "{err:?}");
}

#[tokio::test]
async fn open_issues_filters_out_pull_requests() {
    let s = support::servers().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": { "viewer": { "login": "octocat", "name": null, "avatarUrl": null } }
        })))
        .mount(&s.graphql)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/widgets/issues"))
        .and(query_param("assignee", "octocat"))
        .and(query_param("state", "open"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            gh_issue(42),
            {
                "number": 7,
                "title": "Some PR",
                "body": null,
                "html_url": "https://github.com/acme/widgets/pull/7",
                "pull_request": { "url": "https://api.github.com/repos/acme/widgets/pulls/7" }
            }
        ])))
        .mount(&s.rest)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/widgets/issues"))
        .and(query_param_is_missing("assignee"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&s.rest)
        .await;

    let got = s.adapter.open_issues(&repo()).await.unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].number, 42);
}

#[tokio::test]
async fn open_issues_lists_the_repos_issues_with_yours_first() {
    let s = support::servers().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": { "viewer": { "login": "octocat", "name": null, "avatarUrl": null } }
        })))
        .mount(&s.graphql)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/widgets/issues"))
        .and(query_param("assignee", "octocat"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([gh_issue(9)])))
        .mount(&s.rest)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/widgets/issues"))
        .and(query_param("state", "open"))
        .and(query_param_is_missing("assignee"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            gh_issue(12),
            gh_issue(9),
            gh_issue(3)
        ])))
        .mount(&s.rest)
        .await;

    let got = s.adapter.open_issues(&repo()).await.unwrap();
    let numbers: Vec<u64> = got.iter().map(|i| i.number).collect();
    assert_eq!(numbers, [9, 12, 3]);
}
