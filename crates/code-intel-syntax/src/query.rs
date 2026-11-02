//! Questions over a snapshot. Tags are looked up by name or blob, then kept
//! only where the snapshot has that blob — one stored blob answers for
//! every path (and worktree) that has its content.
//!
//! ```text
//! name ─▶ tags_by_name ─▶ blob ∈ snapshot? ─▶ one hit per path ─▶ rank ─▶ cap
//!                                                        previews ◀─ worktree files
//! ```
//!
//! Every method blocks (SQLite, file reads). Files are read from the
//! snapshot's root as they are now; a snapshot that lags the disk gives
//! stale ranges, so callers refresh it on change (docs/code-intel.md §5.3).

use std::collections::{HashMap, HashSet};

use memchr::memmem;
use theodore_ports::code_intel::{
    CodeHit, CodeIntelError, CodeSymbol, CodeSymbolKind, Location, Position, Range, Source,
};

use crate::error::Result;
use crate::grammar::parse_kind;
use crate::index::{SyntaxIndex, TagRow};
use crate::pos::byte_offset;
use crate::rank::{dir_distance, module_name, same_dir};
use crate::snapshot::Snapshot;
use crate::text::{mentions, preview, word_at};

/// At most this many files are read to look for an import of the defining
/// module when ranking references.
const MAX_IMPORT_SCAN: usize = 256;

/// The identifier under a cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolAt {
    pub name: String,
    /// The identifier's range in the file.
    pub range: Range,
    /// The tag's kind, when the identifier is a tag.
    pub kind: Option<CodeSymbolKind>,
    /// The identifier is a definition's name.
    pub is_definition: bool,
    /// `Syntax` when a tag matched, `Text` when the word was read off the
    /// line (the file is not indexed, or the identifier is no tag).
    pub source: Source,
}

impl TagRow {
    fn to_symbol(&self, path: &str) -> CodeSymbol {
        CodeSymbol {
            name: self.name.clone(),
            kind: parse_kind(&self.kind),
            range: self.range,
            name_range: self.name_range,
            path: path.to_string(),
            container: self.container.clone(),
            source: Source::Syntax,
        }
    }
}

/// Worktree files read once per query.
struct Files<'a> {
    snap: &'a Snapshot,
    cache: HashMap<String, Option<String>>,
}

impl<'a> Files<'a> {
    fn new(snap: &'a Snapshot) -> Self {
        Self {
            snap,
            cache: HashMap::new(),
        }
    }

    fn get(&mut self, path: &str) -> Option<&str> {
        let root = self.snap.root();
        self.cache
            .entry(path.to_string())
            .or_insert_with(|| {
                std::fs::read(root.join(path))
                    .ok()
                    .map(|b| String::from_utf8_lossy(&b).into_owned())
            })
            .as_deref()
    }

    fn hit(&mut self, path: &str, range: Range, is_definition: bool) -> CodeHit {
        CodeHit {
            location: Location {
                path: path.to_string(),
                range,
            },
            preview: self
                .get(path)
                .map(|t| preview(t, range.start.line))
                .unwrap_or_default(),
            is_definition,
            source: Source::Syntax,
        }
    }
}

fn contains(r: &Range, at: Position) -> bool {
    r.start <= at && at <= r.end
}

impl SyntaxIndex {
    /// Definitions named `name` (exact, case-sensitive) in the snapshot,
    /// optionally of one kind; by path, then position.
    pub fn find_definitions(
        &self,
        snap: &Snapshot,
        name: &str,
        kind: Option<CodeSymbolKind>,
    ) -> Result<Vec<CodeSymbol>> {
        let mut out = Vec::new();
        for row in self.tags_named(name, true)? {
            for f in snap.paths_for_blob(&row.sha) {
                let sym = row.to_symbol(&f.path);
                if kind.is_none_or(|k| k == sym.kind) {
                    out.push(sym);
                }
            }
        }
        out.sort_by(|a, b| (&a.path, a.range.start).cmp(&(&b.path, b.range.start)));
        Ok(out)
    }

    /// Every definition in one file, in source order, each with its
    /// innermost enclosing definition (or `impl` target) as `container`.
    /// Empty while the file is not indexed.
    pub fn file_symbols(&self, snap: &Snapshot, path: &str) -> Result<Vec<CodeSymbol>> {
        let entry = snap
            .get(path)
            .ok_or_else(|| CodeIntelError::FileNotFound(path.to_string()))?;
        Ok(self
            .tags_of_blob(&entry.blob_sha)?
            .iter()
            .filter(|t| t.is_def)
            .map(|t| t.to_symbol(path))
            .collect())
    }

    /// The identifier at `at` in `path`: the tag whose name covers it, or
    /// failing that the word on the line.
    pub fn symbol_at(&self, snap: &Snapshot, path: &str, at: Position) -> Result<Option<SymbolAt>> {
        if let Some(entry) = snap.get(path) {
            let best = self
                .tags_of_blob(&entry.blob_sha)?
                .into_iter()
                .filter(|t| contains(&t.name_range, at))
                .min_by_key(|t| {
                    (
                        t.name_range.end.col.abs_diff(t.name_range.start.col),
                        !t.is_def,
                    )
                });
            if let Some(t) = best {
                return Ok(Some(SymbolAt {
                    kind: Some(parse_kind(&t.kind)),
                    name: t.name,
                    range: t.name_range,
                    is_definition: t.is_def,
                    source: Source::Syntax,
                }));
            }
        }
        let text = match std::fs::read(snap.root().join(path)) {
            Ok(b) => String::from_utf8_lossy(&b).into_owned(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(CodeIntelError::FileNotFound(path.to_string()).into());
            }
            Err(e) => return Err(e.into()),
        };
        Ok(word_at(&text, at).map(|(name, range)| SymbolAt {
            name,
            range,
            kind: None,
            is_definition: false,
            source: Source::Text,
        }))
    }

