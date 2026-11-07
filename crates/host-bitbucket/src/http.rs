//! Minimal HTTP transport for Bitbucket Cloud's REST API 2.0.
//!
//! `theodore-host-http`'s shared `HttpClient` only ever sends `Authorization:
//! Bearer <token>` (plan §6.1 built it for GitHub/GitLab's bearer-only
//! story); Bitbucket's app-password flow needs Basic auth instead (plan
//! §6.8's `Credentials::Basic`), which that client has no way to request. A
//! second, Bitbucket-owned client is simpler than teaching the shared one a
//! second auth shape it exists to abstract away — this one is intentionally
//! small (no ETag cache: Bitbucket's PR endpoint doesn't reward one).

use std::sync::Arc;
use std::time::Duration;

use reqwest::{Method, RequestBuilder, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use theodore_ports::{Clock, HostError, HostResult};

use crate::auth::Credentials;

pub(crate) struct BbHttp {
    client: reqwest::Client,
    base_url: url::Url,
    creds: Credentials,
    clock: Arc<dyn Clock>,
}

impl BbHttp {
    /// `base_url` must end with `/` (e.g. `https://api.bitbucket.org/2.0/`).
    pub(crate) fn new(base_url: url::Url, creds: Credentials, clock: Arc<dyn Clock>) -> Self {
        Self {
            client: theodore_host_http::host_reqwest_client(),
            base_url,
            creds,
            clock,
        }
    }

    fn url_for(&self, path: &str) -> HostResult<url::Url> {
        self.base_url
            .join(path.trim_start_matches('/'))
            .map_err(|e| HostError::Protocol(format!("bad path {path:?}: {e}")))
    }

    fn redact(&self, text: &str) -> String {
        let mut out = text.to_string();
        for secret in self.creds.secrets() {
            if !secret.is_empty() && out.contains(secret) {
                out = out.replace(secret, "<redacted>");
            }
        }
        out
    }

    fn authed(&self, req: RequestBuilder) -> RequestBuilder {
        match &self.creds {
            Credentials::Basic {
                username,
                app_password,
            } => req.basic_auth(username, Some(app_password)),
            Credentials::Bearer(token) => req.bearer_auth(token),
            Credentials::None => req,
        }
    }

    /// Exposed so `changed_since`'s opaque cursor can be minted from the
    /// same clock requests are retried against, without a second `Clock`
    /// handle living on the adapter (mirrors `theodore-host-http`'s
    /// `HttpClient::now`, same reason).
    pub(crate) fn now(&self) -> i64 {
        self.clock.now()
    }

    pub(crate) async fn get<T: DeserializeOwned>(&self, path: &str) -> HostResult<T> {
        let url = self.url_for(path)?;
        self.send(Method::GET, url, None).await
    }

    /// Follows a `next` link Bitbucket's own pagination already resolved to
    /// a full URL (plan §6.9: the analogue of GitLab's `x-next-page`).
    pub(crate) async fn get_absolute<T: DeserializeOwned>(&self, url: &str) -> HostResult<T> {
        let url = url::Url::parse(url)
            .map_err(|e| HostError::Protocol(format!("bad next-page url {url:?}: {e}")))?;
        self.send(Method::GET, url, None).await
    }

    pub(crate) async fn post_json<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> HostResult<T> {
        let url = self.url_for(path)?;
        let json = encode(body)?;
        self.send(Method::POST, url, Some(json)).await
    }

    /// POST with no body and no response payload worth decoding (Bitbucket
    /// answers `approve`/`request-changes`/`resolve` with 200 and the
    /// resource, which callers here don't need back).
    pub(crate) async fn post_empty(&self, path: &str) -> HostResult<()> {
        let url = self.url_for(path)?;
        let _: serde_json::Value = self.send(Method::POST, url, Some("{}".to_string())).await?;
        Ok(())
    }

    pub(crate) async fn put_json<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> HostResult<T> {
        let url = self.url_for(path)?;
        let json = encode(body)?;
        self.send(Method::PUT, url, Some(json)).await
    }

    /// Bitbucket answers a successful DELETE with 204 and an empty body;
    /// `send`'s empty-body case (below) already decodes that as `Value::Null`.
    pub(crate) async fn delete(&self, path: &str) -> HostResult<()> {
        let url = self.url_for(path)?;
        let _: serde_json::Value = self.send(Method::DELETE, url, None).await?;
        Ok(())
    }

    async fn send<T: DeserializeOwned>(
        &self,
        method: Method,
        url: url::Url,
        body: Option<String>,
    ) -> HostResult<T> {
        let text = self.send_raw(method, url, body).await?;
        if text.trim().is_empty() {
            // `T` is only ever `serde_json::Value` or `()`-shaped callers
            // that tolerate `null` here (`post_empty`/`delete`).
            return serde_json::from_value(serde_json::Value::Null)
                .map_err(|e| HostError::Protocol(format!("decode empty body: {e}")));
        }
        serde_json::from_str(&text)
            .map_err(|e| HostError::Protocol(self.redact(&format!("decode: {e}: {text}"))))
    }

    async fn send_raw(
        &self,
        method: Method,
        url: url::Url,
        body: Option<String>,
    ) -> HostResult<String> {
        let max_retries = 3u32;
        let mut attempt = 0u32;
        loop {
            let mut req = self
                .client
                .request(method.clone(), url.clone())
                .header("user-agent", "theodore")
                .header("accept", "application/json");
            if let Some(b) = &body {
                req = req
                    .header("content-type", "application/json")
                    .body(b.clone());
            }
            req = self.authed(req);

            tracing::debug!(method = %method, url = %url, attempt, "host-bitbucket request");

            match req.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    let headers = resp.headers().clone();

                    if status.is_server_error() && attempt < max_retries {
                        attempt += 1;
                        self.backoff(attempt).await;
                        continue;
                    }

                    if let Some(err) = classify(status, &headers, self.clock.now()) {
                        let text = resp.text().await.unwrap_or_default();
                        return Err(attach_body(err, &self.redact(&text)));
                    }

                    if status.is_server_error() {
                        return Err(HostError::Server(format!("{status} after retries")));
                    }

                    return resp
                        .text()
                        .await
                        .map_err(|e| HostError::Network(self.redact(&e.to_string())));
                }
                Err(e) if attempt < max_retries && is_retryable(&e) => {
                    attempt += 1;
                    self.backoff(attempt).await;
                }
                Err(e) => return Err(HostError::Network(self.redact(&e.to_string()))),
            }
        }
    }

    async fn backoff(&self, attempt: u32) {
        let base = 200u64;
        let cap = 5_000u64;
        let exp = base.saturating_mul(1u64 << attempt.min(10)).min(cap);
        self.clock.sleep(Duration::from_millis(exp)).await;
    }
}

