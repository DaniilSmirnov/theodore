//! What a change does, for its commit and its PR (docs/spaces.md §6):
//! the title of the next round's PR after a Continue and of a Push's
//! commit. Never a sentence copied from the chat — an agent's reply is
//! first person and tells the story of the round, edits it undid
//! included — but an imperative summary of the net change:
//!
//! ```text
//! net diff (from ─▶ head) ─┬─ empty ─────────────────────────▶ Update <name>
//!                          └─ chat agent, cheapest model ─┬─ TITLE ok ─▶ title + whole-sentence bullets
//!   (diff stat + the replies of the turns that changed files) └─ else ─▶ Update notes/t3.md
//!                                                                         Update 3 files in notes/
//! ```

use std::path::Path;
use std::time::Duration;

use theodore_contract::{Project, Space, models};
use theodore_domain::{AgentId, Author, FileDiff, Sha};
use theodore_ports::DiffOptions;

use crate::core::Core;
use crate::usecases::one_shot;

/// Marks a ship-title call for a test's `push_tagged`, distinct from
/// `space_naming`'s and `space_handoff`'s own system prompts.
pub(crate) const SHIP_TITLE_SYSTEM: &str = "You write the title of a commit and pull request \
     in Theodore, a code review tool, from the change's diff stat and the notes of the agent \
     that made it. Describe only what the diff stat shows: the notes may tell of edits that \
     were undone later, and a file missing from the diff stat is not part of the change. \
     Answer in exactly this form and nothing else:\n\
     TITLE: an imperative summary of the whole change, under 60 characters, no closing period \
     (\"Add notes on ages\", never \"Added …\" or \"I added …\").\n\
     SUMMARY:\n\
     - up to 4 lines, each one whole sentence about a change in the diff stat; \
     no lines when the title says it all.";

/// A subject line's length (`ship::TITLE_CHARS`): a longer proposal is
/// refused, never cut.
const TITLE_CHARS: usize = 72;
/// A summary line longer than this is dropped, never cut.
const BULLET_CHARS: usize = 200;
const MAX_BULLETS: usize = 4;
/// Files listed in the prompt's diff stat.
const STAT_FILES: usize = 60;
/// The replies fed to the prompt: the latest ones, each truncated.
const REPLIES: usize = 8;
const REPLY_CHARS: usize = 1_500;
/// How long Open PR / Push wait for the proposal before the fallback.
const TIMEOUT: Duration = Duration::from_secs(30);

/// A change's title, and the whole sentences listed under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChangeSummary {
    pub title: String,
    pub bullets: Vec<String>,
}

impl ChangeSummary {
    /// The title, then `- <sentence>` per bullet: a commit message, or
    /// the goal a round's PR body is made from.
    pub(crate) fn message(&self) -> String {
        if self.bullets.is_empty() {
            return self.title.clone();
        }
        let list: Vec<String> = self.bullets.iter().map(|b| format!("- {b}")).collect();
        format!("{}\n\n{}", self.title, list.join("\n"))
    }
}

impl Core {
    /// What the space's change from `from` to `head` does: the chat
    /// agent's proposal from the net diff stat and the replies of the
    /// turns since `since` that changed files, else a title from the
    /// changed paths.
    pub(crate) async fn change_summary(
        &self,
        space: &Space,
        project: &Project,
        repo_path: &Path,
        from: &Sha,
        head: &Sha,
        since: i64,
    ) -> ChangeSummary {
        let files = self
            .0
            .deps
            .git
            .diff(repo_path, from, head, &DiffOptions::default())
            .await
            .map(|r| r.files)
            .unwrap_or_default();
        let fallback = ChangeSummary {
            title: fallback_title(&files, &space.name),
            bullets: Vec::new(),
        };
        if files.is_empty() {
            return fallback;
        }
        let replies = self.change_replies(space, since).await;
        let configured = self
            .setting_agent_scoped("agents.chat", Some(&project.repo.slug()), AgentId::Claude)
            .await;
        let agent = self.effective_agent(configured).await;
        let cwd = if Path::new(&space.worktree).exists() {
            Path::new(&space.worktree)
        } else {
            repo_path
        };
        let prompt = title_prompt(&files, &replies);
        let ask = one_shot::run(self, agent, SHIP_TITLE_SYSTEM, &prompt, cwd);
        match tokio::time::timeout(TIMEOUT, ask).await {
            Ok(Some(answer)) => parse(&answer, &files).unwrap_or(fallback),
            _ => fallback,
        }
    }

