#!/usr/bin/env bash
# rust-rename-area-top-level-dir: area::top_level_dir → first_path_segment,
# and only that function. prompt.rs keeps its own `top_level_dir` wrapper
# (and its callers); fixtures keeps its own copy.
. "$(dirname "$0")/_lib.sh"
REV=2a142d87e162ed38212e55a91704a9a90a5ab506
G=crates/grouping/src

[ "$(matches '^pub\(crate\) fn first_path_segment\(path: &str\) -> &str' $G/area.rs)" = 1 ] ||
    fail "area.rs does not define pub(crate) fn first_path_segment(path: &str) -> &str"
[ "$(matches '\btop_level_dir\b' $G/area.rs)" = 0 ] || fail "area.rs still names top_level_dir"
ok "area.rs renamed (definition and its tests)"

[ "$(matches '\btop_level_dir\b' $G/coarsen.rs)" = 0 ] || fail "coarsen.rs still names top_level_dir"
[ "$(matches '\bfirst_path_segment\b' $G/coarsen.rs)" -ge 1 ] ||
    fail "coarsen.rs: merge_area should call first_path_segment"
ok "coarsen.rs uses first_path_segment"

# prompt.rs: its own wrapper stays, now over the renamed helper.
[ "$(matches '^fn top_level_dir\(path: &str\) -> String' $G/prompt.rs)" = 1 ] ||
    fail "prompt.rs lost its own top_level_dir wrapper"
[ "$(matches '\bfirst_path_segment\(' $G/prompt.rs)" = 1 ] ||
    fail "prompt.rs wrapper should call first_path_segment once"
# lines naming it with a call: the def + 361, 467, 574 (two calls on one line)
[ "$(matches '\btop_level_dir\(' $G/prompt.rs)" = 4 ] ||
    fail "prompt.rs: the wrapper's own callers changed"
ok "prompt.rs wrapper kept, delegates to first_path_segment"

unchanged $REV crates/fixtures
others=$(grep -rlE '\btop_level_dir\b' crates apps --include='*.rs' | grep -vE "^($G/prompt.rs|crates/fixtures/src/synth.rs)$" || true)
[ -z "$others" ] || fail "top_level_dir still named in: $others"
ok "no other file names top_level_dir"

cargo_offline check -p theodore-grouping --tests || fail "cargo check -p theodore-grouping --tests"
ok "theodore-grouping compiles (lib + tests)"
