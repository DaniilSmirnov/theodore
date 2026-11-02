//! "Send to agent" (docs/spaces.md §3): a review thread, a draft comment or
//! a finding on a space's PR — or on the space's own branch review —
//! becomes the space's next turn. The turn's message quotes it with its
//! `path:line`, so the agent knows where to look and the transcript shows
//! what was asked.

use theodore_contract::{
    SentFeedback, SpaceFeedback, SpaceFeedbackRequest, SpaceSendRequest, models,
};
use theodore_domain::{Anchor, Draft, Finding, PrDetail, PrId, Severity, Side, Thread};
use theodore_ports::{NewAction, SyncActionCode};

use crate::core::Core;
use crate::error::{AppError, AppResult};
use crate::wire::ThreadRow;

/// One quoted item: a heading line and the comments under it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Quote {
    pub head: String,
    /// `(author, body)`; `None` for a body with no author to name.
    pub comments: Vec<(Option<String>, String)>,
}

impl Core {
    /// Sends `req.items` to the space's agent as one turn. Returns the
    /// message id, like `space_send`.
    pub async fn space_send_feedback(&self, req: SpaceFeedbackRequest) -> AppResult<String> {
        let space = self.space_row(&req.space).await?;
        if req.items.is_empty() {
            return Err(AppError::Invalid("nothing to send to the agent".into()));
        }
        let prs: Vec<PrId> = space
            .pr
            .iter()
            .chain(space.review_pr.iter())
            .cloned()
            .collect();
        let mut quotes = Vec::with_capacity(req.items.len());
        for item in &req.items {
            quotes.push(self.feedback_quote(&prs, item).await?);
        }
        // The turn that is about to start: `space_send` only bumps `turns`
        // once it ends, so this is the number it will carry.
        let turn = space.turns + 1;
        let message_id = self
            .space_send(SpaceSendRequest {
                space: space.id.clone(),
                text: feedback_prompt(&quotes),
                agent: None,
                model: None,
                mode: None,
                attachments: None,
            })
            .await?;
        let mut sent = space.sent_feedback.clone();
        sent.extend(
            req.items
                .iter()
                .cloned()
                .map(|item| SentFeedback { item, turn }),
        );
        self.apply(vec![NewAction {
            model: models::SPACE.into(),
            model_id: space.id,
            action: SyncActionCode::C,
            data: Some(serde_json::json!({ "sent_feedback": sent })),
        }])
        .await?;
        Ok(message_id)
    }

    async fn feedback_quote(&self, prs: &[PrId], item: &SpaceFeedback) -> AppResult<Quote> {
        let elsewhere =
            || AppError::Invalid("that comment is not on this space's pull request".into());
        match item {
            SpaceFeedback::Thread(id) => self
                .space_thread(prs, id)
                .await?
                .map(|t| thread_quote(&t))
                .ok_or_else(|| AppError::NotFound(format!("thread {id} on this space's PR"))),
            SpaceFeedback::Draft(id) => {
                let draft: Draft = self.parsed_row(models::DRAFT, id).await?;
                if !prs.contains(&draft.pr) {
                    return Err(elsewhere());
                }
                Ok(draft_quote(&draft))
            }
            SpaceFeedback::Finding(id) => {
                let finding: Finding = self.parsed_row(models::FINDING, id).await?;
                if !prs.contains(&finding.pr) {
                    return Err(elsewhere());
                }
                Ok(finding_quote(&finding))
            }
        }
    }

    /// A thread of one of `prs`: the host's (inside `pr_detail`), else one
    /// just posted and not fetched back yet (a `thread` row).
    async fn space_thread(&self, prs: &[PrId], id: &str) -> AppResult<Option<Thread>> {
        for pr in prs {
            let Some(data) = self.get_row(models::PR_DETAIL, &pr.0).await? else {
                continue;
            };
            let Ok(detail) = serde_json::from_value::<PrDetail>(data) else {
                continue;
            };
            if let Some(t) = detail.threads.into_iter().find(|t| t.id.0 == id) {
                return Ok(Some(t));
            }
        }
        let row = match self.get_row(models::THREAD, id).await? {
            Some(data) => serde_json::from_value::<ThreadRow>(data).ok(),
            None => None,
        };
        Ok(row.filter(|r| prs.contains(&r.pr)).map(|r| r.thread))
    }

    async fn parsed_row<T: serde::de::DeserializeOwned>(
        &self,
        model: &str,
        id: &str,
    ) -> AppResult<T> {
        let data = self
            .get_row(model, id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("{model} {id}")))?;
        serde_json::from_value(data).map_err(|e| AppError::Invalid(e.to_string()))
    }
}

/// `src/a.ts:41`, `src/a.ts:10-12`, with "(old side)" for removed lines.
pub(crate) fn location(a: &Anchor) -> String {
    let lines = match a.start_line {
        Some(start) if start != a.line => format!("{start}-{}", a.line),
        _ => a.line.to_string(),
    };
    let side = match a.start_side.unwrap_or(a.side) {
        Side::Old => " (old side, removed lines)",
        Side::New => "",
    };
    format!("{}:{lines}{side}", a.path)
}

