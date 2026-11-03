# Agent tools eval: the task suite

16 tasks and one smoke task (`tasks.yaml`) for comparing an agent with and
without the code-intelligence tools (`docs/code-intel.md` §7). Every task is
one where structure should beat grep-and-read: a name that is common, defined
twice, aliased, re-exported or reached through a trait, on a repo big enough
that reading around is costly.

| Repo | Language | Rev (pinned) | Size |
| --- | --- | --- | --- |
| `.` (Theodore) | Rust | `2a142d87e162ed38212e55a91704a9a90a5ab506` (origin/staging, v0.1.12) | 453 `.rs` files, 152k lines |
| honojs/hono | TypeScript | `734755ace341607628219ea1dd8ca17f01bf1a5c` (tag v4.12.34) | 307 `.ts` files under `src/` |
| encode/httpx | Python | `26d48e0634e6ee9cdc0533996db289ce4b430177` (tag 0.28.1) | 18k lines with tests |
| Textualize/rich | Python | `43d3b04725ab9731727fb1126e35980c62f32377` (tag v13.9.4) | 78 modules, 27k lines in `rich/` |

| id | repo | kind | what it asks | the trap for grep |
| --- | --- | --- | --- | --- |
| `smoke-domain-crate-name` | `.` | answer | package name of `crates/domain` | none: harness self-test |
| `rust-trace-language-remove` | `.` | answer | Remove button → the code that deletes the server | `remove` is everywhere; a trait hop and a `blocking` closure |
| `rust-impl-telemetry-sink` | `.` | answer | implementors of telemetry `Sink` | a second, unrelated `struct Sink` in `code-intel-toolchain` |
| `rust-callers-code-intel-status` | `.` | answer | callers of `CodeIntelPort::status` | ~40 `.status()` calls: HTTP responses, processes, pool, index |
| `rust-reexport-sandbox-data-dir` | `.` | answer | what `theodore_runtime::sandbox_data_dir` is | a `pub use … as`; two `default_data_dir` in the crate |
| `rust-rename-area-top-level-dir` | `.` | edit | rename `area::top_level_dir` | `prompt.rs` and `fixtures` each have their own `top_level_dir` |
| `rust-host-github-parse-millis-fallback` | `.` | edit | add a parameter, update callers | copies in gitlab/bitbucket; `.map(time::parse_millis)` uses |
| `ts-impl-router` | hono | answer | classes implementing `Router` | baseline: grep finds these; measures overhead |
| `ts-trace-jsx-runtime` | hono | answer | what `jsx` from `hono/jsx/jsx-runtime` runs | an alias re-export; `jsx` in `base.ts`; a second `jsxDEV` in `jsx/dom/` |
| `ts-callers-compose` | hono | answer | callers of `compose` | `composed`, comments; callers are a method, a private method, a nested function |
| `ts-default-router` | hono | answer | router behind `new Hono()` | `hono/quick` and `hono/tiny` presets use other routers |
| `ts-rename-merge-path` | hono | edit | rename `utils/url.ts` `mergePath` | `client/utils.ts` has its own `mergePath`; type `MergePath` |
| `py-trace-httpx-get` | httpx | answer | `httpx.get` → the call into httpcore | sync and async twins of every hop |
| `py-callers-rich-markup-render` | rich | answer | callers of `rich.markup.render` | imported `as render_markup`; dozens of `render` methods |
| `py-type-rich-screen-context` | rich | answer | type of `ctx` and `ctx.screen` | `screen` is a method, an attribute and a class |
| `py-rename-rich-markup-render` | rich | edit | rename `markup.render` | `Text.render`, `Lines.render`, `Segment` … must stay |
| `py-httpx-primitive-none` | httpx | edit | add a keyword-only parameter | six call sites over three modules |

11 answer tasks, 5 edit tasks, plus the smoke task.

## How the checks work

```
answer task ─▶ final message ─▶ check.value: contains | regex  (case-insensitive where it is a regex)
edit task   ─▶ worktree       ─▶ bash checks/<id>.sh, cwd = worktree ─▶ exit 0 = pass
```

