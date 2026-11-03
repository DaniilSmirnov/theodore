#!/usr/bin/env bash
# rust-host-github-parse-millis-fallback: parse_millis(s, fallback) in
# host-github, every caller updated (incl. the `.map(time::parse_millis)`
# ones and the unit tests), gitlab/bitbucket copies untouched.
. "$(dirname "$0")/_lib.sh"
REV=2a142d87e162ed38212e55a91704a9a90a5ab506
T=crates/host-github/src/time.rs

[ "$(matches '^pub\(crate\) fn parse_millis\([a-z_]+: &str, [a-z_]+: i64\) -> i64' $T)" = 1 ] ||
    fail "$T: expected pub(crate) fn parse_millis(s: &str, fallback: i64) -> i64"
ok "new signature"

unchanged $REV crates/host-gitlab crates/host-bitbucket
ok "gitlab and bitbucket copies untouched"

# Compiles, and the crate's own tests still pass: every call site has two
# arguments (a one-argument call or a bare `.map(time::parse_millis)` would
# not compile).
cargo_offline test -p theodore-host-github --lib >/dev/null || fail "cargo test -p theodore-host-github --lib"
ok "host-github compiles and its lib tests pass"

# The fallback is returned for bad input, and good input is unaffected:
# time.rs alone (it has no imports) plus three asserts, built by rustc.
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cat "$T" >"$tmp/t.rs"
cat >>"$tmp/t.rs" <<'EOF'

#[cfg(test)]
mod agent_tools_check {
    #[test]
    fn fallback() {
        assert_eq!(super::parse_millis("not a date", 7), 7);
        assert_eq!(super::parse_millis("", -1), -1);
        assert_eq!(super::parse_millis("1970-01-01T00:00:01.5Z", 7), 1500);
    }
}
EOF
rustc --edition 2024 --test -A warnings -o "$tmp/t" "$tmp/t.rs" >/dev/null || fail "time.rs does not build alone"
"$tmp/t" -q agent_tools_check >/dev/null || fail "parse_millis does not return the fallback"
ok "fallback returned for bad input, real timestamps unchanged"
