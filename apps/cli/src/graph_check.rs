//! `theodore eval --graph-only`: the relatedness graph on its own, no agent
//! calls. Prints, per fixture, how the graph partitions the PR and — with
//! `--compare <run dir>` — how well its communities and the chunks built
//! from them line up with a saved grouping (pairwise "same group"
//! precision/recall over hunks), next to the directory-only baseline.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use theodore_domain::grouping::RepoContext;
use theodore_domain::{GroupKind, Grouping, HunkId};
use theodore_grouping::graph::RelGraph;
use theodore_grouping::prompt::{PromptBudget, build_prompts_with};
use theodore_grouping::{PrMeta, prepass};

use crate::eval::Fixture;

/// Pairwise agreement of two partitions over the same items.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PairScore {
    /// Pairs both partitions put together.
    pub both: u64,
    /// Pairs the predicted partition puts together.
    pub predicted: u64,
    /// Pairs the reference partition puts together.
    pub reference: u64,
}

impl PairScore {
    pub fn precision(&self) -> f64 {
        ratio(self.both, self.predicted)
    }
    pub fn recall(&self) -> f64 {
        ratio(self.both, self.reference)
    }
}

impl std::ops::AddAssign for PairScore {
    fn add_assign(&mut self, o: Self) {
        self.both += o.both;
        self.predicted += o.predicted;
        self.reference += o.reference;
    }
}

fn ratio(a: u64, b: u64) -> f64 {
    if b == 0 { 1.0 } else { a as f64 / b as f64 }
}

/// `items[i] = (predicted label, reference label, file)`; with
/// `cross_file`, pairs within one file are skipped (a file-level partition
/// trivially keeps a file's hunks together).
pub fn pair_score(items: &[(usize, usize, usize)], cross_file: bool) -> PairScore {
    let mut s = PairScore::default();
    for (x, a) in items.iter().enumerate() {
        for b in &items[x + 1..] {
            if cross_file && a.2 == b.2 {
                continue;
            }
            let p = a.0 == b.0;
            let r = a.1 == b.1;
            s.predicted += u64::from(p);
            s.reference += u64::from(r);
            s.both += u64::from(p && r);
        }
    }
    s
}

fn load_grouping(dir: &Path, name: &str) -> Result<Option<Grouping>> {
    let path = dir.join(format!("{name}.json"));
    if !path.exists() {
        return Ok(None);
    }
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)
        .with_context(|| format!("parsing {}", path.display()))?;
    Ok(Some(serde_json::from_value(v["grouping"].clone())?))
}

fn fmt(s: &PairScore) -> String {
    format!(
        "{:>4.0}% {:>4.0}%",
        100.0 * s.precision(),
        100.0 * s.recall()
    )
}

