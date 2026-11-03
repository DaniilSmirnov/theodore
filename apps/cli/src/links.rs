//! `theodore eval --symbol-links`: code-intelligence links for fixtures
//! that carry only a diff, computed the way the app does
//! (`theodore_app::symbol_links`) over a sparse head checkout.
//!
//! ```text
//! fixture hunks ─▶ <dir>/worktrees/<name>: each changed file holds its
//!                  hunks' new-side lines at their own line numbers,
//!                  blank elsewhere ─▶ git init + commit
//!              ─▶ CodeIntel (syntax layer, no language servers) open
//!              ─▶ wait for the index ─▶ symbol_links ─▶ Fixture::symbol_links
//! ```
//!
//! What the sparse checkout can't show: definitions and uses outside the
//! hunks. Every link the app could make joins two changed hunks, so both
//! ends are in the checkout; what is lost is a definition whose name line
//! sits outside its hunk (a body-only change of a long function). The
//! budget is generous (fresh index, grammars may download once); the
//! app's is 2 s.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use theodore_app::{SymbolLinkLimits, symbol_links};
use theodore_code_intel::{CodeIntel, CodeIntelOptions};
use theodore_domain::grouping::SymbolLink;
use theodore_domain::{FileFlag, FileStatus, LineKind, Revision};
use theodore_ports::{CodeIntelPort, InstallState, LspPolicy, TrustMode, Workspace};

use crate::eval::Fixture;

/// How long one fixture's index may take to finish.
const INDEX_WAIT: Duration = Duration::from_secs(90);

/// Fills `symbol_links` of every fixture that has none, using a code
/// intelligence data dir under `dir` (grammars download there once).
pub async fn fill(fixtures: &mut [(String, Fixture)], dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut options = CodeIntelOptions::new(dir.join("code-intel"));
    options.lsp = LspPolicy {
        review: false,
        spaces: false,
    };
    let ci: Arc<dyn CodeIntelPort> =
        Arc::new(CodeIntel::new(options).map_err(|e| anyhow::anyhow!("code intelligence: {e}"))?);
    for (name, fx) in fixtures.iter_mut() {
        if !fx.symbol_links.is_empty() {
            continue;
        }
        let started = Instant::now();
        let root = dir.join("worktrees").join(name.as_str());
        match links_for(ci.clone(), name, &fx.revision, &root).await {
            Ok(links) => {
                eprintln!(
                    "{name}: {} symbol link(s) in {} ms",
                    links.len(),
                    started.elapsed().as_millis()
                );
                for l in &links {
                    let path = |id| {
                        fx.revision
                            .hunks()
                            .find(|h| &h.id == id)
                            .map_or("?", |h| h.path.as_str())
                    };
                    eprintln!(
                        "  {} ({}) `{}` → {} ({})",
                        l.from,
                        path(&l.from),
                        l.symbol,
                        l.to,
                        path(&l.to)
                    );
                }
                fx.symbol_links = links;
            }
            Err(e) => eprintln!("{name}: no symbol links: {e:#}"),
        }
    }
    // For scoring saved runs against (docs/eval/scripts/link_adherence.py).
    let all: std::collections::BTreeMap<&str, &Vec<SymbolLink>> = fixtures
        .iter()
        .map(|(name, fx)| (name.as_str(), &fx.symbol_links))
        .collect();
    std::fs::write(dir.join("links.json"), serde_json::to_string_pretty(&all)?)?;
    Ok(())
}

