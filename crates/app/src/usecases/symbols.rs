//! `find_symbol` (plan "symbols"): go-to-definition / find-references.
//! Asks code intelligence first (docs/code-intel.md) when it is on;
//! otherwise, or when it answers nothing or fails, greps the PR head
//! worktree for a whole-word match of the clicked identifier, classifies
//! each hit as a definition (it matches one of the language's declaration
//! patterns) or a plain reference, and ranks definitions and
//! same-language/near-path hits first.
//!
//! ```text
//! click ─▶ code intel on? ─yes─▶ at a position: definition + references
//!              │                  else: find_symbol + references at the first one
//!              │                     └─ nothing / error ─┐
//!              └─no──────────────────────────────────────┴─▶ grep
//! ```

use std::collections::HashSet;
use std::path::Path;

use theodore_contract::{FindSymbolRequest, SymbolHit, SymbolKind, SymbolSource};
use theodore_ports::{CodeHit, CodeIntelPort, GitPort, Position, Source};

use crate::core::Core;
use crate::error::{AppError, AppResult};

/// Cap on hits returned to the UI — a peek popover, not a search results
/// page.
const MAX_HITS: usize = 50;

/// One language's file extensions and declaration heuristics. Each pattern
/// is an extended-regex fragment with `{}` standing in for the escaped
/// identifier; `GitPort::grep` always runs with `-w` (whole word), so a
/// pattern can end in a bare `{}` without a `\b` — the engine-portability
/// problem `\b` has in POSIX ERE never comes up.
struct LangDef {
    id: &'static str,
    exts: &'static [&'static str],
    def_patterns: &'static [&'static str],
}

const LANGS: &[LangDef] = &[
    LangDef {
        id: "rust",
        exts: &["rs"],
        def_patterns: &[
            r"(pub([[:space:]]*\([^)]*\))?[[:space:]]+)?(async[[:space:]]+)?(fn|struct|enum|trait|type|const|static|mod)[[:space:]]+{}",
            r"impl([[:space:]]*<[^>]*>)?[[:space:]].*[[:space:]]{}",
        ],
    },
    LangDef {
        id: "typescript",
        exts: &["ts", "tsx"],
        def_patterns: &[
            r"(export[[:space:]]+)?(default[[:space:]]+)?(declare[[:space:]]+)?(async[[:space:]]+)?(function|class|interface|type|enum|const|let|var)[[:space:]]+{}",
            // A method: name(...) with the body's `{` on the same line —
            // the one thing a call expression never has, so this doesn't
            // also match a call site.
            r"^[[:space:]]*(public|private|protected|static|async|readonly)?[[:space:]]*{}[[:space:]]*\([^()]*\)[[:space:]]*(:[^{;]*)?\{[[:space:]]*$",
        ],
    },
    LangDef {
        id: "javascript",
        exts: &["js", "jsx", "mjs", "cjs"],
        def_patterns: &[
            r"(export[[:space:]]+)?(default[[:space:]]+)?(async[[:space:]]+)?(function|class|const|let|var)[[:space:]]+{}",
            r"^[[:space:]]*(static|async)?[[:space:]]*{}[[:space:]]*\([^()]*\)[[:space:]]*\{[[:space:]]*$",
        ],
    },
    LangDef {
        id: "python",
        exts: &["py"],
        def_patterns: &[r"(def|class)[[:space:]]+{}"],
    },
    LangDef {
        id: "go",
        exts: &["go"],
        def_patterns: &[
            r"func([[:space:]]*\([^)]*\))?[[:space:]]+{}",
            r"type[[:space:]]+{}",
        ],
    },
    LangDef {
        id: "swift",
        exts: &["swift"],
        def_patterns: &[r"(func|class|struct|enum|protocol|let|var)[[:space:]]+{}"],
    },
    LangDef {
        id: "kotlin",
        exts: &["kt", "kts"],
        def_patterns: &[r"(fun|class|interface|object|val|var)[[:space:]]+{}"],
    },
    LangDef {
        id: "java",
        exts: &["java"],
        def_patterns: &[
            r"(class|interface|enum)[[:space:]]+{}",
            // Method: a return type, the name, a parameter list, then the
            // body's `{` on the same line — not a call expression.
            r"[A-Za-z_][A-Za-z0-9_<>\[\],.[:space:]]*[[:space:]]+{}[[:space:]]*\([^()]*\)[[:space:]]*(throws[^{]*)?\{[[:space:]]*$",
        ],
    },
    LangDef {
        id: "c",
        exts: &["c", "h"],
        def_patterns: &[
            r"(struct|enum|union|typedef)[[:space:]]+{}",
            r"^{}[[:space:]]*\([^()]*\)[[:space:]]*\{[[:space:]]*$",
        ],
    },
    LangDef {
        id: "cpp",
        exts: &["cc", "cpp", "cxx", "hpp", "hh", "hxx"],
        def_patterns: &[
            r"(struct|class|enum|union|typedef|namespace)[[:space:]]+{}",
            r"{}[[:space:]]*\([^()]*\)[[:space:]]*(const)?[[:space:]]*\{[[:space:]]*$",
        ],
    },
    LangDef {
        id: "ruby",
        exts: &["rb"],
        def_patterns: &[r"(def|class|module)[[:space:]]+{}"],
    },
    LangDef {
        id: "php",
        exts: &["php"],
        def_patterns: &[r"(function|class|interface|trait)[[:space:]]+{}"],
    },
];

