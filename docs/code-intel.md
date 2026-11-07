# Code intelligence — symbols, references, diagnostics

Agents and the review UI understand code by structure, not only by text.
Instead of grepping a name and reading whole files, an agent asks "where is
`save` defined, who calls it, does this still type-check" and gets
`file:line` answers with a few lines of context.

Status: **built**. Grammars download from the `grammars-<tag>` release
(`.github/workflows/grammars.yml`); language servers are found on the
machine or downloaded on consent, with progress; Remove deletes them. The
index has a size cap; limits apply live. Open: the eval (§10).

| Hook | Where |
| --- | --- |
| PR opened, review run starts | `open_pr.rs` `finish_open_full`, `space_tools.rs` `pr_tools`: `TrustMode::Review` |
| space created / unarchived / turn | `spaces.rs` `create_space`, `unarchive_space`, `space_send`: trusted per §8 |
| space turn ends (files the agent wrote) | `spaces.rs` turn end → `code_intel.rs` `spawn_space_code_changed`: the checkpoint's diff → `file_changed` |
| space's worktree rewritten outside a turn | Undo / Back to here / Revert (`space_checkpoints.rs`), Update from base, its Complete and Abort (`space_update.rs`), Continue (`space_pr.rs`) → `spawn_space_code_changed`: the diff of the worktree before → after → `file_changed` |
| closed | `spaces.rs` `archive_space`, `projects.rs` `close_branch_review`, runtime worktree GC → `Core::code_intel_sweep` |

---

## 1. Two layers

```
question ─▶ language server ready? ── yes ─▶ semantic layer (LSP)        precise
                                   └─ no ──▶ syntax layer (tree-sitter)  by name
                                             + status: warming | none
```

| Question | Syntax layer (tree-sitter) | Semantic layer (LSP) |
| --- | --- | --- |
| symbols in a file, their ranges | yes | yes |
| definition of a call | candidates by name | exact, through imports, re-exports, trait/interface impls |
| references | every match of the name, ranked | only this symbol of this type |
| type of an expression | no | yes |
| does the head still compile / type-check | no | diagnostics |
| implementations of an interface | no | yes |
| rename | no | yes |
| needs | a grammar | a server + (for full precision) the project's dependencies |

- The syntax layer always answers, for every language with a grammar, within
  milliseconds once a file is indexed.
- The semantic layer is optional: used when a server for the language is
  installed and warmed up. No single engine gives semantics for every
  language, so precision comes from each language's own server, behind one
  client.

## 2. Architecture

```
┌─ callers ──────────────────────────────────────────────────────────────────┐
│  UI (diff: go-to-definition, hover)   agents claude/codex/gemini   grouping│
│        │ Tauri IPC                          │ MCP                      │   │
└────────┼────────────────────────────────────┼──────────────────────────┼───┘
         ▼                                    ▼                          │
┌─ crates/mcp ──────────────────────────────────────────┐                │
│  review-tools / space-tools (§7)                      │                │
└────────────────────────┬──────────────────────────────┘                │
                         ▼                                               ▼
┌─ crates/app ───────────────────────────────────────────────────────────────┐
│  repo added / PR or space opened ─▶ languages ─▶ plan ─▶ ask ─▶ install    │
│  ─▶ index (§5)            query ─▶ LSP ready? ─▶ LSP : tree-sitter         │
└────────────────────────┬──────────────────────────────┬────────────────────┘
                         ▼                              ▼
┌─ crates/ports ─────────────────────┐  ┌──────────────────────────────────┐
│  CodeIntelPort                     │  │  ToolchainPort                   │
│  symbols · references · definition │  │  detect · plan · install         │
│  diagnostics · status              │  │                                  │
└───────────────┬────────────────────┘  └───────────────┬──────────────────┘
                ▼                                       ▼
┌─ crates/code-intel ────────────────────────────────────────────────────────┐
│  syntax/                   lsp/                      toolchain/            │
│  tree-sitter index         lsp-types + async-lsp     manifest (§3)         │
│  tags queries (.scm)       server pool (§6)          detect · download     │
│  cache by blob sha         trust flags (§8)          sha256 · unpack       │
│                                                      grammar ABI check     │
└────────┬──────────────────────────┬──────────────────────────┬─────────────┘
         ▼                          ▼                          ▼
┌─ on disk ──────────────────────────────────────────────────────────────────┐
│  <data dir>/code-intel/                                                    │
│   ├─ index.sqlite        symbols by blob sha · what is installed (a cache) │
│   ├─ grammars/<pkg>-<ver>.dylib                                            │
│   ├─ servers/<name>/<ver>/                                                 │
│   └─ server-data/<name>/<hash>/   a server's {data}: per worktree and trust │
│                                                                            │
│  in the binary: the manifest only — no grammar, no server                  │
└────────────────────────────────────────────────────────────────────────────┘
         ▲                                        ▲
┌─ CI ───┴────────────────────────┐      ┌────────┴───────────────────────────┐
│ grammars.toml ─▶ cc ─▶ arm64    │      │ upstream releases · npm tgz · wheel│
│ ─▶ sha256 ─▶ release assets     │      │ built by us where upstream has no  │
│ ─▶ manifest (generated)         │      │ macOS binary (gopls)               │
└─────────────────────────────────┘      └────────────────────────────────────┘
```

