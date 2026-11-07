//! Grammars built on this machine (`scripts/build-grammars.sh`), loaded the
//! way the app loads them, and the tags queries run through the syntax
//! layer's own extraction. For the languages whose release is not published
//! yet, or whose queries just changed: the CI release is what ships them,
//! this is the check that they work before it runs.
//!
//! ```text
//! GRAMMARS_OUT_DIR=/tmp/g bash scripts/build-grammars.sh
//! GRAMMARS_OUT_DIR=/tmp/g cargo test -p theodore-code-intel-toolchain \
//!     --test local_grammars -- --ignored --nocapture
//!
//! out dir (dylibs, tags.scm, grammars.json)
//!   ─▶ grammars.json + manifest/languages.yaml parse without warnings
//!   ─▶ copied into <root>/grammars/<package>-<version>/   (the cache layout)
//!   ─▶ Toolchain::ensure_grammar   sha256 ─▶ dlopen ─▶ ABI check
//!   ─▶ sample file ─▶ SyntaxIndex (tree-sitter-tags, same extraction as
//!      the indexer) ─▶ asserted definitions and references
//! ```

use std::path::{Path, PathBuf};

use theodore_code_intel_syntax::{
    DEFAULT_MAX_FILE_BYTES, FileEntry, Grammar, Snapshot, SyntaxIndex, blob_sha,
};
use theodore_code_intel_toolchain::{Manifest, Toolchain, current_platform, supported_abi};
use theodore_ports::code_intel::{CodeSymbolKind as K, LanguageId};

const LANGUAGES_YAML: &str = include_str!("../manifest/languages.yaml");

struct Sample {
    id: &'static str,
    path: &'static str,
    source: &'static str,
    /// (name, kind, container) — each must be among the file's definitions.
    defs: &'static [(&'static str, K, Option<&'static str>)],
    /// (name, text on the line of the use) — a reference (not a definition)
    /// to `name` must exist on a line containing the text.
    refs: &'static [(&'static str, &'static str)],
    /// Names that must NOT be definitions (locals, plain parameters).
    absent: &'static [&'static str],
}

const PHP: Sample = Sample {
    id: "php",
    path: "src/Invoice.php",
    source: r#"<?php
namespace App\Billing;

interface Chargeable {
    public function charge(int $cents): bool;
}

trait Loggable {
    public function log(string $m): void { echo $m; }
}

enum Suit: string {
    case Hearts = 'H';
    case Spades = 'S';
}

class Invoice implements Chargeable {
    use Loggable;
    const MAX = 3;
    private int $total = 0;

    public function charge(int $cents): bool {
        $this->log("charging");
        return helper($cents) > 0;
    }

    public static function make(): Invoice {
        return new Invoice();
    }
}

function helper(int $x): int {
    return $x;
}

$invoice = Invoice::make();
$invoice?->charge(5);
"#,
    defs: &[
        ("Chargeable", K::Interface, None),
        ("Loggable", K::Trait, None),
        ("Suit", K::Enum, None),
        ("Hearts", K::Constant, Some("Suit")),
        ("Invoice", K::Class, None),
        ("MAX", K::Constant, Some("Invoice")),
        ("total", K::Field, Some("Invoice")),
        ("charge", K::Method, Some("Chargeable")),
        ("charge", K::Method, Some("Invoice")),
        ("log", K::Method, Some("Loggable")),
        ("make", K::Method, Some("Invoice")),
        ("helper", K::Function, None),
    ],
    refs: &[
        ("log", "$this->log("),
        ("helper", "return helper("),
        ("make", "Invoice::make()"),
        ("charge", "$invoice?->charge(5)"),
        ("Invoice", "new Invoice()"),
        ("Chargeable", "implements Chargeable"),
    ],
    absent: &["cents", "m"],
};

const RUBY: Sample = Sample {
    id: "ruby",
    path: "lib/shop/cart.rb",
    source: r#"module Shop
  class Cart < Base
    def add(item)
      items << item
      log_it(item)
    end

    def self.build
      new
    end
  end
end

Shop::Cart.build
"#,
    defs: &[
        ("Shop", K::Module, None),
        ("Cart", K::Class, Some("Shop")),
        ("add", K::Method, Some("Cart")),
        ("build", K::Method, Some("Cart")),
    ],
    refs: &[
        ("log_it", "log_it(item)"),
        ("build", "Shop::Cart.build"),
        ("Base", "class Cart < Base"),
    ],
    absent: &["item"],
};