    /// The agent's replies, oldest first, to the live turns since `since`
    /// that changed files. Your own messages never go in.
    async fn change_replies(&self, space: &Space, since: i64) -> Vec<String> {
        let store = self.0.deps.store.clone();
        let key = crate::usecases::spaces::space_chat_key(&space.id).0;
        let rows =
            tokio::task::spawn_blocking(move || store.query(models::CHAT_MESSAGE, "pr", &key))
                .await
                .ok()
                .and_then(|r| r.ok())
                .unwrap_or_default();
        let replies: std::collections::HashMap<String, theodore_contract::ChatMessage> = rows
            .into_iter()
            .filter_map(|r| serde_json::from_value::<theodore_contract::ChatMessage>(r.data).ok())
            .filter(|m| m.author != Author::Me && m.at >= since)
            .map(|m| (m.id.clone(), m))
            .collect();
        let mut turns: Vec<&theodore_contract::ChatMessage> = space
            .turn_log
            .iter()
            .filter(|t| {
                !t.undone && t.reverted.is_empty() && t.base_before.is_none() && t.before != t.after
            })
            .filter_map(|t| replies.get(&t.reply))
            .collect();
        turns.sort_by_key(|m| m.at);
        let skip = turns.len().saturating_sub(REPLIES);
        turns
            .iter()
            .skip(skip)
            .map(|m| m.text.chars().take(REPLY_CHARS).collect())
            .collect()
    }
}

/// The change as data to title. On its own the replies read as a task.
fn title_prompt(files: &[FileDiff], replies: &[String]) -> String {
    let mut stat: Vec<String> = files
        .iter()
        .take(STAT_FILES)
        .map(|f| format!("- {} (+{} −{})", f.path, f.additions, f.deletions))
        .collect();
    if files.len() > STAT_FILES {
        stat.push(format!("- … and {} more", files.len() - STAT_FILES));
    }
    let notes = if replies.is_empty() {
        "(none)".to_string()
    } else {
        replies.join("\n\n---\n\n")
    };
    format!(
        "Don't change anything and don't use any tools. Only title this change, \
         as TITLE: and SUMMARY:.\n\n<diff-stat>\n{}\n</diff-stat>\n\n<agent-notes>\n{notes}\n</agent-notes>",
        stat.join("\n")
    )
}

/// The proposal, when it holds up: a `TITLE:` that is one imperative line
/// short enough for a subject (refused, never cut), and the `- ` lines
/// under `SUMMARY:` kept whole or dropped. Either one naming a path the
/// net diff doesn't have describes something the round undid: the title
/// is refused, the line dropped.
fn parse(answer: &str, files: &[FileDiff]) -> Option<ChangeSummary> {
    let mut title = None;
    let mut bullets = Vec::new();
    for line in answer.lines().map(str::trim) {
        if let Some(rest) = label(line, "TITLE") {
            title = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            let b = rest.trim();
            if !b.is_empty()
                && b.chars().count() <= BULLET_CHARS
                && !first_person(b)
                && names_only(b, files)
                && bullets.len() < MAX_BULLETS
            {
                bullets.push(b.to_string());
            }
        }
    }
    let title = title?
        .trim_matches(|c: char| matches!(c, '"' | '\'' | '`') || c.is_whitespace())
        .trim_end_matches('.')
        .trim()
        .to_string();
    if title.is_empty()
        || title.chars().count() > TITLE_CHARS
        || first_person(&title)
        || !names_only(&title, files)
    {
        return None;
    }
    // A lone bullet that only repeats the title says nothing more.
    bullets.retain(|b| b.trim_end_matches('.') != title);
    Some(ChangeSummary { title, bullets })
}

/// `line` as `"<key>: <rest>"`, case-insensitively.
fn label<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let (head, rest) = line.split_once(':')?;
    head.trim().eq_ignore_ascii_case(key).then(|| rest.trim())
}

/// A chat sentence, not a summary: "I created …", "We now …".
fn first_person(text: &str) -> bool {
    let first = text
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
        .to_ascii_lowercase();
    matches!(
        first.as_str(),
        "i" | "i'm" | "i've" | "i'll" | "we" | "we've" | "we're" | "i’ve"
    )
}

/// Every path-like word in `text` (it has a `/`) is in the net diff:
/// a changed file, a suffix of one, or a folder holding one.
fn names_only(text: &str, files: &[FileDiff]) -> bool {
    text.split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| matches!(c, '`' | '"' | '\'' | '(' | ')' | ',' | ';' | ':'))
                .trim_end_matches(['.', '!', '?'])
        })
        .filter(|w| w.contains('/') && !w.contains("://") && w.len() > 1)
        .all(|w| {
            let dir = w.trim_end_matches('/');
            files.iter().any(|f| {
                f.path == w
                    || f.path.ends_with(&format!("/{w}"))
                    || f.path.starts_with(&format!("{dir}/"))
            })
        })
}

