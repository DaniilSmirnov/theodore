# Project config

One YAML file, in three layers. Later layers win, key by key; a list
replaces the earlier list instead of extending it.

```
~/.theodore/config.yaml                 global   your defaults, every project
<repo>/.theodore/config.yaml            repo     in the repo, shared with the team
<repo>/.theodore/config.local.yaml      local    personal, gitignored
```

```
global ──▶ repo ──▶ local ──▶ effective config
             │
             └─ commands here need your approval (§5)
```

- **Global** sits in Theodore's data directory (`THEODORE_DATA_DIR`, by
  default `~/.theodore`, `~/.theodore-<env>` in a staging, nightly or dev
  build). Settings → Spaces → Global config shows its path
  and opens it in your editor, creating it from a commented template if
  it is missing.
- **Repo** is committed. For a local folder project it is read from the
  folder's working copy. A host project has no checkout, so the file is read
  as committed: at the space's base for a space, at the tip of the PR's base
  branch for a review — never at the PR's head (§1 "Which config").
- **Local** is yours. Add it to `.gitignore` (Theodore does when it saves
  the file for you):

  ```gitignore
  .theodore/config.local.yaml
  ```

  It is read only from a local folder project's own checkout, never from a
  space's worktree (where the agent could write it). A host project has no
  checkout, so its local file lives in Theodore's data directory:
  `~/.theodore/projects/<host>/<owner>/<name>/config.local.yaml`.

Settings → Spaces → Global config also shows each project's effective config
read-only: every value and the file it came from, unknown keys, and whether
the repo's commands are allowed.

**Legacy path.** `<repo>/.review/config.yaml` is still read as the repo
layer when `.theodore/config.yaml` is absent. It is deprecated: Theodore
warns once per session, and the file is ignored as soon as
`.theodore/config.yaml` exists. Its flat keys (`agents.grouping: codex`)
and a top-level `agents:` map both mean `review.agents`.

**Problems are warnings, not errors.** An unknown key, a file that isn't
valid YAML, or a section with a value of the wrong type is shown once per
session as a toast. The broken file or section is skipped; everything else
applies.

---

## 1. `review:`

```yaml
review:
  agents:
    grouping: claude        # claude | codex | gemini
    chat: codex
    skills: [claude, codex]
    fix: claude
```

`review.agents.*` are the per-task agents of Settings → Agents. A value from
any config file wins over the Settings choice for that project; remove the
line and the Settings choice applies again.

### Review rules

```yaml
review:
  instructions: Prefer small functions. Flag any new dependency.
  context: [docs/architecture.md, "docs/adr/*.md"]
  hide: ["gen/**", "*.min.js"]
  skim: ["**/*.snap", "locales/"]
  checklist:
    "migrations/**": Is there a down migration?
    "api/**": [Is the change versioned?, Are the docs updated?]
    "**/*.sql": Is it safe to run twice?
  auto_skills: [security-review]
  ignore_findings: ["vendor/**", "*.pb.go"]
```

| Key | Type | Does |
| --- | --- | --- |
| `instructions` | text | appended to the system prompt of every agent run on the repo: grouping, chat, skills, fixes, space turns |
| `context` | list of paths or globs | named in that same prompt as "read these first" (not inlined); at most 20 |
| `hide` | list of globs | matching files go to the Hidden group, like generated files; never sent to the agent |
| `skim` | list of globs | every hunk of a matching file is flagged to the grouping agent as a skim candidate |
| `checklist` | glob → item or list of items | after grouping, each group holding a matching file gets the items in its "What to check" list |
| `auto_skills` | list of skill ids | each runs once, through the normal skill run, the first time a PR is opened at a head |
| `ignore_findings` | list of globs | a finding anchored on a matching path is dropped when reported, and counted |

```
config ─┬─ instructions, context ─▶ system prompt of every agent run
        ├─ hide, skim ────────────▶ grouping pre-pass ─▶ Hidden group / skim hints
        ├─ checklist ─────────────▶ after grouping ─▶ "What to check" items
        ├─ auto_skills ───────────▶ PR opened at a new head ─▶ skill runs
        └─ ignore_findings ───────▶ finding reported ─▶ dropped, counted on its run
```