- **Sets** (callers, implementors) are graded on one final line the prompt
  asks for: `CALLERS: a, b, c` or `IMPLEMENTORS: …`. The regex is anchored to
  that line, so a missing item **and** an extra item both fail. Items may be
  wrapped in backticks or bold, may carry `()` and a qualifier (`Core::`,
  `HonoBase.`, `Console.`, `layer::`); 2–3 items match in any order, 5–6 items
  must be in the alphabetical order the prompt asks for.
- **Traces and types** are graded on the hops or names that only the right
  answer contains, in order where the order is the point.
- Every regex is in the subset Rust's `regex` crate and Python's `re` share
  (no look-around, no back-references); both were run on the cases below.
- Edit checks need no network: static checks (grep, Python `ast`), `cargo
  check`/`cargo test --offline` (one shared target dir,
  `$AGENT_TOOLS_TARGET_DIR`, default `$TMPDIR/theodore-agent-tools-target`),
  `rustc --test` on a single file, `python3 -m compileall`, the repo's own
  tests run without pytest (`checks/_pytest_lite.py`: a stand-in `pytest`
  with `raises` and `mark`, argument-less tests only), and a TypeScript parse
  (`checks/_ts_parse.mjs`: Node ≥ 22.13 strips the types, then `node --check`;
  skipped with a note when no such Node is found). Nothing type-checks
  TypeScript: hono has no `node_modules` offline.
- Edit checks compare against the pinned rev with `git diff <rev> --`, so a
  committed edit counts the same as an uncommitted one.

## Ground truth

Derived by reading the code at each pinned rev in a shallow clone: ripgrep
for every spelling of the name (method call, path call, `use`/`import`
aliases, function references like `.map(f)`, multi-line calls), then each
hit read in context to drop the same-named things. No language server was
used for the answers.

### smoke-domain-crate-name

`theodore-domain` (`crates/domain/Cargo.toml` line 2). Check: contains.

### rust-trace-language-remove

```
commands::code_intel_remove        apps/desktop/src/commands.rs:1309   #[tauri::command]
 └▶ CoreApi::code_intel_remove      AppCore impl, apps/desktop/src/app_core.rs:902
     └▶ Core::code_intel_remove     crates/app/src/usecases/code_intel.rs:702   port.remove(..)
         └▶ CodeIntelPort::remove   impl for CodeIntel, crates/code-intel/src/lib.rs:1578
             └▶ CodeIntel::remove_language        crates/code-intel/src/lib.rs:1142
                 ├▶ pool().stop_server(name)       stops the sessions
                 └▶ blocking ─▶ Toolchain::remove_server   crates/code-intel-toolchain/src/lib.rs:460
                                └▶ std::fs::remove_dir_all(<data>/servers/<name>)
                    (and GrammarSource::remove ─▶ Toolchain::remove_grammar for the grammar)
```

Check: `(?is)remove_language.*remove_server`: both hops that only the real
path has, in order. A trace that stops at the port, or follows the grammar
branch only, fails.

### rust-impl-telemetry-sink

`Capture` (`telemetry/src/layer.rs:263`), `FileSink` (`file.rs:244`),
`OtlpSink` (`otlp.rs:226`), `PosthogSink` (`posthog.rs:148`), `StderrSink`
(`console.rs:34`). Found with a multi-line search for `impl … Sink … for … {`
over `crates/` and `apps/`; `crates/code-intel-toolchain/src/unpack.rs`'s
`struct Sink<'a>` and its inherent `impl Sink<'_>` are a different type, not
an implementor. Check: final line, sorted.

### rust-callers-code-intel-status

`Core::code_install_flow` (`crates/app/src/usecases/code_intel.rs:447`),
`Core::code_intel_status` (`:589`, the call split over lines),
`Core::code_status` (`:925`). Every `.status(` in the workspace was read:
the rest are `reqwest`/`Command` `status()`, `Pool::status`
(`code-intel/src/lib.rs:1265`), `Index::status` (`code-intel-syntax`), and
tests (`code-intel/tests`, `code-intel-lsp/tests`), which the prompt
excludes. No `CodeIntelPort::status` path call or `.map(…::status)` exists.
Check: final line, any order.

### rust-reexport-sandbox-data-dir

