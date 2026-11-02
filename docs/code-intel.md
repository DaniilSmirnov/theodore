# Code intelligence — symbols, references, diagnostics

Agents and the review UI understand code by structure, not only by text.
Instead of grepping a name and reading whole files, an agent asks "where is
`save` defined, who calls it, does this still type-check" and gets
`file:line` answers with a few lines of context.

Status: **built**. Grammars download from the `grammars-<tag>` release
(`.github/workflows/grammars.yml`); language servers are found on the
machine or downloaded on consent. Open: the eval (§10).

| Hook | Where |
| --- | --- |
| PR opened, review run starts | `open_pr.rs` `finish_open_full`, `space_tools.rs` `pr_tools`: `TrustMode::Review` |
| space created / unarchived / turn | `spaces.rs` `create_space`, `unarchive_space`, `space_send`: trusted per §8 |
| space turn ends (files the agent wrote) | `spaces.rs` turn end → `code_intel.rs` `spawn_space_code_changed`: the checkpoint's diff → `file_changed` |
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
│   └─ servers/<name>/<ver>/                                                 │
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
    - name: pyright                # only if the user has it; needs Node
      detect: [["pyright-langserver", "--version"]]
      launch: ["pyright-langserver", "--stdio"]
```

- The manifest is compiled into the binary. Its grammar entries are generated
  by CI (§4.1); server entries are pinned by hand. A new version of the
  manifest ships with a Theodore release; there is no "latest".
- The app carries no grammar and no server of its own. Everything is
  downloaded for the languages a project actually uses.

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
| `tree-sitter-php` | `php` + `php_only` |
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
- The tags query (`tags.scm`, defs and refs) ships next to its dylib.
- Loaded with `libloading`. Before use, the grammar's ABI version
  (`ts_language_version`) is checked against the range the linked tree-sitter
  runtime supports; out of range → not loaded, the language has no syntax
  layer, nothing crashes.
- Signing: the app is signed ad-hoc without hardened runtime today
  (`docs/plan.md` §11.2), so library validation does not apply. With a
  Developer ID and hardened runtime, CI signs the dylibs with the same Team ID
  and they keep loading.
- No language packs (several languages in one file): they download languages
  nobody asked for and save little.

### 4.2 Servers: upstream binaries, checked by hash

```
manifest entry ─▶ download ─▶ sha256 (required) ─▶ unpack ─▶ servers/<name>/<ver>/
                                                  └─ unsigned? codesign -s -
```

| Question | Answer |
| --- | --- |
| Source | the server's own release, at a pinned version |
| Checksum | sha256 in the manifest, always. No hash → the entry is invalid |
| Hosts | each entry names its host; a redirect elsewhere fails the download |
| Formats | `tar.gz`, `zip`, `gz`, `binary`, npm `tgz`, wheel (a zip). Archive paths are checked: nothing escapes the target directory |
| No macOS binary upstream | built in our CI and published as our asset (gopls: Go, cross-compiles, BSD) |
| Upstream deletes a release | mirror into our assets (MIT / Apache / BSD allow it); policy open (§11) |
| Signing | a server runs as a child process, so our hardened runtime does not apply to it. An unsigned arm64 binary does not start, so it gets an ad-hoc signature after download |
| Updates | a new manifest version in a Theodore release. The old version stays until the new one has started once, then it is removed |

### 4.3 Server availability

| Downloaded by Theodore (no runtime needed) | Used only if already installed |
| --- | --- |
| rust-analyzer, clangd, ty / pyrefly, tsgo, gopls (our build), zls, lua-language-server, taplo, marksman, terraform-ls | sourcekit-lsp (Xcode), typescript-language-server / pyright (Node), JDT LS (JVM), kotlin-lsp, Roslyn (.NET), ruby-lsp, ElixirLS, Metals |

Detection looks, in order: the manifest's `detect` commands on `PATH`,
`rustup which`, `xcrun --find`, the repo's `node_modules/.bin`. A server found
on the machine wins over a download.

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
as processes, so they are.

- Downloads start when a repo is added, in the background, so the first PR
  does not wait.
- Offline, or a check fails → that language has no code intelligence; review
  works as it does today.

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
- Skipped: `.gitignore`d, `linguist-generated`, vendored, files over
  `code_intel.max_file_kb`.
- Eviction: LRU per repo when the cache exceeds its size limit.

### 5.3 Spaces: follow the edits

```
agent edits ─▶ file watcher (notify, debounced) ─▶ new blob ─▶ reindex file
                                                 └─▶ didChange to the LSP server
                                                     ─▶ diagnostics