**Globs.** Paths are relative to the repo root.

| Pattern | Matches |
| --- | --- |
| `*.lock` | no `/`: the file name, at any depth (`Cargo.lock`, `ui/yarn.lock`) |
| `gen/**` | a `/` anchors at the root; `**` is any number of directories |
| `locales/` | a trailing `/`: everything under it |
| `src/*.rs` | `*` and `?` stay inside one path segment |
| `**/*.{ts,tsx}` | `{a,b}` alternatives |

Quote a pattern that starts with `*` or contains `{` (`"*.lock"`): YAML
reads it differently otherwise. A `.` inside a `checklist` key is part of
the glob, not a nested key.

- **Which config.** Review rules come from the base, so a PR can't change
  how itself is reviewed: a PR that adds `hide: ["**"]` still has its own
  files grouped, and its `instructions` don't reach its own agents. A
  config change takes effect once it is merged.

  | Review of | Repo layer read at |
  | --- | --- |
  | a host PR | the tip of its base branch in Theodore's cache (fetched on open), else the PR's base commit |
  | a branch ("Review branch") | the tip of the branch it is compared against |
  | a space | the space's base commit |
  | anything in a local folder project | the folder's working copy (your own tree), plus your local file |

  Your global file applies to all of them, and a host repo's local file
  (above) to its reviews. The files are read on every use and re-merged
  only when their text changes, so an edit applies to the next agent run
  without a restart.
- **`context`** is resolved against the PR's checkout (a space's worktree
  for a space). A plain path that doesn't exist, a glob that matches
  nothing and a path leaving the repo are left out (a debug log line says
  so). Duplicates count once; the first 20 files are kept.
- **`hide`, `skim`, `checklist` and the grouping.** A grouping is stored
  with a hash of these three keys. Opening a PR whose stored grouping was
  made under other values regroups it from scratch, and so does Regroup
  (a push after a change doesn't continue the old groups either). Changing
  `instructions`, `context`, `auto_skills` or `ignore_findings` doesn't
  regroup. After a push, new hunks of a hidden file go straight to the
  Hidden group.
- **`checklist`** items come after the agent's own, globs in sorted order,
  each glob's items as listed; an item a group already has isn't added
  again. A hidden file's items go to the Hidden group.
- **`auto_skills`** run with the Settings → Agents skills agents. Once per
  head: reopening the PR at the same head (in this session or after a
  restart) doesn't run them again; a push and a later open at the new head
  does. Not for a merged or closed PR. A sandbox replaying agents
  (docs/sandbox.md) runs them only when its scenario sets
  `"auto_skills": true`. A skill that doesn't start (no agent installed) is
  a toast.
- **`ignore_findings`** matches the finding's anchor path. The AI panel
  shows "N findings hidden by config" under the skill run that reported
  them. Each dropped report counts, so two agents reporting the same
  finding count two.
- **Trust.** These keys run no commands, so a repo's file applies them
  without asking (§5). They can still steer the agents and hide files from
  the grouping, which is why they come from the base: only what is already
  merged decides how a PR is reviewed, and a PR changing `review:` is
  reviewed under the old rules.

---

## 2. `spaces:`

Spaces are hidden behind build mode (`docs/spaces.md` §10). Everything here
applies to a space of the project.

```yaml
spaces:
  copy: [.env, .env.local]
  ports: 2
  env:
    APP_URL: "http://localhost:$THEODORE_PORT"
    COMPOSE_PROJECT_NAME: "$THEODORE_SPACE"
  setup: |
    pnpm install
    docker compose up -d
  run: pnpm dev --port $THEODORE_PORT
  check: pnpm lint && pnpm test
  archive: docker compose -p $THEODORE_SPACE down
  timeout_s: { setup: 600, check: 600, archive: 120 }
```

| Key | Default | Does |
| --- | --- | --- |
| `copy` | `[".env*"]` | paths copied from the project's local folder into the new space's worktree (§3); `copy: []` turns it off |
| `ports` | `2` | free TCP ports reserved for the space's life (at most 16); `ports: 0` turns it off |
| `env` | `{}` | extra environment for the space's commands and agent turns; values expand variables (§4) |
| `setup` | — | runs once after create, before the first agent turn |
| `run` | — | long-running (a dev server): Run / Stop in the space header, `⇧R` |
| `check` | — | runs after each agent turn that changed files; the result is attached to the turn |
| `archive` | — | runs before the space's worktree is removed |
| `timeout_s.setup` | `600` | seconds before `setup` is stopped |
| `timeout_s.check` | `600` | seconds before `check` is stopped |
| `timeout_s.archive` | `120` | seconds before `archive` is stopped |

Every command runs as `sh -c '<command>'` with the space's worktree as its
working directory, in its own process group. Stop, a timeout or Theodore
quitting sends SIGTERM to the whole group, then SIGKILL 5 s later. Output
lines are capped at 8 KB each; the last 2000 lines per space are kept in
memory for the logs drawer, and a check keeps its last 60 lines on the turn.
Commands inherit your environment without Theodore's own `THEODORE_*`
variables, then get the space's variables and `env`.

### A space's life

```
create ─▶ worktree ─▶ copy ─▶ ports ─▶ setup ──ok──▶ turn 1 (the goal)
                                         │ └─fail─▶ Retry · Continue without setup · Fix with agent
                                         └─ from the repo, not allowed ─▶ "Run commands from this repo?"
turn ─▶ checkpoint ─▶ files changed? ─▶ check ─▶ ✓ / ✗ on the turn
                                                  └─ ✗: its output goes into the next prompt
Run ─▶ run … Stop
archive ─▶ stop run ─▶ checkpoint ─▶ archive ─▶ remove worktree ─▶ release ports
```

- **Setup.** The space page shows "Setting up…" with the live log. The goal
  waits; sending a message is refused until setup ends. On failure (non-zero
  exit, timeout) the page offers **Retry**, **Continue without setup** and
  **Fix with agent**. Notes from copying (a missing file, a path outside the
  project) are shown with it.
- **No commands at all.** A project whose `spaces:` section sets none of
  `setup` / `run` / `check` / `archive` shows **Set up with agent** instead:
  a turn asking the space's agent to look at the repo (package manager,
  lockfiles, README, Makefile/justfile, docker compose, existing scripts)
  and write the section. The agent edits `.theodore/config.yaml` with its
  normal tools, in its own worktree, like any other change — so whatever it
  proposes reaches the project only once the space ships, and waits for the
  usual trust prompt from there.