`<data dir>` is Theodore's data directory (`THEODORE_DATA_DIR`, `~/.theodore`
by default, see `docs/config.md`).

Layering follows `docs/plan.md` §2.1: the shared types and `CodeIntelPort`
live in `crates/ports` (`code_intel.rs`); `app` sees only the port. The
adapter is four crates:

| Crate | Owns |
| --- | --- |
| `code-intel-toolchain` | manifest, detection, downloads, grammar loading |
| `code-intel-syntax` | tree-sitter tags index, `index.sqlite` |
| `code-intel-lsp` | LSP client, server pool, lifecycle |
| `code-intel` | `CodeIntelPort` over the three: routing, fallbacks |

`index.sqlite` is a cache, not app state: deleting it costs a reindex, so it
stays out of `crates/store` and its migrations.

## 3. Languages are data

A language is an entry in the **manifest**: which files it owns, which grammar
parses it, which servers can serve it and how to get each one. Adding a
language is a manifest change, not new code; the few servers with quirks get a
small Rust hook next to their entry.

```yaml
- id: python
  globs: ["*.py", "*.pyi"]
  grammar: { package: tree-sitter-python, version: 0.23.6 }
  servers:                         # first one that is available wins
    - name: ty
      detect: [["ty", "--version"]]
      download:
        darwin-arm64: { url: "…", sha256: "…", format: tar.gz, bin: ty }
      launch: ["{bin}", "server"]
    - name: pyright                # only if the user has it
      detect: [["pyright-langserver"]]
      launch: ["{bin}", "--stdio"]
      requires: { runtime: node, min: 14 }   # checked by running it (§4.3)
```

- The manifest is compiled into the binary. Its grammar entries are generated
  by CI (§4.1); server entries are pinned by hand. A new version of the
  manifest ships with a Theodore release; there is no "latest".
- The app carries no grammar and no server of its own. Everything is
  downloaded for the languages a project actually uses.
- A language needs neither: an entry with only `globs` (Scala, Elixir, Dart,
  SQL, Dockerfile, ...) is **text-only**. Its files are code the text
  fallback (§5.2) searches by name; nothing is downloaded for it, and a
  query's note says `scala: no grammar yet, text search`. Adding a grammar or
  a server later is the same entry plus one line in `grammars.toml` or
  `servers:`.
- `prose: true` marks data and markup (Markdown, JSON, YAML, TOML, HTML):
  symbols and servers may serve it, the text fallback never searches it, and
  `find_symbol` leaves its headings and keys out (an empty answer names the
  prose files that had the name); a file's own symbols still list them. A
  language is code when it is in the manifest and not prose. HTML is prose
  because a name in it is text or an attribute, not a definition; CSS, SCSS
  and SQL are code (classes, mixins, tables, functions are defined there).
- The first language whose glob matches a path owns it, so a new entry goes
  at the end of `languages:` and cannot take files from an older one (`*.h`
  stays C, `*.kts` Kotlin, `*.m` is Objective-C).

### 3.1 Which languages a project gets

```
<repo>/.theodore/config.yaml  code_intel.languages ─┐
~/.theodore/config.yaml       code_intel.*          ─┼─▶ effective list ─▶ plan
detected: file extensions in the repo (git ls-files) ─┘
```

```yaml
code_intel:
  enabled: true              # off: no index, no servers, no tools
  install: ask               # ask | always | never
  languages: [typescript, rust]   # empty: detected
  servers:
    python: pyright          # prefer this server over the manifest order
```

- The repo layer is read the way every repo config is (`docs/config.md` §1):
  at the base, never at a PR's head, so a PR cannot change what gets
  downloaded or run.
- `languages` set → only those. Empty → the languages detected in the repo,
  by file count.

## 4. Artifacts

### 4.1 Grammars: one dylib per grammar package

| Package | In one dylib |
| --- | --- |
| `tree-sitter-typescript` | `typescript` + `tsx` |
| `tree-sitter-markdown` | `markdown` + `markdown_inline` |
| `tree-sitter-php` | `php` (its `php_only` grammar is not built: no manifest language owns it) |
| `tree-sitter-python` | `python` |

```
grammars.toml (repo, commit) ─▶ cc parser.c + scanner.c ─▶ arm64
  ─▶ sha256 ─▶ release assets
  ─▶ manifest entries (generated, never edited by hand)
```

- arm64 only, like the app (`build.yml` builds `aarch64-apple-darwin`). The
  manifest is keyed by platform, so another target is one more build line
  (and `lipo` for a universal file).
- One CI job builds every grammar from a pinned list. A new language is one
  line in `grammars.toml`.
