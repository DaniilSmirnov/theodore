mod support;

use theodore_domain::{HostKind, RepoRef};
use theodore_ports::HostAdapter;
use wiremock::matchers::{method, path, query_param, query_param_is_missing};
use wiremock::{Mock, ResponseTemplate};

fn repo() -> RepoRef {
    RepoRef {
        host: HostKind::Gitlab,
        owner: "acme".into(),
        name: "widgets".into(),
    }
}

fn gl_issue(iid: u64) -> serde_json::Value {
    serde_json::json!({
        "iid": iid,
        "title": "Fix the login redirect",
        "description": "It sends people to /undefined.",
        "web_url": format!("https://gitlab.com/acme/widgets/-/issues/{iid}"),
    })
}

#[tokio::test]
async fn get_issue_reads_number_title_body_and_url() {
    let s = support::servers().await;
    Mock::given(method("GET"))
        .and(path("/projects/acme%2Fwidgets/issues/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(gl_issue(42)))
        .mount(&s.api)
        .await;

    let got = s.adapter.get_issue(&repo(), 42).await.unwrap();
    assert_eq!(got.number, 42);
    assert_eq!(got.title, "Fix the login redirect");
    assert_eq!(got.body, "It sends people to /undefined.");
    assert_eq!(got.url, "https://gitlab.com/acme/widgets/-/issues/42");
    assert!(!got.closed, "no state reads as open");
}

#[tokio::test]
async fn get_issue_reads_a_closed_state() {
    let s = support::servers().await;
    let mut issue = gl_issue(12);
    issue["state"] = serde_json::json!("closed");
    Mock::given(method("GET"))
        .and(path("/projects/acme%2Fwidgets/issues/12"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue))
        .mount(&s.api)
        .await;

    assert!(s.adapter.get_issue(&repo(), 12).await.unwrap().closed);
}

#[tokio::test]
async fn open_issues_lists_the_projects_issues_with_yours_first() {
    let s = support::servers().await;
    Mock::given(method("GET"))
        .and(path("/user"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": 1, "username": "me"
        })))
        .mount(&s.api)
        .await;
    Mock::given(method("GET"))
        .and(path("/projects/acme%2Fwidgets/issues"))
        .and(query_param("assignee_username", "me"))
        .and(query_param("state", "opened"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([gl_issue(42)])))
        .mount(&s.api)
        .await;
    Mock::given(method("GET"))
        .and(path("/projects/acme%2Fwidgets/issues"))
        .and(query_param("state", "opened"))
        .and(query_param_is_missing("assignee_username"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!([gl_issue(5), gl_issue(42)])),
        )
        .mount(&s.api)
        .await;

    let got = s.adapter.open_issues(&repo()).await.unwrap();
    let numbers: Vec<u64> = got.iter().map(|i| i.number).collect();
    assert_eq!(numbers, [42, 5]);
}
