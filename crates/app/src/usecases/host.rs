//! `viewer`, `host_status`, `set_host_token` (plan §6.8): the authenticated
//! host user, connection state for onboarding/Settings → Account, and the
//! one path that changes the stored token. Also the GitHub OAuth device
//! flow (plan "login"): `login_start`/`login_poll`, a sign-in that needs
//! neither a pasted token nor `gh`.

use theodore_contract::{DeviceLogin, HostAuthStatus, LoginState, SetTokenRequest, TokenSource};
use theodore_domain::{HostKind, Person};
use theodore_ports::DevicePoll;

use crate::core::Core;
use crate::error::{AppError, AppResult};

/// Checked before the `host.github.client_id` setting (plan "login"): lets
/// a packaged build or a developer override the setting without touching
/// the store. The owner creates the OAuth app; Theodore never ships one.
const CLIENT_ID_ENV: &str = "THEODORE_GITHUB_CLIENT_ID";
/// The GitHub OAuth App (device flow) a release is built with; public, so
/// it is compiled in like the account's client id.
const BUILT_IN_CLIENT_ID: Option<&str> = option_env!("THEODORE_GITHUB_CLIENT_ID");
const CLIENT_ID_SETTING: &str = "host.github.client_id";
/// Requested once, at `login_start`: enough to read PRs, threads and
/// checks, to resolve team reviewers, and (`workflow`) for a space to push
/// changes under `.github/workflows/`, which GitHub refuses without it.
const DEVICE_FLOW_SCOPE: &str = "repo read:org workflow";

/// `device_code` plus the poll interval, alive between `login_start` and
/// the login finishing (plan "login"). Lives in `Core`'s `Inner::login`
/// only — never persisted, so a restart just means starting over.
#[derive(Debug, Clone)]
pub(crate) struct LoginSession {
    device_code: String,
    interval: u32,
}

/// The vault key for a host's own token (plan §6.8): each host keeps its
/// token independent of the others, keyed by kind rather than hardcoded to
/// whichever host happens to be primary.
fn token_key(host: HostKind) -> AppResult<&'static str> {
    match host {
        HostKind::Github => Ok(theodore_secrets::GITHUB_TOKEN),
        HostKind::Gitlab => Ok(theodore_secrets::GITLAB_TOKEN),
        HostKind::Bitbucket => Ok(theodore_secrets::BITBUCKET_TOKEN),
        HostKind::Local => Err(AppError::Invalid("local reviews have no host token".into())),
    }
}

impl Core {
    pub async fn viewer(&self) -> AppResult<Person> {
        Ok(self.0.deps.host.viewer().await?)
    }

    /// Connection state of one host (GitLab, Bitbucket, …): whether a token
    /// is saved for it, and whose. `host_status` covers only the primary host.
    pub async fn host_status_for(&self, host: HostKind) -> AppResult<HostAuthStatus> {
        if host == self.0.deps.host.kind() {
            return self.host_status().await;
        }
        let key = token_key(host)?;
        let secrets = self.0.deps.secrets.clone();
        let saved = tokio::task::spawn_blocking(move || secrets.get(key))
            .await
            .map_err(AppError::join)?
            .ok()
            .flatten()
            .is_some();
        let device_login_available = false;
        if !saved {
            return Ok(HostAuthStatus {
                host,
                connected: false,
                login: None,
                source: TokenSource::None,
                device_login_available,
            });
        }
        let login = self
            .0
            .deps
            .host
            .viewer_for(host)
            .await
            .ok()
            .map(|p| p.login);
        Ok(HostAuthStatus {
            host,
            connected: login.is_some(),
            login,
            source: TokenSource::Keychain,
            device_login_available,
        })
    }

    pub async fn host_status(&self) -> AppResult<HostAuthStatus> {
        let host = self.0.deps.host.kind();
        let source = self.token_source();
        let device_login_available = self.device_login_available().await;
        if source == TokenSource::None {
            return Ok(HostAuthStatus {
                host,
                connected: false,
                login: None,
                source,
                device_login_available,
            });
        }
        match self.0.deps.host.viewer().await {
            Ok(person) => Ok(HostAuthStatus {
                host,
                connected: true,
                login: Some(person.login),
                source,
                device_login_available,
            }),
            Err(_) => Ok(HostAuthStatus {
                host,
                connected: false,
                login: None,
                source,
                device_login_available,
            }),
        }
    }