- The tags query (`tags.scm`, defs and refs) ships next to its dylib. Most
  come from upstream; a few languages ship none (json, yaml, toml, bash,
  markdown, kotlin — `markdown_inline` has nothing of its own to tag and
  stays without one on purpose), so `scripts/build-grammars.sh` falls back to
  our own, hand-written against that language's own node types:
  `crates/code-intel-toolchain/queries/<id>-tags.scm`. A grammar can also
  name one of these explicitly (`tags_query_local` in `grammars.toml`) to
  **replace** upstream's query or **append** to it
  (`tags_query_local_mode`) — rust uses `append` to add scoped calls
  (`Foo::new()`) and generic/scoped impl containers (`impl Foo<T> for X`,
  `impl fmt::Display for X`) that upstream's query misses, on top of
  upstream's own definitions. php appends enums, nullsafe calls, constants
  and the plain `helper()` / `new Foo()` calls upstream's query misses; java
  appends enums, records and annotation types. c-sharp **replaces** upstream:
  its `tags.scm` ends with a `@module` capture tree-sitter-tags rejects, so it
  does not compile as shipped; ours keeps its valid patterns and adds
  structs, enums, records, properties, local functions, file-scoped and
  dotted namespaces and plain calls. The release tag CI publishes under
  (content-addressed from `grammars.toml`) also hashes every file in that
  queries directory, so editing one of our own queries alone still
  produces a new release.
- Loaded with `libloading`. Before use, the grammar's ABI version
  (`ts_language_version`) is checked against the range the linked tree-sitter
  runtime supports; out of range → not loaded, the language has no syntax
  layer, nothing crashes.
- Signing: the app is signed ad-hoc **with** the hardened runtime
  (`codesign … -o runtime`, as Tauri bundles it). Its library validation
  loads only dylibs of the app's own Team ID, and an ad-hoc signature has
  none, so the app carries `com.apple.security.cs.disable-library-validation`
  (`apps/desktop/Entitlements.plist`). The sha256 check above is what stands
  between a download and `dlopen`.

  | App signature | Grammar dylib | Loads |
  | --- | --- | --- |
  | ad-hoc + runtime | ad-hoc | only with the entitlement |
  | Developer ID + runtime | same Team ID | yes; the entitlement can go |
- No language packs (several languages in one file): they download languages
  nobody asked for and save little.

### 4.2 Servers: upstream binaries, checked by hash

```
manifest entry ─▶ download ─▶ sha256 (required) ─▶ unpack ─▶ servers/<name>/<ver>/
                                                  └─ unsigned? codesign -s -
```

