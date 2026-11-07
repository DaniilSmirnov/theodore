//! The text fallback covers every code file: a language with no grammar and
//! no server (Scala, PHP before its grammar is released, a Dockerfile) is
//! searched by name, and what the manifest calls prose (Markdown, HTML)
//! never is.

mod common;

use common::{Env, StaticGrammars, indexed, repo, status_of, workspace};
use pretty_assertions::assert_eq;
use theodore_ports::code_intel::{CodeIntelPort, InstallState, Position, Source, TrustMode};

fn at(line: u32, col: u32) -> Position {
    Position { line, col }
}

/// `(path, line)` of each symbol, in the order returned.
async fn found(ci: &dyn CodeIntelPort, root: &std::path::Path, name: &str) -> Vec<(String, u32)> {
    let a = ci.find_symbol(root, name, None).await.unwrap();
    assert_eq!(a.source, Source::Text, "{name}");
    a.items
        .iter()
        .map(|s| (s.path.clone(), s.range.start.line))
        .collect()
}

/// PHP has a manifest grammar entry, but no grammar is installed here (the
/// static source withholds it, as an offline machine or an unreleased
/// package would): its files are the text fallback's.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn php_only_worktree_answers_find_symbol_by_text() {
    let env = Env::new();
    let wt = repo(&[
        (
            "src/Invoice.php",
            "<?php\nnamespace App;\n\nclass Invoice implements Chargeable\n{\n    public function charge(int $cents): bool\n    {\n        return true;\n    }\n\n    public static function make(): self\n    {\n        return new self();\n    }\n}\n",
        ),
        (
            "src/Checkout.php",
            "<?php\n\nfunction checkout(Invoice $i)\n{\n    return $i->charge(5);\n}\n",
        ),
        (
            "README.md",
            "# Billing\n\nclass Invoice and function charge are documented here.\n",
        ),
        (
            "public/index.html",
            "<script>function charge() {}\nclass Invoice {}</script>\n",
        ),
    ]);
    let ci = env.facade(StaticGrammars::all());
    ci.open(workspace(wt.path(), TrustMode::Trusted))
        .await
        .unwrap();
    indexed(&ci, wt.path()).await;

    // Definitions only, from code files only: not the Markdown, not the
    // HTML (both prose), not the call in Checkout.php.
    assert_eq!(
        found(&ci, wt.path(), "charge").await,
        vec![("src/Invoice.php".to_string(), 6)]
    );
    assert_eq!(
        found(&ci, wt.path(), "Invoice").await,
        vec![("src/Invoice.php".to_string(), 4)]
    );
    assert_eq!(
        found(&ci, wt.path(), "checkout").await,
        vec![("src/Checkout.php".to_string(), 3)]
    );
    assert_eq!(
        found(&ci, wt.path(), "make").await,
        vec![("src/Invoice.php".to_string(), 11)]
    );

    // The note stays true: there is no grammar here, text search answers.
    let a = ci.find_symbol(wt.path(), "charge", None).await.unwrap();
    let note = a.note.unwrap_or_default();
    assert!(note.contains("php: no grammar yet, text search"), "{note}");
    assert!(!note.contains("html"), "{note}");
    assert!(!note.contains("markdown"), "{note}");

    // References and go-to-definition fall back the same way: the call in
    // Checkout.php (line 5) and the definition, nothing from the prose.
    let refs = ci
        .references(wt.path(), "src/Checkout.php", at(5, 17))
        .await
        .unwrap();
    assert_eq!(refs.source, Source::Text);
    let mut hits: Vec<(&str, u32)> = refs
        .items
        .iter()
        .map(|h| (h.location.path.as_str(), h.location.range.start.line))
        .collect();
    hits.sort();
    assert_eq!(hits, vec![("src/Checkout.php", 5), ("src/Invoice.php", 6)]);
    let def = ci
        .definition(wt.path(), "src/Checkout.php", at(5, 17))
        .await
        .unwrap();
    assert_eq!(def.items.len(), 1);
    assert_eq!(def.items[0].location.path, "src/Invoice.php");
    assert_eq!(def.items[0].location.range.start.line, 6);

    // Nothing is downloaded or installed for it.
    let php = status_of(&ci, wt.path(), "php").await;
    assert_eq!(php.grammar, InstallState::Unavailable);
    assert_eq!(php.index_percent, 0);
}

