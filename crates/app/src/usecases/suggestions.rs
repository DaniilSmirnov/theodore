//! `commit_suggestions` (plan §4 "Fix target", §6.5): applies the
//! ```` ```suggestion ```` blocks of the given review comments to the PR
//! worktree and pushes them as one commit. Own or local PR only — anyone
//! else's suggestions go through `decide_task`'s draft path instead.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use theodore_contract::CommitSuggestionsRequest;
use theodore_domain::{Anchor, CommentId, HostKind, PrDetail};

use crate::core::Core;
use crate::error::{AppError, AppResult};

struct SuggestionEdit {
    anchor: Anchor,
    replacement: String,
}

impl Core {
    pub async fn commit_suggestions(&self, req: CommitSuggestionsRequest) -> AppResult<()> {
        self.ensure_open(&req.pr).await?;
        let pr_ref = self
            .pr_ref(&req.pr)
            .await
            .ok_or_else(|| AppError::NotFound(format!("pr {}", req.pr)))?;
        let detail = self.load_pr_detail(&req.pr).await?;

        let own_or_local = pr_ref.repo.host == HostKind::Local || {
            match self.0.deps.host.viewer().await {
                Ok(me) => detail.summary.author.login == me.login,
                Err(_) => false,
            }
        };
        if !own_or_local {
            return Err(AppError::Invalid(
                "suggestions can only be committed on your own PR or a local branch review".into(),
            ));
        }

        let worktree = self
            .worktree_of(&req.pr)
            .await
            .ok_or_else(|| AppError::Invalid("pr is not open".into()))?;

        let mut edits = Vec::with_capacity(req.comments.len());
        for comment_id in &req.comments {
            let (thread, comment) = find_comment(&detail, comment_id)
                .ok_or_else(|| AppError::NotFound(format!("comment {comment_id} in this pr")))?;
            let anchor = thread.anchor.clone().ok_or_else(|| {
                AppError::Invalid(format!("comment {comment_id} isn't anchored to a line"))
            })?;
            let blocks = parse_suggestion_blocks(&comment.body);
            if blocks.is_empty() {
                return Err(AppError::Invalid(format!(
                    "comment {comment_id} has no suggestion block"
                )));
            }
            // GitHub/GitLab suggestions carry exactly one block per comment;
            // concatenating any extras keeps this forward-compatible without
            // silently dropping content.
            edits.push(SuggestionEdit {
                anchor,
                replacement: blocks.join("\n"),
            });
        }

        check_no_overlap(&edits)?;
        apply_suggestions(&worktree, &edits).await?;

        let branch = detail.summary.head_ref.clone();
        self.0
            .deps
            .git
            .commit_and_push(&worktree, &pr_ref.repo, &branch, &req.message)
            .await?;
        Ok(())
    }
}

fn find_comment<'a>(
    detail: &'a PrDetail,
    id: &CommentId,
) -> Option<(&'a theodore_domain::Thread, &'a theodore_domain::Comment)> {
    for thread in &detail.threads {
        if let Some(comment) = thread.comments.iter().find(|c| &c.id == id) {
            return Some((thread, comment));
        }
    }
    None
}

/// Extracts the body of every ```` ```suggestion ```` fenced block in a
/// comment, in order.
fn parse_suggestion_blocks(body: &str) -> Vec<String> {
    const MARKER: &str = "```suggestion";
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find(MARKER) {
        let after_marker = &rest[start + MARKER.len()..];
        let Some(nl) = after_marker.find('\n') else {
            break;
        };
        let content_start = &after_marker[nl + 1..];
        let Some(end) = content_start.find("```") else {
            break;
        };
        out.push(content_start[..end].trim_end_matches('\n').to_string());
        rest = &content_start[end + 3..];
    }
    out
}

/// Line range an edit touches on the new side (1-indexed, inclusive).
fn range_of(anchor: &Anchor) -> (u32, u32) {
    let start = anchor.start_line.unwrap_or(anchor.line);
    let end = anchor.line;
    (start.min(end), start.max(end))
}

/// Rejects a batch whose anchors overlap on the same file — applying both
/// would make one silently clobber the other.
fn check_no_overlap(edits: &[SuggestionEdit]) -> AppResult<()> {
    let mut ranges: Vec<(&str, u32, u32)> = edits
        .iter()
        .map(|e| {
            let (start, end) = range_of(&e.anchor);
            (e.anchor.path.as_str(), start, end)
        })
        .collect();
    ranges.sort_by(|a, b| a.0.cmp(b.0).then(a.1.cmp(&b.1)));
    for pair in ranges.windows(2) {
        let (path_a, _start_a, end_a) = pair[0];
        let (path_b, start_b, _end_b) = pair[1];
        if path_a == path_b && start_b <= end_a {
            return Err(AppError::Invalid(format!(
                "overlapping suggestions on {path_a}"
            )));
        }
    }
    Ok(())
}

