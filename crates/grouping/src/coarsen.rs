//! Deterministic post-pass: once a grouping has more groups than the PR's
//! size-scaled target (`target::target_group_count`) allows, merge groups
//! with exactly one hunk into the same-kind group it is most related to
//! (relatedness graph: imports, shared names, same edit, same file), or —
//! when it is related to none — the nearest one sharing its merge-area
//! (see `merge_area` below), until the target is met or no more such
//! merges exist.
//!
//! No agent call involved, so it is both free and exact: it only ever moves
//! a hunk from one group to another, never drops, duplicates or invents
//! one — if the input was a valid partition of the revision's hunks, so is
//! the output (`domain::grouping::validate` is unaffected by this pass).
//! `Hidden` groups are never touched, either as donor or recipient — they
//! are not reviewed in detail and merging a real change into one, or a real
//! change out of one, would misrepresent it.

use std::collections::BTreeMap;

use theodore_domain::{Group, GroupKind, HunkId, Revision};

use crate::area::top_level_dir;
use crate::graph::RelGraph;

/// A group touching the tiny group's own file is related at least this much.
pub(crate) const SAME_FILE: f64 = 2.0;
/// Per code-intelligence link between the tiny group's hunk and a group's.
pub(crate) const HUNK_LINK: f64 = 1.0;

/// Merge tiny (<= 1 hunk) groups into their most related same-kind group
/// (falling back to the nearest same-area one) until
/// `groups.len() <= target_max` or no further merge is possible. Order of
/// the surviving groups is otherwise preserved.
pub(crate) fn merge_tiny_groups(
    rev: &Revision,
    mut groups: Vec<Group>,
    target_max: usize,
    graph: &RelGraph,
) -> Vec<Group> {
    if groups.len() <= target_max {
        return groups;
    }
    let area = hunk_areas(rev);

    loop {
        if groups.len() <= target_max {
            break;
        }
        let Some((victim, target)) = tiny_group_indices(&groups)
            .into_iter()
            .find_map(|v| pick_merge_target(&groups, &area, graph, v).map(|t| (v, t)))
        else {
            break; // no remaining tiny group has anywhere compatible to go.
        };
        let absorbed = groups.remove(victim);
        let target = if victim < target { target - 1 } else { target };
        merge_into(&mut groups[target], absorbed);
    }
    groups
}

/// hunk id -> its merge-matching "area": normally the file's top-level
/// directory, but see `merge_area` for the config/env/CI/docs exception.
fn hunk_areas(rev: &Revision) -> BTreeMap<HunkId, String> {
    rev.files
        .iter()
        .flat_map(|f| {
            let area = merge_area(&f.path);
            f.hunks.iter().map(move |h| (h.id.clone(), area.clone()))
        })
        .collect()
}

/// Shared pseudo-area for config/env/CI/docs files, regardless of their
/// literal top-level directory. A real PR scatters exactly these files
/// across the repo root, `.github/`, and a service's own directory even
/// though they all support one change — e.g. a root `.env.example` key, a
/// CI job's env var, a Dockerfile build arg and a changelog entry for the
/// same feature (seen on migratro#77: 4 such one-hunk groups the plain
/// top-level-directory match couldn't see were related). Everything else
/// still matches by directory.
const INFRA_AREA: &str = "\0infra";

fn merge_area(path: &str) -> String {
    if is_infra_file(path) {
        INFRA_AREA.to_string()
    } else {
        top_level_dir(path).to_string()
    }
}

fn is_infra_file(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let base = lower.rsplit('/').next().unwrap_or(&lower);
    base.starts_with(".env")
        // catches "Dockerfile", "Dockerfile.unified", "Dockerfile.prod", ...
        // but not an unrelated file like "dockerfile_parser.rs".
        || base == "dockerfile"
        || base.starts_with("dockerfile.")
        || base.ends_with(".dockerfile")
        // a changelog entry is often its own dated file inside a
        // `changelog/` directory, not literally named `CHANGELOG.md`.
        || lower.contains("changelog")
        || base == "readme.md"
        || lower.contains("/.github/workflows/")
        || lower.starts_with(".github/workflows/")
        || base == ".gitlab-ci.yml"
        || base == "jenkinsfile"
}

