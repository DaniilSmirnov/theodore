//! Prompt builder (plan §5.1–§5.2): PR title/body/commits, file list, and
//! every hunk as id + path + header + a compact excerpt, within a token
//! budget, plus a short "related changes" list from the relatedness graph.
//! Large PRs split into chunks of related files (graph communities) with a
//! merge pass over group summaries.
//!
//! ```text
//! whole PR fits ─▶ one prompt
//! too big ─▶ units: each graph community (2+ files)
//!                   + unconnected files by top-level dir
//!           unit over budget ─▶ split it by top-level dir
//!           pack units into chunks ≤ budget, each into the chunk
//!           it is most related to
//!           each chunk's prompt lists every other chunk's files as one-line
//!           skeletons (`skeleton.rs`), so it sees the whole PR's shape
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde::Deserialize;
use theodore_domain::grouping::RepoContext;
use theodore_domain::{Group, GroupId, Hunk, HunkId, LineKind, Revision};

use crate::PrMeta;
use crate::graph::RelGraph;
use crate::prepass::PrePass;
use crate::skeleton;

/// Caps on the "Related changes" section, so it stays a hint and never
/// crowds out the hunks themselves.
const RELATED_MAX_EDGES: usize = 24;
const RELATED_MAX_CHARS: usize = 2_400;
const RELATED_MIN_WEIGHT: f64 = 0.3;
const ROLLOUT_SHOWN_FILES: usize = 6;
/// Caps on the "Symbol links" section (code intelligence, hunk to hunk).
const SYMBOL_LINK_MAX_LINES: usize = 30;
const SYMBOL_LINK_MAX_CHARS: usize = 1_600;
const SYMBOL_LINK_MAX_USERS: usize = 6;
/// A chunk's "rest of this PR" skeleton section fills the room its chunk
/// leaves under the budget, within these bounds. It is not reserved when
/// packing: the floor may take a full chunk this far over the soft budget,
/// which keeps chunk boundaries exactly what the graph alone decides.
const SKELETON_MIN_CHARS: usize = 2_000;
const SKELETON_MAX_CHARS: usize = 8_000;
/// Pull of one same-directory file pair when choosing a chunk for a unit;
/// small next to any real graph link.
const DIR_AFFINITY: f64 = 0.05;

#[derive(Debug, Clone)]
pub struct PromptBudget {
    /// Soft cap on the user prompt size, in characters. Token counts aren't
    /// available without calling the model, so this is a ~4-chars-per-token
    /// estimate — good enough to decide "split or not", not an exact budget.
    pub max_chars: usize,
    /// Lines shown from the start and end of a hunk's body.
    pub excerpt_lines: usize,
    /// Hard cap on one hunk's excerpt, in characters.
    pub excerpt_max_chars: usize,
}