/// Resolves `path` (host-sourced: a review thread's `Anchor.path`, not an
/// IPC string a client controls directly, but still not trusted) inside
/// `worktree`, rejecting anything that could land outside it:
///
/// - an absolute path, or one with a `..` component — checked on the path
///   text itself, before touching the filesystem;
/// - a symlink inside the worktree that resolves outside it, which
///   neither of those checks catches — caught by canonicalizing both
///   sides and requiring the result to stay under the worktree.
pub(crate) async fn safe_join(worktree: &Path, path: &str) -> AppResult<PathBuf> {
    let rel = Path::new(path);
    if rel.is_absolute() {
        return Err(AppError::Invalid(format!("path must be relative: {path}")));
    }
    if rel
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(AppError::Invalid(format!(
            "path must not contain ..: {path}"
        )));
    }
    let canon_worktree = tokio::fs::canonicalize(worktree)
        .await
        .map_err(|e| AppError::Invalid(format!("worktree: {e}")))?;
    let canon_joined = tokio::fs::canonicalize(worktree.join(rel))
        .await
        .map_err(|e| AppError::Invalid(format!("resolving {path}: {e}")))?;
    if !canon_joined.starts_with(&canon_worktree) {
        return Err(AppError::Invalid(format!(
            "path escapes the worktree: {path}"
        )));
    }
    Ok(canon_joined)
}