const JAVA: Sample = Sample {
    id: "java",
    path: "src/main/java/com/acme/Circle.java",
    source: r#"package com.acme;

import java.util.List;

public interface Shape {
    double area();
}

public enum Color { RED, GREEN }

public record Point(int x, int y) {}

public class Circle implements Shape {
    private final double r;

    public Circle(double r) { this.r = r; }

    public double area() { return Math.PI * r * r; }

    static Circle unit() { return new Circle(1.0); }

    void run() {
        double a = unit().area();
        print(a);
    }
}
"#,
    defs: &[
        ("Shape", K::Interface, None),
        ("area", K::Method, Some("Shape")),
        ("Color", K::Enum, None),
        ("Point", K::Struct, None),
        ("Circle", K::Class, None),
        ("area", K::Method, Some("Circle")),
        ("unit", K::Method, Some("Circle")),
        ("run", K::Method, Some("Circle")),
    ],
    refs: &[
        ("unit", "unit().area()"),
        ("area", "unit().area()"),
        ("print", "print(a)"),
        ("Circle", "new Circle(1.0)"),
        ("Shape", "implements Shape"),
    ],
    absent: &["r"],
};

const KOTLIN: Sample = Sample {
    id: "kotlin",
    path: "src/main/kotlin/shop/Cart.kt",
    source: r#"package shop

interface Priced {
    fun price(): Int
}

enum class Kind { A, B }

data class Money(val amount: Int)

object Registry {
    fun get(): Int = 1
}

typealias Cents = Int

class Cart(val items: List<Int>) : Priced {
    val total = 0

    companion object Factory {
        fun create(): Cart = Cart(listOf())
    }

    override fun price(): Int {
        val local = 2
        return helper(total) + Registry.get()
    }
}

fun helper(x: Int): Int = x

val ROOT = 1
"#,
    defs: &[
        ("Priced", K::Interface, None),
        ("price", K::Method, Some("Priced")),
        ("Kind", K::Class, None),
        ("A", K::Constant, Some("Kind")),
        ("Money", K::Class, None),
        ("amount", K::Field, Some("Money")),
        ("Registry", K::Class, None),
        ("get", K::Method, Some("Registry")),
        ("Cents", K::Type, None),
        ("Cart", K::Class, None),
        ("total", K::Field, Some("Cart")),
        ("Factory", K::Class, Some("Cart")),
        ("create", K::Method, Some("Factory")),
        ("price", K::Method, Some("Cart")),
        ("helper", K::Function, None),
        ("ROOT", K::Field, None),
    ],
    refs: &[
        ("helper", "return helper(total)"),
        ("get", "Registry.get()"),
        ("Cart", "fun create(): Cart = Cart(listOf())"),
        ("listOf", "Cart(listOf())"),
        ("Priced", ") : Priced {"),
    ],
    absent: &["local", "x"],
};

const CSHARP: Sample = Sample {
    id: "csharp",
    path: "src/Cart.cs",
    source: r#"namespace Acme.Shop;

public interface IPriced {
    decimal Price();
}

public struct Money {
    public decimal Amount { get; set; }
}

public enum Kind { A, B }

public record Order(int Id);

public class Cart : IPriced {
    public decimal Price() { return Total(); }

    private decimal Total() { return Helper.Sum(1); }

    int Local() {
        int Twice(int x) => x * 2;
        return Twice(2);
    }
}
"#,
    defs: &[
        ("Shop", K::Module, None),
        ("IPriced", K::Interface, None),
        ("Price", K::Method, Some("IPriced")),
        ("Money", K::Struct, None),
        ("Amount", K::Field, Some("Money")),
        ("Kind", K::Enum, None),
        ("Order", K::Struct, None),
        ("Cart", K::Class, None),
        ("Price", K::Method, Some("Cart")),
        ("Total", K::Method, Some("Cart")),
        ("Twice", K::Function, Some("Local")),
    ],
    refs: &[
        ("Total", "return Total()"),
        ("Sum", "Helper.Sum(1)"),
        ("Twice", "return Twice(2)"),
        ("IPriced", "class Cart : IPriced"),
    ],
    absent: &["x"],
};

fn out_dir() -> PathBuf {
    let dir = std::env::var_os("GRAMMARS_OUT_DIR")
        .map(PathBuf::from)
        .expect(
            "GRAMMARS_OUT_DIR: run `GRAMMARS_OUT_DIR=<dir> bash scripts/build-grammars.sh` first",
        );
    assert!(
        dir.join("grammars.json").is_file(),
        "{}/grammars.json",
        dir.display()
    );
    dir
}

/// The cache layout `Toolchain` reads (`grammars/<package>-<version>/`),
/// filled from the build script's output.
fn install_into(root: &Path, out: &Path, manifest: &Manifest) {
    for g in manifest.grammars() {
        let dir = root.join("grammars").join(g.dir_name());
        std::fs::create_dir_all(&dir).unwrap();
        let name = g.dir_name();
        std::fs::copy(out.join(format!("{name}.dylib")), dir.join("grammar.dylib"))
            .unwrap_or_else(|e| panic!("{name}.dylib: {e}"));
        for l in &g.languages {
            std::fs::copy(
                out.join(format!("{name}-{}.tags.scm", l.id)),
                dir.join(format!("{}.tags.scm", l.id)),
            )
            .unwrap_or_else(|e| panic!("{name} {}: {e}", l.id));
        }
    }
}