/// Scala has neither a grammar nor a server in the manifest. Before the
/// text-only entry it was not a language at all, so `find_symbol` was
/// empty.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scala_only_worktree_answers_find_symbol_by_text() {
    let env = Env::new();
    let wt = repo(&[
        (
            "src/main/scala/shop/Shop.scala",
            "package shop\n\nobject Shop {\n  case class Item(name: String)\n\n  def price(item: Item): Int = 1\n\n  val tax = 2\n}\n\ntrait Priced {\n  def total: Int\n}\n",
        ),
        (
            "src/main/scala/shop/Main.scala",
            "package shop\n\nobject Main extends App {\n  println(Shop.price(Shop.Item(\"a\")))\n}\n",
        ),
        ("docs/shop.md", "def price is documented; object Shop too\n"),
    ]);
    let ci = env.facade(StaticGrammars::all());
    ci.open(workspace(wt.path(), TrustMode::Review))
        .await
        .unwrap();
    indexed(&ci, wt.path()).await;

    let shop = "src/main/scala/shop/Shop.scala".to_string();
    let main = "src/main/scala/shop/Main.scala".to_string();
    assert_eq!(
        found(&ci, wt.path(), "price").await,
        vec![(shop.clone(), 6)]
    );
    assert_eq!(found(&ci, wt.path(), "Item").await, vec![(shop.clone(), 4)]);
    assert_eq!(
        found(&ci, wt.path(), "Priced").await,
        vec![(shop.clone(), 11)]
    );
    assert_eq!(found(&ci, wt.path(), "tax").await, vec![(shop.clone(), 8)]);
    // `object Shop` in Shop.scala and `object Main`: the Markdown line
    // mentioning `object Shop` is prose and stays out.
    assert_eq!(found(&ci, wt.path(), "Shop").await, vec![(shop.clone(), 3)]);
    assert_eq!(found(&ci, wt.path(), "Main").await, vec![(main.clone(), 3)]);

    let note = ci
        .find_symbol(wt.path(), "price", None)
        .await
        .unwrap()
        .note
        .unwrap_or_default();
    assert!(
        note.contains("scala: no grammar yet, text search"),
        "{note}"
    );

    let scala = status_of(&ci, wt.path(), "scala").await;
    assert_eq!(scala.files, 2);
    assert_eq!(scala.grammar, InstallState::Unavailable);
    assert!(scala.server.is_none());
}

/// Several text-only languages at once, each recognizing its own way of
/// defining a name, and a Dockerfile (a language with neither grammar nor
/// server, left out of the fallback before).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn text_only_languages_find_their_own_definitions() {
    let env = Env::new();
    let wt = repo(&[
        (
            "lib/billing.ex",
            "defmodule Billing do\n  def charge(x) do\n    x\n  end\nend\n",
        ),
        (
            "lib/Billing.pm",
            "package Billing;\n\nsub charge {\n  return 1;\n}\n",
        ),
        (
            "src/Billing.hs",
            "module Billing where\n\ncharge :: Int -> Int\ncharge x = x\n",
        ),
        (
            "src/billing.erl",
            "-module(billing).\n\ncharge(X) ->\n    X.\n",
        ),
        (
            "src/billing.clj",
            "(ns billing.core)\n\n(defn charge [x] x)\n",
        ),
        (
            "src/Billing.sol",
            "contract Billing {\n    function charge() public {}\n}\n",
        ),
        (
            "src/billing.dart",
            "class Billing {\n  int charge(int x) {\n    return x;\n  }\n}\n",
        ),
        ("db/billing.sql", "CREATE TABLE billing (id int);\n"),
        ("src/Billing.m", "@interface Billing : NSObject\n@end\n"),
        ("Dockerfile", "FROM node:20 AS builder\nRUN charge --now\n"),
        (
            "docs/billing.html",
            "<p>def charge: the charge function</p>\n",
        ),
        ("docs/billing.md", "def charge in prose\n"),
    ]);
    let ci = env.facade(StaticGrammars::all());
    ci.open(workspace(wt.path(), TrustMode::Review))
        .await
        .unwrap();
    indexed(&ci, wt.path()).await;

    let mut charge = found(&ci, wt.path(), "charge").await;
    charge.sort();
    assert_eq!(
        charge,
        vec![
            ("lib/Billing.pm".to_string(), 3),
            ("lib/billing.ex".to_string(), 2),
            ("src/Billing.hs".to_string(), 3),
            ("src/Billing.sol".to_string(), 2),
            ("src/billing.clj".to_string(), 3),
            ("src/billing.dart".to_string(), 2),
            ("src/billing.erl".to_string(), 3),
        ]
    );
    // The name is also a table (SQL), a module (Elixir), a class (Dart), a
    // contract (Solidity) and an interface (Objective-C).
    let mut billing = found(&ci, wt.path(), "billing").await;
    billing.sort();
    assert_eq!(
        billing,
        vec![
            ("db/billing.sql".to_string(), 1),
            ("src/billing.clj".to_string(), 1),
            ("src/billing.erl".to_string(), 1),
        ]
    );
    let mut upper = found(&ci, wt.path(), "Billing").await;
    upper.sort();
    assert_eq!(
        upper,
        vec![
            ("lib/Billing.pm".to_string(), 1),
            ("lib/billing.ex".to_string(), 1),
            ("src/Billing.hs".to_string(), 1),
            ("src/Billing.m".to_string(), 1),
            ("src/Billing.sol".to_string(), 1),
            ("src/billing.dart".to_string(), 1),
        ]
    );

    // The Dockerfile is covered: a reference from it sees its own line, and
    // the stage name is found as a use in it.
    let refs = ci
        .references(wt.path(), "Dockerfile", at(2, 5))
        .await
        .unwrap();
    assert!(
        refs.items
            .iter()
            .any(|h| h.location.path == "Dockerfile" && h.location.range.start.line == 2),
        "{:?}",
        refs.items
    );
    // ... and prose still is not.
    assert!(
        refs.items
            .iter()
            .all(|h| !h.location.path.ends_with(".md") && !h.location.path.ends_with(".html")),
        "{:?}",
        refs.items
    );
}