fn encode<B: Serialize + ?Sized>(body: &B) -> HostResult<String> {
    serde_json::to_string(body).map_err(|e| HostError::Protocol(format!("encode: {e}")))
}

fn classify(
    status: StatusCode,
    headers: &reqwest::header::HeaderMap,
    now: i64,
) -> Option<HostError> {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Some(HostError::Auth),
        StatusCode::TOO_MANY_REQUESTS => {
            let reset_at = retry_after_ms(headers).unwrap_or(now + 60_000);
            Some(HostError::RateLimited { reset_at })
        }
        StatusCode::NOT_FOUND => Some(HostError::NotFound(String::new())),
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

fn retry_after_ms(headers: &reqwest::header::HeaderMap) -> Option<i64> {
    let secs: i64 = headers
        .get("retry-after")?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(secs * 1000)
}

fn attach_body(err: HostError, body: &str) -> HostError {
    match err {
        HostError::NotFound(_) => HostError::NotFound(body.to_string()),
        HostError::Conflict(_) => HostError::Conflict(body.to_string()),
        HostError::Validation(_) => HostError::Validation(body.to_string()),
        HostError::Protocol(prefix) => HostError::Protocol(format!("{prefix}: {body}")),
        other => other,
    }
}

fn is_retryable(e: &reqwest::Error) -> bool {
    e.is_timeout() || e.is_connect() || e.is_request()
}
