# shellcheck shell=bash
# Shared by the agent-tools check scripts. Sourced, never run.
# cwd = the agent's worktree after the run. No network.
set -euo pipefail

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

ok() {
    echo "ok: $*"
}

# unchanged REV PATH...: PATH is identical to REV, committed or not.
unchanged() {
    local rev=$1
    shift
    git diff --quiet "$rev" -- "$@" || fail "should be untouched: $*"
}

# matches REGEX FILE: number of lines matching (0 when none or no file).
matches() {
    local n
    n=$(grep -cE -- "$1" "$2" 2>/dev/null) || true
    echo "${n:-0}"
}

# One cargo target dir for every run: a cold check of one crate takes
# seconds instead of a full build per worktree.
cargo_offline() {
    CARGO_TARGET_DIR="${AGENT_TOOLS_TARGET_DIR:-${TMPDIR:-/tmp}/theodore-agent-tools-target}" \
        cargo "$@" --offline -q
}

# ts_parse FILE...: TypeScript syntax check, types stripped by Node
# (>= 22.13). No such Node here: says so and passes; the other checks stand.
ts_parse() {
    local node=${NODE:-}
    [ -n "$node" ] || node=$(command -v node || true)
    [ -n "$node" ] || node=$(ls -d "$HOME"/.nvm/versions/node/v2*/bin/node 2>/dev/null | tail -n 1 || true)
    if [ -z "$node" ] || ! "$node" -e 'process.exit(require("node:module").stripTypeScriptTypes ? 0 : 1)' 2>/dev/null; then
        echo "skip: no Node >= 22.13 for the TypeScript parse check"
        return 0
    fi
    "$node" "$(dirname "${BASH_SOURCE[0]}")/_ts_parse.mjs" "$@" || fail "does not parse: $*"
}