async fn links_for(
    ci: Arc<dyn CodeIntelPort>,
    name: &str,
    rev: &Revision,
    root: &Path,
) -> Result<Vec<SymbolLink>> {
    let paths = write_sparse_head(rev, root)?;
    let ws = Workspace {
        repo_key: format!("eval:{name}"),
        root: root.to_path_buf(),
        trust: TrustMode::Review,
        languages: Vec::new(),
        focus_paths: paths,
        lsp: Some(LspPolicy {
            review: false,
            spaces: false,
        }),
    };
    ci.open(ws)
        .await
        .map_err(|e| anyhow::anyhow!("open: {e}"))?;
    let deadline = Instant::now() + INDEX_WAIT;
    loop {
        let status = ci.status(root).await.unwrap_or_default();
        // A language without a grammar (none published, a failed download)
        // never gets indexed: the text layer serves it.
        let done = status.iter().all(|l| {
            l.files == 0
                || l.index_percent >= 100
                || !matches!(
                    l.grammar,
                    InstallState::Downloading
                        | InstallState::Installed { .. }
                        | InstallState::System { .. }
                )
        });
        if done {
            break;
        }
        if Instant::now() >= deadline {
            let left: Vec<String> = status
                .iter()
                .filter(|l| l.files > 0 && l.index_percent < 100)
                .map(|l| format!("{} {}% ({:?})", l.language, l.index_percent, l.grammar))
                .collect();
            eprintln!("{name}: index unfinished: {}", left.join(", "));
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let limits = SymbolLinkLimits {
        budget: Duration::from_secs(60),
        ..SymbolLinkLimits::default()
    };
    let links = symbol_links(ci.clone(), root, rev, limits).await;
    ci.close(root).await;
    Ok(links)
}

/// Writes each linkable changed file of `rev` under `root` with only its
/// hunks' new-side lines, at their line numbers, and commits it. The
/// written paths.
pub fn write_sparse_head(rev: &Revision, root: &Path) -> Result<Vec<String>> {
    if root.exists() {
        std::fs::remove_dir_all(root).with_context(|| format!("clearing {}", root.display()))?;
    }
    std::fs::create_dir_all(root)?;
    let mut written = Vec::new();
    for f in &rev.files {
        let skip = f.status == FileStatus::Deleted
            || f.hunks.is_empty()
            || f.flags.iter().any(|fl| {
                matches!(
                    fl,
                    FileFlag::Binary { .. }
                        | FileFlag::Generated { .. }
                        | FileFlag::Lfs
                        | FileFlag::Submodule
                )
            })
            || !Path::new(&f.path)
                .components()
                .all(|c| matches!(c, Component::Normal(_)));
        if skip {
            continue;
        }
        let path = root.join(&f.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, sparse_text(f))?;
        written.push(f.path.clone());
    }
    git(root, &["init", "-q"])?;
    git(root, &["add", "-A"])?;
    git(
        root,
        &[
            "-c",
            "user.name=theodore-eval",
            "-c",
            "user.email=eval@theodore.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "sparse head",
        ],
    )?;
    Ok(written)
}

/// The file's new-side text as far as its hunks show it: added and
/// context lines at their numbers, blank lines between.
fn sparse_text(f: &theodore_domain::FileDiff) -> String {
    let mut lines: Vec<&str> = Vec::new();
    for h in &f.hunks {
        for l in &h.lines {
            if l.kind == LineKind::Del {
                continue;
            }
            let Some(n) = l.new_no else { continue };
            let i = n.saturating_sub(1) as usize;
            if lines.len() <= i {
                lines.resize(i + 1, "");
            }
            lines[i] = &l.text;
        }
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

fn git(root: &Path, args: &[&str]) -> Result<()> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .context("running git")?;
    if !out.status.success() {
        anyhow::bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Default data dir for `--symbol-links`: kept between runs so grammars
/// download once.
pub fn default_dir() -> PathBuf {
    std::env::temp_dir().join("theodore-eval-links")
}

#[cfg(test)]
mod tests {
    use super::*;
    use theodore_domain::{DiffLine, FileDiff, Hunk, HunkId, Sha};

    #[test]
    fn the_sparse_head_keeps_new_side_lines_at_their_numbers() {
        let line = |kind, text: &str, new_no| DiffLine {
            kind,
            text: text.into(),
            old_no: None,
            new_no,
        };
        let f = FileDiff {
            path: "src/a.rs".into(),
            old_path: None,
            status: FileStatus::Modified,
            similarity: None,
            old_mode: None,
            new_mode: None,
            flags: vec![],
            additions: 1,
            deletions: 1,
            hunks: vec![Hunk {
                id: HunkId::new("h"),
                path: "src/a.rs".into(),
                old_start: 2,
                old_lines: 2,
                new_start: 2,
                new_lines: 2,
                header: String::new(),
                lines: vec![
                    line(LineKind::Ctx, "fn a() {", Some(2)),
                    line(LineKind::Del, "  old()", None),
                    line(LineKind::Add, "  new_call()", Some(3)),
                ],
            }],
        };
        assert_eq!(sparse_text(&f), "\nfn a() {\n  new_call()\n");
        let dir = tempfile::tempdir().unwrap();
        let rev = Revision {
            base: Sha::new("b"),
            head: Sha::new("h"),
            files: vec![f],
        };
        let root = dir.path().join("wt");
        assert_eq!(
            write_sparse_head(&rev, &root).unwrap(),
            vec!["src/a.rs".to_string()]
        );
        assert!(root.join(".git").exists());
    }
}
