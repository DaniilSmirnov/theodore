//! Grouping invariants (plan §5.1): a grouping is valid only if it is a
//! partition of the revision's hunk ids.

use std::collections::{BTreeMap, BTreeSet};

use crate::model::{Group, GroupId, GroupKind, Grouping, HunkId, Revision};

/// Version of the grouping algorithm, stored next to each grouping row
/// (`"algo"`). A stored grouping from an older version is recomputed the
/// next time its PR is opened. Bump it when grouping output changes in a
/// way worth redoing old PRs for.
pub const ALGO_VERSION: u64 = 3;

/// Repository facts beyond the diff that help tell which changed files
/// belong together (relatedness graph in `theodore-grouping`). Everything
/// is optional: grouping works from the diff alone, this only sharpens it
/// when a worktree is at hand.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoContext {
    /// Head contents of changed text files, by repo-relative path, so
    /// imports outside the diff's hunks are seen too.
    pub head_files: BTreeMap<String, String>,
    /// Recent history before the PR: for each commit, the PR's changed paths
    /// it touched. Commits that touched more than
    /// [`CO_CHANGE_MAX_FILES`] files in total are already left out (bulk
    /// reformatting or vendoring says nothing about relatedness).
    pub co_change_commits: Vec<Vec<String>>,
    /// The project config's `review:` rules (`hide`, `skim`, the system
    /// note from `instructions` and `context`).
    pub review: crate::review_rules::ReviewRules,
    /// Hunk-to-hunk links found by code intelligence: a symbol defined in
    /// one changed hunk and used in another. Empty without code
    /// intelligence (or when it ran out of time).
    pub symbol_links: Vec<SymbolLink>,
}

/// `from` changes the definition of `symbol`; `to`, another changed hunk,
/// uses it ("changed here" ↔ "called there").
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct SymbolLink {
    pub from: HunkId,
    pub to: HunkId,
    pub symbol: String,
}

/// Commits touching more files than this are ignored for co-change.
pub const CO_CHANGE_MAX_FILES: usize = 12;

/// How many commits of history to scan for co-change.
pub const CO_CHANGE_MAX_COMMITS: usize = 1000;

/// Grouping row data: the grouping plus the algorithm version that made it.
pub fn to_row(grouping: &Grouping) -> serde_json::Result<serde_json::Value> {
    let mut v = serde_json::to_value(grouping)?;
    if let Some(o) = v.as_object_mut() {
        o.insert("algo".into(), ALGO_VERSION.into());
    }
    Ok(v)
}

/// [`to_row`], plus the hash of the review config it was made with
/// (`ReviewRules::grouping_hash`); `None` leaves it out.
pub fn to_row_with(
    grouping: &Grouping,
    review_hash: Option<&str>,
) -> serde_json::Result<serde_json::Value> {
    let mut v = to_row(grouping)?;
    if let (Some(o), Some(h)) = (v.as_object_mut(), review_hash) {
        o.insert("review_hash".into(), h.into());
    }
    Ok(v)
}

/// The review config hash a stored grouping row was made with.
pub fn row_review_hash(data: &serde_json::Value) -> Option<&str> {
    data.get("review_hash").and_then(|v| v.as_str())
}

