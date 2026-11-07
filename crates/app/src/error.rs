//! App-level errors. Command handlers (shell side) turn these into
//! `contract::CommandError`.

use theodore_ports::{AccountError, GitError, HostError, StoreError};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("git: {0}")]
    Git(#[from] GitError),
    #[error("host: {0}")]
    Host(#[from] HostError),
    #[error("grouping: {0}")]
    Grouper(#[from] GrouperError),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid request: {0}")]
    Invalid(String),
    /// The host refused a write the caller waited on: its own words, as
    /// they'd show in the outbox's toast.
    #[error("{0}")]
    Refused(String),
    #[error("background task failed: {0}")]
    Join(String),
    #[error("account: {0}")]
    Account(#[from] AccountError),
    /// Signing in to the Theodore account is required here and nobody is
    /// signed in (plan §6.12 "Sign-in required").
    #[error("Sign in to your Theodore account to continue")]
    AccountRequired,
    /// The host can't be reached and this PR's commits were never
    /// downloaded, so there's no local copy to open.
    #[error("You're offline and this PR's code hasn't been downloaded yet")]
    OfflineNotCached,
    /// The git remote can't be reached and the project was never cloned to
    /// this Mac, so there's no local copy to work from.
    #[error("You're offline and this project hasn't been downloaded to this Mac yet")]
    Offline,
}

impl AppError {
    pub fn join(e: tokio::task::JoinError) -> Self {
        AppError::Join(e.to_string())
    }

    /// The stable, dotted error code logged as `error.code`
    /// (docs/telemetry.md "Error codes"). Not the UI's `CommandError`
    /// code, which stays coarse.
    pub fn code(&self) -> &'static str {
        match self {
            AppError::Store(e) => e.code(),
            AppError::Git(e) => e.code(),
            AppError::Host(e) => e.code(),
            AppError::Grouper(e) => e.code(),
            AppError::NotFound(_) => "app.not_found",
            AppError::Invalid(_) => "app.invalid",
            AppError::Refused(_) => "host.refused",
            AppError::Join(_) => "app.internal",
            AppError::Account(e) => e.code(),
            AppError::AccountRequired => "account.required",
            AppError::OfflineNotCached => "app.offline_not_cached",
            AppError::Offline => "app.offline",
        }
    }

    /// What a person reads (a toast, a dialog): a refusal says its reason
    /// without the "invalid request:" prefix the log keeps (`Display`).
    pub fn user_message(&self) -> String {
        match self {
            AppError::Invalid(m) => m.clone(),
            other => other.to_string(),
        }
    }

    /// The network (not the request, not the repo) is the problem: the
    /// git remote couldn't be reached.
    pub fn is_offline(&self) -> bool {
        matches!(
            self,
            AppError::Offline | AppError::Git(theodore_ports::GitError::Network(_))
        )
    }

    /// Whether this is the caller's mistake or an expected state (logged
    /// at info) rather than a failure worth a warning.
    pub fn is_expected(&self) -> bool {
        matches!(
            self,
            AppError::NotFound(_)
                | AppError::Invalid(_)
                | AppError::Refused(_)
                | AppError::AccountRequired
                | AppError::OfflineNotCached
                | AppError::Offline
                | AppError::Git(theodore_ports::GitError::Network(_))
                | AppError::Grouper(GrouperError::Offline(_))
                | AppError::Git(theodore_ports::GitError::Refused(_))
                | AppError::Git(theodore_ports::GitError::Busy(_))
                | AppError::Git(theodore_ports::GitError::Unsupported(_))
                | AppError::Git(theodore_ports::GitError::MissingCommit(_))
        )
    }
}

/// Errors from a `Grouper` run (plan §5.1).
#[derive(Debug, thiserror::Error)]
pub enum GrouperError {
    #[error("agent failed: {0}")]
    Agent(String),
    #[error("invalid output: {0}")]
    Invalid(String),
    /// The agent couldn't reach its servers (`ports::offline_message`).
    #[error("{0}")]
    Offline(String),
    /// The agent isn't signed in (`ports::signed_out_message`).
    #[error("{0}")]
    SignedOut(String),
}

impl GrouperError {
    pub fn code(&self) -> &'static str {
        match self {
            GrouperError::Agent(_) => "grouping.agent_failed",
            GrouperError::Invalid(_) => "grouping.invalid_output",
            GrouperError::Offline(_) => theodore_ports::AGENT_OFFLINE_CODE,
            GrouperError::SignedOut(_) => theodore_ports::AGENT_SIGNED_OUT_CODE,
        }
    }
}

pub type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;
    use theodore_ports::{AccountError, GitError, HostError, StoreError};

    #[test]
    fn every_app_error_maps_to_a_stable_dotted_code() {
        let cases: Vec<(AppError, &str)> = vec![
            (StoreError::Db("x".into()).into(), "store.db"),
            (StoreError::Migration("x".into()).into(), "store.migration"),
            (
                GitError::Command {
                    args: "fetch".into(),
                    code: 128,
                    stderr: String::new(),
                }
                .into(),
                "git.command_failed",
            ),
            (GitError::Refused("dirty".into()).into(), "git.refused"),
            (GitError::NotInstalled.into(), "git.not_installed"),
            (HostError::Auth.into(), "host.auth"),
            (
                HostError::RateLimited { reset_at: 0 }.into(),
                "host.rate_limited",
            ),
            (HostError::NotFound(String::new()).into(), "host.not_found"),
            (HostError::Network("dns".into()).into(), "host.network"),
            (HostError::Server("502".into()).into(), "host.server_error"),
            (HostError::Protocol("x".into()).into(), "host.protocol"),
            (
                GrouperError::Invalid("bad json".into()).into(),
                "grouping.invalid_output",
            ),
            (
                GrouperError::Agent("crashed".into()).into(),
                "grouping.agent_failed",
            ),
            (AppError::NotFound("pr".into()), "app.not_found"),
            (AppError::Invalid("x".into()), "app.invalid"),
            (AppError::Join("x".into()), "app.internal"),
            (
                AccountError::Rejected("expired".into()).into(),
                "account.rejected",
            ),
            (AccountError::Network("x".into()).into(), "account.network"),
            (AppError::AccountRequired, "account.required"),
            (AppError::OfflineNotCached, "app.offline_not_cached"),
            (AppError::Offline, "app.offline"),
            (GitError::Network("x".into()).into(), "git.network"),
            (
                GrouperError::Offline("Couldn't reach Claude's servers".into()).into(),
                "agent.offline",
            ),
        ];
        for (err, code) in cases {
            assert_eq!(err.code(), code, "{err}");
            let mut parts = code.split('.');
            assert!(parts.next().is_some_and(|p| !p.is_empty()));
            assert!(parts.next().is_some_and(|p| !p.is_empty()));
        }
    }

    #[test]
    fn a_refusal_reads_without_the_invalid_request_prefix() {
        let e = AppError::Invalid("conflicts left in notes.md; abort the update".into());
        assert_eq!(
            e.user_message(),
            "conflicts left in notes.md; abort the update"
        );
        assert!(
            e.to_string().starts_with("invalid request: "),
            "logs keep it"
        );
        assert_eq!(
            AppError::NotFound("pr".into()).user_message(),
            "not found: pr"
        );
    }
}
