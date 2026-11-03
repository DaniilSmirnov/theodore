#!/usr/bin/env bash
# ts-rename-merge-path: src/utils/url.ts mergePath → joinPaths, every user
# updated; the client's own mergePath (src/client/utils.ts) and the
# MergePath type stay.
. "$(dirname "$0")/_lib.sh"
REV=734755ace341607628219ea1dd8ca17f01bf1a5c

# Code lines only: drop // and /* */-style comment lines and quoted strings
# (test titles like describe('mergePath') may stay or go).
code() { grep -vE '^\s*(//|/?\*)' "$1" | sed -E "s/'[^']*'//g; s/\"[^\"]*\"//g"; }
names() { code "$2" | grep -cE "\\b$1\\b" || true; }

[ "$(matches '^export const joinPaths\b' src/utils/url.ts)" = 1 ] || fail "src/utils/url.ts does not export const joinPaths"
[ "$(names mergePath src/utils/url.ts)" = 0 ] || fail "src/utils/url.ts code still names mergePath"
ok "url.ts: joinPaths defined, recursive call renamed"

[ "$(names mergePath src/hono-base.ts)" = 0 ] || fail "src/hono-base.ts still uses mergePath"
# the import + 6 calls
[ "$(code src/hono-base.ts | grep -oE '\bjoinPaths\b' | wc -l | tr -d ' ')" = 7 ] ||
    fail "src/hono-base.ts: expected joinPaths in the import and at 6 call sites"
ok "hono-base.ts: import and 6 call sites"

[ "$(names mergePath src/utils/url.test.ts)" = 0 ] || fail "src/utils/url.test.ts still imports or calls mergePath"
[ "$(names joinPaths src/utils/url.test.ts)" -ge 2 ] || fail "src/utils/url.test.ts does not use joinPaths"
ok "url.test.ts updated"

unchanged $REV src/client src/types.ts
ok "client mergePath and the MergePath type untouched"

left=$(grep -rlE '\bmergePath\b' src --include='*.ts' --include='*.tsx' | grep -vE '^src/client/' || true)
for f in $left; do
    [ "$(names mergePath "$f")" = 0 ] || fail "$f still uses mergePath"
done
ok "no other code uses mergePath"

# Syntax only (no node_modules, so no tsc).
ts_parse src/utils/url.ts src/hono-base.ts src/utils/url.test.ts
ok "edited files parse"
