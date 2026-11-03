use super::*;
use theodore_domain::{DiffLine, FileStatus, Hunk, Sha};

/// A file whose single hunk has the given lines; `+`/`-`/` ` prefixes.
fn file(path: &str, lines: &[&str]) -> FileDiff {
    let lines: Vec<DiffLine> = lines
        .iter()
        .map(|l| {
            let (kind, text) = match l.split_at(1) {
                ("+", t) => (LineKind::Add, t),
                ("-", t) => (LineKind::Del, t),
                (_, t) => (LineKind::Ctx, t),
            };
            DiffLine {
                kind,
                text: text.to_string(),
                old_no: None,
                new_no: None,
            }
        })
        .collect();
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
        hunks: vec![Hunk {
            id: HunkId::new(format!("h-{path}")),
            path: path.into(),
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 1,
            header: String::new(),
            lines,
        }],
    }
}

fn rev(files: Vec<FileDiff>) -> Revision {
    Revision {
        base: Sha::new("b"),
        head: Sha::new("h"),
        files,
    }
}

fn graph(files: Vec<FileDiff>) -> RelGraph {
    let r = rev(files);
    RelGraph::build(&r, &crate::prepass::run(&r), &RepoContext::default())
}

fn idx(g: &RelGraph, p: &str) -> usize {
    g.index_of(p).unwrap_or_else(|| panic!("{p} not in graph"))
}

fn together(g: &RelGraph, a: &str, b: &str) -> bool {
    g.community_of(idx(g, a)) == g.community_of(idx(g, b))
}

#[test]
fn typescript_imports_link_router_controller_service_across_folders() {
    let g = graph(vec![
        file(
            "api/routes/invoice.ts",
            &["+import { InvoiceController } from '../controllers/invoice';"],
        ),
        file(
            "api/controllers/invoice.ts",
            &["+import { billInvoice } from '../../core/billing/invoice-service';"],
        ),
        file(
            "core/billing/invoice-service.ts",
            &["+export function billInvoice(id: string) {"],
        ),
        file(
            "web/src/settings/theme.ts",
            &["+export const themeColor = 'red';"],
        ),
        file(
            "web/src/settings/theme-picker.tsx",
            &["+import { themeColor } from './theme';"],
        ),
    ]);
    let e = g
        .edge_between(
            idx(&g, "api/routes/invoice.ts"),
            idx(&g, "api/controllers/invoice.ts"),
        )
        .expect("import edge");
    assert!(e.a_imports_b);
    assert!(together(
        &g,
        "api/routes/invoice.ts",
        "core/billing/invoice-service.ts"
    ));
    assert!(together(
        &g,
        "web/src/settings/theme.ts",
        "web/src/settings/theme-picker.tsx"
    ));
    assert!(!together(
        &g,
        "api/routes/invoice.ts",
        "web/src/settings/theme.ts"
    ));
}

#[test]
fn symbol_rename_links_definition_and_call_sites() {
    let g = graph(vec![
        file(
            "src/store/session.rs",
            &[
                "-pub fn load_session(id: u64) -> Session {",
                "+pub fn restore_session(id: u64) -> Session {",
            ],
        ),
        file(
            "src/ui/login.rs",
            &[
                "-    let s = load_session(id);",
                "+    let s = restore_session(id);",
            ],
        ),
        file(
            "src/ui/logout.rs",
            &[
                "-    drop(load_session(id));",
                "+    drop(restore_session(id));",
            ],
        ),
        file("docs/unrelated.md", &["+Some words about nothing."]),
    ]);
    let def = idx(&g, "src/store/session.rs");
    let e = g
        .edge_between(def, idx(&g, "src/ui/login.rs"))
        .expect("symbol edge");
    assert!(
        matches!(
            e.symbol_name.as_deref(),
            Some("restore_session" | "load_session")
        ),
        "{e:?}"
    );
    assert!(together(&g, "src/store/session.rs", "src/ui/logout.rs"));
    assert_eq!(g.connection(def, idx(&g, "docs/unrelated.md")), 0.0);
}

#[test]
fn env_key_links_config_files_to_the_code_reading_it() {
    let g = graph(vec![
        file(".env.example", &["+CORE_TO_AUTH_SYNC_SECRET=change-me"]),
        file(
            ".github/workflows/ci.yml",
            &["+      CORE_TO_AUTH_SYNC_SECRET: test"],
        ),
        file(
            "apps/auth/src/config.ts",
            &["+  syncSecret: process.env.CORE_TO_AUTH_SYNC_SECRET ?? '',"],
        ),
    ]);
    assert!(g.connection(idx(&g, ".env.example"), idx(&g, "apps/auth/src/config.ts")) > 0.0);
    assert!(together(
        &g,
        ".github/workflows/ci.yml",
        "apps/auth/src/config.ts"
    ));
}