/// Indices of groups with at most one hunk, excluding `Hidden`, smallest
/// first (so a merge chain prefers absorbing the very smallest groups).
fn tiny_group_indices(groups: &[Group]) -> Vec<usize> {
    let mut idx: Vec<usize> = groups
        .iter()
        .enumerate()
        .filter(|(_, g)| g.kind != GroupKind::Hidden && g.hunks.len() <= 1)
        .map(|(i, _)| i)
        .collect();
    idx.sort_by_key(|&i| groups[i].hunks.len());
    idx
}

/// How related group `g` is to the files of `victim`'s hunks: summed graph
/// weight to each distinct file `g` touches (`SAME_FILE` for a shared one),
/// so a big group isn't preferred just for having many hunks of one file.
/// A code-intelligence link between a hunk of each adds `HUNK_LINK`: it
/// tells apart two groups touching the same files.
pub(crate) fn relatedness(graph: &RelGraph, victim: &Group, g: &Group) -> f64 {
    let files = |grp: &Group| -> std::collections::BTreeSet<usize> {
        grp.hunks.iter().filter_map(|h| graph.file_of(h)).collect()
    };
    let theirs = files(g);
    let by_file: f64 = files(victim)
        .iter()
        .flat_map(|&v| {
            theirs.iter().map(move |&f| {
                if v == f {
                    SAME_FILE
                } else {
                    graph.connection(v, f)
                }
            })
        })
        .sum();
    by_file + graph.hunk_links_between(&victim.hunks, &g.hunks) as f64 * HUNK_LINK
}

/// The same-kind group most related to `victim` in the graph (ties: the
/// nearest in reading order); if none is related at all, the nearest one
/// touching the same top-level area; `None` if there is neither.
fn pick_merge_target(
    groups: &[Group],
    area: &BTreeMap<HunkId, String>,
    graph: &RelGraph,
    victim: usize,
) -> Option<usize> {
    let kind = groups[victim].kind;
    let compatible =
        |(i, g): &(usize, &Group)| *i != victim && g.kind == kind && g.kind != GroupKind::Hidden;
    let related = groups
        .iter()
        .enumerate()
        .filter(compatible)
        .map(|(i, g)| (i, relatedness(graph, &groups[victim], g)))
        .filter(|(_, w)| *w > 0.0)
        .max_by(|a, b| {
            a.1.total_cmp(&b.1)
                .then_with(|| b.0.abs_diff(victim).cmp(&a.0.abs_diff(victim)))
        })
        .map(|(i, _)| i);
    if related.is_some() {
        return related;
    }
    let victim_area = groups[victim]
        .hunks
        .first()
        .and_then(|h| area.get(h))
        .cloned()?;
    groups
        .iter()
        .enumerate()
        .filter(compatible)
        .filter(|(_, g)| g.hunks.iter().any(|h| area.get(h) == Some(&victim_area)))
        .min_by_key(|(i, _)| i.abs_diff(victim))
        .map(|(i, _)| i)
}

pub(crate) fn merge_into(target: &mut Group, absorbed: Group) {
    if !absorbed.title.is_empty() && absorbed.title != target.title {
        target
            .why
            .push_str(&format!(" Also includes: {}.", absorbed.title));
    }
    target.check.extend(absorbed.check);
    target.hunks.extend(absorbed.hunks);
    target.outliers.extend(absorbed.outliers);
}

#[cfg(test)]
mod tests {
    use super::*;
    use theodore_domain::grouping;
    use theodore_domain::{DiffLine, FileDiff, FileStatus, GroupId, Grouping, Hunk, LineKind, Sha};

    fn line() -> DiffLine {
        DiffLine {
            kind: LineKind::Add,
            text: "x".into(),
            old_no: None,
            new_no: None,
        }
    }