    /// Where the identifier at `at` is defined: definitions with its name,
    /// the same file first, then the same language, then the nearest
    /// directory. Each hit points at the definition's name.
    pub fn definition(
        &self,
        snap: &Snapshot,
        path: &str,
        at: Position,
        cap: usize,
    ) -> Result<Vec<CodeHit>> {
        let Some(sym) = self.symbol_at(snap, path, at)? else {
            return Ok(Vec::new());
        };
        let lang = snap.get(path).and_then(|f| f.language.clone());
        let mut defs = self.find_definitions(snap, &sym.name, None)?;
        defs.sort_by_cached_key(|d| {
            let d_lang = snap.get(&d.path).and_then(|f| f.language.clone());
            (
                d.path != path,
                lang.is_some() && d_lang != lang,
                dir_distance(path, &d.path),
                d.path.clone(),
                d.range.start,
            )
        });
        defs.truncate(cap);
        let mut files = Files::new(snap);
        Ok(defs
            .iter()
            .map(|d| files.hit(&d.path, d.name_range, true))
            .collect())
    }

    /// Uses and definitions of `name`, ranked for someone reading `from`:
    ///
    /// ```text
    /// 0 `from` itself
    /// 1 a file defining `name`
    /// 2 next to a defining file, or mentioning its module (an import, by
    ///   heuristic)
    /// 3 the rest
    /// ```
    ///
    /// Within a tier: nearest directory to `from`, then path, then line.
    /// Capped at `cap`.
    pub fn references(
        &self,
        snap: &Snapshot,
        from: &str,
        name: &str,
        cap: usize,
    ) -> Result<Vec<CodeHit>> {
        let mut hits: Vec<(String, TagRow)> = Vec::new();
        for row in self.tags_named(name, false)? {
            for f in snap.paths_for_blob(&row.sha) {
                hits.push((f.path.clone(), row.clone()));
            }
        }
        let def_paths: HashSet<&str> = hits
            .iter()
            .filter(|(_, r)| r.is_def)
            .map(|(p, _)| p.as_str())
            .collect();
        let modules: Vec<String> = def_paths.iter().filter_map(|p| module_name(p)).collect();

        let mut files = Files::new(snap);
        let mut near: HashSet<String> = HashSet::new();
        let mut far: Vec<&str> = Vec::new();
        for (p, _) in &hits {
            if p == from || def_paths.contains(p.as_str()) || near.contains(p) {
                continue;
            }
            if def_paths.iter().any(|d| same_dir(d, p)) {
                near.insert(p.clone());
            } else if !far.contains(&p.as_str()) {
                far.push(p);
            }
        }
        far.sort_by_key(|p| (dir_distance(from, p), *p));
        let finders: Vec<memmem::Finder<'_>> = modules
            .iter()
            .map(|m| memmem::Finder::new(m.as_bytes()))
            .collect();
        for p in far.into_iter().take(MAX_IMPORT_SCAN) {
            let imports = files
                .get(p)
                .is_some_and(|t| finders.iter().any(|f| mentions(t.as_bytes(), f)));
            if imports {
                near.insert(p.to_string());
            }
        }

        let tier = |p: &str| -> u8 {
            if p == from {
                0
            } else if def_paths.contains(p) {
                1
            } else if near.contains(p) {
                2
            } else {
                3
            }
        };
        let mut ranked: Vec<(u8, u32, &str, Position, &TagRow)> = hits
            .iter()
            .map(|(p, r)| {
                (
                    tier(p),
                    dir_distance(from, p),
                    p.as_str(),
                    r.name_range.start,
                    r,
                )
            })
            .collect();
        ranked.sort_by(|a, b| (a.0, a.1, a.2, a.3).cmp(&(b.0, b.1, b.2, b.3)));
        ranked.dedup_by(|a, b| a.2 == b.2 && a.3 == b.3);
        ranked.truncate(cap);
        let ranked: Vec<(String, Range, bool)> = ranked
            .into_iter()
            .map(|(_, _, p, _, r)| (p.to_string(), r.name_range, r.is_def))
            .collect();
        Ok(ranked
            .into_iter()
            .map(|(p, range, is_def)| files.hit(&p, range, is_def))
            .collect())
    }

    /// Symbol `name` in `path` and its source text, read from the file as
    /// it is now. With several (overloads, a method and a free function),
    /// the first in the file.
    pub fn symbol_body(
        &self,
        snap: &Snapshot,
        path: &str,
        name: &str,
    ) -> Result<(CodeSymbol, String)> {
        let sym = self
            .file_symbols(snap, path)?
            .into_iter()
            .find(|s| s.name == name)
            .ok_or_else(|| CodeIntelError::NoSymbol {
                path: path.to_string(),
                name: name.to_string(),
            })?;
        let text = match std::fs::read_to_string(snap.root().join(path)) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(CodeIntelError::FileNotFound(path.to_string()).into());
            }
            Err(e) => return Err(e.into()),
        };
        let (Some(start), Some(end)) = (
            byte_offset(&text, sym.range.start),
            byte_offset(&text, sym.range.end),
        ) else {
            return Err(
                CodeIntelError::Other(format!("{path} changed since it was indexed")).into(),
            );
        };
        let body = text[start..end].to_string();
        Ok((sym, body))
    }
}