pub fn run(
    fixtures: &[(String, Fixture)],
    compare: Option<&Path>,
    budget: &PromptBudget,
    show: bool,
) -> Result<()> {
    println!(
        "{:<20} {:>5} {:>5} {:>4} {:>5} {:>6} {:>13} | {:<11} {:<11} {:<11} {:<11} {:<11}",
        "fixture",
        "files",
        "edges",
        "hubs",
        "comms",
        "chunks",
        "dir->graph",
        "graph P/R",
        "graph xP/R",
        "dir P/R",
        "dir xP/R",
        "chunk R",
    );
    let mut totals = [PairScore::default(); 4];
    let mut compared = 0;
    let mut replays: Vec<(String, [Replay; 2])> = Vec::new();
    // (fixture, links, same group in the compared run, across files)
    let mut links: Vec<(String, usize, usize, usize)> = Vec::new();
    for (name, fx) in fixtures {
        let rev = &fx.revision;
        let pre = prepass::run(rev);
        let ctx = RepoContext {
            symbol_links: fx.symbol_links.clone(),
            ..RepoContext::default()
        };
        let graph = RelGraph::build(rev, &pre, &ctx);
        let pr: PrMeta = fx.pr.clone().into();
        let new_chunks = build_prompts_with(&pr, rev, &pre, &graph, &ctx, budget);
        let dir_chunks = build_prompts_with(&pr, rev, &pre, &RelGraph::default(), &ctx, budget);
        let comms = graph.communities();
        let multi = comms.iter().filter(|c| c.len() > 1).count();
        let hubs = (0..graph.files.len()).filter(|&i| graph.is_hub(i)).count();
        let edges = (0..graph.files.len())
            .flat_map(|a| (a + 1..graph.files.len()).map(move |b| (a, b)))
            .filter(|&(a, b)| graph.connection(a, b) > 0.0)
            .count();

        let mut cols = vec![String::new(); 5];
        if let Some(dir) = compare
            && let Some(reference) = load_grouping(dir, name)?
        {
            let group_of: BTreeMap<&HunkId, usize> = reference
                .groups
                .iter()
                .enumerate()
                .filter(|(_, g)| g.kind != GroupKind::Hidden)
                .flat_map(|(i, g)| g.hunks.iter().map(move |h| (h, i)))
                .collect();
            let chunk_of = |chunks: &[theodore_grouping::prompt::PromptChunk]| {
                chunks
                    .iter()
                    .enumerate()
                    .flat_map(|(i, c)| c.hunk_ids.iter().map(move |h| (h.clone(), i)))
                    .collect::<BTreeMap<HunkId, usize>>()
            };
            let new_chunk = chunk_of(&new_chunks);
            let mut dir_ids: BTreeMap<&str, usize> = BTreeMap::new();
            let mut graph_items = vec![];
            let mut dir_items = vec![];
            let mut chunk_items = vec![];
            for f in rev.files.iter().filter(|f| !pre.is_hidden(&f.path)) {
                let Some(file) = graph.index_of(&f.path) else {
                    continue;
                };
                let top = f.path.split_once('/').map_or("", |(d, _)| d);
                let next = dir_ids.len();
                let d = *dir_ids.entry(top).or_insert(next);
                for h in &f.hunks {
                    let Some(&g) = group_of.get(&h.id) else {
                        continue;
                    };
                    graph_items.push((graph.community_of(file), g, file));
                    dir_items.push((d, g, file));
                    chunk_items.push((new_chunk.get(&h.id).copied().unwrap_or(0), g, file));
                }
            }
            let scores = [
                pair_score(&graph_items, false),
                pair_score(&graph_items, true),
                pair_score(&dir_items, false),
                pair_score(&dir_items, true),
            ];
            for (t, s) in totals.iter_mut().zip(&scores) {
                *t += *s;
            }
            if !graph.hunk_links().is_empty() {
                let (together, cross) = link_agreement(&graph, &group_of);
                links.push((name.clone(), graph.hunk_links().len(), together, cross));
            }
            compared += 1;
            replays.push((name.clone(), replay_push(rev, &reference, &graph)));
            cols = scores.iter().map(fmt).collect();
            cols.push(format!(
                "{:>4.0}%",
                100.0 * pair_score(&chunk_items, false).recall()
            ));
        }
        println!(
            "{:<20} {:>5} {:>5} {:>4} {:>5} {:>6} {:>13} | {:<11} {:<11} {:<11} {:<11} {:<11}",
            name,
            graph.files.len(),
            edges,
            hubs,
            multi,
            new_chunks.len(),
            format!("{} -> {}", dir_chunks.len(), new_chunks.len()),
            cols[0],
            cols[1],
            cols[2],
            cols[3],
            cols[4],
        );
        if show {
            print_detail(&graph, &new_chunks);
        }
    }
    if compared > 1 {
        println!(
            "{:<20} {:>5} {:>5} {:>4} {:>5} {:>6} {:>13} | {:<11} {:<11} {:<11} {:<11}",
            "(all pairs)",
            "",
            "",
            "",
            "",
            "",
            "",
            fmt(&totals[0]),
            fmt(&totals[1]),
            fmt(&totals[2]),
            fmt(&totals[3]),
        );
    }
    println!(
        "\nP/R: pairwise precision/recall of \"same group\" over hunks against the compared run; \
x = only pairs across different files; chunk R = share of the compared run's same-group \
pairs that the new chunking keeps in one prompt."
    );
    if !links.is_empty() {
        println!(
            "\nSymbol links (code intelligence) against the compared run:\n{:<20} {:>6} {:>12} {:>12}",
            "fixture", "links", "same group", "cross-file"
        );
        let (mut n, mut t) = (0, 0);
        for (name, count, together, cross) in &links {
            n += count;
            t += together;
            println!("{name:<20} {count:>6} {together:>12} {cross:>12}");
        }
        println!(
            "{:<20} {n:>6} {:>11.0}%",
            "(all)",
            100.0 * t as f64 / n.max(1) as f64
        );
    }
    if !replays.is_empty() {
        println!(
            "\nPush replay (incremental regroup suggestions against the compared run):\n\
{:<20} {:>28} {:>28}",
            "fixture", "whole file new: sugg / right", "one hunk new: sugg / right"
        );
        for (name, [file, hunk]) in &replays {
            println!("{:<20} {:>28} {:>28}", name, file.show(), hunk.show());
        }
        println!(
            "whole file new: each file's hunks in turn arrive in a push; one hunk new: each \
hunk of a multi-hunk file in turn. sugg = hunks given a suggested group, right = of those, \
the compared run's group."
        );
    }
    Ok(())
}

/// Of the graph's hunk links between hunks the reference grouping shows:
/// how many it put in one group, and how many join two different files.
fn link_agreement(graph: &RelGraph, group_of: &BTreeMap<&HunkId, usize>) -> (usize, usize) {
    let mut together = 0;
    let mut cross = 0;
    for l in graph.hunk_links() {
        if let (Some(a), Some(b)) = (group_of.get(&l.from), group_of.get(&l.to))
            && a == b
        {
            together += 1;
        }
        if graph.file_of(&l.from) != graph.file_of(&l.to) {
            cross += 1;
        }
    }
    (together, cross)
}