- **Fix with agent.** Setup's `failed` state already lets you type in the
  composer (only `running` / `waiting` block it); this button sends the
  agent a turn with the setup command, how it failed and the tail of its
  output, asking it to fix the script or the config. Setup itself is left
  `failed` — Retry and Continue without setup stay put, and a Retry that
  later succeeds still fires the goal it was waiting on.
- **Check** is part of the turn: the space stays running until it ends, and
  Stop stops it too. A turn that changed nothing runs no check. Passed /
  failed shows as a chip on the turn, expandable to the output.
- **Run** has no timeout. It stops on Stop, on archive, and when Theodore
  quits; after a restart a run that was going is shown as stopped.
- **Archive** failing is a warning; the space is archived anyway. Unarchive
  reserves fresh ports.

## 2b. `code_intel:`

Symbols, references and diagnostics for agents and the diff
(docs/code-intel.md).

```yaml
code_intel:
  enabled: true              # off: no index, no servers, no tools
  install: ask               # ask | always | never: language servers
  languages: [typescript, rust]   # empty: detected
  servers: { python: pyright }    # preferred server per language
  max_servers: 4
  idle_minutes: 10
  max_file_kb: 1024
  lsp:                       # your global / local file only
    spaces: true
    review: false            # true: a PR's code may run during reviews
```

| Key | Default | Does |
| --- | --- | --- |
| `enabled` | `true` | off: no worktree is indexed and no run gets the code tools |
| `install` | `ask` | `ask`: a toast offers each missing server once per session, a No is remembered; `always`: downloaded; `never`: only servers already installed. Grammars are downloaded either way |
| `languages` | `[]` | only these; empty: detected from the repo's files |
| `servers.<language>` | — | the server preferred over the manifest's order |
| `max_servers` | `4` | language servers alive at once |
| `idle_minutes` | `10` | an idle server is shut down after this |
| `max_file_kb` | `1024` | larger files are not indexed |
| `lsp.spaces` | `true` | language servers serve your spaces (an untrusted space only with `lsp.review` too) |
| `lsp.review` | `false` | language servers serve review runs on PRs. **A PR's code may run** (build scripts, macros, plugins), written by whoever opened the PR |