/// Applies every edit to the files under `worktree`, replacing the fewest
/// lines possible per edit (within one file, bottom-to-top so earlier
/// edits' line numbers stay valid).
async fn apply_suggestions(worktree: &Path, edits: &[SuggestionEdit]) -> AppResult<()> {
    let mut by_path: HashMap<&str, Vec<&SuggestionEdit>> = HashMap::new();
    for e in edits {
        by_path.entry(e.anchor.path.as_str()).or_default().push(e);
    }
    for (path, mut es) in by_path {
        es.sort_by_key(|e| std::cmp::Reverse(range_of(&e.anchor).0));
        let full_path = safe_join(worktree, path).await?;
        let content = tokio::fs::read_to_string(&full_path)
            .await
            .map_err(|e| AppError::Invalid(format!("reading {path}: {e}")))?;
        let had_trailing_newline = content.ends_with('\n');
        let mut lines: Vec<String> = content.lines().map(str::to_string).collect();
        for e in es {
            let (start, end) = range_of(&e.anchor);
            let (start, end) = (start as usize, end as usize);
            if start == 0 || end == 0 || end > lines.len() || start > end {
                return Err(AppError::Invalid(format!(
                    "suggestion range out of bounds in {path}"
                )));
            }
            let replacement: Vec<String> = if e.replacement.is_empty() {
                Vec::new()
            } else {
                e.replacement.split('\n').map(str::to_string).collect()
            };
            lines.splice((start - 1)..end, replacement);
        }
        let mut new_content = lines.join("\n");
        if had_trailing_newline && !new_content.is_empty() {
            new_content.push('\n');
        }
        tokio::fs::write(&full_path, new_content)
            .await
            .map_err(|e| AppError::Invalid(format!("writing {path}: {e}")))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use theodore_domain::{Comment, Person, Side, Thread, ThreadId};

    fn anchor(path: &str, line: u32, start_line: Option<u32>) -> Anchor {
        Anchor {
            rev: theodore_domain::Sha::new("head"),
            path: path.into(),
            side: Side::New,
            line,
            start_line,
            start_side: None,
        }
    }

    #[test]
    fn parses_a_single_suggestion_block() {
        let body = "do it like this:\n```suggestion\nfixed line\n```\nthanks";
        assert_eq!(
            parse_suggestion_blocks(body),
            vec!["fixed line".to_string()]
        );
    }

    #[test]
    fn parses_a_multi_line_suggestion_block() {
        let body = "```suggestion\nline a\nline b\n```";
        assert_eq!(
            parse_suggestion_blocks(body),
            vec!["line a\nline b".to_string()]
        );
    }

    #[test]
    fn no_block_is_empty() {
        assert!(parse_suggestion_blocks("just a comment").is_empty());
    }

    #[test]
    fn overlapping_suggestions_on_the_same_file_are_rejected() {
        let edits = vec![
            SuggestionEdit {
                anchor: anchor("a.rs", 5, Some(3)),
                replacement: "x".into(),
            },
            SuggestionEdit {
                anchor: anchor("a.rs", 4, None),
                replacement: "y".into(),
            },
        ];
        assert!(check_no_overlap(&edits).is_err());
    }

    #[test]
    fn non_overlapping_suggestions_are_accepted() {
        let edits = vec![
            SuggestionEdit {
                anchor: anchor("a.rs", 3, None),
                replacement: "x".into(),
            },
            SuggestionEdit {
                anchor: anchor("a.rs", 10, Some(8)),
                replacement: "y".into(),
            },
            SuggestionEdit {
                anchor: anchor("b.rs", 1, None),
                replacement: "z".into(),
            },
        ];
        assert!(check_no_overlap(&edits).is_ok());
    }

    #[tokio::test]
    async fn applies_a_single_line_and_a_multi_line_edit_bottom_to_top() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        tokio::fs::write(&path, "a\nb\nc\nd\ne\n").await.unwrap();

        let edits = vec![
            SuggestionEdit {
                anchor: anchor("f.txt", 2, None),
                replacement: "B".into(),
            },
            SuggestionEdit {
                anchor: anchor("f.txt", 5, Some(4)),
                replacement: "D1\nD2".into(),
            },
        ];
        apply_suggestions(dir.path(), &edits).await.unwrap();
        let out = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(out, "a\nB\nc\nD1\nD2\n");
    }

    #[tokio::test]
    async fn safe_join_accepts_a_plain_relative_path() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::create_dir(dir.path().join("sub")).await.unwrap();
        tokio::fs::write(dir.path().join("sub/f.txt"), "x")
            .await
            .unwrap();

        let resolved = safe_join(dir.path(), "sub/f.txt").await.unwrap();
        assert_eq!(
            resolved,
            tokio::fs::canonicalize(dir.path().join("sub/f.txt"))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn safe_join_rejects_an_absolute_path() {
        let dir = tempfile::tempdir().unwrap();
        let err = safe_join(dir.path(), "/etc/passwd").await.unwrap_err();
        assert!(matches!(err, AppError::Invalid(_)));
    }

    #[tokio::test]
    async fn safe_join_rejects_a_dot_dot_component() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::write(dir.path().join("f.txt"), "x")
            .await
            .unwrap();
        let err = safe_join(dir.path(), "../f.txt").await.unwrap_err();
        assert!(matches!(err, AppError::Invalid(_)));

        // `..` is rejected even buried inside an otherwise-plausible path,
        // not just at the start.
        let err = safe_join(dir.path(), "sub/../../f.txt").await.unwrap_err();
        assert!(matches!(err, AppError::Invalid(_)));
    }

    #[tokio::test]
    async fn safe_join_rejects_a_symlink_that_escapes_the_worktree() {
        let worktree = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        tokio::fs::write(outside.path().join("secret.txt"), "x")
            .await
            .unwrap();
        // No `..`, no absolute path in the request — the escape is
        // entirely inside a symlink planted in the worktree.
        std::os::unix::fs::symlink(outside.path(), worktree.path().join("linked")).unwrap();

        let err = safe_join(worktree.path(), "linked/secret.txt")
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Invalid(_)));
    }

    #[test]
    fn find_comment_locates_it_by_id_across_threads() {
        let detail_thread = Thread {
            id: ThreadId::new("t1"),
            anchor: Some(anchor("a.rs", 1, None)),
            resolved: false,
            outdated: false,
            comments: vec![Comment {
                id: CommentId::new("c1"),
                author: Person {
                    login: "a".into(),
                    name: None,
                    avatar_url: None,
                },
                body: "```suggestion\nx\n```".into(),
                created_at: 0,
                edited: false,
                reactions: Vec::new(),
            }],
        };
        let detail = crate::testkit::sample_pr_detail(
            &theodore_domain::PrRef {
                repo: theodore_domain::RepoRef {
                    host: HostKind::Github,
                    owner: "o".into(),
                    name: "r".into(),
                },
                number: 1,
                host_id: None,
            },
            &theodore_domain::Sha::new("base"),
            &theodore_domain::Sha::new("head"),
        );
        let mut detail = detail;
        detail.threads = vec![detail_thread];
        let found = find_comment(&detail, &CommentId::new("c1"));
        assert!(found.is_some());
        assert!(find_comment(&detail, &CommentId::new("missing")).is_none());
    }
}