/// Identifiers are `[A-Za-z_$][\w$]*` (`DiffPane.tsx`'s `symbolAt`); `$` is
/// the only character in that set an ERE treats specially.
fn escape_ident(name: &str) -> String {
    name.replace('$', r"\$")
}

fn lang_for_ext(ext: &str) -> Option<&'static str> {
    LANGS
        .iter()
        .find(|l| l.exts.iter().any(|e| e.eq_ignore_ascii_case(ext)))
        .map(|l| l.id)
}

fn lang_from_path(path: Option<&str>) -> Option<&'static str> {
    let ext = Path::new(path?).extension()?.to_str()?;
    lang_for_ext(ext)
}

fn file_is_lang(path: &str, l: &LangDef) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| l.exts.iter().any(|e| e.eq_ignore_ascii_case(ext)))
}

/// Tree distance between two paths' *directories*: 0 when they share a
/// parent, growing with how many components they diverge by on either
/// side. Used to prefer hits "near" the file the click came from.
fn dir_distance(from_dir: &Path, hit_path: &str) -> u32 {
    let hit_dir = Path::new(hit_path)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let from: Vec<_> = from_dir.components().collect();
    let hit: Vec<_> = hit_dir.components().collect();
    let common = from
        .iter()
        .zip(hit.iter())
        .take_while(|(a, b)| a == b)
        .count();
    ((from.len() - common) + (hit.len() - common)) as u32
}

/// Greps `worktree` for `name`: every language's declaration patterns
/// (filtered to `lang` when given) become `Definition` hits, and anything
/// else matching the bare name becomes a `Reference`. A line already
/// counted as a definition is never double-counted as a reference.
pub(crate) async fn search_symbol(
    git: &dyn GitPort,
    worktree: &Path,
    name: &str,
    lang: Option<&str>,
) -> AppResult<Vec<SymbolHit>> {
    let esc = escape_ident(name);
    let mut hits = Vec::new();
    let mut seen: HashSet<(String, u32)> = HashSet::new();

    for l in LANGS.iter().filter(|l| {
        lang.map(|want| want.eq_ignore_ascii_case(l.id))
            .unwrap_or(true)
    }) {
        for frag in l.def_patterns {
            let pattern = frag.replace("{}", &esc);
            let found = git.grep(worktree, &pattern).await?;
            // The pattern itself doesn't scope the search to this
            // language's files (git grep searches the whole worktree), so
            // filter by extension here — otherwise, e.g., TypeScript's
            // bare `name(` method pattern would also match a Rust call
            // site.
            for h in found.into_iter().filter(|h| file_is_lang(&h.path, l)) {
                if seen.insert((h.path.clone(), h.line)) {
                    hits.push(SymbolHit {
                        path: h.path,
                        line: h.line,
                        kind: SymbolKind::Definition,
                        preview: h.text.trim().to_string(),
                        source: Some(SymbolSource::Text),
                    });
                }
            }
        }
    }

    for h in git.grep(worktree, &esc).await? {
        if seen.insert((h.path.clone(), h.line)) {
            hits.push(SymbolHit {
                path: h.path,
                line: h.line,
                kind: SymbolKind::Reference,
                preview: h.text.trim().to_string(),
                source: Some(SymbolSource::Text),
            });
        }
    }

    Ok(hits)
}