- `lsp.*` is honoured only from your global and local files. A repo's file
  setting it is ignored, with a toast, and the editor warns on that line:
  a repo turning servers on for reviews of its own PRs would run their
  authors' code. Unset, the Settings → Languages toggles apply.

- Read like the rest of the repo layer: at the base, never at a PR's head,
  so a PR can't change what gets downloaded or run.
- Trust (docs/code-intel.md §8): a review's worktree is always served
  read-only (no build scripts, no proc macros). A space's is trusted once
  nothing from the repo layer waits for Allow (§5); "Not now" keeps it
  read-only.

## 3. `copy`

Each entry is a path relative to the project's folder: a file, a directory
(copied whole), or a `*` / `?` glob in its last segment (`.env*`). Absolute
paths, `..`, `.git` and symlinks are skipped with a note. Copying needs the
project's own checkout: a host project has none, so `copy` is skipped there
with a note. Unset by every layer, a space copies `.env*` by default — so a
project with no config at all still gets its local secrets; set `copy: []`
to turn that off.

## 4. Variables

Set for every command and agent turn of a space:

| Variable | Value |
| --- | --- |
| `THEODORE_SPACE` | the space's name as a slug (`fix-login-redirect`) |
| `THEODORE_SPACE_ID` | the space's id |
| `THEODORE_PORT` | the first reserved port (same as `THEODORE_PORT_0`) |
| `THEODORE_PORT_<n>` | the n-th reserved port, from 0 |
| `THEODORE_ROOT` | the project's local folder; unset for a host project |
| `THEODORE_WORKTREE` | the space's worktree (the working directory) |
| `THEODORE_BRANCH` | the branch the space ships as |

In `env` values, `$NAME` and `${NAME}` expand to these variables or to your
environment; an unknown name expands to nothing, `$$` is a literal `$`. A
value can't refer to another `env` key, and `env` can't override a
`THEODORE_*` variable. Commands themselves are expanded by `sh`, so use the
variables there directly.

## 5. Trust

A repo's file is written by whoever can push to the repo, so commands from
the **repo** layer run only after you allow them. Gated: `spaces.setup`,
`spaces.run`, `spaces.check`, `spaces.archive` and every `spaces.env` key,
whenever their effective value comes from the repo layer. Commands set in
your global or local file run without asking.

```
repo commands in effect ─▶ sha256 ─▶ allowed for this project? ─yes─▶ run
                                         │
                                         └─no─▶ space shows the commands:
                                                "Run commands from this repo?"
                                                  Allow   ─▶ hash stored for the project, run
                                                  Not now ─▶ skipped in this space, with a note
```

- The approval is stored per project against a hash of the exact set. Any
  change to a gated value — a new command, an edited one, a new `env` key —
  asks again.
- `review:` is not gated: it runs nothing, and a review reads it from the
  base, so a PR's own `review:` never applies to that PR (§1 "Which
  config"). `spaces:` is read as before (a space's base), and its commands
  still wait for Allow.
- **Not now** applies to that space only: its repo commands (and repo
  `env`) are skipped, everything else works, and the space shows that they
  were skipped with a way to review and allow them later.
- A value your local or global file overrides is not part of the set.

## 6. Global template

What "Open in editor" creates when `~/.theodore/config.yaml` is missing:
every line commented, the layers explained, an example of each section.

## 7. Tools the agent gets

Each agent turn in a space gets the `space-tools` MCP server: the space's
configured commands as tools, plus the tools that act on the change (below).
No tool runs a command the agent writes; there is no shell tool.