    fn mk_hunk(id: &str) -> Hunk {
        Hunk {
            id: HunkId::new(id),
            path: String::new(),
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 1,
            header: String::new(),
            lines: vec![line()],
        }
    }

    fn mk_file(path: &str, hunk_ids: &[&str]) -> FileDiff {
        FileDiff {
            path: path.into(),
            old_path: None,
            status: FileStatus::Modified,
            similarity: None,
            old_mode: None,
            new_mode: None,
            flags: vec![],
            additions: 1,
            deletions: 0,
            hunks: hunk_ids.iter().map(|id| mk_hunk(id)).collect(),
        }
    }

    fn mk_group(id: &str, kind: GroupKind, hunks: &[&str]) -> Group {
        Group {
            id: GroupId::new(id),
            title: id.into(),
            why: "w".into(),
            kind,
            risk: None,
            check: vec![],
            hunks: hunks.iter().map(|h| HunkId::new(*h)).collect(),
            outliers: vec![],
        }
    }

    /// Revision backing the groups below: two areas ("a" and "b"), each
    /// with one real group plus a couple of one-hunk stragglers.
    fn rev() -> Revision {
        Revision {
            base: Sha::new("b"),
            head: Sha::new("h"),
            files: vec![
                mk_file("a/core.rs", &["a1", "a2"]),
                mk_file("a/env.rs", &["a3"]),
                mk_file("a/ci.yml", &["a4"]),
                mk_file("b/core.rs", &["b1", "b2"]),
                mk_file("b/docs.md", &["b3"]),
            ],
        }
    }

    fn groups_for(rev: &Revision, groups: Vec<Group>) -> Grouping {
        Grouping {
            rev: rev.head.clone(),
            groups,
            in_progress: false,
        }
    }

    #[test]
    fn below_target_is_a_no_op() {
        let r = rev();
        let groups = vec![
            mk_group("a-core", GroupKind::Normal, &["a1", "a2"]),
            mk_group("a-env", GroupKind::Normal, &["a3"]),
        ];
        let before = groups.clone();
        let after = merge_tiny_groups(&r, groups, 5, &RelGraph::default());
        assert_eq!(after, before);
    }

    #[test]
    fn merges_tiny_same_area_groups_into_their_neighbour() {
        let r = rev();
        let groups = vec![
            mk_group("a-core", GroupKind::Normal, &["a1", "a2"]),
            mk_group("a-env", GroupKind::Normal, &["a3"]),
            mk_group("a-ci", GroupKind::Normal, &["a4"]),
            mk_group("b-core", GroupKind::Normal, &["b1", "b2"]),
            mk_group("b-docs", GroupKind::Normal, &["b3"]),
        ];
        let after = merge_tiny_groups(&r, groups, 2, &RelGraph::default());
        assert_eq!(after.len(), 2, "should coarsen down to the target");
        // every hunk still placed in exactly one group, nothing lost.
        let g = groups_for(&r, after.clone());
        assert!(grouping::validate(&r, &g).is_ok());
        let a = after
            .iter()
            .find(|g| g.id == GroupId::new("a-core"))
            .unwrap();
        assert!(a.hunks.contains(&HunkId::new("a3")));
        assert!(a.hunks.contains(&HunkId::new("a4")));
        let b = after
            .iter()
            .find(|g| g.id == GroupId::new("b-core"))
            .unwrap();
        assert!(b.hunks.contains(&HunkId::new("b3")));
    }

    #[test]
    fn never_merges_a_hidden_group_as_donor_or_recipient() {
        let r = Revision {
            base: Sha::new("b"),
            head: Sha::new("h"),
            files: vec![mk_file("a/lock.json", &["h1"]), mk_file("a/x.rs", &["h2"])],
        };
        let groups = vec![
            mk_group("hidden", GroupKind::Hidden, &["h1"]),
            mk_group("solo", GroupKind::Normal, &["h2"]),
        ];
        let before = groups.clone();
        // target_max 1 would normally force a merge, but there is no
        // same-kind neighbour for either group (hidden is excluded, and
        // "solo" has no other candidate), so nothing should change.
        let after = merge_tiny_groups(&r, groups, 1, &RelGraph::default());
        assert_eq!(after, before);
    }

