use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::header::HeaderMap;
use reqwest::{Method, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use theodore_ports::{Clock, Fetched, HostError, HostResult};

use crate::rate::{RateState, is_rate_limited_403, reset_at_from_headers};
use crate::redact::redact;
use crate::token::TokenSource;

/// Retry policy for 5xx and network errors. 4xx (including 429, which maps to
/// `HostError::RateLimited`) are never retried here — the caller/outbox
/// decides.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    pub max_retries: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_millis(200),
            max_delay: Duration::from_secs(5),
        }
    }
}

/// A decoded JSON response plus the bits adapters read themselves
/// (`x-poll-interval`, `link`, …) — host-http only understands the generic
/// concerns (auth, ETag, rate limit, retry).
#[derive(Debug, Clone)]
pub struct HttpResponse<T> {
    pub value: T,
    pub etag: Option<String>,
    pub headers: HeaderMap,
}

enum Outcome {
    NotModified(HeaderMap),
    Ok { body: String, headers: HeaderMap },
}

/// Raw conditional GET for callers that use a header other than
/// `If-None-Match` (e.g. `/notifications` with `If-Modified-Since`) and still
/// need the headers on a 304 (`X-Poll-Interval` is sent either way).
#[derive(Debug, Clone)]
pub struct RawResponse {
    pub not_modified: bool,
    pub body: String,
    pub headers: HeaderMap,
}

/// Time to open a connection (DNS + TCP + TLS) before giving up.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Whole-request budget, body included. Host API answers are JSON pages
/// (diffs come from git, not HTTP); the largest — GitLab's `/changes` for
/// a big MR — still fits well inside this on a slow link.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// A `reqwest::Client` with the host timeouts above. Without them a
/// half-open connection (laptop sleep, captive portal) hangs a request —
/// and the outbox item or poll waiting on it — forever. A timeout is a
/// retryable network error, so the usual retry/backoff takes over.
pub fn host_reqwest_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "host http client without timeouts");
            reqwest::Client::new()
        })
}

/// Shared HTTP client for code-host adapters. One instance per API endpoint
/// (e.g. GitHub REST root and GitHub GraphQL each get their own, since they
/// have different base URLs on GitHub Enterprise).
pub struct HttpClient {
    client: reqwest::Client,
    base_url: url::Url,
    token: Arc<dyn TokenSource>,
    clock: Arc<dyn Clock>,
    rate: Mutex<RateState>,
    retry: RetryConfig,
    user_agent: String,
    /// `accept` sent with requests that carry a body.
    accept: String,
}

impl HttpClient {
    /// `base_url` must end with `/` for REST-style relative paths to join
    /// correctly (e.g. `https://api.github.com/`). A GraphQL endpoint is a
    /// full URL with no further joining (call `post_json` with `path = ""`).
    pub fn new(base_url: url::Url, token: Arc<dyn TokenSource>, clock: Arc<dyn Clock>) -> Self {
        Self::with_reqwest_client(base_url, token, clock, host_reqwest_client())
    }

    pub fn with_reqwest_client(
        base_url: url::Url,
        token: Arc<dyn TokenSource>,
        clock: Arc<dyn Clock>,
        client: reqwest::Client,
    ) -> Self {
        Self {
            client,
            base_url,
            token,
            clock,
            rate: Mutex::new(RateState::default()),
            retry: RetryConfig::default(),
            user_agent: "theodore".to_string(),
            accept: "application/vnd.github+json".to_string(),
        }
    }

    pub fn with_retry(mut self, retry: RetryConfig) -> Self {
        self.retry = retry;
        self
    }

    /// Media type for requests with a body (default: GitHub's).
    pub fn with_accept(mut self, accept: impl Into<String>) -> Self {
        self.accept = accept.into();
        self
    }

    pub fn with_user_agent(mut self, ua: impl Into<String>) -> Self {
        self.user_agent = ua.into();
        self
    }

    fn url_for(&self, path: &str) -> HostResult<url::Url> {
        if path.is_empty() {
            return Ok(self.base_url.clone());
        }
        self.base_url
            .join(path.trim_start_matches('/'))
            .map_err(|e| HostError::Protocol(format!("bad path {path:?}: {e}")))
    }