```

## 6. Language servers at run time

```
query ─▶ pool[(repo, language)] ── alive ─▶ request
                                 └─ none ──▶ spawn (stdio) ─▶ initialize
                                             ─▶ status: warming ─▶ ready
idle code_intel.idle_minutes ─▶ shutdown ─▶ exit ─▶ kill after 5 s
```

- One client (`lsp-types` + `async-lsp`), every server behind it. Theodore's
  own code is the pool and the lifecycle.
- One server per (repo, language) where the server supports
  `workspaceFolders`; worktrees join it as folders. Otherwise one per
  worktree.
- At most `code_intel.max_servers` alive; the least recently used goes first.
- A server sees one tree. Base files are served by the syntax layer; a second
  server on the base is started only when a question needs it.
- A server that crashes is restarted once per session; a second crash marks
  it failed for that worktree, and the syntax layer answers.
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

The UI uses the same port: go-to-definition and hover in the diff. Grouping
can use references to link "changed here" with "called there" without an LLM
call.

### 7.2 Spaces (`space-tools`)

Everything above, plus edits in modes that allow them (`AgentMode::Edit`,
`AgentMode::Run`; never in Ask):

| Tool | Replaces |
| --- | --- |
| `replace_symbol_body(path, name, body)` | Edit with hand-matched old text |
| `insert_after_symbol(path, name, text)` | the same |
| `rename_symbol(path, line, col, new)` | grep + N edits |
| `diagnostics(path?)` | a full `run_check` after every edit; `run_check` stays for the end |

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
  servers already on disk start. Answers say why a server is not used.
- Review sessions never share a process with a space's, even for one repo.

## 9. Settings

| Setting | Default | Notes |
| --- | --- | --- |
| `code_intel.enabled` | `true` | off: no downloads, no index, no tools |
| `code_intel.install` | `ask` | `ask` / `always` / `never` (§5.1) |
| `code_intel.languages` | `[]` | empty: detected |
| `code_intel.servers.<language>` | — | preferred server name |
| `code_intel.max_servers` | `4` | alive at once |
| `code_intel.idle_minutes` | `10` | idle server shutdown |
| `code_intel.max_file_kb` | `1024` | larger files are not indexed |
| `code_intel.lsp.review` | `false` | language servers in review runs: a PR's code may run (§8); global / local file only, never the repo's |
| `code_intel.lsp.spaces` | `true` | language servers in spaces (§8); global / local file only |

Settings → Languages lists each detected language with its grammar, server
(found / downloaded / not available), index progress, and Install / Remove.

## 10. Rollout

```
S1  syntax layer + toolchain/manifest + grammar CI + review-tools   ✓
    Settings → Languages                                            ✓
S2  LSP client + pool + trust switches + space-tools edits          ✓
    reindex after each space turn (the checkpoint's diff)           ✓
S3  eval with and without the tools; limits; Grep decision          open
```

The eval decides each step: `theodore eval` for grouping, and review / space
runs with the tools on and off, compared on findings, tokens, turns and time.
A layer that does not improve them is not turned on by default.

## 11. Open questions

- tsgo's LSP maturity: default for TypeScript, or behind
  typescript-language-server until it is?
- Mirror every server into our assets, or only when upstream breaks?
- `max_servers` and `idle_minutes` defaults: measured on a large repo, not
  guessed.
- JVM languages (Java, Kotlin, Scala): supported only when installed, or not
  at all — they need a build and start slowly.
- Grammars added by a repo or a user (not in the manifest): not planned. If
  they ever are, they load as WASM in a sandbox, not as native dylibs.