| Tool | Does |
| --- | --- |
| `run_check` | runs `check` now; returns `passed`, `exit_code`, `timed_out`, a one-line `summary` and the last 60 lines of `output` |
| `start_app` | starts `run` unless it is running; returns the status |
| `stop_app` | stops `run`, waits up to 10 s for it to exit; returns the status |
| `app_status` | `running`, `ports`, `urls` (`http://localhost:<port>` each), whether `run` is set and allowed, how the last run ended |
| `read_logs` | the last `tail` lines (default 100, at most 400) of one `kind`: `setup`, `run`, `check` or `archive`; stderr lines start with `! ` |
| `space_info` | name, branch, base, worktree, ports and URLs, the **names** of the variables a command gets (§4, `env`) — never their values — and which commands are set and allowed |

```
agent ─▶ run_check / start_app ─▶ set? ─no──▶ error: no `spaces.check` command is configured
                                    │
                                    └─yes─▶ allowed (§5)? ─no──▶ error: not allowed by the user yet
                                                │                (nothing runs)
                                                └─yes─▶ the command, as the space runs it
```

- **Trust holds.** A repo command you haven't allowed — or said Not now to —
  is refused with "not allowed by the user yet", and never runs. The agent
  is told to ask you.
- **Same commands, same state.** A tool's `check` or `run` is the space's:
  same worktree, variables and timeouts, same logs drawer, same Run / Stop
  in the header. The turn's Stop also stops a `run_check` in flight. An app
  the agent started keeps running after the turn, until Stop, archive or
  quit.
- **Scoped to the space.** Each turn gets its own token, revoked when the
  agent exits; it reaches only that space. The agent never names a space.
- **Told in the prompt.** The turn's system prompt lists the tools: use
  `run_check` after changes, `start_app` to try the app, `ship` when asked.
- **A space's PR.** Chat, skills and fixes on a space's review (or the PR
  it shipped as) keep `review-tools` and also get the read-only `read_logs`
  and `app_status`. Any other PR is unchanged.
- **In the turn.** The turn lists its tool calls; these read as what they
  do ("run the check", "start the app").
- **Ask mode too.** A turn in Ask mode may call `run_check` and
  `start_app` (owner decision, 2026-10-23: "if it needs to run, it can
  run"). They run the configured commands only, under the same trust.

The change itself (docs/spaces.md §5):

| Tool | Takes | Returns |
| --- | --- | --- |
| `ship` | `action`: `open_pr · open_draft_pr · push · merge_locally · keep_branch`; `title?` | `branch`, `sha`, `pr_number`, `pr_url`, a one-line `summary` |
| `get_space_diff` | `file?`, `stat?` | `base`, `base_sha` (merge-base), `head`, `uncommitted`, `files` (status, +/−), `diff` (unified, at most 120 000 chars; `truncated`) |
| `get_review_comments` | `file?` | `items`: `kind` (`draft · finding · thread`), `on` (`space · pr`), `location`, `state` (`draft · open · resolved · outdated`), `severity`, `title`, `comments` (`author`: `you`, an agent or a login) |
| `add_review_comment` | `file`, `line` (new side, 1-based), `body`, `severity?` (`info` default · `warn` · `high`) | `finding_id` |
| `set_base` | `branch` | `base`, `base_sha`, `previous`, `note` (an open PR is not retargeted) |
| `update_progress` | `steps` (`label`, `state`: `pending · active · done`; at most 20), `note?` | `ok` |

```
ship ─▶ same turn? ─no──▶ error: the agent is still working
          └─yes─▶ ship_space: checkpoint ─▶ squash (you) ─▶ push / merge / branch
                     └─ refused (local folder vs host, nothing new, branch taken,
                        conflict, no identity) ─▶ the reason, nothing changed
```

- **Ship on request only.** The prompt and the tool say: call `ship` when
  the user asks to push, open a PR or merge; never `git push` or `gh pr
  create`. Open PR waits up to 15 s for the host and returns the PR; after
  that it says the PR is on its way (the outbox finishes it).
- **Ignored and drafted findings** are left out of `get_review_comments`;
  the agent's own comments are findings of skill `space-agent`.
- **`set_base`** refuses an unknown branch, the space's own branch, an
  archived space, or a space another run holds.

**Code tools.** With code intelligence on for the project (§2b), both
`review-tools` and `space-tools` also list `find_symbol`, `get_symbol`,
`references`, `definition`, `diagnostics` and `code_status`; a space turn
in Edit or Run mode also gets `replace_symbol_body`, `insert_after_symbol`
and `rename_symbol` (refused in Ask mode). Off, they aren't listed.

Your own MCP servers come along unless you turn them off: with Settings →
Agents → **Allow my Claude MCP servers in spaces** on (the default),
Claude also loads your Claude Code servers (Linear, GitHub…, claude.ai
connectors); off, a turn gets `space-tools` only (`--strict-mcp-config`).

Claude and Gemini get the server over HTTP, Codex through its
`mcp_servers.space-tools` override. A CLI that only speaks stdio MCP uses
`theodore mcp-bridge`, same as `review-tools`.

## 8. Creating and saving the config from Theodore

Settings → Projects → ⟨project⟩ → **Config**.

```
Create from current settings ─▶ the repo's file, else your local file, else generated:
                                  review.agents per task (Settings → Agents)
                                  + values already in the repo / local file
                                  + the rest of review: and spaces: as comments
edit ─▶ checked as you type ─▶ problems with line numbers · effective config preview
Save to… ─┬─ Repo (team)
          ├─ Repo (team) and commit      local folder, nothing else uncommitted
          ├─ Local (mine)
          └─ Global
```

- **Load** switches the editor to another layer's file, or back to the text
  generated from your settings. The preview shows the project's effective
  config with the editor's text in place of that layer (generated text is
  previewed as the repo file), each value with the file it comes from —
  the same view as Settings → Spaces.