#[test]
fn name_roots_and_tests_pair_up() {
    let g = graph(vec![
        file("lib/Controllers/FriendsController.php", &["+    $x = 1;"]),
        file("lib/Processors/FriendProcessor.php", &["+    $y = 2;"]),
        file("tests/FriendsControllerTest.php", &["+    $z = 3;"]),
        file("lib/Controllers/AudioController.php", &["+    $w = 4;"]),
    ]);
    let c = idx(&g, "lib/Controllers/FriendsController.php");
    let p = idx(&g, "lib/Processors/FriendProcessor.php");
    let t = idx(&g, "tests/FriendsControllerTest.php");
    assert_eq!(
        g.edge_between(c, p).unwrap().name_root.as_deref(),
        Some("friend")
    );
    assert!(g.edge_between(c, t).unwrap().test_pair);
    assert!(
        g.connection(c, t) > g.connection(c, p),
        "a test pairs tighter than a namesake"
    );
    assert!(together(
        &g,
        "lib/Processors/FriendProcessor.php",
        "tests/FriendsControllerTest.php"
    ));
    assert!(!together(
        &g,
        "lib/Controllers/AudioController.php",
        "lib/Controllers/FriendsController.php"
    ));
}

#[test]
fn same_edit_across_entry_files_is_one_rollout() {
    let mut files = vec![file(
        "lib/al.lib.php",
        &[
            "+function al_dispatch(string $controllerClass): void",
            "+{",
            "+    (new $controllerClass())->handle();",
            "+}",
        ],
    )];
    for name in ["Audio", "Fans", "Fave", "Feed", "Groups"] {
        let lower = name.to_ascii_lowercase();
        files.push(file(
            &format!("al_{lower}.php"),
            &[
                &format!("-use VK\\Controllers\\{name}Controller;"),
                &format!("-$controller = new {name}Controller();"),
                "-$controller->handle();",
                &format!("+al_dispatch(\\VK\\Controllers\\{name}Controller::class);"),
            ],
        ));
        files.push(file(
            &format!("lib/classes/Controllers/{name}Controller.php"),
            &[
                "-        $rows = $this->fetchAll(\"SELECT * FROM t\");",
                "+        $rows = $this->getPdo()->query('SELECT * FROM t');",
            ],
        ));
    }
    let g = graph(files);
    let entries: Vec<usize> = ["audio", "fans", "fave", "feed", "groups"]
        .iter()
        .map(|n| idx(&g, &format!("al_{n}.php")))
        .collect();
    let c = g.community_of(entries[0]);
    assert!(
        entries.iter().all(|&e| g.community_of(e) == c),
        "{:?}",
        g.communities()
    );
    assert!(
        g.rollouts
            .iter()
            .any(|r| entries.iter().all(|e| r.files.contains(e)))
    );
}

#[test]
fn hub_files_do_not_glue_unrelated_features_together() {
    // A DI container importing every service would make one giant
    // community; dropped as a hub, the two features stay apart.
    let mut files = vec![];
    let mut main_lines: Vec<String> = vec![];
    for feature in ["billing", "search", "avatar", "upload"] {
        for part in ["route", "service", "store"] {
            main_lines.push(format!("+import {{ x }} from './{feature}/{part}';"));
        }
        files.push(file(
            &format!("src/{feature}/route.ts"),
            &["+import { s } from './service';"],
        ));
        files.push(file(
            &format!("src/{feature}/service.ts"),
            &["+import { st } from './store';"],
        ));
        // Distinct edit shapes per feature, so only imports relate them.
        let fields: Vec<String> = (0..feature.len())
            .map(|i| format!("+  f{i}: {i},"))
            .collect();
        let fields: Vec<&str> = fields.iter().map(String::as_str).collect();
        files.push(file(&format!("src/{feature}/store.ts"), &fields));
    }
    let main_refs: Vec<&str> = main_lines.iter().map(String::as_str).collect();
    files.push(file("src/main.ts", &main_refs));
    let g = graph(files);
    let main = idx(&g, "src/main.ts");
    assert!(g.is_hub(main));
    assert!(together(&g, "src/billing/route.ts", "src/billing/store.ts"));
    assert!(!together(&g, "src/billing/route.ts", "src/search/route.ts"));
    // The hub still lands somewhere sensible, not alone.
    let size = g
        .communities()
        .iter()
        .find(|c| c.contains(&main))
        .unwrap()
        .len();
    assert!(size > 1);
}