| Question | Answer |
| --- | --- |
| Source | the server's own release (GitHub, npm, NuGet, its vendor's CDN), at a pinned version |
| Checksum | sha256 in the manifest, always. No hash → the entry is invalid |
| Hosts | each entry names its host; a redirect elsewhere fails the download |
| Formats | `tar.gz`, `zip` (a `.nupkg` and JetBrains' `.sit` are zips), `gz`, `binary`, npm `tgz`, wheel (a zip). Archive paths are checked: nothing escapes the target directory |
| No macOS binary upstream | built in our CI and published as our asset (gopls: Go, cross-compiles, BSD) |
| Upstream deletes a release | mirror into our assets (MIT / Apache / BSD allow it); policy open (§11) |
| Signing | a server runs as a child process, so our hardened runtime does not apply to it. An unsigned arm64 binary does not start, so it gets an ad-hoc signature after download |
| Updates | a new manifest version in a Theodore release. The old version stays until the new one has started once, then it is removed |
| Licence | checked per entry, in the manifest's comment: nothing is fetched behind our prompt whose licence asks the user to accept it personally (intelephense stays detect-only) |

### 4.3 Server availability

| Language | Server, in order | How it gets here | Runtime |
| --- | --- | --- | --- |
| Rust | rust-analyzer | download | none |
| TypeScript, TSX, JavaScript | tsgo | download | none |
| | typescript-language-server | found on the machine | Node 18+ |
| Python | ty | download | none |
| | pyright | found on the machine | Node 14+ |
| Go | gopls | download (our build) | none |
| C, C++ | clangd | download | none |
| Lua, TOML, Markdown, Terraform | lua-language-server, taplo, marksman, terraform-ls | download | none |
| Swift | sourcekit-lsp | found on the machine (Xcode) | none |
| Zig | zls¹ | found on the machine | none |
| Kotlin | kotlin-lsp | download, 360 MB, 1.1 GB unpacked | its own JBR |
| Java | jdtls | download | Java 21+ |
| C# | csharp-ls | download | .NET SDK 10+ |
| PHP | intelephense² | found on the machine | Node 20+ |
| | phpactor | download | PHP 8.2+ |
| Ruby | ruby-lsp³ | found on the machine | Ruby 3+ |

¹ zigtools/zls only publishes darwin-arm64 as `.tar.xz`; `ArchiveFormat`
(`crates/code-intel-toolchain/src/manifest.rs`) has no xz variant yet, so zls
is detect-only for now.

² Its licence is proprietary and personal ("by installing this software you
agree"), for one person using it with an editor. That is yours to accept, not
a download behind our prompt or `install: always`.

³ A gem with native dependencies and no standalone build.

A server's runtime is real data: `requires: { runtime: java, min: 21 }` in the
manifest, checked by running it, because `which` lies (macOS's `/usr/bin/java`
exists with no JDK behind it).

```
requires ─▶ candidates ─▶ inside the worktree and a review? ─▶ skip
   java: JAVA_HOME · java_home -v <min>+ · PATH · Homebrew's keg-only openjdk*
   else: PATH
         ─▶ run it (5 s; a review run's cwd is neutral: a PR's .tool-versions
            or .ruby-version does not steer it) ─▶ parse the version
               ├─ ≥ min ─▶ found      {runtime} in `launch`; JAVA_HOME or
               │                      DOTNET_ROOT and PATH in the environment
               ├─ < min ─▶ remembered, next
               └─ fails ─▶ next
```

| Runtime | Probe |
| --- | --- |
| `node`, `ruby`, `php` | `--version`, then its own idea of where it is (`process.execPath`, `RbConfig.ruby`, `PHP_BINARY`), so a version manager's shim can't pick again from the server's cwd |
| `java` | `-XshowSettings:properties -version`: stderr has the version and `java.home` |
| `dotnet` | `--list-runtimes`, newest `Microsoft.NETCore.App` |
| `dotnet-sdk` | the same, and `--list-sdks` not empty: MSBuild needs an SDK, a runtime can be alone |

Choosing, per language (`Toolchain::server_for`):

```
each server, preferred first (code_intel.servers.<language>), else manifest order
  ├─ trusted_only and a review run ─▶ skipped (§8)
  ├─ runtime missing or too old    ─▶ skipped, remembered
  └─ found on the machine or downloaded ─▶ this one: System | Installed
none ─▶ the first runnable one with a download ─▶ Missing, Downloading, Failed
     ─▶ else a remembered runtime              ─▶ NeedsRuntime { runtime, min, found }
     ─▶ else skipped as trusted_only           ─▶ TrustedOnly
     ─▶ else the first                         ─▶ Unavailable, with its hint
```

- A binary on the machine needs its runtime too: pyright with no Node is not
  available.
- Nothing is downloaded for a server that can't run: not by `install: ask`'s
  suggestion, not by `install: always`, not by the Install button.
- A runtime installed later is picked up the next time a worktree opens, or
  when the app restarts. A worktree already open keeps its answer (an Install
  of the language asks again).
- `{data}` in `launch`: `server-data/<name>/<hash>/`, per worktree and trust
  mode, so a review reads nothing a trusted session imported. Never in the
  worktree. Remove deletes it; a launch drops the ones no launch touched in
  14 days.

Detection looks, in order: the manifest's `detect` commands on `PATH`,
`rustup which`, `xcrun --find`, the repo's `node_modules/.bin`. A server found
on the machine wins over a download. jdtls and csharp-ls have no `detect`:
installed elsewhere they have another command line, and ours is the one tested.

## 5. Install and index

### 5.1 Asking

```
repo added ─▶ detect languages ─▶ plan
   ├─ rust-analyzer: found via rustup ✓
   ├─ ty: download?       [Yes] [No]
   └─ tsgo: download?     [Yes] [No]

new language in a PR ─▶ toast "C++: download clangd for exact references?" ─▶ never blocks
```

| `code_intel.install` | Grammars | Servers |
| --- | --- | --- |
| `ask` (default) | downloaded without asking | asked per server, answer remembered |
| `always` | downloaded | downloaded |
| `never` | downloaded | only ones already installed |

Grammars are small and run nothing, so they are not asked about. Servers run
as processes, so they are. A server whose runtime is missing is neither asked
about nor downloaded (§4.3): its row says what it needs. Neither is one that
never serves a review, while a review is what asks.

- Downloads start when a repo is added, in the background, so the first PR
  does not wait.
- Offline, or a check fails → that language has no code intelligence; review
  works as it does today.
- The answers live in one list (`code_intel.declined`, an app setting).
  "Not now" on the toast declines every language it names at once; the core
  edits the list one answer at a time (a lock around read, edit, write), so
  none is lost.

Progress: an install (`code_intel_install`, or `install: always`) emits
`Ephemeral::CodeIntelProgress` per item, at most four times a second while
it downloads:

```
install(rust) ─▶ grammar tree-sitter-rust ─▶ downloading{received, total?} … ─▶ installed | failed{error}
              ─▶ server  rust-analyzer    ─▶ downloading{…} …               ─▶ installed | failed{error}
                 (on disk already: installed only; found on the machine: no server event)
```

### 5.1.1 Remove

Project settings → Languages → Remove (`code_intel_remove(language)`, §9.1):

```
close worktrees opened only for the settings ─▶ an open review or space has the language? ── yes ─▶ refused: "a space or review is using it (space “…”, 1 review)"
                                                     └─ no ─▶ stop the server's sessions (every worktree)
                                                              ─▶ delete servers/<name>/ (every version) and the grammar package
                                                              ─▶ both Missing; language "removed" + declined
```

| After Remove | Grammar | Server |
| --- | --- | --- |
| a worktree with the language opens | not fetched on its own (`removed.json`): text search | not suggested, not installed by `always` |
| Install | downloaded again, the mark is gone | downloaded again, the No is forgotten |

- A grammar package takes all its languages: removing `typescript`
  removes `tsx`.
- A server found on the machine is not ours: it stays, and still serves.
- Remove takes a server's `{data}` too. What a server keeps elsewhere on its
  own is not ours to find: intelephense under `~/.config/intelephense`, Gradle
  and Maven under `~/.gradle` and `~/.m2`.

### 5.2 Index by blob, not by worktree

The cache key is the git blob sha of a file. Worktrees of one repo share it.

```
blob sha ─▶ symbols (defs, refs, ranges)        index.sqlite, shared by repos

worktree A (PR #12 head)  ─┐
worktree B (PR #12 base)  ─┼─▶ most blobs are shared ─▶ only new blobs are parsed
space C (agent editing)   ─┘
new PR revision           ─▶ only changed blobs are parsed
```

- Blob ids of a commit come from `git ls-tree -r`; nothing is hashed for
  committed files. A dirty file in a space is hashed by us.
- Order, as one background priority queue:

```
1. files in the diff (head + base)                          seconds
2. files mentioning the changed symbols' names (text scan)  then
3. the rest of the repo, low priority                       may never finish
```

- Queries never wait for the index: they answer from what is indexed and say
  `index: 62%`, and fill gaps with a text search for the name.
- The text search covers every code file the syntax layer does not: files of
  a language with no grammar (text-only ones included, §3), and files not
  indexed yet. Prose is left out. A hit counts as a definition (what
  `find_symbol` returns) when the line says so: a keyword right before the
  name (`def foo`, `class Foo`, `fn`, `fun`, `func`, `sub`, `defmodule`,
  `(defn`, `CREATE TABLE`, `contract`, ...), a typed signature
  (`void foo(int x) {`), or the language's own form (Erlang `foo(X) ->`,
  Haskell `foo ::`, R `foo <- function`). Calls (`x = foo(1)`, `self.foo()`) are not; they
  show up in `references`. Best effort, and the kind is always `other`.
- A name question (`find_symbol`) first asks `git status` what changed
  since the snapshot: new untracked files (not ignored) and edits nobody
  reported are hashed, added and indexed first (≤ 2 s), so a file written
  earlier in the same turn is found, by the syntax layer or the text search.
  At most 2000 such paths per question.
- Skipped: `.gitignore`d, `linguist-generated`, vendored, files over
  `code_intel.max_file_kb`. A larger limit, set while worktrees are open,
  retries the files skipped under the old one. An empty `find_symbol` says
  which skipped code files mention the name (`not searched, over
  code_intel.max_file_kb: src/big.ts`).
- Eviction: `index.sqlite` is kept under `code_intel.cache_mb`, least
  recently used blob first. A blob is used when it is indexed, found cached
  by a worktree opening, or answers a query.

```
open · close · every 10 min · a smaller cap ─▶ used bytes ≤ cap? ── yes ─▶ nothing (three pragmas)
   └─ no ─▶ 1. blobs no open worktree has, oldest first  ─┐ down to 90% of the cap
            2. still over: blobs of open worktrees        ─┘ they count as not indexed:
                                                             text search fills in, a query re-indexes the file
         ─▶ incremental_vacuum: the file shrinks
```

### 5.3 Spaces: follow the edits

```
edit tool (replace_symbol_body, …) ─▶ write ────────────────────┐
turn ends ──────────────────────────┐                            │
Undo · Back to here · Revert        ├─▶ worktree before → after ─┴─▶ file_changed ─▶ reindex the file
Update from base · Complete · Abort │   (its diff)                                └─▶ didChange to the
Continue ───────────────────────────┘                                                 LSP server ─▶ diagnostics
```

Every operation that rewrites a space's worktree reports the files it
moved (`spawn_space_code_changed`), so a file Undo removed leaves no symbol
behind. Abort diffs the merged base's tip against the checkpoint it goes
back to: a superset of what the merge wrote. Hand edits are not watched:
they reach the index with the next turn's checkpoint.

The agent's own Edit tool writes mid-turn and tells nobody, so a question
never trusts an earlier read of a file:

```
question about path ─▶ hash it again ─▶ not the indexed blob? ─▶ reindex it first
any LSP request     ─▶ re-read every document open on the server in that worktree
                       └─ differs ─▶ didChange (next version) · gone ─▶ didClose
diagnostics         ─▶ after that change: pulled again (`textDocument/diagnostic`),
                       or wait ≤ 2 s for a publish of that version; older ones
                       are dropped, never shown for the new text
rename edits        ─▶ each carries what its range held in the server's copy
apply (any edit)    ─▶ the file no longer holds that? ─▶ refused, nothing written:
                       "changed since the language server read it; ask again"
```

`replace_symbol_body` / `insert_after_symbol` check the disk against the
indexed blob the same way: a body is read only from the bytes its ranges
were parsed from.

## 6. Language servers at run time

```
query ─▶ pool[(repo, language)] ── alive ─▶ request
                                 └─ none ──▶ spawn (stdio) ─▶ initialize
                                             ─▶ status: warming ─▶ ready
idle code_intel.idle_minutes ─▶ shutdown ─▶ exit ─▶ kill after 5 s
                              └─ back on: a question · the worktree used again
                                          · project settings → Languages read
```

- One client (`lsp-types` + `async-lsp`), every server behind it. Theodore's
  own code is the pool and the lifecycle.
- One server per (repo, language) where the server supports
  `workspaceFolders`; worktrees join it as folders. Otherwise one per
  worktree.
- At most `code_intel.max_servers` alive; the least recently used goes first.
  A change of it or of `idle_minutes` restarts the pool. Coming back from idle
  on its own (not for a question) a server only takes a free slot: it never
  pushes out a running one.
- Locations a server answers outside the worktree (std, a dependency's
  source) never enter `items`, whose paths are repo-relative. They are
  counted, and the note says so: `1 result outside the worktree (std /
  dependencies): /…/core/src/option.rs`. A definition only outside the
  worktree is still the server's answer: no fallback to a same-named local
  symbol.
- A server sees one tree. Base files are served by the syntax layer; a second
  server on the base is started only when a question needs it.
- A server that crashes is restarted once per session; a second crash marks
  it failed for that worktree, and the syntax layer answers.
- A request a server did not offer at `initialize` (taplo has no
  `workspace/symbol`) is not sent, and a "method not found" answer counts the
  same: the syntax layer answers, with no note about it.
- JVM servers start slowly and answer empty while they import. `warming` holds
  until no `$/progress` token is in flight for `ready_settle` (1.5 s), so a
  question meanwhile goes to the syntax layer, not to an empty answer. A slow
  server sets its own in the manifest:

  | | `initialize` + warm-up | settle | Why |
  | --- | --- | --- | --- |
  | default | 60 s + 180 s | 1.5 s | |
  | jdtls, kotlin-lsp | 600 s | 1.5 s | a real build's import takes minutes |
  | ruby-lsp | 300 s | 1.5 s | the first start installs a composed bundle before it answers `initialize` |
  | csharp-ls | 300 s | 5 s | announces loading about 1.3 s after `initialized` |
- Full precision needs the project's dependencies (`node_modules`,
  `cargo fetch`). A space has them after `setup`; a review worktree does not,
  and Theodore never installs them there (it is not our code to run), so
  review answers are partial: unresolved imports fall back to the syntax
  layer.