- **Problems.** The text is checked by the same parser that reads the file.
  Invalid YAML, a top level that isn't a mapping and a value of the wrong
  type (`ports: lots`) are errors, at their line; an unknown key is a
  warning. Save is off while there is an error.

| Save to | Local folder project | Host project |
| --- | --- | --- |
| Repo (team) | `<folder>/.theodore/config.yaml` | a commit on `<login>/theodore-config` (`theodore/config` without a login), pushed, and a PR "Add Theodore config" into the default branch |
| Repo (team) and commit | the file, then `git commit` of that file only | — |
| Local (mine) | `<folder>/.theodore/config.local.yaml`, added to the folder's `.gitignore` if no line names it | `~/.theodore/projects/<host>/<owner>/<name>/config.local.yaml` |
| Global | `~/.theodore/config.yaml` | same |

`~/.theodore` is Theodore's data directory (`THEODORE_DATA_DIR`;
`~/.theodore-<env>` in other builds). A file is
written through a temporary file and a rename.

**A host repo, through a PR.**

```
default branch tip ─┐
                    ├─▶ one commit: the tip's tree + .theodore/config.yaml ─▶ push (never forced)
branch exists? ─────┘   (on top of the branch's tip; authored by you)          │
                                                                                ▼
                                             outbox create_config_pr: open PR for the branch?
                                               yes ─▶ that PR   no ─▶ open "Add Theodore config"
                                                                ▼
                                                    toast "Config PR #n is open" · Open
```

- The commit is built from git trees in Theodore's own copy of the repo: no
  checkout, never your working copy. You are its author and committer (git's
  `user.name` / `user.email`, else your GitHub login's noreply address),
  unsigned.
- Saving again pushes one more commit to the same branch, and the PR is
  found by its branch instead of opened twice. Saving the same text again
  pushes nothing. The PR only changes how other PRs are reviewed once it is
  merged (§1 "Which config").

**A local folder, and commit.** Offered only when the folder is on a branch
and has no uncommitted change besides `.theodore/config.yaml` itself. The
commit (`Add Theodore config`, or `Update …`) takes that path only, with your
identity and your hooks; a hook that fails leaves the file saved but not
committed. Asked when the folder became dirty meanwhile, the save is
refused before anything is written.

**Trust.** Repo commands you save to the repo are yours: the save marks
that set allowed for the project (§5), and the set allowed before stays
allowed too, so a host repo's commands keep running while the PR waits to
be merged. A local or global save never allows the repo's commands — it
only keeps an allowed set allowed when it leaves nothing new in it. A later
change to the repo file by anyone else asks again, as always.