/// Hunks replayed as new, how many got a suggestion, how many of those
/// named the reference group.
#[derive(Debug, Clone, Copy, Default)]
struct Replay {
    hunks: usize,
    suggested: usize,
    right: usize,
}

impl Replay {
    fn show(&self) -> String {
        let pct = |a: usize, b: usize| {
            if b == 0 {
                0.0
            } else {
                100.0 * a as f64 / b as f64
            }
        };
        format!(
            "{}/{} ({:.0}%) / {} ({:.0}%)",
            self.suggested,
            self.hunks,
            pct(self.suggested, self.hunks),
            self.right,
            pct(self.right, self.suggested)
        )
    }
}

/// Replay pushes on a saved grouping: treat a whole file's hunks, then each
/// single hunk of a multi-hunk file, as new, and check the group the
/// incremental regroup would suggest against where the reference put it.
fn replay_push(
    rev: &theodore_domain::Revision,
    reference: &Grouping,
    graph: &RelGraph,
) -> [Replay; 2] {
    use theodore_domain::interdiff::HunkChange;
    let group_of: BTreeMap<&HunkId, &theodore_domain::GroupId> = reference
        .groups
        .iter()
        .flat_map(|g| g.hunks.iter().map(move |h| (h, &g.id)))
        .collect();
    let mut out = [Replay::default(); 2];
    for f in rev
        .files
        .iter()
        .filter(|f| graph.index_of(&f.path).is_some())
    {
        let mut cases: Vec<(usize, Vec<&HunkId>)> =
            vec![(0, f.hunks.iter().map(|h| &h.id).collect())];
        if f.hunks.len() > 1 {
            cases.extend(f.hunks.iter().map(|h| (1, vec![&h.id])));
        }
        for (variant, new) in cases {
            let changes: BTreeMap<HunkId, HunkChange> = rev
                .hunks()
                .map(|h| {
                    let c = if new.contains(&&h.id) {
                        HunkChange::New
                    } else {
                        HunkChange::Unchanged
                    };
                    (h.id.clone(), c)
                })
                .collect();
            let suggested =
                theodore_grouping::incremental::suggested_groups(reference, rev, &changes, graph);
            for h in new {
                let r = &mut out[variant];
                r.hunks += 1;
                if let Some(g) = suggested.get(h) {
                    r.suggested += 1;
                    r.right += usize::from(group_of.get(h) == Some(&g));
                }
            }
        }
    }
    out
}

fn print_detail(graph: &RelGraph, chunks: &[theodore_grouping::prompt::PromptChunk]) {
    for (i, c) in graph.communities().iter().enumerate() {
        if c.len() < 2 {
            continue;
        }
        let names: Vec<String> = c
            .iter()
            .map(|&f| {
                let hub = if graph.is_hub(f) { "*" } else { "" };
                format!("{}{hub}", graph.files[f])
            })
            .collect();
        println!("    community {i} ({}): {}", c.len(), names.join(", "));
    }
    for r in &graph.rollouts {
        let names: Vec<&str> = r.files.iter().map(|&f| graph.files[f].as_str()).collect();
        println!(
            "    rollout ({} files) {}: {}",
            names.len(),
            r.describe(),
            names.join(", ")
        );
    }
    for c in chunks {
        println!(
            "    chunk `{}`: {} hunks, {} chars",
            c.label,
            c.hunk_ids.len(),
            c.prompt.user.len()
        );
        let files: std::collections::BTreeSet<&str> = c
            .hunk_ids
            .iter()
            .filter_map(|h| graph.file_of(h))
            .map(|f| graph.files[f].as_str())
            .collect();
        println!(
            "      files: {}",
            files.into_iter().collect::<Vec<_>>().join(", ")
        );
        let user = &c.prompt.user;
        if let Some(start) = user.find("# Related changes") {
            let end = user[start..]
                .find("\n# Files and hunks")
                .map_or(user.len(), |e| start + e);
            for l in user[start..end].lines().skip(2) {
                println!("      {l}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pair_score_counts_agreeing_pairs() {
        // predicted {a,b}{c}, reference {a,b,c}
        let items = [(0, 0, 0), (0, 0, 1), (1, 0, 2)];
        let s = pair_score(&items, false);
        assert_eq!(
            s,
            PairScore {
                both: 1,
                predicted: 1,
                reference: 3
            }
        );
        assert_eq!(s.precision(), 1.0);
        assert!((s.recall() - 1.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn cross_file_skips_same_file_pairs() {
        let items = [(0, 0, 0), (0, 0, 0), (0, 1, 1)];
        let s = pair_score(&items, true);
        assert_eq!(
            s,
            PairScore {
                both: 0,
                predicted: 2,
                reference: 0
            }
        );
    }
}