    #[test]
    fn does_not_merge_across_incompatible_kinds() {
        let r = Revision {
            base: Sha::new("b"),
            head: Sha::new("h"),
            files: vec![mk_file("a/x.rs", &["h1", "h2"]), mk_file("a/y.rs", &["h3"])],
        };
        let groups = vec![
            mk_group("normal", GroupKind::Normal, &["h1", "h2"]),
            mk_group("skim", GroupKind::Skim, &["h3"]),
        ];
        let before = groups.clone();
        let after = merge_tiny_groups(&r, groups, 1, &RelGraph::default());
        // "skim" is tiny and same-area, but not same-kind as "normal", so
        // there is no compatible target; the pass must leave it alone
        // rather than force an incorrect merge.
        assert_eq!(after, before);
    }

    /// Regression, found on migratro#77: a real PR scattered 4 one-hunk
    /// groups for `.env.example`, a CI workflow, a `Dockerfile` and
    /// `README.md` across 4 different top-level directories. Plain
    /// top-level-directory matching can't see they belong together, so the
    /// post-pass needs the config/env/CI/docs pseudo-area to merge them.
    #[test]
    fn scattered_config_env_ci_docs_files_merge_into_one_infra_group() {
        let r = Revision {
            base: Sha::new("b"),
            head: Sha::new("h"),
            files: vec![
                mk_file("a/core.rs", &["a1", "a2"]),
                mk_file(".env.example", &["e1"]),
                mk_file(".github/workflows/ci.yml", &["c1"]),
                mk_file("apps/shell/Dockerfile", &["d1"]),
                mk_file("README.md", &["r1"]),
            ],
        };
        let groups = vec![
            mk_group("core", GroupKind::Normal, &["a1", "a2"]),
            mk_group("env", GroupKind::Normal, &["e1"]),
            mk_group("ci", GroupKind::Normal, &["c1"]),
            mk_group("docker", GroupKind::Normal, &["d1"]),
            mk_group("readme", GroupKind::Normal, &["r1"]),
        ];
        let after = merge_tiny_groups(&r, groups, 2, &RelGraph::default());
        assert_eq!(
            after.len(),
            2,
            "the 4 scattered infra groups should all land in one"
        );
        let g = Grouping {
            rev: r.head.clone(),
            groups: after.clone(),
            in_progress: false,
        };
        assert!(grouping::validate(&r, &g).is_ok());
        let infra = after.iter().find(|g| g.id != GroupId::new("core")).unwrap();
        for id in ["e1", "c1", "d1", "r1"] {
            assert!(
                infra.hunks.contains(&HunkId::new(id)),
                "{id} should have joined the merged infra group"
            );
        }
    }

    /// Regression, found on the same real PR: a multi-stage Dockerfile is
    /// often named `Dockerfile.unified` / `Dockerfile.prod`, not literally
    /// `Dockerfile`, and a changelog entry is often its own dated file
    /// inside a `changelog/` directory, not `CHANGELOG.md`. Both must still
    /// be recognized as the shared infra area.
    #[test]
    fn recognizes_suffixed_dockerfiles_and_directory_based_changelogs() {
        assert!(is_infra_file("Dockerfile.unified"));
        assert!(is_infra_file("apps/shell/Dockerfile.prod"));
        assert!(is_infra_file("changelog/2026-10-15-some-change.md"));
        assert!(!is_infra_file("src/dockerfile_parser.rs"));
    }

    fn file_with(path: &str, id: &str, text: &str) -> FileDiff {
        let mut f = mk_file(path, &[id]);
        f.hunks[0].lines = vec![DiffLine {
            kind: LineKind::Add,
            text: text.into(),
            old_no: None,
            new_no: None,
        }];
        f
    }

