#!/usr/bin/env bash
# py-rename-rich-markup-render: rich.markup.render → render_markup; every
# import of it updated (text.py, console.py's alias, tests); no other
# `render` touched. Then rich's own markup, text and console tests run.
. "$(dirname "$0")/_lib.sh"
REV=43d3b04725ab9731727fb1126e35980c62f32377
HERE=$(cd "$(dirname "$0")" && pwd)

python3 - <<'EOF' || fail "imports or definition"
import ast, pathlib, sys

markup = ast.parse(pathlib.Path("rich/markup.py").read_text())
top = {n.name for n in markup.body if isinstance(n, ast.FunctionDef)}
if "render_markup" not in top or "render" in top:
    sys.exit("rich/markup.py: expected def render_markup and no def render")

bad = []
for path in [*pathlib.Path("rich").rglob("*.py"), *pathlib.Path("tests").rglob("*.py"),
             *pathlib.Path("examples").rglob("*.py")]:
    tree = ast.parse(path.read_text(), str(path))
    for node in ast.walk(tree):
        # from .markup import render / from rich.markup import render
        if isinstance(node, ast.ImportFrom):
            mod = node.module or ""
            if (node.level == 1 and mod == "markup") or mod == "rich.markup":
                bad += [f"{path}:{node.lineno}" for a in node.names if a.name == "render"]
        # rich.markup.render / markup.render
        if (isinstance(node, ast.Attribute) and node.attr == "render"
                and isinstance(node.value, (ast.Name, ast.Attribute))
                and (getattr(node.value, "id", None) == "markup"
                     or getattr(node.value, "attr", None) == "markup")):
            bad.append(f"{path}:{node.lineno}")
if bad:
    sys.exit("still imports the old name: " + ", ".join(bad))
EOF
ok "render_markup defined; no import of markup.render left"

# Methods named render elsewhere (Text.render, Lines.render, …) stay.
for f in rich/text.py rich/containers.py rich/console.py rich/segment.py; do
    [ "$(git show "$REV:$f" | grep -cE '^\s*def render\b')" = "$(matches '^\s*def render\b' "$f")" ] ||
        fail "$f: a method named render was renamed"
done
ok "other render methods kept"

PYTHONPYCACHEPREFIX=$(mktemp -d) python3 -m compileall -q rich tests >/dev/null || fail "does not compile"
for t in tests/test_markup.py tests/test_text.py tests/test_console.py; do
    env -i PATH="$PATH" HOME="$HOME" PYTHONPATH=. PYTHONDONTWRITEBYTECODE=1 python3 "$HERE/_pytest_lite.py" "$t" >/dev/null 2>"${TMPDIR:-/tmp}/lite.$$" ||
        { tail -n 20 "${TMPDIR:-/tmp}/lite.$$" >&2; fail "$t"; }
done
rm -f "${TMPDIR:-/tmp}/lite.$$"
ok "markup, text and console tests pass (argument-less ones, no pytest)"
