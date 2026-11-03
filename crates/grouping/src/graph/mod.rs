//! Relatedness graph over a PR's changed files: which files belong to the
//! same change, independent of where they sit in the tree.
//!
//! ```text
//!  diff hunks ──┬─ import/require/use/include lines ─▶ import edges
//!  (+ head      ├─ names defined on changed lines ───▶ symbol edges (def ↔ use)
//!   files)      ├─ rare compound identifiers ────────▶ token edges
//!               ├─ identical edit shape ─────────────▶ rollout cliques
//!               └─ file-name roots, test ↔ source ───▶ name edges
//!  git history ──── co-change (Jaccard) ─────────────▶ co-change edges
//!  code intel ───── hunk defines X ↔ hunk uses X ─────▶ link edges
//!                   (`RepoContext::symbol_links`)       (+ hunk pairs kept
//!                                                        for prompts, coarsening)
//!                                │
//!            drop hub files ─▶ Louvain ─▶ communities ─▶ hubs rejoin
//!                                                        their strongest one
//! ```
//!
//! Directory layout is deliberately not a signal: a feature's router,
//! service, repository and UI hook live in different trees, and what
//! matters for review is that they change together. Everything here is
//! deterministic — same diff, same graph, same communities.

mod imports;
mod louvain;
pub(crate) mod names;
pub(crate) mod symbols;

use std::collections::{BTreeMap, BTreeSet};

use theodore_domain::grouping::{RepoContext, SymbolLink};
use theodore_domain::{FileDiff, HunkId, LineKind, Revision};

use crate::prepass::PrePass;

// Edge weights. Import edges are the backbone; the rest add evidence on
// top. Tuned on the eval fixtures (docs/eval/graph-grouping.md).
const W_IMPORT: f64 = 1.0;
const W_MENTION: f64 = 0.8;
const W_SYMBOL: f64 = 1.0;
const W_DEF_DEF: f64 = 0.5;
const W_CO_USE: f64 = 0.3;
const W_TOKEN: f64 = 0.5;
const W_SHAPE: f64 = 0.6;
const W_NAME: f64 = 0.8;
const W_TEST: f64 = 0.5;
const W_CO_CHANGE: f64 = 1.5;
/// Per distinct symbol linking two files' hunks (code intelligence): a
/// resolved reference, as strong as an import.
const W_LINK: f64 = 1.0;
/// Code-intelligence links kept from the context: plenty for a hint, and
/// bounded work for the graph.
const MAX_HUNK_LINKS: usize = 400;

/// A compound identifier shared by more files than this is vocabulary,
/// not a link.
const TOKEN_MAX_FILES: usize = 6;
/// A plain (non-compound, short) defined name links at most this many
/// using files: `State` or `Color` in thirty views is vocabulary.
const PLAIN_NAME_MAX_USERS: usize = 3;
/// Name-root groups larger than this are a naming convention, not a feature.
const NAME_MAX_FILES: usize = 8;
/// Rollouts (same edit / same new symbol) larger than this only get a
/// star, not a clique, to keep the edge count bounded.
const ROLLOUT_MAX_CLIQUE: usize = 120;
/// Co-change needs at least this many shared commits.
const CO_CHANGE_MIN: u32 = 2;
/// Head files bigger than this aren't parsed for imports.
const HEAD_FILE_MAX_BYTES: usize = 512 * 1024;

/// Why two files are related; summed into one weight.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Edge {
    /// `a` imports `b` / `b` imports `a` (`a < b` by file index).
    pub a_imports_b: bool,
    pub b_imports_a: bool,
    import: f64,
    /// One file names the other (`CMakeLists.txt` → `parser.c`).
    mention: f64,
    symbol: f64,
    /// The strongest defined name linking the two files.
    pub symbol_name: Option<String>,
    symbol_top: f64,
    token: f64,
    pub token_name: Option<String>,
    shape: f64,
    co_use: f64,
    name: f64,
    pub name_root: Option<String>,
    pub test_pair: bool,
    co_change: f64,
    /// A hunk of one file uses a symbol defined in a hunk of the other
    /// (code intelligence).
    link: f64,
    pub link_name: Option<String>,
}