    /// Validates the new token against the host before storing it, so a
    /// typo never silently replaces a working token. Removing the token
    /// (`req.token: None`) always succeeds.
    pub async fn set_host_token(&self, req: SetTokenRequest) -> AppResult<HostAuthStatus> {
        let key = token_key(req.host)?;
        let secrets = self.0.deps.secrets.clone();
        let device_login_available = self.device_login_available().await;
        match req.token {
            Some(token) => {
                // Only `key` (this host's own slot) is ever touched below —
                // another host's token is never read, written or deleted
                // by this call.
                let previous = secrets.get(key).ok().flatten();
                secrets
                    .set(key, &token)
                    .map_err(|e| AppError::Invalid(e.to_string()))?;
                self.invalidate_token_cache();
                match self.0.deps.host.viewer_for(req.host).await {
                    Ok(person) => Ok(HostAuthStatus {
                        host: req.host,
                        connected: true,
                        login: Some(person.login),
                        source: TokenSource::Keychain,
                        device_login_available,
                    }),
                    Err(e) => {
                        match previous {
                            Some(p) => {
                                let _ = secrets.set(key, &p);
                            }
                            None => {
                                let _ = secrets.delete(key);
                            }
                        }
                        self.invalidate_token_cache();
                        Err(AppError::Host(e))
                    }
                }
            }
            None => {
                secrets
                    .delete(key)
                    .map_err(|e| AppError::Invalid(e.to_string()))?;
                self.invalidate_token_cache();
                Ok(HostAuthStatus {
                    host: req.host,
                    connected: false,
                    login: None,
                    source: TokenSource::None,
                    device_login_available,
                })
            }
        }
    }

    /// Starts a GitHub device-flow login (plan "login"). The UI is
    /// expected to hide the button when `HostAuthStatus::device_login_available`
    /// is false, but a direct call still gets a clear error rather than a
    /// confusing host one.
    pub async fn login_start(&self) -> AppResult<DeviceLogin> {
        let client_id = self
            .github_client_id()
            .await
            .ok_or_else(|| AppError::Invalid("no GitHub OAuth client id configured".into()))?;
        let start = self
            .0
            .deps
            .device_login
            .start(&client_id, DEVICE_FLOW_SCOPE)
            .await?;
        *self.0.login.lock().await = Some(LoginSession {
            device_code: start.device_code,
            interval: start.interval,
        });
        Ok(DeviceLogin {
            user_code: start.user_code,
            verification_uri: start.verification_uri,
            expires_in: start.expires_in,
            interval: start.interval,
        })
    }

    /// Polls once for the outcome of the login `login_start` began. On
    /// success, stores the token exactly as `set_host_token` would:
    /// validate with `viewer`, then invalidate the token cache.
    pub async fn login_poll(&self) -> AppResult<LoginState> {
        let session = self
            .0
            .login
            .lock()
            .await
            .clone()
            .ok_or_else(|| AppError::Invalid("no login in progress".into()))?;
        let client_id = self
            .github_client_id()
            .await
            .ok_or_else(|| AppError::Invalid("no GitHub OAuth client id configured".into()))?;

        let outcome = self
            .0
            .deps
            .device_login
            .poll(&client_id, &session.device_code, session.interval)
            .await?;

        match outcome {
            DevicePoll::Pending => Ok(LoginState::Pending),
            DevicePoll::SlowDown { interval } => {
                *self.0.login.lock().await = Some(LoginSession {
                    device_code: session.device_code,
                    interval,
                });
                Ok(LoginState::SlowDown { interval })
            }
            DevicePoll::AccessDenied => {
                *self.0.login.lock().await = None;
                Ok(LoginState::Denied)
            }
            DevicePoll::ExpiredToken => {
                *self.0.login.lock().await = None;
                Ok(LoginState::Expired)
            }
            DevicePoll::Success { access_token } => {
                *self.0.login.lock().await = None;
                let status = self
                    .set_host_token(SetTokenRequest {
                        host: HostKind::Github,
                        token: Some(access_token),
                    })
                    .await?;
                Ok(LoginState::Done { status })
            }
        }
    }

    async fn device_login_available(&self) -> bool {
        self.github_client_id().await.is_some()
    }

    /// `THEODORE_GITHUB_CLIENT_ID`, else the `host.github.client_id`
    /// setting, else the one compiled in; `None` when none is set (plan
    /// "login").
    async fn github_client_id(&self) -> Option<String> {
        if let Ok(id) = std::env::var(CLIENT_ID_ENV)
            && !id.trim().is_empty()
        {
            return Some(id);
        }
        let s = self.setting_string(CLIENT_ID_SETTING, "").await;
        if !s.trim().is_empty() {
            return Some(s);
        }
        BUILT_IN_CLIENT_ID
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
    }

    fn token_source(&self) -> TokenSource {
        self.0
            .deps
            .token_status
            .as_ref()
            .map(|t| t.source())
            .unwrap_or(TokenSource::None)
    }

    fn invalidate_token_cache(&self) {
        if let Some(t) = &self.0.deps.token_status {
            t.invalidate();
        }
    }
}
