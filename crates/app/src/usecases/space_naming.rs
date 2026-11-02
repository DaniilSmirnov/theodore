//! A space's instant name (`spaces::slug`) is a slug of the goal's first
//! five words — fine as a placeholder, not as what the space is called
//! for its whole life. This module replaces it, in the background, with a
//! short name and a chat title the chat agent proposes from the goal
//! (`one_shot`), without ever delaying turn 1 (docs/spaces.md §3 "New
//! space").
//!
//! It also fixes the titling GitHub issue #78 describes: a chat titled by
//! its raw first line reads as a run-on sentence ("For LIN-145, I dont
//! agree with the proposed solution…"). `Space::title` is shown instead
//! (`ui/src/ui/space/spaces.ts`'s `defaultTitle`), falling back to that
//! first line until it lands or if it never does.

use std::path::{Path, PathBuf};

use theodore_contract::{Project, SpaceStatus, models};
use theodore_domain::AgentId;
use theodore_ports::{NewAction, SyncActionCode};

use crate::core::Core;
use crate::usecases::one_shot;
use crate::usecases::spaces::{slug, unique_name};

/// Marks a naming/title call for a test's `push_tagged`, distinct from any
/// other one-shot utility call (`space_handoff`'s own system prompt).
pub(crate) const IDENTITY_SYSTEM: &str = "You name short-lived work in Theodore, a code review tool, from \
     what someone asked an agent to change. Answer with exactly two lines and nothing else:\n\
     NAME: 2 to 4 lowercase words for the change, separated by hyphens, no other punctuation.\n\
     TITLE: a title for this chat, 6 words or fewer, in sentence case. If the request names an \
     issue or ticket key (like LIN-145 or #123), put it first.";

struct Identity {
    name: String,
    title: String,
}

/// After the goal is sent as turn 1: the chat agent's cheapest model
/// proposes a name and a title. The title is stored whenever a valid
/// answer lands — it has no dialog field to override and no branch to
/// stay consistent with. The name (and the branch that follows it) is
/// applied only where nothing already claimed it — the dialog's own name
/// or branch, or a ship — and only after deduping it against its
/// siblings. Any failure (offline, no CLI, an answer that doesn't parse,
/// an archived space) leaves the slug name and the goal's first line
/// title in place, silently.
pub(crate) async fn apply(
    core: &Core,
    space_id: String,
    agent: AgentId,
    goal: String,
    cwd: PathBuf,
    user_named: bool,
    user_branched: bool,
) {
    let Some(identity) = propose(core, agent, &goal, &cwd).await else {
        return;
    };
    let Ok(space) = core.space_row(&space_id).await else {
        return;
    };
    if space.status == SpaceStatus::Archived {
        return;
    }
    let mut patch = serde_json::json!({ "title": identity.title });
    if !user_named && space.shipped.is_none() {
        let siblings = core
            .list_spaces(Some(space.project.clone()))
            .await
            .unwrap_or_default();
        let taken: Vec<&str> = siblings
            .iter()
            .filter(|s| s.id != space.id)
            .map(|s| s.name.as_str())
            .collect();
        let name = unique_name(&identity.name, &taken);
        if !user_branched && let Ok(project) = core.space_project(&space).await {
            patch["branch"] = serde_json::json!(branch_for(core, &project, &name).await);
        }
        patch["name"] = serde_json::json!(name);
    }
    let _ = core
        .apply(vec![NewAction {
            model: models::SPACE.into(),
            model_id: space.id.clone(),
            action: SyncActionCode::C,
            data: Some(patch),
        }])
        .await;
}

/// `<host login>/<slug>`, or `space/<slug>` for a local folder or a host
/// we can't reach — the same rule `create_space` derives its own branch
/// with, reused here so a later rename stays consistent with it.
pub(crate) async fn branch_for(core: &Core, project: &Project, name: &str) -> String {
    let login = match project.path {
        Some(_) => None,
        None => core
            .0
            .deps
            .host
            .viewer_for(project.repo.host)
            .await
            .ok()
            .map(|p| p.login),
    };
    crate::usecases::ship::branch_for(login.as_deref(), &slug(name))
}

async fn propose(core: &Core, agent: AgentId, goal: &str, cwd: &Path) -> Option<Identity> {
    let answer = one_shot::run(core, agent, IDENTITY_SYSTEM, goal, cwd).await?;
    parse(&answer)
}

fn parse(answer: &str) -> Option<Identity> {
    let mut name = None;
    let mut title = None;
    for line in answer.lines() {
        if let Some(rest) = label(line, "NAME") {
            name = sanitize_name(rest);
        } else if let Some(rest) = label(line, "TITLE") {
            title = sanitize_title(rest);
        }
    }
    Some(Identity {
        name: name?,
        title: title?,
    })
}

/// `line` as `"<key>: <rest>"`, case-insensitively; `None` if it isn't.
fn label<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let (head, rest) = line.split_once(':')?;
    head.trim().eq_ignore_ascii_case(key).then(|| rest.trim())
}

/// `slug` already lowercases, hyphenates and transliterates; a usable
/// name is 2 to 4 of its words — anything shorter or longer (the goal had
/// nothing to say, or the agent rambled past the instructions) is
/// rejected rather than guessed at.
fn sanitize_name(raw: &str) -> Option<String> {
    let s = slug(raw);
    let words: Vec<&str> = s.split('-').filter(|w| !w.is_empty()).collect();
    if !(2..=4).contains(&words.len()) {
        return None;
    }
    Some(words.join("-"))
}

/// Trimmed, outer quotes dropped, cut to 6 words.
fn sanitize_title(raw: &str) -> Option<String> {
    let cleaned = raw
        .trim()
        .trim_matches(|c: char| matches!(c, '"' | '\'' | '`'));
    let words: Vec<&str> = cleaned.split_whitespace().take(6).collect();
    (!words.is_empty()).then(|| words.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_lines_case_insensitively() {
        let id = parse("name: retry export backoff\ntitle: LIN-145 - retry export backoff")
            .expect("parses");
        assert_eq!(id.name, "retry-export-backoff");
        assert_eq!(id.title, "LIN-145 - retry export backoff");
    }

    #[test]
    fn extra_chatter_around_the_two_lines_is_ignored() {
        let id =
            parse("Sure, here you go:\nNAME: retry export backoff\nTITLE: Retry export\nThanks!")
                .expect("parses");
        assert_eq!(id.name, "retry-export-backoff");
        assert_eq!(id.title, "Retry export");
    }

    #[test]
    fn a_name_outside_two_to_four_words_is_rejected() {
        assert!(parse("NAME: fix\nTITLE: Fix it").is_none());
        assert!(parse("NAME: one two three four five\nTITLE: Fix it").is_none());
    }

    #[test]
    fn a_missing_line_is_rejected() {
        assert!(parse("NAME: retry export backoff").is_none());
        assert!(parse("TITLE: Fix it").is_none());
        assert!(parse("nothing useful here").is_none());
    }

    #[test]
    fn a_title_is_cut_to_six_words_and_unquoted() {
        assert_eq!(
            sanitize_title("\"one two three four five six seven\"").unwrap(),
            "one two three four five six"
        );
    }

    #[test]
    fn an_empty_title_is_rejected() {
        assert_eq!(sanitize_title("   "), None);
    }
}