fn thread_quote(t: &Thread) -> Quote {
    let mut head = match &t.anchor {
        Some(a) => format!("Review thread on {}", location(a)),
        None => "Review thread on the pull request".to_string(),
    };
    if t.outdated {
        head.push_str(", outdated: the code may have moved since");
    }
    if t.resolved {
        head.push_str(", resolved");
    }
    Quote {
        head,
        comments: t
            .comments
            .iter()
            .map(|c| (Some(c.author.login.clone()), c.body.clone()))
            .collect(),
    }
}

fn draft_quote(d: &Draft) -> Quote {
    let mut head = format!("Review comment on {}", location(&d.anchor));
    if d.outdated {
        head.push_str(", outdated: the code may have moved since");
    }
    Quote {
        head,
        comments: vec![(None, d.body.clone())],
    }
}

fn finding_quote(f: &Finding) -> Quote {
    let severity = match f.severity {
        Severity::High => "high",
        Severity::Warn => "warning",
        Severity::Info => "info",
    };
    let mut head = format!(
        "Finding ({severity}) on {}: {}",
        location(&f.anchor),
        f.title.trim()
    );
    if f.outdated {
        head.push_str(", outdated: the code may have moved since");
    }
    Quote {
        head,
        comments: vec![(None, f.body.clone())],
    }
}

/// The turn's message: each item's heading, then its comments as quotes.
pub(crate) fn feedback_prompt(quotes: &[Quote]) -> String {
    let mut out = String::from(if quotes.len() == 1 {
        "Address this review feedback on the change.\n"
    } else {
        "Address this review feedback on the change, item by item.\n"
    });
    for (i, q) in quotes.iter().enumerate() {
        out.push('\n');
        if quotes.len() > 1 {
            out.push_str(&format!("{}. ", i + 1));
        }
        out.push_str(&q.head);
        out.push_str(":\n");
        for (author, body) in &q.comments {
            if let Some(author) = author {
                out.push_str(&format!("{author} wrote:\n"));
            }
            let body = strip_html_comments(body);
            let body = body.trim();
            for line in body.lines() {
                out.push_str(if line.is_empty() { ">" } else { "> " });
                out.push_str(line);
                out.push('\n');
            }
            if body.is_empty() {
                out.push_str(">\n");
            }
        }
    }
    out.push_str(
        "\nChange the code where the feedback applies. If you disagree with a point, \
         leave the code and say why. End with what you changed.",
    );
    out
}

/// Hosts and bots hide metadata in `<!-- … -->`; the agent needn't read it.
fn strip_html_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(end) => rest = &rest[start + end + 3..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use theodore_domain::Sha;

    fn anchor(path: &str, line: u32, start: Option<u32>, side: Side) -> Anchor {
        Anchor {
            rev: Sha::new("h"),
            path: path.into(),
            side,
            line,
            start_line: start,
            start_side: None,
        }
    }

    #[test]
    fn locations_name_the_file_and_lines() {
        assert_eq!(
            location(&anchor("src/a.ts", 41, None, Side::New)),
            "src/a.ts:41"
        );
        assert_eq!(
            location(&anchor("src/a.ts", 12, Some(10), Side::New)),
            "src/a.ts:10-12"
        );
        assert_eq!(
            location(&anchor("src/a.ts", 7, Some(7), Side::Old)),
            "src/a.ts:7 (old side, removed lines)"
        );
    }

    #[test]
    fn one_item_is_quoted_with_its_location() {
        let text = feedback_prompt(&[Quote {
            head: "Review comment on src/redirect.ts:10".into(),
            comments: vec![(None, "also handle //evil.com\n\nand `\\\\evil`".into())],
        }]);
        assert_eq!(
            text,
            "Address this review feedback on the change.\n\
             \n\
             Review comment on src/redirect.ts:10:\n\
             > also handle //evil.com\n\
             >\n\
             > and `\\\\evil`\n\
             \n\
             Change the code where the feedback applies. If you disagree with a point, \
             leave the code and say why. End with what you changed."
        );
    }

    #[test]
    fn several_items_are_numbered_and_threads_name_their_authors() {
        let text = feedback_prompt(&[
            Quote {
                head: "Review thread on src/limiter.ts:41".into(),
                comments: vec![
                    (Some("anna".into()), "Per-token, not global?".into()),
                    (Some("ada".into()), "<!-- bot:meta -->Agreed.".into()),
                ],
            },
            Quote {
                head: "Finding (high) on src/a.ts:3: SQL injection".into(),
                comments: vec![(None, "The query concatenates input.".into())],
            },
        ]);
        assert!(text.starts_with("Address this review feedback on the change, item by item.\n"));
        assert!(text.contains(
            "\n1. Review thread on src/limiter.ts:41:\nanna wrote:\n> Per-token, not global?\nada wrote:\n> Agreed.\n"
        ));
        assert!(text.contains(
            "\n2. Finding (high) on src/a.ts:3: SQL injection:\n> The query concatenates input.\n"
        ));
        assert!(!text.contains("bot:meta"));
    }

    #[test]
    fn html_comments_are_dropped_even_unclosed() {
        assert_eq!(strip_html_comments("a<!-- x -->b<!-- y -->c"), "abc");
        assert_eq!(strip_html_comments("keep<!-- cut"), "keep");
        assert_eq!(strip_html_comments("plain"), "plain");
    }
}
