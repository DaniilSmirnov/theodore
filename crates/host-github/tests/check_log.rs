mod support;

use theodore_domain::{Check, CheckStatus, HostKind, PrRef, RepoRef};
use theodore_ports::HostAdapter;
use wiremock::matchers::{method, path};
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

fn check(id: &str, url: Option<&str>) -> Check {
    Check {
        id: id.into(),
        name: "build".into(),
        status: CheckStatus::Fail,
        url: url.map(str::to_string),
        duration_ms: None,
    }
}

#[tokio::test]
async fn check_log_fetches_an_actions_job_log_by_its_job_id() {
    let s = support::servers().await;
    let log = "2024-01-01T00:00:00.0000000Z ##[group]Run cargo test\n\
               2024-01-01T00:00:01.0000000Z \u{1b}[31merror[E0001]\u{1b}[0m: it failed\n\
               2024-01-01T00:00:02.0000000Z ##[error]Process completed with exit code 1.\n";

    Mock::given(method("GET"))
        .and(path("/repos/acme/widgets/actions/jobs/999/logs"))
        .respond_with(ResponseTemplate::new(200).set_body_string(log))
        .mount(&s.rest)
        .await;

    let url = "https://github.com/acme/widgets/actions/runs/555/job/999";
    let got = s
        .adapter
        .check_log(&pr(), &check("900002", Some(url)))
        .await
        .unwrap();
    assert_eq!(
        got,
        Some(
            "##[group]Run cargo test\n\
             error[E0001]: it failed\n\
             ##[error]Process completed with exit code 1."
                .to_string()
        )
    );
}

#[tokio::test]
async fn check_log_is_none_for_a_check_run_with_no_actions_job() {
    let s = support::servers().await;
    // A third-party check-run app, or a plain commit status: no job id to
    // ask GitHub for logs with, and no request should be made for one.
    let got = s
        .adapter
        .check_log(&pr(), &check("900001", Some("https://ci.example/build/1")))
        .await
        .unwrap();
    assert_eq!(got, None);

    let got_no_url = s
        .adapter
        .check_log(&pr(), &check("sc1", None))
        .await
        .unwrap();
    assert_eq!(got_no_url, None);
}

#[tokio::test]
async fn check_log_propagates_a_not_found_job() {
    let s = support::servers().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/widgets/actions/jobs/404/logs"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&s.rest)
        .await;

    let url = "https://github.com/acme/widgets/actions/runs/1/job/404";
    let err = s
        .adapter
        .check_log(&pr(), &check("900003", Some(url)))
        .await
        .unwrap_err();
    assert!(matches!(err, theodore_ports::HostError::NotFound(_)));
}