#[test]
fn co_change_history_links_files_without_code_signals() {
    let files = vec![
        file("src/parser/lexer.c", &["+  tokens++;"]),
        file("docs/grammar.md", &["+ New rule."]),
        file("src/net/socket.c", &["+  fd++;"]),
    ];
    let ctx = RepoContext {
        head_files: Default::default(),
        co_change_commits: vec![
            vec!["src/parser/lexer.c".into(), "docs/grammar.md".into()],
            vec!["src/parser/lexer.c".into(), "docs/grammar.md".into()],
            vec!["src/net/socket.c".into()],
            vec!["src/parser/lexer.c".into(), "src/net/socket.c".into()],
        ],
        ..RepoContext::default()
    };
    let g = RelGraph::build(&rev(files), &PrePass::default(), &ctx);
    let lexer = idx(&g, "src/parser/lexer.c");
    assert!(g.connection(lexer, idx(&g, "docs/grammar.md")) > 0.0);
    // one shared commit is below the minimum
    assert_eq!(g.connection(lexer, idx(&g, "src/net/socket.c")), 0.0);
    assert!(together(&g, "src/parser/lexer.c", "docs/grammar.md"));
}

#[test]
fn head_file_contents_add_imports_outside_the_hunks() {
    let files = vec![
        file("pkg/billing/tax.py", &["+    rate = 0.2"]),
        file("pkg/billing/invoice.py", &["+    total = 1"]),
    ];
    let mut ctx = RepoContext::default();
    ctx.head_files.insert(
        "pkg/billing/invoice.py".into(),
        "from pkg.billing import tax\n\ndef total():\n    return 1\n".into(),
    );
    let without = graph(files.clone());
    assert_eq!(without.connection(0, 1), 0.0);
    let with = RelGraph::build(&rev(files), &PrePass::default(), &ctx);
    assert!(with.connection(0, 1) > 0.0);
}

#[test]
fn symbol_links_from_code_intelligence_link_files_and_hunks() {
    let files = vec![
        file("src/store/cache.rs", &["+    self.evict(n)"]),
        file("src/http/handler.rs", &["+    let v = 2;"]),
        file("src/net/socket.rs", &["+    fd += 1;"]),
    ];
    let link = |from: &str, to: &str| SymbolLink {
        from: HunkId::new(format!("h-{from}")),
        to: HunkId::new(format!("h-{to}")),
        symbol: "evict_older".into(),
    };
    let ctx = RepoContext {
        symbol_links: vec![
            link("src/store/cache.rs", "src/http/handler.rs"),
            // The same pair again, and a hunk that isn't in the PR.
            link("src/http/handler.rs", "src/store/cache.rs"),
            link("src/store/cache.rs", "gone.rs"),
        ],
        ..RepoContext::default()
    };
    let without = graph(files.clone());
    assert_eq!(without.connection(0, 1), 0.0);
    let g = RelGraph::build(&rev(files), &PrePass::default(), &ctx);
    let (cache, handler) = (
        idx(&g, "src/store/cache.rs"),
        idx(&g, "src/http/handler.rs"),
    );
    assert!(g.connection(cache, handler) >= 1.0);
    assert_eq!(g.connection(cache, idx(&g, "src/net/socket.rs")), 0.0);
    assert_eq!(
        g.edge_between(cache, handler).unwrap().reasons()[0],
        "uses `evict_older`"
    );
    assert_eq!(g.hunk_links().len(), 1);
    let ids = |p: &str| vec![HunkId::new(format!("h-{p}"))];
    assert_eq!(
        g.hunk_links_between(&ids("src/http/handler.rs"), &ids("src/store/cache.rs")),
        1
    );
    assert_eq!(
        g.hunk_links_between(&ids("src/net/socket.rs"), &ids("src/store/cache.rs")),
        0
    );
    assert!(together(&g, "src/store/cache.rs", "src/http/handler.rs"));
}

#[test]
fn hidden_files_are_not_nodes() {
    let mut prepass = PrePass::default();
    prepass.hidden.insert(
        "pnpm-lock.yaml".into(),
        crate::prepass::HiddenReason::Lockfile,
    );
    let g = RelGraph::build(
        &rev(vec![
            file("pnpm-lock.yaml", &["+x"]),
            file("src/a.ts", &["+y"]),
        ]),
        &prepass,
        &RepoContext::default(),
    );
    assert_eq!(g.files, vec!["src/a.ts".to_string()]);
}

#[test]
fn build_is_deterministic() {
    let make = || {
        graph(vec![
            file(
                "a/x.ts",
                &[
                    "+import { y } from '../b/y';",
                    "+export function shared_thing() {",
                ],
            ),
            file("b/y.ts", &["+shared_thing();"]),
            file("c/z.ts", &["+shared_thing();"]),
            file("d/w.ts", &["+import { z } from '../c/z';"]),
        ])
    };
    let a = make();
    for _ in 0..5 {
        let b = make();
        assert_eq!(a.communities(), b.communities());
        assert_eq!(a.edges, b.edges);
    }
}