impl Edge {
    pub fn weight(&self) -> f64 {
        self.import.min(2.0)
            + self.mention.min(1.0)
            + self.symbol.min(2.0)
            + self.token.min(1.0)
            + self.shape.min(1.5)
            + self.co_use.min(1.0)
            + self.name
            + if self.test_pair { W_TEST } else { 0.0 }
            + self.co_change
            + self.link.min(2.0)
    }

    /// Whether the edge is only "part of a larger rollout" — those are
    /// listed once per rollout in prompts, not pair by pair. A shared
    /// identifier on top of the same edit is part of that edit.
    fn rollout_only(&self) -> bool {
        self.import == 0.0
            && self.mention == 0.0
            && self.symbol_name.is_none()
            && (self.token == 0.0 || self.shape + self.co_use > 0.0)
            && self.name == 0.0
            && self.co_change == 0.0
            && self.link == 0.0
    }
}

/// A set of files making the same edit (identical shape) or all adopting
/// one newly defined name.
#[derive(Debug, Clone, PartialEq)]
pub struct Rollout {
    pub label: String,
    pub files: Vec<usize>,
    /// More names / edits shared by exactly the same files.
    pub extra: usize,
}

impl Rollout {
    /// "use the new `ApiMember` (+12 more)".
    pub fn describe(&self) -> String {
        if self.extra == 0 {
            self.label.clone()
        } else {
            format!("{} (+{} more)", self.label, self.extra)
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct RelGraph {
    /// Visible changed files, in revision order.
    pub files: Vec<String>,
    hunk_file: BTreeMap<HunkId, usize>,
    by_path: BTreeMap<String, usize>,
    edges: BTreeMap<(usize, usize), Edge>,
    hubs: BTreeSet<usize>,
    /// Community id per file (singletons included).
    community: Vec<usize>,
    pub rollouts: Vec<Rollout>,
    /// Files whose entire change is one same-edit rollout: kept in one
    /// community no matter what else they link to (`al_a.php` … `al_z.php`
    /// all switching to one dispatcher are one review step, even though
    /// each also imports its own controller).
    bundles: Vec<Vec<usize>>,
    /// Code-intelligence links between visible hunks, in context order.
    hunk_links: Vec<SymbolLink>,
    /// The same links as unordered hunk pairs.
    linked: BTreeSet<(HunkId, HunkId)>,
}

/// `(a, b)` with the smaller id first.
fn hunk_pair(a: &HunkId, b: &HunkId) -> (HunkId, HunkId) {
    if a < b {
        (a.clone(), b.clone())
    } else {
        (b.clone(), a.clone())
    }
}

#[derive(Default)]
struct FileFacts {
    /// name → (defined on an added line, defined on a deleted line)
    defs: BTreeMap<String, (bool, bool)>,
    changed_idents: BTreeSet<String>,
    /// Identifiers on deleted or context lines: names that existed before.
    old_idents: BTreeSet<String>,
    compound: BTreeSet<String>,
    shapes: BTreeSet<String>,
    /// Every non-skim hunk of the file has an edit shape: the file's whole
    /// change is (one or more) mechanical edits.
    only_shaped: bool,
}

pub(crate) fn is_prose(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [".md", ".mdx", ".rst", ".txt", ".adoc"]
        .iter()
        .any(|e| lower.ends_with(e))
}

fn facts(file: &FileDiff, prepass: &PrePass) -> FileFacts {
    let mut f = FileFacts {
        only_shaped: !file.hunks.is_empty(),
        ..Default::default()
    };
    // Prose mentions names (and links docs to the code they describe) but
    // never defines them.
    let defines = !is_prose(&file.path);
    for h in &file.hunks {
        for l in &h.lines {
            if l.text.len() > 400 {
                continue;
            }
            let added = match l.kind {
                LineKind::Add => true,
                LineKind::Del => false,
                LineKind::Ctx => {
                    // Unchanged lines show what already existed.
                    f.old_idents
                        .extend(symbols::identifiers(&l.text).map(str::to_string));
                    continue;
                }
            };
            if defines {
                for d in symbols::definitions(&l.text) {
                    let e = f.defs.entry(d).or_default();
                    if added {
                        e.0 = true;
                    } else {
                        e.1 = true;
                    }
                }
            }
            for t in symbols::identifiers(&l.text) {
                if symbols::is_stop(t) {
                    continue;
                }
                if symbols::is_compound(t) {
                    f.compound.insert(t.to_string());
                }
                if !added {
                    f.old_idents.insert(t.to_string());
                }
                f.changed_idents.insert(t.to_string());
            }
        }
        if prepass.skim.contains_key(&h.id) {
            continue;
        }
        let text = |k: LineKind| {
            h.lines
                .iter()
                .filter(move |l| l.kind == k)
                .map(|l| l.text.as_str())
        };
        match symbols::edit_shape(text(LineKind::Add), text(LineKind::Del)) {
            Some(s) => {
                f.shapes.insert(s);
            }
            None => f.only_shaped = false,
        }
    }
    f
}

impl RelGraph {
    /// Build the graph over `rev`'s non-hidden files.
    pub fn build(rev: &Revision, prepass: &PrePass, ctx: &RepoContext) -> Self {
        let visible: Vec<&FileDiff> = rev
            .files
            .iter()
            .filter(|f| !prepass.is_hidden(&f.path))
            .collect();
        let files: Vec<String> = visible.iter().map(|f| f.path.clone()).collect();
        let n = files.len();
        let mut g = RelGraph {
            files: files.clone(),
            by_path: files
                .iter()
                .enumerate()
                .map(|(i, p)| (p.clone(), i))
                .collect(),
            hunk_file: visible
                .iter()
                .enumerate()
                .flat_map(|(i, f)| f.hunks.iter().map(move |h| (h.id.clone(), i)))
                .collect(),
            ..Default::default()
        };
        let facts: Vec<FileFacts> = visible.iter().map(|f| facts(f, prepass)).collect();

        g.add_import_edges(&visible, ctx);
        g.add_symbol_edges(&facts);
        g.add_token_edges(&facts);
        g.add_shape_edges(&facts);
        g.add_name_edges();
        g.add_co_change_edges(ctx);
        g.add_link_edges(ctx);
        g.hubs = g.find_hubs();
        g.community = g.detect_communities();
        debug_assert_eq!(g.community.len(), n);
        g
    }

    fn edge(&mut self, a: usize, b: usize) -> Option<&mut Edge> {
        if a == b {
            return None;
        }
        Some(self.edges.entry((a.min(b), a.max(b))).or_default())
    }

    fn add_import_edges(&mut self, visible: &[&FileDiff], ctx: &RepoContext) {
        let idx = imports::PathIndex::new(&self.files);
        let mut refs: Vec<BTreeSet<usize>> = Vec::with_capacity(visible.len());
        let mut mentions: Vec<BTreeSet<usize>> = Vec::with_capacity(visible.len());
        for (i, f) in visible.iter().enumerate() {
            let hunk_lines = f
                .hunks
                .iter()
                .flat_map(|h| h.lines.iter().map(|l| l.text.as_str()));
            let head = ctx
                .head_files
                .get(&f.path)
                .filter(|s| s.len() <= HEAD_FILE_MAX_BYTES)
                .map(|s| s.lines())
                .into_iter()
                .flatten();
            refs.push(imports::references(&idx, i, hunk_lines.chain(head)));
            let changed = f.hunks.iter().flat_map(|h| {
                h.lines
                    .iter()
                    .filter(|l| l.kind != LineKind::Ctx)
                    .map(|l| l.text.as_str())
            });
            let named = imports::mentions(&idx, i, changed);
            mentions.push(named);
        }
        let mut indeg = vec![0usize; visible.len()];
        for r in &refs {
            for &t in r {
                indeg[t] += 1;
            }
        }
        for (from, targets) in refs.iter().enumerate() {
            for &to in targets {
                let w = W_IMPORT / (indeg[to] as f64).sqrt();
                let e = self.edge(from, to).expect("references exclude self");
                e.import += w;
                if from < to {
                    e.a_imports_b = true;
                } else {
                    e.b_imports_a = true;
                }
            }
        }
        let mut named_by = vec![0usize; visible.len()];
        for m in &mentions {
            for &t in m {
                named_by[t] += 1;
            }
        }
        for (from, targets) in mentions.iter().enumerate() {
            for &to in targets {
                let w = W_MENTION / (named_by[to] as f64).sqrt();
                if let Some(e) = self.edge(from, to) {
                    e.mention += w;
                }
            }
        }
    }

    fn add_symbol_edges(&mut self, facts: &[FileFacts]) {
        let n = facts.len();
        let mut defined_in: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for (i, f) in facts.iter().enumerate() {
            for name in f.defs.keys() {
                defined_in.entry(name.as_str()).or_default().push(i);
            }
        }
        for (name, defs) in defined_in {
            let users: Vec<usize> = (0..n)
                .filter(|i| !defs.contains(i) && facts[*i].changed_idents.contains(name))
                .collect();
            let reach = defs.len() + users.len();
            let distinctive = symbols::is_distinctive(name);
            if reach < 2
                || (n >= 10 && reach as f64 > 0.6 * n as f64)
                || (!distinctive && users.len() > PLAIN_NAME_MAX_USERS)
            {
                continue;
            }
            if !users.is_empty() {
                let w = W_SYMBOL / (users.len() as f64).sqrt();
                for &d in &defs {
                    for &u in &users {
                        let e = self.edge(d, u).expect("users exclude defs");
                        if w > e.symbol_top {
                            e.symbol_top = w;
                            e.symbol_name = Some(name.to_string());
                        }
                        e.symbol += w;
                    }
                }
            }
            if (2..=6).contains(&defs.len()) {
                for (x, &a) in defs.iter().enumerate() {
                    for &b in &defs[x + 1..] {
                        let e = self.edge(a, b).expect("distinct");
                        if W_DEF_DEF > e.symbol_top {
                            e.symbol_top = W_DEF_DEF;
                            e.symbol_name = Some(name.to_string());
                        }
                        e.symbol += W_DEF_DEF;
                    }
                }
            }
            // A distinctive name this PR introduces (defined only on added
            // lines, never seen on a deleted or unchanged one) and adopts across many
            // files: the adopting files are one rollout, whatever their
            // folders.
            let new_name = distinctive
                && defs.iter().all(|&d| {
                    let (added, deleted) = facts[d].defs[name];
                    added && !deleted
                })
                && !facts.iter().any(|f| f.old_idents.contains(name));
            if new_name && users.len() >= 3 {
                let mut members = defs.clone();
                members.extend(&users);
                members.sort_unstable();
                self.add_clique(&users, W_CO_USE, |e| &mut e.co_use);
                self.push_rollout(format!("`{name}`"), members, "use the new");
            }
        }
    }

    /// Record a rollout; one with exactly the same files as an earlier one
    /// (forty protocol constants shared by the same eight files) is merged
    /// into it instead of listed again.
    fn push_rollout(&mut self, what: String, files: Vec<usize>, verb: &str) {
        if let Some(r) = self
            .rollouts
            .iter_mut()
            .find(|r| r.files == files && r.label.starts_with(verb))
        {
            r.extra += 1;
            return;
        }
        let label = if what.is_empty() {
            verb.to_string()
        } else {
            format!("{verb} {what}")
        };
        self.rollouts.push(Rollout {
            label,
            files,
            extra: 0,
        });
    }

    fn add_token_edges(&mut self, facts: &[FileFacts]) {
        let mut by_token: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for (i, f) in facts.iter().enumerate() {
            for t in &f.compound {
                if !f.defs.contains_key(t) {
                    by_token.entry(t.as_str()).or_default().push(i);
                }
            }
        }
        let defined: BTreeSet<&str> = facts
            .iter()
            .flat_map(|f| f.defs.keys().map(String::as_str))
            .collect();
        for (tok, holders) in by_token {
            if defined.contains(tok) || holders.len() < 2 || holders.len() > TOKEN_MAX_FILES {
                continue;
            }
            let w = W_TOKEN / (holders.len() - 1) as f64;
            for (x, &a) in holders.iter().enumerate() {
                for &b in &holders[x + 1..] {
                    let e = self.edge(a, b).expect("distinct");
                    if e.token_name.is_none() {
                        e.token_name = Some(tok.to_string());
                    }
                    e.token += w;
                }
            }
        }
    }

    fn add_shape_edges(&mut self, facts: &[FileFacts]) {
        let mut by_shape: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for (i, f) in facts.iter().enumerate() {
            for s in &f.shapes {
                by_shape.entry(s.as_str()).or_default().push(i);
            }
        }
        for (shape, holders) in by_shape {
            // Two files making the same short edit can be chance; a longer
            // identical edit, or three files, is a pattern.
            if holders.len() < 2 || (holders.len() == 2 && shape.len() < 40) {
                continue;
            }
            self.add_clique(&holders, W_SHAPE, |e| &mut e.shape);
            let bundle: Vec<usize> = holders
                .iter()
                .copied()
                .filter(|&i| facts[i].only_shaped && facts[i].shapes.len() == 1)
                .collect();
            if bundle.len() >= 3 {
                self.bundles.push(bundle);
            }
            if holders.len() >= 3 {
                self.push_rollout(String::new(), holders, "make the same edit");
            }
        }
    }

    fn add_clique(&mut self, members: &[usize], w: f64, field: impl Fn(&mut Edge) -> &mut f64) {
        if members.len() <= ROLLOUT_MAX_CLIQUE {
            for (x, &a) in members.iter().enumerate() {
                for &b in &members[x + 1..] {
                    if let Some(e) = self.edge(a, b) {
                        *field(e) += w;
                    }
                }
            }
        } else if let Some((&first, rest)) = members.split_first() {
            for &b in rest {
                if let Some(e) = self.edge(first, b) {
                    *field(e) += w;
                }
            }
        }
    }

    fn add_name_edges(&mut self) {
        let mut by_root: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, p) in self.files.iter().enumerate() {
            if let Some(r) = names::name_root(p) {
                by_root.entry(r).or_default().push(i);
            }
        }
        for (root, members) in by_root {
            if members.len() < 2 || members.len() > NAME_MAX_FILES {
                continue;
            }
            let w = W_NAME / (members.len() - 1) as f64;
            for (x, &a) in members.iter().enumerate() {
                for &b in &members[x + 1..] {
                    let test_pair =
                        names::is_test(&self.files[a]) != names::is_test(&self.files[b]);
                    let e = self.edge(a, b).expect("distinct");
                    e.name += w;
                    e.name_root = Some(root.clone());
                    e.test_pair |= test_pair;
                }
            }
        }
    }

    fn add_co_change_edges(&mut self, ctx: &RepoContext) {
        if ctx.co_change_commits.is_empty() {
            return;
        }
        let index: BTreeMap<&str, usize> = self
            .files
            .iter()
            .enumerate()
            .map(|(i, p)| (p.as_str(), i))
            .collect();
        let mut seen = vec![0u32; self.files.len()];
        let mut pairs: BTreeMap<(usize, usize), u32> = BTreeMap::new();
        for commit in &ctx.co_change_commits {
            let mut ids: Vec<usize> = commit
                .iter()
                .filter_map(|p| index.get(p.as_str()).copied())
                .collect();
            ids.sort_unstable();
            ids.dedup();
            for &i in &ids {
                seen[i] += 1;
            }
            for (x, &a) in ids.iter().enumerate() {
                for &b in &ids[x + 1..] {
                    *pairs.entry((a, b)).or_default() += 1;
                }
            }
        }
        for ((a, b), co) in pairs {
            if co < CO_CHANGE_MIN {
                continue;
            }
            let jaccard = f64::from(co) / f64::from(seen[a] + seen[b] - co);
            if let Some(e) = self.edge(a, b) {
                e.co_change += W_CO_CHANGE * jaccard;
            }
        }
    }

    /// Code-intelligence links (`RepoContext::symbol_links`) between
    /// visible hunks: kept per hunk pair, plus one file edge per distinct
    /// symbol across two files.
    fn add_link_edges(&mut self, ctx: &RepoContext) {
        let mut seen: BTreeSet<(usize, usize, &str)> = BTreeSet::new();
        for l in &ctx.symbol_links {
            if self.hunk_links.len() >= MAX_HUNK_LINKS {
                break;
            }
            let (Some(a), Some(b)) = (self.file_of(&l.from), self.file_of(&l.to)) else {
                continue;
            };
            if l.from == l.to || !self.linked.insert(hunk_pair(&l.from, &l.to)) {
                continue;
            }
            self.hunk_links.push(l.clone());
            if a != b && seen.insert((a.min(b), a.max(b), l.symbol.as_str())) {
                let e = self.edge(a, b).expect("distinct files");
                e.link += W_LINK;
                e.link_name.get_or_insert_with(|| l.symbol.clone());
            }
        }
    }

    /// Hub files — a barrel, a DI container, a base class every controller
    /// now calls into — touch so many others that keeping them in would glue
    /// every community into one. Counted on direct references (imports and
    /// defined names), not on rollout or name edges.
    fn find_hubs(&self) -> BTreeSet<usize> {
        let n = self.files.len();
        let threshold = 10usize.max((2.0 * (n as f64).sqrt()).ceil() as usize);
        let mut degree = vec![0usize; n];
        for (&(a, b), e) in &self.edges {
            if e.import > 0.0 || e.mention > 0.0 || e.symbol > 0.0 || e.link > 0.0 {
                degree[a] += 1;
                degree[b] += 1;
            }
        }
        (0..n).filter(|&i| degree[i] > threshold).collect()
    }

    fn detect_communities(&self) -> Vec<usize> {
        let n = self.files.len();
        // Bundled files are contracted into one node before Louvain; a
        // file is in at most one bundle (it has exactly one shape).
        let mut node: Vec<usize> = (0..n).collect();
        for b in &self.bundles {
            for &i in b {
                node[i] = b[0];
            }
        }
        let mut rep: BTreeMap<usize, usize> = BTreeMap::new();
        for &x in &node {
            let next = rep.len();
            rep.entry(x).or_insert(next);
        }
        let node: Vec<usize> = node.iter().map(|x| rep[x]).collect();
        let edges: Vec<(usize, usize, f64)> = self
            .edges
            .iter()
            .filter(|((a, b), _)| !self.hubs.contains(a) && !self.hubs.contains(b))
            .map(|(&(a, b), e)| (node[a], node[b], e.weight()))
            .collect();
        let by_node = louvain::communities(rep.len(), &edges, 1.0);
        let mut comm: Vec<usize> = (0..n).map(|i| by_node[node[i]]).collect();
        // Hubs rejoin the community they are most connected to.
        for &h in &self.hubs {
            let mut to: BTreeMap<usize, f64> = BTreeMap::new();
            for (&(a, b), e) in &self.edges {
                let other = if a == h {
                    b
                } else if b == h {
                    a
                } else {
                    continue;
                };
                if !self.hubs.contains(&other) {
                    *to.entry(comm[other]).or_default() += e.weight();
                }
            }
            if let Some((&c, _)) = to
                .iter()
                .max_by(|x, y| x.1.total_cmp(y.1).then_with(|| y.0.cmp(x.0)))
            {
                comm[h] = c;
            }
        }
        // Renumber by lowest member index.
        let mut map: BTreeMap<usize, usize> = BTreeMap::new();
        comm.iter()
            .map(|c| {
                let next = map.len();
                *map.entry(*c).or_insert(next)
            })
            .collect()
    }

    pub fn file_of(&self, hunk: &HunkId) -> Option<usize> {
        self.hunk_file.get(hunk).copied()
    }

    pub fn index_of(&self, path: &str) -> Option<usize> {
        self.by_path.get(path).copied()
    }

    pub fn community_of(&self, file: usize) -> usize {
        self.community[file]
    }

    pub fn is_hub(&self, file: usize) -> bool {
        self.hubs.contains(&file)
    }

    /// Communities as sorted file-index lists, ordered by first member.
    pub fn communities(&self) -> Vec<Vec<usize>> {
        let mut by: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for (i, &c) in self.community.iter().enumerate() {
            by.entry(c).or_default().push(i);
        }
        by.into_values().collect()
    }

    /// Code-intelligence links between visible hunks.
    pub fn hunk_links(&self) -> &[SymbolLink] {
        &self.hunk_links
    }

    /// How many code-intelligence links join a hunk of `a` to one of `b`.
    pub fn hunk_links_between(&self, a: &[HunkId], b: &[HunkId]) -> usize {
        if self.linked.is_empty() {
            return 0;
        }
        a.iter()
            .flat_map(|x| b.iter().map(move |y| (x, y)))
            .filter(|(x, y)| self.linked.contains(&hunk_pair(x, y)))
            .count()
    }

    pub fn edge_between(&self, a: usize, b: usize) -> Option<&Edge> {
        self.edges.get(&(a.min(b), a.max(b)))
    }

    /// Total edge weight between two files (0 when unrelated).
    pub fn connection(&self, a: usize, b: usize) -> f64 {
        self.edge_between(a, b).map_or(0.0, Edge::weight)
    }

    /// Strongest pairwise edges among `within`, heaviest first, leaving out
    /// pairs that are only part of a rollout (those are listed per rollout).
    pub fn strongest_edges(
        &self,
        within: &BTreeSet<usize>,
        limit: usize,
    ) -> Vec<(usize, usize, &Edge)> {
        let mut out: Vec<(usize, usize, &Edge)> = self
            .edges
            .iter()
            .filter(|((a, b), e)| within.contains(a) && within.contains(b) && !e.rollout_only())
            .map(|(&(a, b), e)| (a, b, e))
            .collect();
        out.sort_by(|x, y| {
            y.2.weight()
                .total_cmp(&x.2.weight())
                .then_with(|| (x.0, x.1).cmp(&(y.0, y.1)))
        });
        out.truncate(limit);
        out
    }
}

impl Edge {
    /// Short human reason, strongest first: "imports", "`foo`", "same name".
    pub fn reasons(&self) -> Vec<String> {
        let mut r: Vec<(f64, String)> = Vec::new();
        if self.import > 0.0 {
            r.push((self.import.min(2.0), "imports".into()));
        }
        if self.mention > 0.0 {
            r.push((self.mention.min(1.0), "names the file".into()));
        }
        if let Some(s) = &self.symbol_name
            && self.link_name.as_ref() != Some(s)
        {
            r.push((self.symbol.min(2.0), format!("`{s}`")));
        }
        if let Some(s) = &self.link_name {
            r.push((self.link.min(2.0), format!("uses `{s}`")));
        }
        if let Some(t) = &self.token_name {
            r.push((self.token.min(1.0), format!("`{t}`")));
        }
        if self.name > 0.0 {
            let label = if self.test_pair {
                "test of"
            } else {
                "same name"
            };
            r.push((
                self.name + if self.test_pair { W_TEST } else { 0.0 },
                label.into(),
            ));
        }
        if self.co_use > 0.0 {
            r.push((self.co_use.min(1.0), "same new name".into()));
        }
        if self.shape > 0.0 {
            r.push((self.shape, "same edit".into()));
        }
        if self.co_change > 0.0 {
            r.push((self.co_change, "often changed together".into()));
        }
        r.sort_by(|a, b| b.0.total_cmp(&a.0));
        r.into_iter().map(|(_, s)| s).collect()
    }
}

#[cfg(test)]
mod tests;
