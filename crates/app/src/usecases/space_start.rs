//! More ways to start a space than a fresh branch off the default
//! (docs/spaces.md §3 "New space"): from a host issue, an existing remote
//! branch, or an existing PR. `create_space` (`usecases/spaces.rs`) calls
//! `resolve_space_start` once, before it decides the space's `base`,
//! `branch` and worktree start — everything else about creating a space is
//! unchanged.

use theodore_contract::{
    CreateSpaceRequest, Project, ProjectIssuesRequest, SpaceIssue, SpaceStart,
};
use theodore_domain::{PrDetail, PrRef, Sha};
use theodore_ports::{Fetched, TrackedIssue};

use crate::core::Core;
use crate::error::{AppError, AppResult};

/// An existing branch or PR a space adopts instead of forking a fresh
/// `<login>/<slug>` off the default branch.
pub(crate) struct Adopted {
    /// The branch the worktree starts on, and that shipping pushes to.
    pub branch: String,
    /// Set when the branch already has an open PR — the space opens "In
    /// review" right away instead of waiting for Open PR. `detail` is
    /// already fetched (`start_from: Pr`) or not yet (found for a plain
    /// `Branch` via `find_open_pr`, which answers a bare `PrRef`); either
    /// way `create_space` stores what it has and refreshes the rest.
    pub pr: Option<(PrRef, Option<PrDetail>)>,
    /// The branch's tip when the space forked: shipping fast-forwards onto
    /// it (`ship.rs::push_squashed`), never squashing onto `base_sha`.
    pub fork_sha: Sha,
}

#[derive(Default)]
pub(crate) struct StartResolution {
    /// Set from `SpaceStart::Issue`: the project's issue number, kept so
    /// `ship_space`'s PR body can say "Closes #<n>".
    pub issue: Option<u64>,
    pub adopt: Option<Adopted>,
    /// What an adopted branch or PR is called until the user says more: the
    /// PR's "#<n> <title>", or the branch's own name. A space started this
    /// way often has no goal to title it by.
    pub title: Option<String>,
}

impl Core {
    /// Reads `req.start_from` (taking it: it's spent once) and acts on it —
    /// an issue rewrites `req.goal`, a branch or PR resolves the commit the
    /// worktree should start from. `Ok(StartResolution::default())` for a
    /// plain `None` (the common case: a fresh branch from the goal).
    pub(crate) async fn resolve_space_start(
        &self,
        project: &Project,
        req: &mut CreateSpaceRequest,
    ) -> AppResult<StartResolution> {
        let Some(start) = req.start_from.take() else {
            return Ok(StartResolution::default());
        };
        match start {
            SpaceStart::Issue(raw) => {
                let number = parse_issue_number(&raw).ok_or_else(|| {
                    AppError::Invalid(format!("not an issue url or number: {raw:?}"))
                })?;
                let issue = self.0.deps.host.get_issue(&project.repo, number).await?;
                // A typed note goes on top of the issue, it doesn't replace it.
                req.goal = match req.goal.trim() {
                    "" => issue_goal(&issue),
                    note => format!("{note}\n\n{}", issue_goal(&issue)),
                };
                Ok(StartResolution {
                    issue: Some(number),
                    adopt: None,
                    title: None,
                })
            }
            SpaceStart::Branch(branch) => {
                let repo_path = self.ensure_project_cache(project).await?;
                let fork_sha = self
                    .0
                    .deps
                    .git
                    .resolve_ref(&repo_path, &branch)
                    .await
                    .map_err(|_| AppError::Invalid(format!("no branch {branch:?} on the host")))?;
                // Best-effort: a host that can't tell, or that has no open
                // PR for it yet, just starts without one.
                let pr = self
                    .0
                    .deps
                    .host
                    .find_open_pr(&project.repo, &branch)
                    .await
                    .ok()
                    .flatten()
                    .map(|r| (r, None));
                Ok(StartResolution {
                    issue: None,
                    title: Some(branch.clone()),
                    adopt: Some(Adopted {
                        branch,
                        pr,
                        fork_sha,
                    }),
                })
            }
            SpaceStart::Pr(number) => {
                let pr_ref = PrRef {
                    repo: project.repo.clone(),
                    number,
                    host_id: None,
                };
                let detail = match self.0.deps.host.get_pr(&pr_ref, None).await? {
                    Fetched::Modified { value, .. } => value,
                    Fetched::NotModified => {
                        return Err(AppError::Invalid(format!("PR #{number} not found")));
                    }
                };
                // The diff and Update go against what the PR targets.
                req.base
                    .get_or_insert_with(|| detail.summary.base_ref.clone());
                Ok(StartResolution {
                    issue: None,
                    title: Some(pr_title(number, &detail.summary.title)),
                    adopt: Some(Adopted {
                        branch: detail.summary.head_ref.clone(),
                        fork_sha: detail.summary.head_sha.clone(),
                        pr: Some((detail.summary.pr.clone(), Some(detail))),
                    }),
                })
            }
        }
    }