    /// Exposed so a caller building its own `HostError::RateLimited` (e.g.
    /// GraphQL's body-level `RATE_LIMITED` errors, which carry no reset
    /// header for `reset_at_from_headers` to read) can fall back to the same
    /// `now + 60s` this client uses for a 403/429 with no reset header.
    pub fn now(&self) -> i64 {
        self.clock.now()
    }

    /// For callers outside `execute` (`decode`, on a successful response)
    /// that never captured the token a request sent — the current token is
    /// the best guess there, and good enough for a success body.
    fn redact(&self, text: &str) -> String {
        redact(self.token.token().as_deref(), text)
    }

    /// GET, decoded as JSON. No conditional request.
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> HostResult<HttpResponse<T>> {
        match self.execute(Method::GET, path, None, &[], &[]).await? {
            Outcome::Ok { body, headers } => {
                let value = self.decode(&body)?;
                let etag = extract_etag(&headers);
                Ok(HttpResponse {
                    value,
                    etag,
                    headers,
                })
            }
            Outcome::NotModified(_) => unreachable!("no conditional header sent"),
        }
    }

    /// GET with `If-None-Match: <if_none_match>`. `None` is a plain,
    /// unconditional GET that always answers `Modified`: the caller owns
    /// the ETag (it has a body to compare against; this client doesn't).
    /// A per-URL cache here used to fill in `None` with the last ETag seen,
    /// so a caller asking for a fresh body — opening a PR — got a 304 it
    /// had nothing to show for.
    pub async fn get_conditional<T: DeserializeOwned>(
        &self,
        path: &str,
        if_none_match: Option<&str>,
    ) -> HostResult<Fetched<HttpResponse<T>>> {
        let headers: &[(&str, String)] = match if_none_match {
            Some(e) => &[("if-none-match", e.to_string())],
            None => &[],
        };

        match self.execute(Method::GET, path, None, headers, &[]).await? {
            Outcome::NotModified(_) => Ok(Fetched::NotModified),
            Outcome::Ok { body, headers } => {
                let value = self.decode(&body)?;
                let etag = extract_etag(&headers);
                Ok(Fetched::Modified {
                    etag: etag.clone(),
                    value: HttpResponse {
                        value,
                        etag,
                        headers,
                    },
                })
            }
        }
    }

    /// Raw conditional GET with caller-chosen headers (e.g.
    /// `If-Modified-Since`). Headers are returned on both outcomes.
    pub async fn get_raw(&self, path: &str, headers: &[(&str, String)]) -> HostResult<RawResponse> {
        match self.execute(Method::GET, path, None, headers, &[]).await? {
            Outcome::NotModified(headers) => Ok(RawResponse {
                not_modified: true,
                body: String::new(),
                headers,
            }),
            Outcome::Ok { body, headers } => Ok(RawResponse {
                not_modified: false,
                body,
                headers,
            }),
        }
    }

    pub async fn post_json<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> HostResult<HttpResponse<T>> {
        self.body_request(Method::POST, path, body, &[]).await
    }