/// A title from the changed paths alone: `Update notes/t3.md`,
/// `Update 3 files in notes/`, `Update 3 files`; `Update <name>` when
/// nothing changed.
pub(crate) fn fallback_title(files: &[FileDiff], name: &str) -> String {
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    match paths.as_slice() {
        [] => format!("Update {name}"),
        [one] => {
            let t = format!("Update {one}");
            if t.chars().count() <= TITLE_CHARS {
                t
            } else {
                format!("Update {}", one.rsplit('/').next().unwrap_or(one))
            }
        }
        many => {
            let n = many.len();
            match common_dir(many) {
                Some(dir) if dir.chars().count() + 24 <= TITLE_CHARS => {
                    format!("Update {n} files in {dir}/")
                }
                _ => format!("Update {n} files"),
            }
        }
    }
}

/// The deepest folder every path is in; `None` at the repo's root.
fn common_dir(paths: &[&str]) -> Option<String> {
    let dirs: Vec<Vec<&str>> = paths
        .iter()
        .map(|p| {
            let mut parts: Vec<&str> = p.split('/').collect();
            parts.pop();
            parts
        })
        .collect();
    let first = dirs.first()?;
    let depth = (0..first.len())
        .take_while(|&i| dirs.iter().all(|d| d.get(i) == first.get(i)))
        .count();
    (depth > 0).then(|| first[..depth].join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use theodore_domain::FileStatus;

    fn file(path: &str) -> FileDiff {
        FileDiff {
            path: path.into(),
            old_path: None,
            status: FileStatus::Modified,
            similarity: None,
            old_mode: None,
            new_mode: None,
            flags: vec![],
            additions: 3,
            deletions: 0,
            hunks: vec![],
        }
    }

    #[test]
    fn the_fallback_names_the_changed_paths() {
        assert_eq!(fallback_title(&[], "tama"), "Update tama");
        assert_eq!(
            fallback_title(&[file("notes/t3.md")], "tama"),
            "Update notes/t3.md"
        );
        assert_eq!(
            fallback_title(
                &[
                    file("notes/a.md"),
                    file("notes/b.md"),
                    file("notes/old/c.md")
                ],
                "tama"
            ),
            "Update 3 files in notes/"
        );
        assert_eq!(
            fallback_title(&[file("README.md"), file("src/lib.rs")], "tama"),
            "Update 2 files"
        );
        let deep = format!("{}/file.rs", "very-long-folder-name".repeat(4));
        assert_eq!(fallback_title(&[file(&deep)], "tama"), "Update file.rs");
    }

    #[test]
    fn a_proposal_is_a_title_and_whole_sentences() {
        let files = [file("notes/t3.md")];
        let s = parse(
            "TITLE: Add t3 notes.\nSUMMARY:\n- Add notes/t3.md with the round's notes.\n",
            &files,
        )
        .expect("parses");
        assert_eq!(s.title, "Add t3 notes");
        assert_eq!(s.bullets, ["Add notes/t3.md with the round's notes."]);
        assert_eq!(
            s.message(),
            "Add t3 notes\n\n- Add notes/t3.md with the round's notes."
        );
    }

    #[test]
    fn a_chat_sentence_or_a_long_title_is_refused_not_cut() {
        let files = [file("notes/t3.md")];
        assert!(parse("TITLE: I created notes/t3.md", &files).is_none());
        assert!(parse(&format!("TITLE: Add {}", "a".repeat(80)), &files).is_none());
        assert!(parse("ok", &files).is_none());
        assert!(parse("TITLE:   ", &files).is_none());
    }

    #[test]
    fn what_the_round_undid_is_not_described() {
        let files = [file("notes/t3.md")];
        assert!(parse("TITLE: Add a note to .github/workflows/test.yml", &files).is_none());
        let s = parse(
            "TITLE: Add t3 notes\nSUMMARY:\n- Restore .github/workflows/test.yml.\n\
             - I added the notes.\n- Add notes/ for the round.",
            &files,
        )
        .unwrap();
        assert_eq!(s.bullets, ["Add notes/ for the round."]);
    }

    #[test]
    fn the_prompt_has_the_diff_stat_and_the_replies() {
        let p = title_prompt(&[file("notes/t3.md")], &["Created the note.".into()]);
        assert!(p.contains("- notes/t3.md (+3 −0)"), "{p}");
        assert!(p.contains("Created the note."), "{p}");
        assert!(p.contains("Don't change anything"), "{p}");
    }
}