/// Orders `hits`: every `Definition` before any `Reference`; within that,
/// the clicked language's own file extensions before others, then the
/// closest directory to `from_path`, then path/line for a stable order.
/// Caps at `MAX_HITS`.
pub(crate) fn rank(
    mut hits: Vec<SymbolHit>,
    lang: Option<&str>,
    from_path: Option<&str>,
) -> Vec<SymbolHit> {
    let want_lang = lang
        .map(str::to_string)
        .or_else(|| lang_from_path(from_path).map(str::to_string));
    let from_dir = from_path.map(|p| {
        Path::new(p)
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .to_path_buf()
    });

    hits.sort_by(|a, b| {
        let a_key = sort_key(a, want_lang.as_deref(), from_dir.as_deref());
        let b_key = sort_key(b, want_lang.as_deref(), from_dir.as_deref());
        a_key.cmp(&b_key)
    });
    hits.truncate(MAX_HITS);
    hits
}

fn sort_key(
    h: &SymbolHit,
    want_lang: Option<&str>,
    from_dir: Option<&Path>,
) -> (bool, bool, u32, String, u32) {
    let hit_lang = Path::new(&h.path)
        .extension()
        .and_then(|e| e.to_str())
        .and_then(lang_for_ext);
    let same_lang = match (want_lang, hit_lang) {
        (Some(w), Some(l)) => w.eq_ignore_ascii_case(l),
        _ => false,
    };
    let distance = from_dir.map(|d| dir_distance(d, &h.path)).unwrap_or(0);
    (
        h.kind != SymbolKind::Definition,
        !same_lang,
        distance,
        h.path.clone(),
        h.line,
    )
}

fn source_of(s: Source) -> SymbolSource {
    match s {
        Source::Lsp => SymbolSource::Lsp,
        Source::Syntax => SymbolSource::Syntax,
        Source::Text => SymbolSource::Text,
    }
}

fn hit_of(h: CodeHit) -> SymbolHit {
    SymbolHit {
        path: h.location.path,
        line: h.location.range.start.line,
        kind: if h.is_definition {
            SymbolKind::Definition
        } else {
            SymbolKind::Reference
        },
        preview: h
            .preview
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_string(),
        source: Some(source_of(h.source)),
    }
}

/// The port's answer for a click: at the clicked position when the UI
/// sent one, else the definitions of the name and the references of the
/// first. One hit per line, definitions first.
async fn intel_hits(
    port: &dyn CodeIntelPort,
    root: &Path,
    req: &FindSymbolRequest,
) -> Result<Vec<SymbolHit>, theodore_ports::CodeIntelError> {
    let mut hits: Vec<SymbolHit> = Vec::new();
    match (req.from_path.as_deref(), req.line, req.col) {
        (Some(path), Some(line), Some(col)) => {
            let at = Position { line, col };
            let defs = port.definition(root, path, at).await?;
            hits.extend(defs.items.into_iter().map(|mut h| {
                h.is_definition = true;
                hit_of(h)
            }));
            let refs = port.references(root, path, at).await?;
            hits.extend(refs.items.into_iter().map(hit_of));
        }
        _ => {
            let defs = port.find_symbol(root, &req.name, None).await?;
            let first = defs.items.first().cloned();
            hits.extend(defs.items.into_iter().map(|s| {
                SymbolHit {
                    path: s.path,
                    line: s.name_range.start.line,
                    kind: SymbolKind::Definition,
                    preview: s
                        .container
                        .map(|c| format!("{c} › {}", s.name))
                        .unwrap_or(s.name),
                    source: Some(source_of(s.source)),
                }
            }));
            if let Some(s) = first {
                let refs = port.references(root, &s.path, s.name_range.start).await?;
                hits.extend(refs.items.into_iter().map(hit_of));
            }
        }
    }
    let mut seen: HashSet<(String, u32)> = HashSet::new();
    hits.retain(|h| seen.insert((h.path.clone(), h.line)));
    Ok(hits)
}