    /// New space's "From an issue" picker: the project's open issues,
    /// the viewer's own first.
    pub async fn project_issues(&self, req: ProjectIssuesRequest) -> AppResult<Vec<SpaceIssue>> {
        let project = self
            .project_row(&req.project)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("project {}", req.project)))?;
        let issues = self.0.deps.host.open_issues(&project.repo).await?;
        Ok(issues.into_iter().map(tracked_to_wire).collect())
    }
}

fn tracked_to_wire(i: TrackedIssue) -> SpaceIssue {
    SpaceIssue {
        number: i.number,
        title: i.title,
        body: i.body,
        url: i.url,
    }
}

/// goal = issue title + body + a link, so the space's chat keeps where it
/// came from; the issue key stays in the title (`slug()` keeps the leading
/// `#<n>` as its first word, and so does the squashed commit's title later).
fn issue_goal(issue: &TrackedIssue) -> String {
    let mut out = format!("#{} {}", issue.number, issue.title.trim());
    if !issue.body.trim().is_empty() {
        out.push_str("\n\n");
        out.push_str(issue.body.trim());
    }
    out.push_str("\n\n");
    out.push_str(&issue.url);
    out
}

/// "#118 Fix the login redirect": a PR's number and title in one line.
fn pr_title(number: u64, title: &str) -> String {
    match title.trim() {
        "" => format!("#{number}"),
        t => format!("#{number} {t}"),
    }
}

/// The issue number out of a pasted URL (`.../issues/123`, any host's
/// shape) or a bare `#123` / `123`. `None` when nothing numeric is found.
pub(crate) fn parse_issue_number(input: &str) -> Option<u64> {
    let s = input.trim();
    if s.starts_with("http://") || s.starts_with("https://") {
        let path = s.split(['?', '#']).next().unwrap_or(s);
        return path
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .and_then(|seg| seg.parse().ok());
    }
    s.trim_start_matches('#').parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_numbers_come_from_a_url_or_a_bare_number() {
        assert_eq!(
            parse_issue_number("https://github.com/acme/widgets/issues/123"),
            Some(123)
        );
        assert_eq!(
            parse_issue_number("https://gitlab.com/acme/widgets/-/issues/42/"),
            Some(42)
        );
        assert_eq!(parse_issue_number("#123"), Some(123));
        assert_eq!(parse_issue_number("  123 "), Some(123));
        assert_eq!(parse_issue_number("not an issue"), None);
        assert_eq!(parse_issue_number(""), None);
    }

    #[test]
    fn the_goal_keeps_the_issue_key_title_body_and_a_link() {
        let issue = TrackedIssue {
            number: 123,
            title: "Fix the login redirect".into(),
            body: "It sends people to /undefined.".into(),
            url: "https://github.com/acme/widgets/issues/123".into(),
            closed: false,
        };
        assert_eq!(
            issue_goal(&issue),
            "#123 Fix the login redirect\n\nIt sends people to /undefined.\n\n\
             https://github.com/acme/widgets/issues/123"
        );
    }

    #[test]
    fn a_pr_is_titled_by_its_number_and_title() {
        assert_eq!(pr_title(118, " Fix login "), "#118 Fix login");
        assert_eq!(pr_title(118, "  "), "#118");
    }

    #[test]
    fn a_bodyless_issue_skips_the_blank_paragraph() {
        let issue = TrackedIssue {
            number: 7,
            title: "Typo".into(),
            body: String::new(),
            url: "https://github.com/acme/widgets/issues/7".into(),
            closed: false,
        };
        assert_eq!(
            issue_goal(&issue),
            "#7 Typo\n\nhttps://github.com/acme/widgets/issues/7"
        );
    }
}