## 7. Tools

Every tool returns locations with a few lines of context and its source:
`lsp`, `syntax`, or `text`, so the agent knows how far to trust it.

### 7.1 Review (`review-tools`, read-only)

| Tool | Returns |
| --- | --- |
| `find_symbol(name, kind?)` | definitions matching the name |
| `get_symbol(path, name)` | the symbol's body only, not the whole file |
| `references(path, line, col)` | call sites / uses, ranked: same file → imports → the rest |
| `definition(path, line, col)` | where it is defined |
| `diagnostics(path?)` | errors and warnings on the head |
| `status()` | per language: grammar, server, index %, warming |

The UI uses the same port: go-to-definition (`find_symbol`) and hover
(`code_intel_hover({pr, path, line, col})`, at the PR's head worktree) in
the diff. Grouping can use references to link "changed here" with "called
there" without an LLM call.

⌘-click in the diff (`SymbolPeek`): a head-side line sends its position
(`line`, `col`, `side: head`), so the answer is that exact symbol; a
base-side line has no head-worktree position and goes by name (references
at the clicked file's definition). Definitions rank first, then hits in the
clicked file. The chip names the least trusted source of the hits shown:
language server, parser or text search.

```
hover(path, line, col) ─▶ server ready? ── yes ─▶ its hover text         source: lsp
                                        └─ no / empty ─▶ definition by name ─▶ its signature lines   source: syntax
                                                         (to the body's `{`, `:` or `;`, ≤ 4 lines)
                                                         "Matched by name, no language server: path:line"
                                                         none ─▶ null
```

In the diff (`ui/src/ui/symbols/hover.ts`): the pointer rests 0.5 s on an
identifier of a head-side line (⌘ held: 80 ms), one call per position
(`pr@head:path:line:col`; only a server answer is cached: no answer, a
by-name one or a failure is asked again, so a warming server gets its
turn), the markdown shown in
a card under the word; a `syntax` answer is marked "from the parser".
Leaving, scrolling, a click or Esc hides it.

### 7.2 Spaces (`space-tools`)

Everything above, plus edits in modes that allow them (`AgentMode::Edit`,
`AgentMode::Run`; never in Ask):

| Tool | Replaces |
| --- | --- |
| `replace_symbol_body(path, name, body)` | Edit with hand-matched old text |
| `insert_after_symbol(path, name, text)` | the same |
| `rename_symbol(path, line, col, new)` | grep + N edits |
| `diagnostics(path?)` | a full `run_check` after every edit; `run_check` stays for the end |

- A symbol's range may start after the modifiers on its line (`export`,
  `pub`). A body that starts with those same words replaces them too, so
  `export function …` never becomes `export export function …`; a body
  without them leaves them in place.

- Grep and Read stay. Strings, config, comments and error texts are not
  symbols. The system prompt says: for code, use the symbol tools first.
  Taking Grep away is decided only by an eval (§10).
- The tools work the same for claude, codex and gemini and share the cache
  with review.

## 8. Trust

Some servers run code from the project they open.

| Server behavior | Review run | Space |
| --- | --- | --- |
| rust-analyzer build scripts (`cargo.buildScripts.enable`) | off | on, after the repo's commands are trusted (`docs/config.md` §5) |
| rust-analyzer proc macros (`procMacro.enable`) | off | same |
| plugins named in project config (e.g. tsserver plugins) | off | same |
| jdtls: Gradle and Maven import, a `.factorypath`'s annotation processors (`java.import.gradle.enabled`, `java.import.maven.enabled`, `java.autobuild.enabled`, `java.import.exclusions`) | off | same |

Servers with no option that stops it are `trusted_only` in the manifest: never
started in `TrustMode::Review`, even with `code_intel.lsp.review` on, and the
answer says why. An untrusted space is served like a review, so neither there.

| Server | What it runs, seen with a marker file (`tests/server_lsp.rs`) |
| --- | --- |
| kotlin-lsp | `settings.gradle.kts`, `build.gradle.kts`: Gradle or Maven import, no documented switch |
| csharp-ls | a `.csproj`'s `<Exec>`: MSBuild evaluates the project and runs design-time targets |
| ruby-lsp | the `Gemfile`: composes a bundle, runs Bundler, loads add-ons from it |
| phpactor | `vendor/composer/autoload_*.php`, even with `composer.enable: false` by `initializationOptions` or `--config-extra` |

intelephense parses PHP in JavaScript and ran no marker: it may serve reviews.

- Each manifest entry lists the initialization options that turn these off;
  review runs always send them.
- A server is never started from a path inside the worktree in a review run:
  a PR could ship its own `node_modules/.bin/…`. In a space, the repo's own
  binaries are used under the same trust rule as its commands.
- Those options narrow what a server runs; they do not make it safe.
  rust-analyzer still runs `cargo metadata`, which honours a PR's
  `.cargo/config.toml` (`rustc-wrapper`) and `rust-toolchain.toml`;
  typescript-language-server loads the worktree's `node_modules/typescript`.
  So servers start in review runs only when the user turns that on:

| `code_intel.lsp` | Review run | Space |
| --- | --- | --- |
| `review: false` (default) | syntax + text only | — |
| `review: true` | server, review options | — |
| `spaces: true` (default) | — | server |
| `spaces: false` | — | syntax + text only |

- A switch turned off stops the sessions it no longer allows; turned on, the
  servers already on disk start, in the worktrees open now. Answers say why a
  server is not used: `needs java 21 or newer, found 17.0.2`, `serves spaces
  and not reviews`.
- Review sessions never share a process with a space's, even for one repo.

## 9. Settings

| Setting | Default | Notes |
| --- | --- | --- |
| `code_intel.enabled` | `true` | off: no downloads, no index, no tools |
| `code_intel.install` | `ask` | `ask` / `always` / `never` (§5.1) |
| `code_intel.languages` | `[]` | empty: detected |
| `code_intel.servers.<language>` | — | preferred server name: tried first, and skipped like any other if its runtime is missing (§4.3). The repo's layer is read at the base (§3.1); a worktree already open keeps its choice until it reopens |
| `code_intel.max_servers` | `4` | alive at once; global file only ¹ |
| `code_intel.idle_minutes` | `10` | idle server shutdown; global file only ¹ |
| `code_intel.max_file_kb` | `1024` | larger files are not indexed; global file only ¹ |
| `code_intel.cache_mb` | `512` | `index.sqlite` size cap, LRU (§5.2); at least 16; global file only ¹ |
| `code_intel.lsp.review` | `false` | language servers in review runs: a PR's code may run (§8); global / local file only, never the repo's; the project's switch writes its local file (§9.1) |
| `code_intel.lsp.spaces` | `true` | language servers in spaces (§8); global / local file only; switch: §9.1 |

¹ Machine-wide: the app has one server pool and one index for every
project, so only `~/.theodore/config.yaml` sets these; a repo's or a local
file's value is ignored, and the config editor warns about it. Read at
startup and applied live (no restart) when the file changes: after a save
in the config editor, or by the next worktree opened after a hand edit.

### 9.1 Project settings → Languages

Workspace switcher → Project settings → Languages: one project's switches and
languages, a section of the project's own settings next to General and the
config editor.

```
Language servers in reviews   [ ]   off by default: a PR's code can run on this Mac
Language servers in spaces    [x]
  switch ─▶ code_intel.lsp.<review|spaces> in the project's local file
            (<folder>/.theodore/config.local.yaml, gitignored; a host project's
             <data dir>/projects/<host>/<owner>/<name>/config.local.yaml)

Rust · 214 files    Grammar Installed   rust-analyzer System  Ready   100% indexed   [Remove]
C++  · 3 files      Grammar Installed   clangd Downloading
                    clangd · 12.3 of 40 MB ▓▓▓▓░░░░░                     40% indexed
```

| Switch's value | Comes from |
| --- | --- |
| the project's local file sets it | that file (what the switch writes) |
| else your global file sets it | `~/.theodore/config.yaml`: the default under every project |
| else | the app-wide switch of earlier versions, if you flipped it, else the default |

- The switch edits the one line (`code_intel.lsp.review: true`) and keeps
  the rest of the file, comments included. A shape a line edit can't
  handle (a flow mapping, a dotted key) is refused with the file untouched:
  set it in the config editor. Never the repo's file (§8).
- A repo that commits its `.theodore/config.local.yaml` makes that file the
  repo's (docs/config.md), whose `lsp` keys are ignored: the switch is
  refused with the file untouched, and the toast says to set
  `code_intel.lsp.<scope>` in your global file (its path).
- Each language: grammar, server (found / downloaded / not available / what
  it needs), run state, index progress; Install, Decline (a missing server) and
  Remove (what Theodore downloaded, §5.1.1; asks first, and shows the
  refusal when a review or a space uses the language).
- Across every worktree of the project open now, not one of them: a language
  shows its most alive server.

  | Run state | Means |
  | --- | --- |
  | Off | both switches are off |
  | Off for reviews | only reviews' worktrees are open, and the reviews switch is off. With no space open, the page reads the project's checkout (a local folder), served like a review: servers start in a space |
  | Off in spaces | only spaces are open, and the spaces switch is off |
  | Warming up → Ready | started; reading this section starts what went idle |
  | Not running | allowed, but `max_servers` is full (or a server crashed once and returns on the next question) |
  | Failed | crashed twice in that worktree; not restarted |
- A server with nothing to fetch says why, and offers no Install:

  | State | Row | Under it |
  | --- | --- | --- |
  | `needs_runtime` | Needs Java 21+ | Found 17.0.2 (when one ran) |
  | `unavailable` | Unavailable | the manifest's `hint` (`gem install ruby-lsp`), else No download for this Mac |
  | `trusted_only` | Spaces only | Runs the project's build, so it does not start for reviews |

- A language with no grammar for this build reads `Text search`, with no index
  percent: nothing is wrong, and nothing can be installed for it.
- Install progress comes from `Ephemeral::CodeIntelProgress` (§5.1): the
  row shows the bar and reads the status again once an item settles. The
  status is polled (every 5 s) only while a server warms up, or a download
  shows no events.

## 10. Rollout

```
S1  syntax layer + toolchain/manifest + grammar CI + review-tools   ✓
    project settings → Languages (was a global Settings page)       ✓
S2  LSP client + pool + trust switches + space-tools edits          ✓
    reindex after each space turn and each rewrite (Undo, update…)  ✓
S3  eval with and without the tools; limits; Grep decision          open
```

The eval decides each step: `theodore eval` for grouping, and review / space
runs with the tools on and off, compared on findings, tokens, turns and time.
A layer that does not improve them is not turned on by default.

The agent-tools harness is `theodore eval-tools` (tasks with the code
tools and without, docs/eval/agent-tools/README.md); the tools alone are
`theodore code-tools-mcp`, a stdio MCP server over one worktree.

## 11. Open questions

- tsgo's LSP maturity: resolved for the basics — a two-file fixture project
  through `theodore-code-intel-lsp` gets correct `definition`, cross-file
  `references` and `documentSymbol` from `tsgo --lsp --stdio`
  (`crates/code-intel-toolchain/tests/tsgo_lsp.rs`, `--ignored`); it is
  listed ahead of typescript-language-server for `typescript` / `tsx`.
  It also serves plain `.js` with no tsconfig or jsconfig
  (`tests/server_lsp.rs`), so it goes first for `javascript` too.
  Still open: diagnostics, rename, hover and large real-world projects
  (this crate's own fixture has no `node_modules`) — the eval (§10)
  decides whether it stays first for those.
- Mirror every server into our assets, or only when upstream breaks?
- `max_servers` and `idle_minutes` defaults: measured on a large repo, not
  guessed.
- JVM languages: resolved for Java and Kotlin. jdtls and kotlin-lsp download
  on consent and answer through the same client (`tests/server_lsp.rs`). In a
  review jdtls reads sources with the build off, so references stop at the
  worktree's own files, and kotlin-lsp does not start at all (§8). Scala
  (Metals) is not in the manifest; ElixirLS neither. Measured only on two-file
  fixtures: a real build's import time and memory are the eval's (§10).
- Grammars added by a repo or a user (not in the manifest): not planned. If
  they ever are, they load as WASM in a sandbox, not as native dylibs.