    /// `post_json` for a caller that handles `expected` failure statuses
    /// itself (e.g. retries without labels after a 403): those are logged
    /// at info, not as a refusal. The error returned is the same.
    pub async fn post_json_expecting<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
        expected: &[StatusCode],
    ) -> HostResult<HttpResponse<T>> {
        self.body_request(Method::POST, path, body, expected).await
    }

    pub async fn patch_json<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> HostResult<HttpResponse<T>> {
        self.body_request(Method::PATCH, path, body, &[]).await
    }

    pub async fn put_json<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> HostResult<HttpResponse<T>> {
        self.body_request(Method::PUT, path, body, &[]).await
    }

    async fn body_request<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: &B,
        expected: &[StatusCode],
    ) -> HostResult<HttpResponse<T>> {
        let json =
            serde_json::to_string(body).map_err(|e| HostError::Protocol(format!("encode: {e}")))?;
        match self
            .execute(method, path, Some(json), &[], expected)
            .await?
        {
            Outcome::Ok { body, headers } => {
                let value = self.decode(&body)?;
                let etag = extract_etag(&headers);
                Ok(HttpResponse {
                    value,
                    etag,
                    headers,
                })
            }
            Outcome::NotModified(_) => unreachable!("no conditional header sent"),
        }
    }

    /// POST, ignoring any response body (204 / empty on success).
    pub async fn post_no_content<B: Serialize + ?Sized>(
        &self,
        path: &str,
        body: &B,
    ) -> HostResult<()> {
        let json =
            serde_json::to_string(body).map_err(|e| HostError::Protocol(format!("encode: {e}")))?;
        match self
            .execute(Method::POST, path, Some(json), &[], &[])
            .await?
        {
            Outcome::Ok { .. } => Ok(()),
            Outcome::NotModified(_) => unreachable!("no conditional header sent"),
        }
    }

    /// DELETE, ignoring any response body (GitHub returns 204/empty on
    /// success for comment deletion etc).
    pub async fn delete(&self, path: &str) -> HostResult<()> {
        match self.execute(Method::DELETE, path, None, &[], &[]).await? {
            Outcome::Ok { .. } => Ok(()),
            Outcome::NotModified(_) => unreachable!("no conditional header sent"),
        }
    }

    /// DELETE with a JSON body (GitHub's `requested_reviewers` endpoint
    /// requires one to say who to remove).
    pub async fn delete_with_body<B: Serialize + ?Sized>(
        &self,
        path: &str,
        body: &B,
    ) -> HostResult<()> {
        let json =
            serde_json::to_string(body).map_err(|e| HostError::Protocol(format!("encode: {e}")))?;
        match self
            .execute(Method::DELETE, path, Some(json), &[], &[])
            .await?
        {
            Outcome::Ok { .. } => Ok(()),
            Outcome::NotModified(_) => unreachable!("no conditional header sent"),
        }
    }

    fn decode<T: DeserializeOwned>(&self, body: &str) -> HostResult<T> {
        serde_json::from_str(body)
            .map_err(|e| HostError::Protocol(self.redact(&format!("decode: {e}: {body}"))))
    }

    async fn execute(
        &self,
        method: Method,
        path: &str,
        body: Option<String>,
        extra_headers: &[(&str, String)],
        expected: &[StatusCode],
    ) -> HostResult<Outcome> {
        let token = self.token.token();
        if let Some(until) = self
            .rate
            .lock()
            .expect("rate lock")
            .preemptive_block(self.now(), token.as_deref())
        {
            return Err(HostError::RateLimited { reset_at: until });
        }

        let url = self.url_for(path)?;
        let mut attempt: u32 = 0;
        let started = std::time::Instant::now();
        loop {
            let mut req = self
                .client
                .request(method.clone(), url.clone())
                .header("user-agent", &self.user_agent);
            if let Some(token) = &token {
                req = req.bearer_auth(token);
            }
            if method != Method::GET || body.is_some() {
                req = req.header("accept", &self.accept);
            }
            for (name, value) in extra_headers {
                req = req.header(*name, value.clone());
            }
            if let Some(b) = &body {
                req = req
                    .header("content-type", "application/json")
                    .body(b.clone());
            }

            tracing::debug!(method = %method, url = %url, attempt, "host-http request");

            match req.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    let headers = resp.headers().clone();
                    self.rate.lock().expect("rate lock").observe(&headers);

                    if status == StatusCode::NOT_MODIFIED {
                        return Ok(Outcome::NotModified(headers));
                    }

                    // Retries are logged quietly: most succeed on the next
                    // try. Only the final failure below is a warning.
                    if status.is_server_error() && attempt < self.retry.max_retries {
                        tracing::info!(
                            method = %method,
                            url = url.path(),
                            status = status.as_u16(),
                            attempt,
                            "host-http retrying server error"
                        );
                        attempt += 1;
                        self.backoff(attempt).await;
                        continue;
                    }

                    if let Some(err) = self.classify(status, &headers, token.as_deref()) {
                        log_status(
                            &method,
                            &url,
                            status,
                            attempt,
                            started,
                            matches!(err, HostError::RateLimited { .. }),
                            expected.contains(&status),
                        );
                        let text = resp.text().await.unwrap_or_default();
                        return Err(self.attach_body(err, &text, token.as_deref()));
                    }

                    if status.is_server_error() {
                        tracing::warn!(
                            error.code = "host.server_error",
                            method = %method,
                            url = url.path(),
                            status = status.as_u16(),
                            attempt,
                            duration_ms = started.elapsed().as_millis() as u64,
                            "host server error after retries"
                        );
                        return Err(HostError::Server(format!("{status} after retries")));
                    }

                    let text = resp.text().await.map_err(|e| {
                        HostError::Network(redact(token.as_deref(), &e.to_string()))
                    })?;
                    return Ok(Outcome::Ok {
                        body: text,
                        headers,
                    });
                }
                Err(e) if attempt < self.retry.max_retries && is_retryable_send_error(&e) => {
                    tracing::debug!(
                        method = %method,
                        url = url.path(),
                        error = %redact(token.as_deref(), &e.to_string()),
                        attempt,
                        "host-http retrying network error"
                    );
                    attempt += 1;
                    self.backoff(attempt).await;
                }
                Err(e) => {
                    let msg = redact(token.as_deref(), &e.to_string());
                    tracing::warn!(
                        error.code = "host.network",
                        method = %method,
                        url = url.path(),
                        attempt,
                        duration_ms = started.elapsed().as_millis() as u64,
                        error = %msg,
                        "host request failed"
                    );
                    return Err(HostError::Network(msg));
                }
            }
        }
    }

    fn classify(
        &self,
        status: StatusCode,
        headers: &HeaderMap,
        token: Option<&str>,
    ) -> Option<HostError> {
        match status {
            StatusCode::UNAUTHORIZED => Some(HostError::Auth),
            StatusCode::FORBIDDEN => {
                if is_rate_limited_403(headers) {
                    let reset_at = reset_at_from_headers(headers, self.now());
                    self.rate
                        .lock()
                        .expect("rate lock")
                        .block_until(reset_at, token);
                    Some(HostError::RateLimited { reset_at })
                } else {
                    Some(HostError::Auth)
                }
            }
            StatusCode::TOO_MANY_REQUESTS => {
                let reset_at = reset_at_from_headers(headers, self.now());
                self.rate
                    .lock()
                    .expect("rate lock")
                    .block_until(reset_at, token);
                Some(HostError::RateLimited { reset_at })
            }
            StatusCode::NOT_FOUND => Some(HostError::NotFound(String::new())),
            // 405: GitHub/GitLab's merge endpoints answer this when the PR
            // isn't currently mergeable (open conflicts, blocked, etc).
            StatusCode::CONFLICT | StatusCode::METHOD_NOT_ALLOWED => {
                Some(HostError::Conflict(String::new()))
            }
            StatusCode::UNPROCESSABLE_ENTITY | StatusCode::BAD_REQUEST => {
                Some(HostError::Validation(String::new()))
            }
            s if s.is_client_error() => Some(HostError::Protocol(format!("unexpected status {s}"))),
            _ => None,
        }
    }

    /// `token` is the one the request actually sent (captured once at the
    /// top of `execute`), not whatever `self.token` reads now — the two can
    /// differ if the token was swapped while this response was in flight.
    fn attach_body(&self, err: HostError, body: &str, token: Option<&str>) -> HostError {
        let msg = redact(token, &error_message(body));
        match err {
            HostError::NotFound(_) => HostError::NotFound(msg),
            HostError::Conflict(_) => HostError::Conflict(msg),
            HostError::Validation(_) => HostError::Validation(msg),
            HostError::Protocol(prefix) => HostError::Protocol(format!("{prefix}: {msg}")),
            other => other,
        }
    }

    async fn backoff(&self, attempt: u32) {
        let base = self.retry.base_delay.as_millis() as u64;
        let cap = self.retry.max_delay.as_millis() as u64;
        let exp = base.saturating_mul(1u64 << attempt.min(10)).min(cap.max(1));
        let jitter = rand::random_range(0..=(exp / 2 + 1));
        self.clock
            .sleep(Duration::from_millis(exp / 2 + jitter))
            .await;
    }
}