`crates/runtime/src/lib.rs:99` `pub use sandbox::default_data_dir as
sandbox_data_dir;` → `sandbox` is the crate's own module, not the
`theodore_sandbox` crate → `default_data_dir` at
`crates/runtime/src/sandbox.rs:19`, returning `~/.theodore-sandbox`
(`.theodore-sandbox` relative when there is no home dir). The other
`default_data_dir` (`identity.rs:82`) is the build's own dir, the wrong
answer. Caller: `apps/cli/src/sandbox.rs:232`. Check: the file and
`.theodore-sandbox`, either order.

### ts-impl-router

`LinearRouter`, `PatternRouter`, `PreparedRegExpRouter`, `RegExpRouter`,
`SmartRouter`, `TrieRouter` (each `src/router/<name>/…:  implements
Router<T>`). `RegExpRouterWithMatcherExport` in `prepared-router.ts` only
`extends RegExpRouter`, and `build/remove-private-fields.test.ts` declares a
class inside a test string; neither counts. Check: final line, sorted.

### ts-trace-jsx-runtime

```
hono/jsx/jsx-runtime (package.json exports ─▶ src/jsx/jsx-runtime.ts)
  export { jsxDEV as jsx } from './jsx-dev-runtime'
    └▶ jsxDEV          src/jsx/jsx-dev-runtime.ts:12   children from props
        └▶ jsxFn       src/jsx/base.ts:319             new JSXNode / JSXFunctionNode
```

Wrong turns: `export const jsx` in `src/jsx/base.ts:304` (the classic,
non-automatic runtime) and `jsxDEV` in `src/jsx/dom/jsx-dev-runtime.ts:10`
(the DOM runtime, builds a plain object). Check: `jsx/jsx-dev-runtime.ts`
not preceded by `dom/`, and `jsxFn`, either order.

### ts-callers-compose

`HonoBase.route` (`src/hono-base.ts:225`, in the handler it wraps a sub-app's
error handler with), `HonoBase.#dispatch` (`:450`), `every`
(`src/middleware/combine/index.ts:102`, inside the returned
`async function every`). The other hits are the two imports, `composed` and
a comment. Check: final line, any order; `#` and `HonoBase.`/`Hono.` optional.

### ts-default-router

`src/hono.ts`: `options.router ?? new SmartRouter({ routers: [new
RegExpRouter(), new TrieRouter()] })`. `hono/quick` (`LinearRouter` +
`TrieRouter`) and `hono/tiny` (`PatternRouter`) are the wrong answers.
Check: `SmartRouter`, then both inner routers in either order.

### py-trace-httpx-get

```
httpx.get                         httpx/_api.py:174
 └▶ httpx.request                 httpx/_api.py:39      with Client(...) as client
     └▶ Client.request            httpx/_client.py:771
         └▶ Client.send           :879
             └▶ _send_handling_auth       :930
                 └▶ _send_handling_redirects  :964
                     └▶ _send_single_request  :1001   transport = _transport_for_url(url)
                         └▶ HTTPTransport.handle_request   httpx/_transports/default.py:230
                             └▶ self._pool.handle_request(req)   httpcore
```

`Client._init_transport` (`_client.py:731`) makes the default
`HTTPTransport`. The async twins (`AsyncClient`, `AsyncHTTPTransport`) are
the trap. Check: `_send_single_request`, then `HTTPTransport` (word-bounded,
so `AsyncHTTPTransport` does not count) and `handle_request`.

### py-callers-rich-markup-render

`Console.render_str` (`rich/console.py:1429`, through `from .markup import
render as render_markup` at line 56) and `Text.from_markup`
(`rich/text.py:287`, a local `from .markup import render`). The only imports
of `markup.render` in the repo are those two and `tests/test_markup.py`.
Check: final line, either order, class prefix optional.

### py-type-rich-screen-context

`Console.screen()` (`console.py:1250`) returns `ScreenContext`, whose
`__enter__` returns itself (`console.py:444`), so `ctx` is a
`ScreenContext`. `ctx.screen` is `Screen(style=style)` (`console.py:423`),
`class Screen` in `rich/screen.py:18`. Check: `ScreenContext`, then
`screen.py` or `rich.screen.Screen`.

## Edit checks, proven both ways

Each script was run on the unmodified checkout (must fail), on a hand-made
correct solution (must pass) and on a plausible wrong one (must fail).