    /// The graph beats directory proximity: a one-hunk change to a service
    /// joins the group of the controller that imports it, in another tree,
    /// not the nearer group that merely shares its top-level directory.
    #[test]
    fn prefers_the_most_related_group_over_the_nearest_same_area_one() {
        let r = Revision {
            base: Sha::new("b"),
            head: Sha::new("h"),
            files: vec![
                file_with(
                    "api/billing/route.ts",
                    "r1",
                    "import { charge } from '../../core/billing/charge';",
                ),
                file_with(
                    "core/billing/charge.ts",
                    "c1",
                    "export const chargeRetries = 3;",
                ),
                file_with(
                    "core/search/index.ts",
                    "s1",
                    "export const searchLimit = 10;",
                ),
                file_with("core/search/rank.ts", "s2", "export const rankBoost = 2;"),
            ],
        };
        let graph = RelGraph::build(&r, &crate::prepass::run(&r), &Default::default());
        let groups = vec![
            mk_group("billing-route", GroupKind::Normal, &["r1"]),
            mk_group("search", GroupKind::Normal, &["s1", "s2"]),
            mk_group("charge", GroupKind::Normal, &["c1"]),
        ];
        let after = merge_tiny_groups(&r, groups.clone(), 2, &graph);
        assert_eq!(after.len(), 2);
        let search = after
            .iter()
            .find(|g| g.id == GroupId::new("search"))
            .unwrap();
        assert_eq!(
            search.hunks.len(),
            2,
            "search group must not absorb anything"
        );
        // Without the graph the nearest same-area group would have won.
        let plain = merge_tiny_groups(&r, groups, 2, &RelGraph::default());
        let search = plain
            .iter()
            .find(|g| g.id == GroupId::new("search"))
            .unwrap();
        assert_eq!(search.hunks.len(), 3);
    }

    /// Two groups touch the tiny group's file equally; a code-intelligence
    /// link from its hunk to one of them decides, over reading order.
    #[test]
    fn a_symbol_link_breaks_a_tie_between_groups_of_the_same_file() {
        let r = Revision {
            base: Sha::new("b"),
            head: Sha::new("h"),
            files: vec![mk_file("src/cache.rs", &["c1", "c2", "c3", "c4"])],
        };
        let groups = vec![
            mk_group("evict", GroupKind::Normal, &["c1"]),
            mk_group("near", GroupKind::Normal, &["c2", "c3"]),
            mk_group("far", GroupKind::Normal, &["c4"]),
        ];
        let plain = RelGraph::build(&r, &crate::prepass::run(&r), &Default::default());
        let after = merge_tiny_groups(&r, groups.clone(), 2, &plain);
        assert_eq!(after[0].id, GroupId::new("near"));

        let ctx = theodore_domain::grouping::RepoContext {
            symbol_links: vec![theodore_domain::grouping::SymbolLink {
                from: HunkId::new("c4"),
                to: HunkId::new("c1"),
                symbol: "evict_older".into(),
            }],
            ..Default::default()
        };
        let linked = RelGraph::build(&r, &crate::prepass::run(&r), &ctx);
        let after = merge_tiny_groups(&r, groups, 2, &linked);
        let far = after.iter().find(|g| g.id.0 == "far").expect("far kept");
        assert_eq!(far.hunks, vec![HunkId::new("c4"), HunkId::new("c1")]);
        assert!(grouping::validate(&r, &groups_for(&r, after)).is_ok());
    }

    #[test]
    fn preserves_the_partition_invariant_on_a_larger_mixed_input() {
        let r = rev();
        let groups = vec![
            mk_group("a-core", GroupKind::Normal, &["a1", "a2"]),
            mk_group("a-env", GroupKind::Normal, &["a3"]),
            mk_group("a-ci", GroupKind::Skim, &["a4"]),
            mk_group("b-core", GroupKind::Normal, &["b1", "b2"]),
            mk_group("b-docs", GroupKind::Skim, &["b3"]),
        ];
        let after = merge_tiny_groups(&r, groups, 1, &RelGraph::default());
        let g = groups_for(&r, after);
        assert!(grouping::validate(&r, &g).is_ok());
    }
}