/// The words a person should see from an error body: its `message` (GitHub,
/// GitLab) or `error` (GitLab), with GitHub's per-field `errors[].message`
/// appended, instead of the raw JSON. A body that isn't JSON, or has none
/// of these, is kept as it came.
///
/// ```text
/// {"message":"Review cannot be requested from pull request author.",…}
///   → Review cannot be requested from pull request author.
/// {"message":"Validation Failed","errors":[{"message":"A pull request already exists…"}]}
///   → Validation Failed: A pull request already exists…
/// ```
fn error_message(body: &str) -> String {
    let trimmed = body.trim();
    let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return trimmed.to_string();
    };
    // GitLab's `message` can be a list, or a map of field → list.
    fn words(v: &serde_json::Value) -> Option<String> {
        let text = match v {
            serde_json::Value::String(s) => s.trim().to_string(),
            serde_json::Value::Array(items) => items
                .iter()
                .filter_map(words)
                .collect::<Vec<_>>()
                .join("; "),
            serde_json::Value::Object(map) => match map.get("message") {
                Some(m) => words(m)?,
                None => map
                    .iter()
                    .filter_map(|(field, v)| words(v).map(|w| format!("{field} {w}")))
                    .collect::<Vec<_>>()
                    .join("; "),
            },
            _ => return None,
        };
        (!text.is_empty()).then_some(text)
    }
    let head = v
        .get("message")
        .and_then(words)
        .or_else(|| v.get("error").and_then(words));
    let details = v.get("errors").and_then(words);
    match (head, details) {
        (Some(h), Some(d)) if h != d => format!("{h}: {d}"),
        (Some(h), _) => h,
        (None, Some(d)) => d,
        (None, None) => trimmed.to_string(),
    }
}