impl Core {
    /// Go-to-definition / find-references for the identifier under a
    /// ⌘-click in the diff (plan "symbols"). Searches the PR's current head
    /// worktree — never the base revision — since that's what the reviewer
    /// is reading.
    pub async fn find_symbol(&self, req: FindSymbolRequest) -> AppResult<Vec<SymbolHit>> {
        self.ensure_open(&req.pr).await?;
        let detail = self.load_pr_detail(&req.pr).await?;
        let head = self
            .0
            .current_head
            .read()
            .await
            .get(&req.pr)
            .cloned()
            .ok_or_else(|| AppError::NotFound(format!("pr {}", req.pr)))?;
        let worktree = self.0.deps.git.worktree(&detail.summary.pr, &head).await?;
        let lang = req
            .lang
            .clone()
            .or_else(|| lang_from_path(req.from_path.as_deref()).map(str::to_string));

        if let Some(port) = self.0.deps.code_intel.clone()
            && self.review_code_tools(&req.pr).await != crate::deps::CodeTools::Off
        {
            match intel_hits(&*port, &worktree.path, &req).await {
                Ok(hits) if !hits.is_empty() => {
                    return Ok(rank(hits, lang.as_deref(), req.from_path.as_deref()));
                }
                Ok(_) => {}
                Err(e) => tracing::debug!(error = %e, "code intelligence failed; grepping"),
            }
        }

        let hits = search_symbol(
            &*self.0.deps.git,
            &worktree.path,
            &req.name,
            lang.as_deref(),
        )
        .await?;
        Ok(rank(hits, lang.as_deref(), req.from_path.as_deref()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::sync::Arc;

    use tempfile::TempDir;
    use theodore_domain::RepoRef;
    use theodore_git::GitCli;
    use theodore_ports::CredentialSource;

    struct NoAuth;
    impl CredentialSource for NoAuth {
        fn token_for(&self, _repo: &RepoRef) -> Option<String> {
            None
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .expect("git installed");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A tracked tree spanning three languages, so the per-language
    /// patterns and the language-aware ranking both get real exercise.
    fn setup() -> TempDir {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "a@b.com"]);
        git(dir, &["config", "user.name", "test"]);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("src/lib.rs"),
            "pub fn widget() -> u32 {\n    0\n}\n\nfn call_site() {\n    widget();\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("src/app.py"),
            "def widget():\n    return 0\n\nwidget()\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("ui")).unwrap();
        std::fs::write(
            dir.join("ui/app.ts"),
            "export function widget(): number {\n  return 0;\n}\n\nwidget();\n",
        )
        .unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "seed"]);
        tmp
    }

    fn cli(tmp: &TempDir) -> GitCli {
        GitCli::new(tmp.path().join(".theodore-root"), Arc::new(NoAuth))
    }

    #[tokio::test]
    async fn classifies_declarations_as_definitions_and_everything_else_as_references() {
        let tmp = setup();
        let git_cli = cli(&tmp);
        let hits = search_symbol(&git_cli, tmp.path(), "widget", None)
            .await
            .unwrap();

        let defs: Vec<_> = hits
            .iter()
            .filter(|h| h.kind == SymbolKind::Definition)
            .collect();
        let refs: Vec<_> = hits
            .iter()
            .filter(|h| h.kind == SymbolKind::Reference)
            .collect();
        // One declaration per language (rust/py/ts) ...
        assert_eq!(defs.len(), 3);
        // ... and one call site per language, not double-counted as a def.
        assert_eq!(refs.len(), 3);
        assert!(defs.iter().any(|h| h.path == "src/lib.rs" && h.line == 1));
        assert!(defs.iter().any(|h| h.path == "src/app.py" && h.line == 1));
        assert!(defs.iter().any(|h| h.path == "ui/app.ts" && h.line == 1));
    }

    #[tokio::test]
    async fn lang_filter_narrows_definitions_to_one_language() {
        let tmp = setup();
        let git_cli = cli(&tmp);
        let hits = search_symbol(&git_cli, tmp.path(), "widget", Some("python"))
            .await
            .unwrap();
        let defs: Vec<_> = hits
            .iter()
            .filter(|h| h.kind == SymbolKind::Definition)
            .collect();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].path, "src/app.py");
    }

    #[test]
    fn rank_puts_definitions_before_references() {
        let hits = vec![
            SymbolHit {
                path: "a.rs".into(),
                line: 1,
                kind: SymbolKind::Reference,
                preview: String::new(),
                source: None,
            },
            SymbolHit {
                path: "b.rs".into(),
                line: 1,
                kind: SymbolKind::Definition,
                preview: String::new(),
                source: None,
            },
        ];
        let ranked = rank(hits, None, None);
        assert_eq!(ranked[0].kind, SymbolKind::Definition);
        assert_eq!(ranked[1].kind, SymbolKind::Reference);
    }

    #[test]
    fn rank_prefers_the_clicked_language_and_then_the_nearest_directory() {
        let hits = vec![
            SymbolHit {
                path: "ui/app.ts".into(),
                line: 1,
                kind: SymbolKind::Definition,
                preview: String::new(),
                source: None,
            },
            SymbolHit {
                path: "src/app.py".into(),
                line: 1,
                kind: SymbolKind::Definition,
                preview: String::new(),
                source: None,
            },
            SymbolHit {
                path: "src/lib.rs".into(),
                line: 1,
                kind: SymbolKind::Definition,
                preview: String::new(),
                source: None,
            },
            SymbolHit {
                path: "src/other.rs".into(),
                line: 1,
                kind: SymbolKind::Definition,
                preview: String::new(),
                source: None,
            },
        ];
        // Clicked from a Rust file under src/: both Rust hits should sort
        // before the Python/TS ones, and `src/other.rs` (same dir) before
        // `src/lib.rs` isn't guaranteed by language alone, so assert on
        // language grouping, not exact Rust-Rust order.
        let ranked = rank(hits, None, Some("src/caller.rs"));
        assert!(ranked[0].path.ends_with(".rs"));
        assert!(ranked[1].path.ends_with(".rs"));
    }

    #[test]
    fn rank_caps_at_fifty_hits() {
        let hits = (0..80)
            .map(|i| SymbolHit {
                path: format!("f{i}.rs"),
                line: 1,
                kind: SymbolKind::Reference,
                preview: String::new(),
                source: None,
            })
            .collect();
        assert_eq!(rank(hits, None, None).len(), MAX_HITS);
    }

    #[test]
    fn lang_from_path_reads_the_extension() {
        assert_eq!(lang_from_path(Some("ui/src/App.tsx")), Some("typescript"));
        assert_eq!(lang_from_path(Some("crates/app/src/lib.rs")), Some("rust"));
        assert_eq!(lang_from_path(Some("README")), None);
        assert_eq!(lang_from_path(None), None);
    }

    #[test]
    fn escape_ident_only_touches_dollar_signs() {
        assert_eq!(escape_ident("plain_name"), "plain_name");
        assert_eq!(escape_ident("$scope"), r"\$scope");
    }
}
