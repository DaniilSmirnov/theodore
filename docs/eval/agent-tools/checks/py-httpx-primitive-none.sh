#!/usr/bin/env bash
# py-httpx-primitive-none: primitive_value_to_str(value, *, none) — a
# required keyword-only argument for what None becomes — and all six call
# sites pass none="" so behaviour is unchanged.
. "$(dirname "$0")/_lib.sh"

python3 - <<'EOF' || fail "definition, call sites or behaviour"
import ast, pathlib, sys

src = pathlib.Path("httpx/_utils.py").read_text()
mod = ast.parse(src)
fn = next((n for n in mod.body if isinstance(n, ast.FunctionDef)
           and n.name == "primitive_value_to_str"), None)
if fn is None:
    sys.exit("httpx/_utils.py: primitive_value_to_str is gone")
kw = [a.arg for a in fn.args.kwonlyargs]
if [a.arg for a in fn.args.args] != ["value"] or kw != ["none"] or fn.args.kw_defaults != [None]:
    sys.exit("expected def primitive_value_to_str(value, *, none) with no default for none")

calls, bad = 0, []
for path in [*pathlib.Path("httpx").rglob("*.py"), *pathlib.Path("tests").rglob("*.py")]:
    for node in ast.walk(ast.parse(path.read_text(), str(path))):
        if not isinstance(node, ast.Call):
            continue
        f = node.func
        name = f.id if isinstance(f, ast.Name) else f.attr if isinstance(f, ast.Attribute) else None
        if name != "primitive_value_to_str":
            continue
        calls += 1
        k = {x.arg: x.value for x in node.keywords}
        v = k.get("none")
        if len(node.args) != 1 or not (isinstance(v, ast.Constant) and v.value == ""):
            bad.append(f"{path}:{node.lineno}")
if bad:
    sys.exit('call sites without none="": ' + ", ".join(bad))
if calls != 6:
    sys.exit(f"expected the 6 call sites (_content.py 2, _multipart.py 1, _urls.py 3), found {calls}")

# Behaviour: the function alone, run.
ns = {}
code = "from __future__ import annotations\nPrimitiveData = object\n" + ast.get_source_segment(src, fn)
exec(compile(code, "primitive_value_to_str", "exec"), ns)
f = ns["primitive_value_to_str"]
cases = [((True, ""), "true"), ((False, ""), "false"), ((None, ""), ""),
         ((None, "null"), "null"), ((3, "x"), "3"), (("a", "x"), "a"), ((1.5, ""), "1.5")]
for (value, none), want in cases:
    got = f(value, none=none)
    if got != want:
        sys.exit(f"primitive_value_to_str({value!r}, none={none!r}) = {got!r}, want {want!r}")
try:
    f(None, "")
    sys.exit("none must be keyword-only")
except TypeError:
    pass
EOF
ok "keyword-only none, 6 call sites pass none=\"\", behaviour as asked"

PYTHONPYCACHEPREFIX=$(mktemp -d) python3 -m compileall -q httpx tests >/dev/null || fail "does not compile"
ok "httpx and tests compile"