/// The code for a failed status, as logged (docs/telemetry.md): the host
/// adapters see 401 and a non-rate-limit 403 alike as `HostError::Auth`,
/// so the log is where they're told apart.
pub fn status_code(status: StatusCode, rate_limited: bool) -> &'static str {
    match status {
        _ if rate_limited => "host.rate_limited",
        StatusCode::UNAUTHORIZED => "host.unauthorized",
        StatusCode::FORBIDDEN => "host.forbidden",
        StatusCode::NOT_FOUND => "host.not_found",
        StatusCode::TOO_MANY_REQUESTS => "host.rate_limited",
        StatusCode::CONFLICT | StatusCode::METHOD_NOT_ALLOWED => "host.conflict",
        StatusCode::UNPROCESSABLE_ENTITY | StatusCode::BAD_REQUEST => "host.validation",
        _ => "host.protocol",
    }
}

/// A 404 / 409 / 422 is often an answer, not a failure (a PR without a
/// file, a merge that isn't possible yet): logged at debug. Auth and rate
/// limits are warnings — unless the caller said it `expected` the status
/// and handles it (info).
fn log_status(
    method: &Method,
    url: &url::Url,
    status: StatusCode,
    attempt: u32,
    started: std::time::Instant,
    rate_limited: bool,
    expected: bool,
) {
    let code = status_code(status, rate_limited);
    let ms = started.elapsed().as_millis() as u64;
    match status {
        _ if expected && !rate_limited => tracing::info!(
            error.code = code,
            method = %method,
            url = url.path(),
            status = status.as_u16(),
            duration_ms = ms,
            "host refused the request; the caller handles it"
        ),
        _ if rate_limited => tracing::warn!(
            error.code = code,
            method = %method,
            url = url.path(),
            status = status.as_u16(),
            duration_ms = ms,
            "host rate limit reached"
        ),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS => {
            tracing::warn!(
                error.code = code,
                method = %method,
                url = url.path(),
                status = status.as_u16(),
                attempt,
                duration_ms = ms,
                "host refused the request"
            );
        }
        _ => tracing::debug!(
            error.code = code,
            method = %method,
            url = url.path(),
            status = status.as_u16(),
            duration_ms = ms,
            "host answered with an error status"
        ),
    }
}

fn extract_etag(headers: &HeaderMap) -> Option<String> {
    headers
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

/// `reqwest::Error` has no status for connection-level failures (DNS,
/// timeout, connect refused) — those are the ones worth retrying. A
/// `error_for_status`-style error would carry a status and is handled by the
/// status branch instead, so this only sees transport failures.
fn is_retryable_send_error(e: &reqwest::Error) -> bool {
    e.is_timeout() || e.is_connect() || e.is_request()
}

#[cfg(test)]
mod status_tests {
    use super::*;

    #[test]
    fn statuses_map_to_their_codes() {
        assert_eq!(
            status_code(StatusCode::UNAUTHORIZED, false),
            "host.unauthorized"
        );
        assert_eq!(status_code(StatusCode::FORBIDDEN, false), "host.forbidden");
        assert_eq!(
            status_code(StatusCode::FORBIDDEN, true),
            "host.rate_limited"
        );
        assert_eq!(
            status_code(StatusCode::TOO_MANY_REQUESTS, false),
            "host.rate_limited"
        );
        assert_eq!(status_code(StatusCode::NOT_FOUND, false), "host.not_found");
        assert_eq!(status_code(StatusCode::IM_A_TEAPOT, false), "host.protocol");
    }
}