/// The algorithm version a stored grouping row was made with (0 = before
/// versions were recorded).
pub fn row_algo(data: &serde_json::Value) -> u64 {
    data.get("algo").and_then(|v| v.as_u64()).unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GroupingError {
    #[error("hunk {0} is in no group")]
    Missing(HunkId),
    #[error("hunk {hunk} is in several groups: {groups:?}")]
    Duplicate { hunk: HunkId, groups: Vec<GroupId> },
    #[error("unknown hunk id {0}")]
    Unknown(HunkId),
    #[error("duplicate group id {0}")]
    DuplicateGroup(GroupId),
    #[error("group {0} has no hunks")]
    EmptyGroup(GroupId),
    #[error("group {group}: outlier {hunk} is not one of its hunks")]
    OutlierNotInGroup { group: GroupId, hunk: HunkId },
    #[error("group {0}: empty title")]
    EmptyTitle(GroupId),
}

/// Every error found, not only the first — the list is fed back to the agent
/// in the repair call.
pub fn validate(rev: &Revision, grouping: &Grouping) -> Result<(), Vec<GroupingError>> {
    let known: BTreeSet<&HunkId> = rev.hunks().map(|h| &h.id).collect();
    let mut errors = Vec::new();
    let mut seen_groups = BTreeSet::new();
    let mut owners: BTreeMap<&HunkId, Vec<GroupId>> = BTreeMap::new();

    for g in &grouping.groups {
        if !seen_groups.insert(&g.id) {
            errors.push(GroupingError::DuplicateGroup(g.id.clone()));
        }
        if g.title.trim().is_empty() {
            errors.push(GroupingError::EmptyTitle(g.id.clone()));
        }
        if g.hunks.is_empty() {
            errors.push(GroupingError::EmptyGroup(g.id.clone()));
        }
        for h in &g.hunks {
            if !known.contains(h) {
                errors.push(GroupingError::Unknown(h.clone()));
            }
            owners.entry(h).or_default().push(g.id.clone());
        }
        let own: BTreeSet<&HunkId> = g.hunks.iter().collect();
        for o in &g.outliers {
            if !own.contains(o) {
                errors.push(GroupingError::OutlierNotInGroup {
                    group: g.id.clone(),
                    hunk: o.clone(),
                });
            }
        }
    }
    for (hunk, groups) in &owners {
        if groups.len() > 1 && known.contains(hunk) {
            errors.push(GroupingError::Duplicate {
                hunk: (*hunk).clone(),
                groups: groups.clone(),
            });
        }
    }
    for h in rev.hunks() {
        if !owners.contains_key(&h.id) {
            errors.push(GroupingError::Missing(h.id.clone()));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Put every hunk not in any group into a trailing "Other" group and drop
/// unknown / duplicate assignments (first wins). Result always validates.
pub fn repair_with_other(rev: &Revision, grouping: Grouping) -> Grouping {
    let known: BTreeSet<HunkId> = rev.hunks().map(|h| h.id.clone()).collect();
    let mut taken: BTreeSet<HunkId> = BTreeSet::new();
    let mut ids: BTreeSet<GroupId> = BTreeSet::new();
    let mut groups = Vec::new();

    for mut g in grouping.groups {
        if !ids.insert(g.id.clone()) {
            continue;
        }
        g.hunks
            .retain(|h| known.contains(h) && taken.insert(h.clone()));
        let own: BTreeSet<HunkId> = g.hunks.iter().cloned().collect();
        g.outliers.retain(|o| own.contains(o));
        if g.title.trim().is_empty() {
            g.title = g.id.0.clone();
        }
        if !g.hunks.is_empty() {
            groups.push(g);
        }
    }
    let rest: Vec<HunkId> = rev
        .hunks()
        .map(|h| h.id.clone())
        .filter(|h| !taken.contains(h))
        .collect();
    if !rest.is_empty() {
        let mut id = GroupId::new("other");
        let mut n = 2;
        while ids.contains(&id) {
            id = GroupId(format!("other-{n}"));
            n += 1;
        }
        groups.push(Group {
            id,
            title: "Other".into(),
            why: "Changes the grouping did not place.".into(),
            kind: GroupKind::Normal,
            risk: None,
            check: vec![],
            hunks: rest,
            outliers: vec![],
        });
    }
    Grouping {
        rev: grouping.rev,
        groups,
        in_progress: false,
    }
}

#[cfg(test)]
mod algo_tests {
    use super::*;
    use crate::model::Sha;

    #[test]
    fn rows_record_the_algorithm_version() {
        let g = Grouping {
            rev: Sha::new("h"),
            groups: vec![],
            in_progress: false,
        };
        let row = to_row(&g).unwrap();
        assert_eq!(row_algo(&row), ALGO_VERSION);
        let back: Grouping = serde_json::from_value(row).unwrap();
        assert_eq!(back, g);
        assert_eq!(
            row_algo(&serde_json::json!({ "rev": "h", "groups": [] })),
            0
        );
    }
}
