//! "Fix with agent" (docs/spaces.md "Fix CI"): a space's PR has failing
//! checks, so one action fetches their logs and sends them to the space's
//! agent as its next turn, instead of copying logs over by hand.
//!
//! ```text
//! space.pr ─▶ fresh checks ─▶ failing ones ─▶ check_log (GitHub Actions)
//!                                                 │
//!                               save_attachment ◀─┴─▶ no log: name + url
//!                                     │
//!                                     ▼
//!                               space_send (one turn, attachments)
//! ```

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use theodore_contract::{
    SaveAttachmentRequest, SpaceCommandKind, SpaceFixCiRequest, SpaceSendRequest, SpaceStatus,
};
use theodore_domain::{Check, CheckStatus, PrRef};
use theodore_ports::HostError;

use crate::core::Core;
use crate::error::{AppError, AppResult};

impl Core {
    /// Fetches the space PR's failing checks' logs and sends them to its
    /// agent as one turn. Returns the message id, like `space_send`.
    pub async fn space_fix_ci(&self, req: SpaceFixCiRequest) -> AppResult<String> {
        let space = self.space_row(&req.space).await?;
        match space.status {
            SpaceStatus::Running => {
                return Err(AppError::Invalid(
                    "the agent is still working in this space".into(),
                ));
            }
            SpaceStatus::Archived => {
                return Err(AppError::Invalid("this space is archived".into()));
            }
            SpaceStatus::Idle | SpaceStatus::Failed => {}
        }
        let pr_id = space
            .pr
            .clone()
            .ok_or_else(|| AppError::Invalid("this space has no pull request yet".into()))?;
        let pr_ref = self
            .pr_ref(&pr_id)
            .await
            .ok_or_else(|| AppError::NotFound(format!("pr {pr_id}")))?;
        // Fresh checks when the host answers; the last synced copy
        // otherwise (offline, or nothing changed since the last poll).
        let detail = match self.refresh_pr(&pr_id, &pr_ref).await {
            Some(d) => d,
            None => self.load_pr_detail(&pr_id).await?,
        };
        let failing: Vec<Check> = detail
            .checks
            .into_iter()
            .filter(|c| c.status == CheckStatus::Fail)
            .collect();
        if failing.is_empty() {
            return Err(AppError::Invalid(
                "no failing checks on this pull request".into(),
            ));
        }

        let mut items = Vec::with_capacity(failing.len());
        let mut attachments = Vec::new();
        for check in &failing {
            let log = self.failed_check_log(&pr_ref, check).await;
            let log_attachment = match log {
                Some(log) => {
                    let att = self
                        .save_attachment(SaveAttachmentRequest {
                            name: format!("{}.log", check.name),
                            mime: "text/plain".into(),
                            data: STANDARD.encode(log.as_bytes()),
                        })
                        .await?;
                    attachments.push(att.id.clone());
                    Some(att.name)
                }
                None => None,
            };
            items.push(FailingCheck {
                name: check.name.clone(),
                url: check.url.clone(),
                log_attachment,
            });
        }

        let project = self.space_project(&space).await?;
        let cfg = self.space_config_of(&space, &project).await;
        let check_cmd = cfg.command(SpaceCommandKind::Check).map(|c| c.script);

        self.space_send(SpaceSendRequest {
            space: space.id,
            text: fix_ci_prompt(&items, check_cmd.as_deref()),
            agent: None,
            model: None,
            mode: None,
            attachments: Some(attachments),
        })
        .await
    }

    /// `check.log`, when the host has one to give — `None` on an
    /// unsupported host/check (an external status context, GitLab/Bitbucket
    /// today) and, best-effort, on any other failure: one check's log
    /// shouldn't block the whole turn from going out with the rest.
    async fn failed_check_log(&self, pr_ref: &PrRef, check: &Check) -> Option<String> {
        match self.0.deps.host.check_log(pr_ref, check).await {
            Ok(log) => log,
            Err(HostError::Unsupported(_)) => None,
            Err(e) => {
                tracing::warn!(
                    check = %check.name,
                    error = %e,
                    "couldn't fetch this check's log"
                );
                None
            }
        }
    }
}

/// One failing check, resolved to either an attached log or a bare link.
struct FailingCheck {
    name: String,
    url: Option<String>,
    log_attachment: Option<String>,
}

/// The turn's message: which checks failed, how to look at each, and what
/// to do about it.
fn fix_ci_prompt(checks: &[FailingCheck], check_cmd: Option<&str>) -> String {
    let mut out = String::from(if checks.len() == 1 {
        "This pull request's check is failing:\n"
    } else {
        "This pull request's checks are failing:\n"
    });
    for c in checks {
        out.push_str("\n- ");
        out.push_str(&c.name);
        match (&c.log_attachment, &c.url) {
            (Some(name), _) => out.push_str(&format!(" — its log is attached ({name})")),
            (None, Some(url)) => out.push_str(&format!(" — {url} (no log to fetch)")),
            (None, None) => out.push_str(" (no log to fetch)"),
        }
    }
    out.push('\n');
    if let Some(cmd) = check_cmd {
        out.push_str(&format!(
            "\nThe space's check command may reproduce this locally: `{cmd}`.\n"
        ));
    }
    out.push_str(
        "\nFix what's failing. For a check with no log attached, use its link or say what \
         you tried. End with a summary of what was wrong and what you changed.",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(name: &str, log_attachment: Option<&str>, url: Option<&str>) -> FailingCheck {
        FailingCheck {
            name: name.into(),
            url: url.map(str::to_string),
            log_attachment: log_attachment.map(str::to_string),
        }
    }

    #[test]
    fn one_failing_check_with_a_log_is_singular() {
        let text = fix_ci_prompt(&[check("build", Some("build.log"), None)], None);
        assert!(text.starts_with("This pull request's check is failing:\n"));
        assert!(text.contains("\n- build — its log is attached (build.log)\n"));
        assert!(text.ends_with("what you changed."));
    }

    #[test]
    fn several_checks_are_plural_and_each_resolved_its_own_way() {
        let text = fix_ci_prompt(
            &[
                check("build", Some("build.log"), None),
                check("deploy-preview", None, Some("https://example.com/x")),
                check("lint", None, None),
            ],
            Some("pnpm lint && pnpm test"),
        );
        assert!(text.starts_with("This pull request's checks are failing:\n"));
        assert!(text.contains("\n- build — its log is attached (build.log)\n"));
        assert!(text.contains("\n- deploy-preview — https://example.com/x (no log to fetch)\n"));
        assert!(text.contains("\n- lint (no log to fetch)\n"));
        assert!(text.contains("`pnpm lint && pnpm test`"));
    }

    #[test]
    fn no_check_command_means_no_reproduce_line() {
        let text = fix_ci_prompt(&[check("build", Some("build.log"), None)], None);
        assert!(!text.contains("reproduce"));
        assert!(!text.contains("check command"));
    }
}