| id | unmodified | correct solution | wrong solution |
| --- | --- | --- | --- |
| `rust-rename-area-top-level-dir` | FAIL `area.rs does not define … first_path_segment` | PASS, incl. `cargo check -p theodore-grouping --tests` | rename everywhere (sed): FAIL `prompt.rs lost its own top_level_dir wrapper`; coarsen.rs missed: FAIL |
| `rust-host-github-parse-millis-fallback` | FAIL `expected … parse_millis(s: &str, fallback: i64)` | PASS: `cargo test -p theodore-host-github --lib`, then the fallback asserts | body still returns 0: FAIL `does not return the fallback`; a bad call site: FAIL (cargo) |
| `ts-rename-merge-path` | FAIL `does not export const joinPaths` | PASS, incl. parse of the three files | client's `mergePath` renamed too: FAIL `should be untouched: src/client`; a syntax slip: FAIL (parse) |
| `py-rename-rich-markup-render` | FAIL `expected def render_markup` | PASS: markup (21), text (82), console (87) tests | `text.py` import missed: FAIL `rich/text.py:285`; every `render` renamed: FAIL `a method named render was renamed` |
| `py-httpx-primitive-none` | FAIL `expected def … (value, *, none)` | PASS: 7 behaviour cases + keyword-only | one call site missed: FAIL `httpx/_multipart.py:87` |

What each one proves:

- `rust-rename-area-top-level-dir`: `area.rs` defines `pub(crate) fn
  first_path_segment(path: &str) -> &str`, `top_level_dir` is gone from
  `area.rs` and `coarsen.rs`, `prompt.rs` keeps its `fn top_level_dir(…) ->
  String` wrapper and its own callers and now delegates to
  `first_path_segment`, `crates/fixtures` is untouched, no other file names
  `top_level_dir`, and the crate builds with its tests.
- `rust-host-github-parse-millis-fallback`: the signature, gitlab/bitbucket
  untouched, the crate compiles with its tests and they pass (so every call
  site, the five `.map(time::parse_millis)` included, takes two arguments),
  and `time.rs` alone returns the fallback for bad input and the right
  millis for good input.
- `ts-rename-merge-path`: `export const joinPaths` and no `mergePath` left in
  `url.ts` code (comments and strings ignored), 7 uses in `hono-base.ts`
  (the import and 6 calls), `url.test.ts` updated, `src/client` and
  `src/types.ts` untouched, no other code uses `mergePath`, the edited files
  parse.
- `py-rename-rich-markup-render`: by `ast`, a module-level `def
  render_markup` and no `def render` in `markup.py`, no `from .markup` /
  `from rich.markup import render` and no `markup.render` anywhere in
  `rich/`, `tests/`, `examples/`; the `def render` counts of `text.py`,
  `containers.py`, `console.py`, `segment.py` match the rev; everything
  compiles; rich's markup, text and console tests pass.
- `py-httpx-primitive-none`: by `ast`, `def primitive_value_to_str(value, *,
  none)` without a default, exactly the 6 call sites each pass one positional
  and `none=""`; the function run alone gives `"true"`, `"false"`, `""`,
  `none` for `None`, `str(value)` otherwise, and rejects `none` passed
  positionally; httpx and its tests compile. httpx's own tests need httpcore
  and are not run.

## Answer checks, proven both ways

55 cases (written answers in several formats, and wrong answers: a missing
item, an extra item, the same-named wrong symbol, the async twin, the other
preset) were run through Python `re` and through the Rust `regex` crate
(1.x, `serde_yaml` parsing `tasks.yaml`): 0 mismatches in either. Examples
of what fails: `IMPLEMENTORS: FileSink, OtlpSink, PosthogSink, StderrSink`
(missing `Capture`), `CALLERS: code_install_flow, code_intel_status,
code_status, status`, `jsx is … src/jsx/base.ts … jsxFn`, `… →
AsyncHTTPTransport.handle_async_request`, `ctx.screen is a Live`.

## Least certain

- `rust-trace-language-remove` names a path, so an answer that lists the right
  two hops inside a wrong story still passes; the regex can't grade the prose.
- `ts-callers-compose`: `route` calls `compose` inside the handler closure it
  builds; an answer naming that closure "anonymous handler" instead of
  `route` fails.
- The final-line format is an instruction the agent can ignore; a correct
  answer without the line fails. Both arms get the same prompt, so this is
  noise, not bias.