fn line_of(source: &str, line: u32) -> &str {
    source.lines().nth(line as usize - 1).unwrap_or_default()
}

#[tokio::test]
#[ignore = "needs GRAMMARS_OUT_DIR from scripts/build-grammars.sh"]
async fn locally_built_grammars_load_and_their_queries_find_defs_and_refs() {
    let out = out_dir();
    let grammars_json = std::fs::read_to_string(out.join("grammars.json")).unwrap();
    let (manifest, warnings) = Manifest::parse(LANGUAGES_YAML, &grammars_json).unwrap();
    assert_eq!(
        warnings,
        Vec::<String>::new(),
        "languages.yaml and the generated grammars.json disagree"
    );

    let platform = current_platform();
    let root = tempfile::tempdir().unwrap();
    install_into(root.path(), &out, &manifest);
    let tc = Toolchain::builder(root.path())
        .manifest(manifest.clone())
        .platform(platform)
        .build();

    for s in [PHP, RUBY, JAVA, KOTLIN, CSHARP] {
        let id = LanguageId::new(s.id);

        // The manifest routes the sample's path to the language.
        assert_eq!(
            manifest.language_for_path(s.path).map(|l| l.id.clone()),
            Some(id.clone()),
            "{}: glob for {}",
            s.id,
            s.path
        );

        // The app's load path: sha256 of the cached files, dlopen, ABI.
        let loaded = tc
            .ensure_grammar(&id)
            .await
            .unwrap_or_else(|e| panic!("{}: {e}", s.id));
        let abi = loaded.language.abi_version();
        assert!(supported_abi().contains(&abi), "{}: ABI {abi}", s.id);
        assert!(!loaded.tags_query.is_empty(), "{}: empty tags query", s.id);

        // The indexer's extraction: tree-sitter-tags over the sample.
        let tree = tempfile::tempdir().unwrap();
        let file = tree.path().join(s.path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, s.source).unwrap();
        let sha = blob_sha(s.source.as_bytes());
        let snap = Snapshot::new(
            tree.path(),
            vec![FileEntry {
                path: s.path.to_string(),
                blob_sha: sha.clone(),
                language: Some(id.clone()),
            }],
        );
        let grammar = Grammar::new(
            id.clone(),
            loaded.language.clone(),
            loaded.tags_query.clone(),
        );
        let idx = SyntaxIndex::open_in_memory().unwrap();
        idx.index_blob(&sha, &grammar, s.source.as_bytes(), DEFAULT_MAX_FILE_BYTES)
            .unwrap_or_else(|e| panic!("{}: {e}", s.id));

        let symbols = idx.file_symbols(&snap, s.path).unwrap();
        let found: Vec<(String, K, Option<String>)> = symbols
            .iter()
            .map(|d| (d.name.clone(), d.kind, d.container.clone()))
            .collect();
        println!("{} (ABI {abi}) definitions: {found:?}", s.id);
        for (name, kind, container) in s.defs {
            assert!(
                found
                    .iter()
                    .any(|(n, k, c)| n == name && k == kind && c.as_deref() == *container),
                "{}: definition {name} ({kind:?}, in {container:?}) not found among {found:?}",
                s.id
            );
            // And the lookup `find_symbol` uses finds it by name.
            let by_name = idx.find_definitions(&snap, name, Some(*kind)).unwrap();
            assert!(!by_name.is_empty(), "{}: find_definitions({name})", s.id);
        }

        for name in s.absent {
            assert!(
                found.iter().all(|(n, ..)| n != name),
                "{}: {name} is a local or parameter, not a definition: {found:?}",
                s.id
            );
        }

        let mut seen = Vec::new();
        for (name, text) in s.refs {
            let hits = idx.references(&snap, s.path, name, 100).unwrap();
            seen.push(format!(
                "{name}:{}",
                hits.iter().filter(|h| !h.is_definition).count()
            ));
            assert!(
                hits.iter().any(|h| {
                    !h.is_definition
                        && line_of(s.source, h.location.range.start.line).contains(text)
                }),
                "{}: no reference to {name} on a line with {text:?}; hits: {:?}",
                s.id,
                hits.iter()
                    .map(|h| (h.is_definition, h.location.range.start.line))
                    .collect::<Vec<_>>()
            );
        }
        println!(
            "{} references (non-definition hits per name): {seen:?}",
            s.id
        );
    }
}