impl Default for PromptBudget {
    fn default() -> Self {
        Self {
            max_chars: 48_000,
            excerpt_lines: 4,
            excerpt_max_chars: 320,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PromptPart {
    pub system: String,
    pub user: String,
}

#[derive(Debug, Clone)]
pub struct PromptChunk {
    /// Empty for a single whole-PR prompt; the top-level directory otherwise.
    pub label: String,
    pub prompt: PromptPart,
    /// Hunk ids covered by this chunk (everything not hidden).
    pub hunk_ids: Vec<HunkId>,
    /// The group count this chunk's prompt asks for, `(lo, hi)`.
    pub target: (usize, usize),
}

/// `part_of_larger` is the chunk's own top-level directory when a large PR
/// was split by directory (plan §5.2); `None` for a whole-PR single prompt.
fn system_prompt(
    target: (usize, usize),
    total_hunks: usize,
    part_of_larger: Option<&str>,
) -> String {
    let (lo, hi) = target;
    let mut out = String::new();
    match part_of_larger {
        None => {
            let _ = write!(
                out,
                "You are reviewing a pull request with {total_hunks} changed hunk(s). \
Split its diff into about {lo}-{hi} groups, each one logical step a reviewer \
can approve as a unit."
            );
        }
        Some(dir) => {
            let _ = write!(
                out,
                "You are reviewing one part (`{dir}`) of a larger pull request, \
split into parts of related files for size; this part has {total_hunks} changed hunk(s). \
Aim for about {lo}-{hi} groups in THIS part; a later pass puts every part's \
groups in reading order. The other parts are listed one line per file under \
\"The rest of this PR\": use them to see what this part's changes connect to \
and to title groups so they read well next to the other parts' groups, but \
group only the hunks listed in this part."
            );
        }
    }
    out.push_str(
        "\n\nA logical step is one coherent change a reviewer approves as a unit, \
not one file or one kind of file: config, env, CI and docs edits that exist \
to support a single change belong inside that change's group, not split out \
on their own just because they are a different file type. Prefer a few \
well-explained groups over many tiny ones.\n\n\
Order groups in reading order: foundations first (schema, models, types), \
then the code that uses them, then tests, then anything mechanical or \
generated last.\n\n\
Respond with JSON only — no prose, no markdown fence — matching exactly:\n\
{\n  \"groups\": [\n    {\n      \"id\": \"kebab-case-slug, unique\",\n      \
\"title\": \"short title\",\n      \"why\": \"1-3 sentences: what changed and why\",\n      \
\"kind\": \"normal\" | \"skim\" | \"hidden\",\n      \"risk\": \"one sentence on what could go wrong, or omit\",\n      \
\"check\": [\"short, concrete things a reviewer should verify\"],\n      \
\"hunks\": [\"hunk id\", \"...\"],\n      \"outliers\": [\"hunk ids in this group that break its own pattern\"]\n    }\n  ]\n}\n\n\
Rules: every hunk id listed below must appear in exactly one group's \"hunks\"; \
do not invent hunk ids; \"outliers\" must be a subset of that same group's \"hunks\"; \
use \"skim\" for mechanical changes (pure renames, formatting, import-only) and \
\"hidden\" only for lockfiles/generated/vendored content you were not shown in full.",
    );
    out
}

/// `lines`/`max_chars` are resolved by the caller from `PromptBudget`,
/// shrunk for hunks the pre-pass already flagged as a skim candidate — the
/// model doesn't need a full excerpt of a rename or an import block to
/// confirm the hint (goal: trim the prompt for speed/cost on large PRs).
fn excerpt(hunk: &Hunk, lines: usize, max_chars: usize) -> String {
    let n = hunk.lines.len();
    let show_all = n <= lines * 2;
    let mut out = String::new();
    let push_line = |out: &mut String, idx: usize| {
        let l = &hunk.lines[idx];
        let sign = match l.kind {
            LineKind::Add => '+',
            LineKind::Del => '-',
            LineKind::Ctx => ' ',
        };
        let _ = writeln!(out, "{sign}{}", l.text);
    };
    if show_all {
        for i in 0..n {
            push_line(&mut out, i);
        }
    } else {
        for i in 0..lines {
            push_line(&mut out, i);
        }
        out.push_str("…\n");
        for i in (n - lines)..n {
            push_line(&mut out, i);
        }
    }
    if out.len() > max_chars {
        // `String::truncate` panics unless the cut point lies on a char
        // boundary — back off until it does (a multi-byte char, including
        // the "…" this function itself inserts above, can straddle the cap).
        let mut cut = max_chars;
        while cut > 0 && !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        out.push('…');
    }
    out
}

fn write_pr_meta(out: &mut String, pr: &PrMeta) {
    let _ = writeln!(out, "# PR");
    let _ = writeln!(out, "Title: {}", pr.title);
    if !pr.body.is_empty() {
        let _ = writeln!(out, "Body:\n{}", pr.body);
    }
    if !pr.commits.is_empty() {
        let _ = writeln!(out, "Commits:");
        for c in &pr.commits {
            let _ = writeln!(out, "- {c}");
        }
    }
}

fn write_hidden_summary(out: &mut String, prepass: &PrePass) {
    if prepass.hidden.is_empty() {
        return;
    }
    let _ = writeln!(
        out,
        "\n# Not shown (collapsed into a hidden group, already handled)"
    );
    for (path, reason) in &prepass.hidden {
        let _ = writeln!(out, "- {path} ({})", reason.label());
    }
}

fn write_files_and_hunks(
    out: &mut String,
    files: &[&theodore_domain::FileDiff],
    prepass: &PrePass,
    budget: &PromptBudget,
) {
    let _ = writeln!(out, "\n# Files and hunks");
    for file in files {
        let _ = writeln!(out, "\n## {} ({:?})", file.path, file.status);
        for hunk in &file.hunks {
            let skim_hint = prepass.skim.get(&hunk.id);
            let hint = skim_hint
                .map(|c| format!(" [skim candidate: {}]", c.label()))
                .unwrap_or_default();
            let _ = writeln!(out, "\n### hunk {} — {}{hint}", hunk.id, hunk.header);
            let (lines, max_chars) = if skim_hint.is_some() {
                // already flagged mechanical by the pre-pass: a short peek
                // is enough for the model to confirm it, not a full excerpt.
                (
                    (budget.excerpt_lines / 2).max(1),
                    budget.excerpt_max_chars / 2,
                )
            } else {
                (budget.excerpt_lines, budget.excerpt_max_chars)
            };
            out.push_str(&excerpt(hunk, lines, max_chars));
        }
    }
}

/// Strongest links among `files`, one line each: rollouts first
/// (`33 files make the same edit: …`), then pairwise edges. Nothing when no
/// two files in this prompt are related.
fn write_related(out: &mut String, files: &[&theodore_domain::FileDiff], graph: &RelGraph) {
    let within: BTreeSet<usize> = files
        .iter()
        .filter_map(|f| graph.index_of(&f.path))
        .collect();
    let mut lines: Vec<String> = Vec::new();
    for r in &graph.rollouts {
        let members: Vec<&str> = r
            .files
            .iter()
            .filter(|i| within.contains(i))
            .map(|&i| graph.files[i].as_str())
            .collect();
        if members.len() < 3 {
            continue;
        }
        let shown = members[..members.len().min(ROLLOUT_SHOWN_FILES)].join(", ");
        let more = members.len().saturating_sub(ROLLOUT_SHOWN_FILES);
        let more = if more > 0 {
            format!(", … (+{more} more)")
        } else {
            String::new()
        };
        lines.push(format!(
            "- {} files {}: {shown}{more}",
            members.len(),
            r.describe()
        ));
    }
    for (a, b, e) in graph.strongest_edges(&within, RELATED_MAX_EDGES) {
        if e.weight() < RELATED_MIN_WEIGHT {
            break;
        }
        let (fa, fb) = (&graph.files[a], &graph.files[b]);
        let link = match (e.a_imports_b, e.b_imports_a) {
            (true, false) => format!("{fa} → {fb}"),
            (false, true) => format!("{fb} → {fa}"),
            _ => format!("{fa} ~ {fb}"),
        };
        let reasons = e.reasons();
        lines.push(format!(
            "- {link} ({})",
            reasons[..reasons.len().min(2)].join(", ")
        ));
    }
    if lines.is_empty() {
        return;
    }
    let _ = writeln!(
        out,
        "\n# Related changes\nFiles linked by imports, shared names or the same edit, \
strongest first. Keep the changes that link them in one group unless they are clearly \
separate steps. Links are between files, not hunks: a purely mechanical hunk (formatting, \
syntax cleanup) in a linked file can still go to its own skim group."
    );
    let mut used = 0;
    for l in lines {
        used += l.len() + 1;
        if used > RELATED_MAX_CHARS {
            break;
        }
        let _ = writeln!(out, "{l}");
    }
}

/// Code-intelligence links among the hunks of `files`, one line per
/// defining hunk and symbol: `- <hunk> `name` → <hunk>, <hunk>`. Nothing
/// when there are none.
fn write_symbol_links(out: &mut String, files: &[&theodore_domain::FileDiff], graph: &RelGraph) {
    if graph.hunk_links().is_empty() {
        return;
    }
    let here: BTreeSet<&HunkId> = files
        .iter()
        .flat_map(|f| f.hunks.iter().map(|h| &h.id))
        .collect();
    let mut lines: Vec<((&HunkId, &str), Vec<&HunkId>)> = Vec::new();
    for l in graph.hunk_links() {
        if !here.contains(&l.from) || !here.contains(&l.to) {
            continue;
        }
        let key = (&l.from, l.symbol.as_str());
        match lines.iter_mut().find(|(k, _)| *k == key) {
            Some((_, users)) => {
                if users.len() < SYMBOL_LINK_MAX_USERS {
                    users.push(&l.to);
                }
            }
            None => lines.push((key, vec![&l.to])),
        }
    }
    if lines.is_empty() {
        return;
    }
    let _ = writeln!(
        out,
        "\n# Symbol links\nFound by code intelligence: the first hunk defines or changes `name`, \
the hunks after → use it. Put them in one group, or the defining hunk in an earlier group \
than its uses."
    );
    let mut used = 0;
    for ((from, name), users) in lines.into_iter().take(SYMBOL_LINK_MAX_LINES) {
        let users: Vec<&str> = users.iter().map(|h| h.as_str()).collect();
        let line = format!("- {from} `{name}` → {}", users.join(", "));
        used += line.len() + 1;
        if used > SYMBOL_LINK_MAX_CHARS {
            break;
        }
        let _ = writeln!(out, "{line}");
    }
}

fn user_prompt(
    pr: &PrMeta,
    files: &[&theodore_domain::FileDiff],
    prepass: &PrePass,
    graph: &RelGraph,
    budget: &PromptBudget,
    rest_of_pr: &str,
) -> String {
    let mut out = String::new();
    write_pr_meta(&mut out, pr);
    write_hidden_summary(&mut out, prepass);
    write_related(&mut out, files, graph);
    write_symbol_links(&mut out, files, graph);
    out.push_str(rest_of_pr);
    write_files_and_hunks(&mut out, files, prepass, budget);
    out
}

/// Room for the skeleton section in a chunk whose prompt is `used` long
/// without it: never more than a sixth of the budget.
fn skeleton_budget(budget: &PromptBudget, used: usize) -> usize {
    budget
        .max_chars
        .saturating_sub(used)
        .clamp(SKELETON_MIN_CHARS, SKELETON_MAX_CHARS)
        .min(budget.max_chars / 6)
}

/// Prompt size for `files` without the related-changes hint.
fn graph_free_size(
    pr: &PrMeta,
    files: &[&theodore_domain::FileDiff],
    prepass: &PrePass,
    budget: &PromptBudget,
) -> usize {
    user_prompt(pr, files, prepass, &RelGraph::default(), budget, "").len()
}

fn top_level_dir(path: &str) -> String {
    crate::area::top_level_dir(path).to_string()
}

/// Files that should share a chunk, with their estimated prompt size.
struct Unit {
    /// Indices into the visible-file list, ascending.
    files: Vec<usize>,
    size: usize,
}

/// Split `files` by top-level directory (the pre-graph chunking rule).
fn by_dir(files: &[usize], visible: &[&theodore_domain::FileDiff]) -> Vec<Vec<usize>> {
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for &i in files {
        groups
            .entry(top_level_dir(&visible[i].path))
            .or_default()
            .push(i);
    }
    groups.into_values().collect()
}

/// Pack units into as few chunks as fit `max`: largest unit first (ties by
/// first file, so the result is stable), each into the chunk with room it
/// is most related to by `affinity` — the first one with room when it is
/// related to none (first-fit decreasing). A unit over `max` on its own
/// still gets a chunk of its own. Chunks come out in file order.
fn pack(
    mut units: Vec<Unit>,
    overhead: usize,
    max: usize,
    affinity: &dyn Fn(&[usize], &[usize]) -> f64,
) -> Vec<Vec<usize>> {
    let room = max.saturating_sub(overhead);
    units.sort_by(|a, b| {
        b.size
            .cmp(&a.size)
            .then_with(|| a.files[0].cmp(&b.files[0]))
    });
    let mut bins: Vec<(usize, Vec<usize>)> = Vec::new();
    for u in units {
        let mut best: Option<(usize, f64)> = None;
        for (i, (used, files)) in bins.iter().enumerate() {
            if used + u.size > room {
                continue;
            }
            let a = affinity(&u.files, files);
            if best.is_none_or(|(_, b)| a > b) {
                best = Some((i, a));
            }
        }
        match best {
            Some((i, _)) => {
                let (used, files) = &mut bins[i];
                *used += u.size;
                files.extend(u.files);
            }
            None => bins.push((u.size, u.files)),
        }
    }
    let mut out: Vec<Vec<usize>> = bins
        .into_iter()
        .map(|(_, mut f)| {
            f.sort_unstable();
            f
        })
        .collect();
    out.sort_by_key(|f| f[0]);
    out
}

/// The units a too-big PR is packed from: each graph community with 2+
/// files (split by directory if it alone is over budget), then the
/// unconnected files grouped by top-level directory.
fn chunk_units(
    visible: &[&theodore_domain::FileDiff],
    graph: &RelGraph,
    body: &dyn Fn(&[usize]) -> usize,
    room: usize,
) -> Vec<Unit> {
    let visible_index: BTreeMap<&str, usize> = visible
        .iter()
        .enumerate()
        .map(|(i, f)| (f.path.as_str(), i))
        .collect();
    let mut units: Vec<Unit> = Vec::new();
    let mut seen: BTreeSet<usize> = BTreeSet::new();
    let mut loose: Vec<usize> = Vec::new();
    for community in graph.communities() {
        let mut files: Vec<usize> = community
            .iter()
            .filter_map(|&g| visible_index.get(graph.files[g].as_str()).copied())
            .collect();
        files.sort_unstable();
        seen.extend(files.iter().copied());
        if files.len() < 2 {
            loose.extend(files);
            continue;
        }
        let size = body(&files);
        if size <= room {
            units.push(Unit { files, size });
        } else {
            for part in by_dir(&files, visible) {
                let size = body(&part);
                units.push(Unit { files: part, size });
            }
        }
    }
    loose.extend((0..visible.len()).filter(|i| !seen.contains(i)));
    loose.sort_unstable();
    for part in by_dir(&loose, visible) {
        let size = body(&part);
        units.push(Unit { files: part, size });
    }
    units
}

fn chunk_label(files: &[&theodore_domain::FileDiff]) -> String {
    let mut dirs: Vec<String> = Vec::new();
    for f in files {
        let d = top_level_dir(&f.path);
        if !dirs.contains(&d) {
            dirs.push(d);
        }
    }
    let extra = dirs.len().saturating_sub(4);
    let mut label = dirs
        .iter()
        .take(4)
        .map(|d| {
            if d.is_empty() {
                "(repository root)"
            } else {
                d.as_str()
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    if extra > 0 {
        let _ = write!(label, " +{extra} more");
    }
    label
}

/// Build one prompt per chunk, from the diff alone (no repository context).
pub fn build_prompts(
    pr: &PrMeta,
    rev: &Revision,
    prepass: &PrePass,
    budget: &PromptBudget,
) -> Vec<PromptChunk> {
    let ctx = RepoContext::default();
    let graph = RelGraph::build(rev, prepass, &ctx);
    build_prompts_with(pr, rev, prepass, &graph, &ctx, budget)
}

/// Build one prompt per chunk. A single chunk covers the whole PR unless the
/// estimated prompt size is over budget, in which case files are split into
/// chunks of related files (graph communities), falling back to top-level
/// directories for unrelated files (plan §5.2). Each prompt tells the model
/// the PR-size-scaled group count it should aim for
/// (`target::target_group_count`). A chunk's prompt also lists every other
/// chunk's files as one-line skeletons, read from `ctx`'s head files when
/// present, else from the diff.
pub fn build_prompts_with(
    pr: &PrMeta,
    rev: &Revision,
    prepass: &PrePass,
    graph: &RelGraph,
    ctx: &RepoContext,
    budget: &PromptBudget,
) -> Vec<PromptChunk> {
    let visible_files: Vec<&theodore_domain::FileDiff> = rev
        .files
        .iter()
        .filter(|f| !prepass.is_hidden(&f.path))
        .collect();
    let total_hunks: usize = visible_files.iter().map(|f| f.hunks.len()).sum();
    let whole_target = crate::target::target_group_count(total_hunks);

    let whole = user_prompt(pr, &visible_files, prepass, graph, budget, "");
    if whole.len() <= budget.max_chars || visible_files.len() <= 1 {
        let hunk_ids = visible_files
            .iter()
            .flat_map(|f| f.hunks.iter().map(|h| h.id.clone()))
            .collect();
        return vec![PromptChunk {
            label: String::new(),
            prompt: PromptPart {
                system: system_prompt(whole_target, total_hunks, None),
                user: whole,
            },
            hunk_ids,
            target: whole_target,
        }];
    }

    // A chunk's size = shared header (PR meta, hidden list) + its hint
    // section + its files' bodies; units are sized by body only. A chunk's
    // hints are a subset of the whole PR's, so the whole PR's (capped)
    // hint size is a fair reserve.
    let empty = graph_free_size(pr, &[], prepass, budget);
    let whole_hints =
        whole
            .len()
            .saturating_sub(graph_free_size(pr, &visible_files, prepass, budget));
    let overhead = empty + whole_hints.min(RELATED_MAX_CHARS);
    let body = |files: &[usize]| {
        let fs: Vec<&theodore_domain::FileDiff> = files.iter().map(|&i| visible_files[i]).collect();
        graph_free_size(pr, &fs, prepass, budget).saturating_sub(empty)
    };
    let room = budget.max_chars.saturating_sub(overhead);
    let units = chunk_units(&visible_files, graph, &body, room);
    // Which chunk a unit joins: graph links first, then a small pull
    // towards chunks holding files of the same top-level directory (so
    // unconnected files still keep their directory's company).
    let graph_idx: Vec<Option<usize>> = visible_files
        .iter()
        .map(|f| graph.index_of(&f.path))
        .collect();
    let affinity = |unit: &[usize], chunk: &[usize]| -> f64 {
        let mut a = 0.0;
        for &u in unit {
            for &c in chunk {
                if let (Some(gu), Some(gc)) = (graph_idx[u], graph_idx[c]) {
                    a += graph.connection(gu, gc);
                }
                if top_level_dir(&visible_files[u].path) == top_level_dir(&visible_files[c].path) {
                    a += DIR_AFFINITY;
                }
            }
        }
        a
    };

    let packed = pack(units, overhead, budget.max_chars, &affinity);
    let labels: Vec<String> = packed
        .iter()
        .map(|idx| chunk_label(&idx.iter().map(|&i| visible_files[i]).collect::<Vec<_>>()))
        .collect();
    let skeleton_lines: Vec<String> = visible_files
        .iter()
        .map(|f| {
            let head = ctx.head_files.get(&f.path).map(String::as_str);
            skeleton::render(f, &skeleton::extract(f, head), skeleton::LINE_MAX_CHARS)
        })
        .collect();
    let rest_of_pr = |this: usize, used: usize| -> String {
        let parts: Vec<skeleton::Part> = packed
            .iter()
            .zip(&labels)
            .map(|(idx, label)| skeleton::Part {
                label: label.clone(),
                files: idx
                    .iter()
                    .map(|&i| (skeleton_lines[i].clone(), affinity(&[i], &packed[this])))
                    .collect(),
            })
            .collect();
        skeleton::section(this, &parts, skeleton_budget(budget, used))
    };

    packed
        .iter()
        .enumerate()
        .map(|(k, idx)| {
            let files: Vec<&theodore_domain::FileDiff> =
                idx.iter().map(|&i| visible_files[i]).collect();
            let label = labels[k].clone();
            let chunk_hunks: usize = files.iter().map(|f| f.hunks.len()).sum();
            let chunk_target =
                crate::target::scaled_for_chunk(whole_target, chunk_hunks, total_hunks);
            let used = user_prompt(pr, &files, prepass, graph, budget, "").len();
            let user = user_prompt(pr, &files, prepass, graph, budget, &rest_of_pr(k, used));
            let hunk_ids = files
                .iter()
                .flat_map(|f| f.hunks.iter().map(|h| h.id.clone()))
                .collect();
            PromptChunk {
                prompt: PromptPart {
                    system: system_prompt(chunk_target, chunk_hunks, Some(&label)),
                    user,
                },
                label,
                hunk_ids,
                target: chunk_target,
            }
        })
        .collect()
}

/// Repair prompt: same system message, user message carries the previous
/// answer and the exact validation errors to fix.
pub fn repair_prompt(known_ids: &[HunkId], previous_text: &str, errors: &[String]) -> PromptPart {
    let mut user = String::new();
    let _ = writeln!(
        user,
        "Your previous answer was not a valid grouping. Problems found:"
    );
    for e in errors {
        let _ = writeln!(user, "- {e}");
    }
    let _ = writeln!(
        user,
        "\nKnown hunk ids (every one must appear exactly once across all groups):"
    );
    for id in known_ids {
        let _ = writeln!(user, "- {id}");
    }
    let _ = writeln!(user, "\nYour previous answer:\n{previous_text}");
    let _ = writeln!(
        user,
        "\nReturn corrected JSON only, same schema as before, fixing exactly these problems."
    );
    let target = crate::target::target_group_count(known_ids.len());
    PromptPart {
        system: system_prompt(target, known_ids.len(), None),
        user,
    }
}

fn group_summary(g: &Group) -> String {
    format!(
        "{} — \"{}\" ({:?}, {} hunks): {}",
        g.id,
        g.title,
        g.kind,
        g.hunks.len(),
        g.why
    )
}

/// Merge-pass prompt for multi-chunk grouping: asks only for the final
/// reading order across every chunk's groups (membership is already fixed —
/// each hunk was sent to exactly one chunk, so there is nothing left to
/// reassign, only to order and, optionally, retitle for the combined PR).
pub fn merge_prompt(pr: &PrMeta, groups: &[Group]) -> PromptPart {
    let mut user = String::new();
    write_pr_meta(&mut user, pr);
    let _ = writeln!(
        user,
        "\nThis PR was large enough to be grouped in parts. Below are the \
groups produced for each part. Decide the final reading order across all of \
them (foundations → usage → tests → mechanical/hidden last).\n"
    );
    for g in groups {
        let _ = writeln!(user, "- {}", group_summary(g));
    }
    let _ = writeln!(
        user,
        "\nRespond with JSON only: {{\"order\": [\"group id\", \"...\"]}} listing every \
group id above exactly once, in the reading order you recommend."
    );
    PromptPart {
        system: "You decide only the reading order of already-formed review groups. \
Do not change, merge, split, rename, or drop any group."
            .to_string(),
        user,
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct RawOrder {
    #[serde(default)]
    pub order: Vec<String>,
}

impl RawOrder {
    pub fn into_ids(self) -> Vec<GroupId> {
        self.order.into_iter().map(GroupId::new).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use theodore_domain::grouping::SymbolLink;
    use theodore_domain::{DiffLine, FileDiff, FileStatus, Hunk, HunkId, LineKind, Sha};

    fn line(kind: LineKind, text: &str) -> DiffLine {
        DiffLine {
            kind,
            text: text.into(),
            old_no: None,
            new_no: None,
        }
    }

    fn mk_hunk(id: &str, n: usize) -> Hunk {
        Hunk {
            id: HunkId::new(id),
            path: String::new(),
            old_start: 1,
            old_lines: n as u32,
            new_start: 1,
            new_lines: n as u32,
            header: "fn foo()".into(),
            lines: (0..n)
                .map(|i| line(LineKind::Add, &format!("line {i}")))
                .collect(),
        }
    }

    fn mk_file(path: &str, hunks: Vec<Hunk>) -> FileDiff {
        FileDiff {
            path: path.into(),
            old_path: None,
            status: FileStatus::Modified,
            similarity: None,
            old_mode: None,
            new_mode: None,
            flags: vec![],
            additions: 0,
            deletions: 0,
            hunks,
        }
    }

    #[test]
    fn single_chunk_for_small_pr() {
        let rev = Revision {
            base: Sha::new("b"),
            head: Sha::new("h"),
            files: vec![mk_file("src/a.ts", vec![mk_hunk("h1", 3)])],
        };
        let prepass = PrePass::default();
        let chunks = build_prompts(&PrMeta::default(), &rev, &prepass, &PromptBudget::default());
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].prompt.user.contains("hunk h1"));
    }

    #[test]
    fn splits_by_top_level_dir_over_budget() {
        let rev = Revision {
            base: Sha::new("b"),
            head: Sha::new("h"),
            files: vec![
                mk_file("a/one.ts", vec![mk_hunk("h1", 50)]),
                mk_file("b/two.ts", vec![mk_hunk("h2", 50)]),
            ],
        };
        let prepass = PrePass::default();
        let tiny_budget = PromptBudget {
            max_chars: 10,
            ..PromptBudget::default()
        };
        let chunks = build_prompts(&PrMeta::default(), &rev, &prepass, &tiny_budget);
        assert_eq!(chunks.len(), 2);
        let labels: Vec<&str> = chunks.iter().map(|c| c.label.as_str()).collect();
        assert!(labels.contains(&"a"));
        assert!(labels.contains(&"b"));
    }

    #[test]
    fn packs_small_directories_into_one_chunk() {
        let files: Vec<_> = (0..6)
            // Unrelated files (different edits, so no "same edit" link):
            // only directory packing applies.
            .map(|i| {
                mk_file(
                    &format!("d{i}/f.ts"),
                    vec![mk_hunk(&format!("h{i}"), 20 + i)],
                )
            })
            .collect();
        let rev = Revision {
            base: Sha::new("b"),
            head: Sha::new("h"),
            files,
        };
        let prepass = PrePass::default();
        let whole = build_prompts(&PrMeta::default(), &rev, &prepass, &PromptBudget::default());
        let whole_len = whole[0].prompt.user.len();
        // Room for about half the PR per chunk: 2 chunks, not 6.
        let budget = PromptBudget {
            max_chars: whole_len / 2 + whole_len / 6,
            ..PromptBudget::default()
        };
        let chunks = build_prompts(&PrMeta::default(), &rev, &prepass, &budget);
        assert_eq!(
            chunks.len(),
            2,
            "{:?}",
            chunks.iter().map(|c| &c.label).collect::<Vec<_>>()
        );
        let ids: usize = chunks.iter().map(|c| c.hunk_ids.len()).sum();
        assert_eq!(ids, 6);
    }

    #[test]
    fn root_files_share_one_chunk() {
        let files: Vec<_> = (0..5)
            .map(|i| {
                mk_file(
                    &format!("entry{i}.php"),
                    vec![mk_hunk(&format!("h{i}"), 20)],
                )
            })
            .chain([mk_file("lib/big.ts", vec![mk_hunk("hx", 20)])])
            .collect();
        let rev = Revision {
            base: Sha::new("b"),
            head: Sha::new("h"),
            files,
        };
        let prepass = PrePass::default();
        let tiny_budget = PromptBudget {
            max_chars: 10,
            ..PromptBudget::default()
        };
        let chunks = build_prompts(&PrMeta::default(), &rev, &prepass, &tiny_budget);
        // Over budget everywhere: one chunk per area, and the root is one area.
        assert_eq!(chunks.len(), 2);
        assert!(
            chunks
                .iter()
                .any(|c| c.label == "(repository root)" && c.hunk_ids.len() == 5)
        );
    }

    #[test]
    fn excerpt_caps_long_hunks() {
        let h = mk_hunk("h1", 100);
        let budget = PromptBudget::default();
        let e = excerpt(&h, budget.excerpt_lines, budget.excerpt_max_chars);
        assert!(e.len() <= budget.excerpt_max_chars + 1);
        assert!(e.contains('…'));
    }

    /// Regression: a real tokio PR crashed `theodore group` here because the
    /// default 320-char cap landed in the middle of a multi-byte character,
    /// and `String::truncate` panics unless the cut point is a char
    /// boundary. Built so the cap (320) falls on the second byte of an 'é'.
    #[test]
    fn excerpt_backs_off_to_a_char_boundary_instead_of_panicking() {
        let ascii_line = "a".repeat(70);
        let mut straddling_line = String::new();
        straddling_line.push_str(&"a".repeat(26));
        straddling_line.push('é'); // 2-byte char; its 2nd byte lands at offset 320
        straddling_line.push_str(&"a".repeat(42));
        assert_eq!(straddling_line.len(), 70);

        let lines: Vec<DiffLine> = (0..9)
            .map(|i| {
                let text = if i == 5 {
                    straddling_line.as_str()
                } else {
                    ascii_line.as_str()
                };
                line(LineKind::Add, text)
            })
            .collect();
        let h = Hunk {
            id: HunkId::new("h1"),
            path: String::new(),
            old_start: 1,
            old_lines: 9,
            new_start: 1,
            new_lines: 9,
            header: String::new(),
            lines,
        };
        let budget = PromptBudget::default();
        let e = excerpt(&h, budget.excerpt_lines, budget.excerpt_max_chars); // must not panic
        assert!(e.ends_with('…'));
        assert!(e.is_char_boundary(e.len()));
    }

    #[test]
    fn hidden_files_are_summarized_not_dumped() {
        let rev = Revision {
            base: Sha::new("b"),
            head: Sha::new("h"),
            files: vec![mk_file("pnpm-lock.yaml", vec![mk_hunk("h1", 5)])],
        };
        let mut prepass = PrePass::default();
        prepass.hidden.insert(
            "pnpm-lock.yaml".into(),
            crate::prepass::HiddenReason::Lockfile,
        );
        let chunks = build_prompts(&PrMeta::default(), &rev, &prepass, &PromptBudget::default());
        assert_eq!(chunks.len(), 1);
        assert!(!chunks[0].prompt.user.contains("hunk h1"));
        assert!(chunks[0].prompt.user.contains("pnpm-lock.yaml"));
    }

    /// Speed/cost goal: a hunk the pre-pass already flagged as mechanical
    /// (pure rename / whitespace / import-only) needs only a short peek, not
    /// the full excerpt — trims the prompt on PRs with many such hunks.
    #[test]
    fn skim_candidate_hunks_get_a_shorter_excerpt() {
        let budget = PromptBudget::default();
        let long = mk_hunk("h1", budget.excerpt_lines * 4);
        let normal = excerpt(&long, budget.excerpt_lines, budget.excerpt_max_chars);
        let shrunk = excerpt(
            &long,
            (budget.excerpt_lines / 2).max(1),
            budget.excerpt_max_chars / 2,
        );
        assert!(
            shrunk.len() < normal.len(),
            "skim excerpt ({}) should be shorter than the full one ({})",
            shrunk.len(),
            normal.len()
        );
    }

    /// Prompt-size goal: the system prompt tells the model a PR-size-scaled
    /// target group count and spells out what "one logical step" means
    /// (config/env/CI/docs belong with the change they support).
    #[test]
    fn system_prompt_states_the_scaled_target_and_logical_step_rule() {
        let rev = Revision {
            base: Sha::new("b"),
            head: Sha::new("h"),
            files: vec![mk_file("src/a.ts", vec![mk_hunk("h1", 3)])],
        };
        let prepass = PrePass::default();
        let chunks = build_prompts(&PrMeta::default(), &rev, &prepass, &PromptBudget::default());
        assert_eq!(chunks.len(), 1);
        let system = &chunks[0].prompt.system;
        let (lo, hi) = crate::target::target_group_count(1);
        assert!(system.contains(&format!("{lo}-{hi} groups")));
        assert!(system.contains("logical step"));
        assert!(system.contains("config, env, CI and docs"));
    }

    fn file_with_lines(path: &str, id: &str, lines: &[&str], pad: usize) -> FileDiff {
        let mut h = mk_hunk(id, 0);
        h.lines = lines
            .iter()
            .map(|t| line(LineKind::Add, t))
            .chain((0..pad).map(|i| line(LineKind::Add, &format!("let filler_{id}_{i} = {i};"))))
            .collect();
        mk_file(path, vec![h])
    }

    /// Chunking follows relatedness, not directories: a route and the
    /// service it imports share a chunk even though they sit in different
    /// top-level trees, while an unrelated file in the route's own tree
    /// goes elsewhere.
    #[test]
    fn over_budget_prs_chunk_related_files_together_across_directories() {
        let rev = Revision {
            base: Sha::new("b"),
            head: Sha::new("h"),
            files: vec![
                file_with_lines(
                    "api/routes/invoice.ts",
                    "r1",
                    &["import { billInvoice } from '../../core/billing/invoice-service';"],
                    30,
                ),
                file_with_lines(
                    "api/routes/health.ts",
                    "r2",
                    &["export const healthPath = '/health';"],
                    30,
                ),
                file_with_lines(
                    "core/billing/invoice-service.ts",
                    "s1",
                    &["export function billInvoice(id: string) {"],
                    30,
                ),
                file_with_lines(
                    "core/search/rank.ts",
                    "s2",
                    &["export const rankBoost = 2;"],
                    30,
                ),
            ],
        };
        let prepass = crate::prepass::run(&rev);
        let whole = build_prompts(&PrMeta::default(), &rev, &prepass, &PromptBudget::default());
        let budget = PromptBudget {
            max_chars: whole[0].prompt.user.len() * 8 / 10,
            ..PromptBudget::default()
        };
        let chunks = build_prompts(&PrMeta::default(), &rev, &prepass, &budget);
        assert!(chunks.len() >= 2, "over budget: {}", chunks.len());
        let chunk_of = |id: &str| {
            chunks
                .iter()
                .position(|c| c.hunk_ids.contains(&HunkId::new(id)))
                .unwrap()
        };
        assert_eq!(
            chunk_of("r1"),
            chunk_of("s1"),
            "route + its service share a chunk: {:?} budget {}",
            chunks
                .iter()
                .map(|c| (c.prompt.user.len(), &c.hunk_ids))
                .collect::<Vec<_>>(),
            budget.max_chars
        );
        let total: usize = chunks.iter().map(|c| c.hunk_ids.len()).sum();
        assert_eq!(total, 4, "every hunk in exactly one chunk");
        let invoice_chunk = &chunks[chunk_of("r1")];
        assert!(invoice_chunk.prompt.user.contains("# Related changes"));
        assert!(
            invoice_chunk
                .prompt
                .user
                .contains("api/routes/invoice.ts → core/billing/invoice-service.ts (imports")
        );
    }

    /// Whole hunks in the prompt, so test files are big enough to chunk
    /// with room left for a skeleton section.
    fn full_excerpts() -> PromptBudget {
        PromptBudget {
            excerpt_lines: 1_000,
            excerpt_max_chars: 100_000,
            ..PromptBudget::default()
        }
    }

    /// Each chunk of an over-budget PR sees the other chunks' files as
    /// one-line skeletons (and never its own); a single-prompt PR has no
    /// such section.
    #[test]
    fn chunks_list_the_rest_of_the_pr_as_skeletons() {
        let rev = Revision {
            base: Sha::new("b"),
            head: Sha::new("h"),
            files: vec![
                file_with_lines(
                    "api/routes/invoice.ts",
                    "r1",
                    &["router.post('/invoices', billInvoice);"],
                    300,
                ),
                file_with_lines(
                    "web/src/useInvoices.ts",
                    "w1",
                    &["export function useInvoices() {"],
                    300,
                ),
            ],
        };
        let prepass = crate::prepass::run(&rev);
        let whole = build_prompts(&PrMeta::default(), &rev, &prepass, &full_excerpts());
        assert!(!whole[0].prompt.user.contains("# The rest of this PR"));
        let budget = PromptBudget {
            max_chars: whole[0].prompt.user.len() * 7 / 10,
            ..full_excerpts()
        };
        let chunks = build_prompts(&PrMeta::default(), &rev, &prepass, &budget);
        assert_eq!(chunks.len(), 2);
        let of = |id: &str| {
            chunks
                .iter()
                .find(|c| c.hunk_ids.contains(&HunkId::new(id)))
                .unwrap()
        };
        let api = &of("r1").prompt.user;
        let web = &of("w1").prompt.user;
        assert!(
            api.contains("- web/src/useInvoices.ts (+301 -0): adds useInvoices, filler_w1_0"),
            "{api}"
        );
        assert!(!api.contains("- api/routes/invoice.ts ("));
        assert!(
            web.contains("; routes POST /invoices; calls router.post"),
            "{web}"
        );
        // The section sits before the hunks.
        assert!(web.find("# The rest of this PR") < web.find("# Files and hunks"));
    }

    /// With head contents, the skeleton names the definition a change sits
    /// in even when the hunk shows none.
    #[test]
    fn skeletons_read_head_files_when_the_app_supplies_them() {
        let mut h = mk_hunk("s1", 0);
        h.new_start = 3;
        h.lines = vec![DiffLine {
            kind: LineKind::Add,
            text: "    return total * rate;".into(),
            old_no: None,
            new_no: Some(3),
        }];
        let rev = Revision {
            base: Sha::new("b"),
            head: Sha::new("h"),
            files: vec![
                mk_file("core/tax.py", vec![h]),
                file_with_lines("web/view.ts", "w1", &["export const shown = 1;"], 300),
            ],
        };
        let mut ctx = RepoContext::default();
        ctx.head_files.insert(
            "core/tax.py".into(),
            "class Tax:\n  def apply(self, total, rate):\n    return total * rate;\n".into(),
        );
        let prepass = crate::prepass::run(&rev);
        let graph = RelGraph::build(&rev, &prepass, &ctx);
        let whole = build_prompts_with(
            &PrMeta::default(),
            &rev,
            &prepass,
            &graph,
            &ctx,
            &full_excerpts(),
        );
        let budget = PromptBudget {
            max_chars: whole[0].prompt.user.len() * 7 / 10,
            ..full_excerpts()
        };
        let chunks = build_prompts_with(&PrMeta::default(), &rev, &prepass, &graph, &ctx, &budget);
        let web = chunks
            .iter()
            .find(|c| c.hunk_ids.contains(&HunkId::new("w1")))
            .unwrap();
        assert!(
            web.prompt
                .user
                .contains("- core/tax.py (+1 -0): edits Tax.apply"),
            "{}",
            web.prompt.user
        );
    }

    #[test]
    fn related_section_is_absent_when_nothing_is_related() {
        let rev = Revision {
            base: Sha::new("b"),
            head: Sha::new("h"),
            files: vec![
                file_with_lines("a/one.ts", "h1", &["export const alpha = 1;"], 0),
                file_with_lines("b/two.ts", "h2", &["export const beta = 2;"], 0),
            ],
        };
        let chunks = build_prompts(
            &PrMeta::default(),
            &rev,
            &crate::prepass::run(&rev),
            &PromptBudget::default(),
        );
        assert!(!chunks[0].prompt.user.contains("# Related changes"));
    }

    #[test]
    fn rollouts_are_listed_once_not_pair_by_pair() {
        let files: Vec<FileDiff> = ["audio", "fans", "fave", "feed"]
            .iter()
            .map(|n| {
                let mut h = mk_hunk(&format!("h-{n}"), 0);
                h.lines = vec![
                    line(
                        LineKind::Del,
                        &format!("$controller = new {n}Controller();"),
                    ),
                    line(LineKind::Del, "$controller->handle();"),
                    line(
                        LineKind::Add,
                        &format!("al_dispatch(\\VK\\Controllers\\{n}Controller::class);"),
                    ),
                ];
                mk_file(&format!("al_{n}.php"), vec![h])
            })
            .collect();
        let rev = Revision {
            base: Sha::new("b"),
            head: Sha::new("h"),
            files,
        };
        let chunks = build_prompts(
            &PrMeta::default(),
            &rev,
            &crate::prepass::run(&rev),
            &PromptBudget::default(),
        );
        let user = &chunks[0].prompt.user;
        assert!(
            user.contains("- 4 files make the same edit: al_audio.php, al_fans.php"),
            "{user}"
        );
        assert!(!user.contains("al_audio.php ~ al_fans.php"), "{user}");
    }

    /// Code-intelligence links show hunk to hunk, one line per defining
    /// hunk and symbol; a link to a hunk outside the prompt is left out,
    /// and without links there is no section at all.
    #[test]
    fn symbol_links_are_listed_per_defining_hunk() {
        let rev = Revision {
            base: Sha::new("b"),
            head: Sha::new("h"),
            files: vec![
                mk_file("src/cache.rs", vec![mk_hunk("c1", 2), mk_hunk("c2", 2)]),
                mk_file("src/api.rs", vec![mk_hunk("a1", 2)]),
            ],
        };
        let link = |from: &str, to: &str, symbol: &str| SymbolLink {
            from: HunkId::new(from),
            to: HunkId::new(to),
            symbol: symbol.into(),
        };
        let ctx = RepoContext {
            symbol_links: vec![
                link("c1", "a1", "evict_older"),
                link("c1", "c2", "evict_older"),
                link("c2", "a1", "CacheKey"),
                link("c2", "gone", "CacheKey"),
            ],
            ..RepoContext::default()
        };
        let prepass = crate::prepass::run(&rev);
        let graph = RelGraph::build(&rev, &prepass, &ctx);
        let chunks = build_prompts_with(
            &PrMeta::default(),
            &rev,
            &prepass,
            &graph,
            &ctx,
            &PromptBudget::default(),
        );
        let user = &chunks[0].prompt.user;
        assert!(user.contains("\n# Symbol links\n"), "{user}");
        assert!(user.contains("\n- c1 `evict_older` → a1, c2\n"), "{user}");
        assert!(user.contains("\n- c2 `CacheKey` → a1\n"), "{user}");
        assert!(!user.contains("gone"), "{user}");
        assert!(user.find("# Symbol links") < user.find("# Files and hunks"));

        let plain = build_prompts(&PrMeta::default(), &rev, &prepass, &PromptBudget::default());
        assert!(!plain[0].prompt.user.contains("# Symbol links"));
    }
}
