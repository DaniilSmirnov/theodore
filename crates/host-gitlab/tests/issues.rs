mod support;

use theodore_domain::{HostKind, RepoRef};
use theodore_ports::HostAdapter;
use wiremock::matchers::{method, path, query_param};
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
}

#[tokio::test]
async fn my_open_issues_reads_the_viewer_then_lists_their_issues() {
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

    let got = s.adapter.my_open_issues(&repo()).await.unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].number, 42);
}
