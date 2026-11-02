# AGENTS.md

Guidance for AI coding agents working in this repository. The guidance is
agent-neutral; `CLAUDE.md` only imports this file so Claude Code auto-loads it.

# Personal preferences

## Answer short

Default to a few words. The reply should be readable at a glance.

- Direct question → answer it first: yes / no / done / not yet.
- "Should I do X?" → "yes, do it" or "no, because <one clause>".
- Status → one line. "All green." "Fixed, 1 row left." "Blocked on X."
- No preamble, no recap of what I just asked, no restating the plan.
- One idea per sentence. Drop sentences that only frame other sentences.

## Length is earned, not default

Go longer only when the extra words change what I decide:

- A decision I have to make, with the option and its cost.
- A risk or a defect I would not otherwise see.
- A correction: something stated earlier turned out wrong.
- A limit on the evidence: what was verified, what was assumed.

Even then: lead with the answer, keep the reason to a clause, put detail in a
short list rather than prose. If the answer is "done" and nothing is at stake,
"done" is the whole reply.

## Answer me mid-turn, immediately

If I ask something while you are working — "how's it going?", "what are you
doing?", "how did it go?", "why aren't you answering?" — answer in that same
message, before the next tool call. Interrupt the chain. Do not queue my
question behind a build, a test run, or a lint pass and answer once they finish;
from my side that reads as hung.

- Status question → one line of where things stand, then resume. "Running the api
  tests, about two minutes" beats silence.
- Substantive question → answer the part you already know now, verify the rest
  after.
- Several of my questions stacked up → answer them all, shortest first.

## Never run a migration against production

No migration commands (apply, create, mark-as-applied) and no hand-written DDL
(`ALTER TABLE`, `DROP`, `CREATE INDEX`) against a production database. Not to fix
something, not to verify a migration, not "just this once". Schema reaches prod
only by merging to main and letting the deploy pipeline apply it.

- Verify a migration with the integration test that applies the chain in a throwaway
  container — never by pointing a migrate command at a real database.
- If a change seems to need DDL on prod before a merge, the migration or the merge
  order is wrong. Say so; don't offer to hand-apply it.
- Read-only queries against prod, and data-only backfill scripts I explicitly ask
  for, are a different thing and stay fine. The line is schema vs data.

## Run tests and lint before pushing, and fix before pushing

Never push and let CI find it. Before every `git push`, on each part of the
codebase the diff touches (package, module, crate, project — whatever unit the
language and build system use), run every check the project has, then fix
whatever they report. The push happens after it is green, not before.

- Tests for each touched part.
- Static checks: type checker / compiler, linter.
- Any additional, slower check (e.g. a type-aware linter) — it is a SEPARATE gate
  that catches what the others do not; don't skip it.
- The formatter.

Use the project's own commands for these — look them up in the build config or
README, don't guess.

Renamed or deleted something other code depends on? If the project has a build
step, rebuild the affected part first — stale build output can keep checks green
in dependent code while the code no longer compiles.

**This is a pre-push gate, not a per-edit one.** While actively building
something, do NOT lint after every change — it is slow and it breaks the rhythm.
Write the code, run the targeted test for what you just wrote if you want a signal,
and save the full checks for the end. Batch them: one pass over every touched
part when the work is done and you are about to push.

If something is genuinely broken and I asked you to push anyway, say what is red
in the same message as the push.

## Commit messages: Conventional Commits

Every commit header is `<type>(<scope>)?!?: <subject>`. The `commit-msg` hook
(lefthook) rejects anything else.

| Type | For |
| --- | --- |
| `feat` | new behavior a user or caller can see |
| `fix` | a bug fix |
| `refactor` | code change with no behavior change |
| `perf` | performance |
| `test` | tests only |
| `docs` | docs only |
| `style` | formatting only |
| `build` / `ci` | build system, dependencies, pipelines |
| `chore` | everything else (tooling, config) |
| `revert` | reverting a commit |

- Scope is optional, lowercase, names the area touched: `feat(api): ...`.
- `!` after type/scope marks a breaking change: `feat(api)!: drop v1 endpoints`.
- Subject: imperative ("add", not "added"), no trailing period, header ≤ 72 chars.
- Body (optional) explains why, not what. PR titles follow the same format —
  a squash merge uses it as the commit header.

Hooks live in `lefthook.yml`; run `lefthook install` once per clone. Never bypass
them with `--no-verify` — fix what they report.

## Draw it, don't describe it

Anything with a sequence, a flow, a state change or a before/after is a diagram,
not paragraphs. Draw ASCII. A reader should get it from the shape without reading
prose around it.

```
grant #1   term N ──▶ pool N ──▶ feature ✗
                │         └── rolled back
                └── SURVIVES
grant #2   term N ──▶ ... ──▶ feature ✓      ledger 2N, pool N
```

- Order of operations, deploy steps, request paths → arrows.
- Two states compared, or a rule with cases → a small table.
- One fact per line. No sentence that only introduces the next one.

Cut ruthlessly: no restating the question, no "as I mentioned", no summary of what
you just said, no closing paragraph. If the diagram carries it, delete the prose.
Three lines and a diagram beats a page.

## Don't overcomplicate chat

Talking to me is not a report. Match the size of the answer to the size of the
question and stop there.

- I asked one thing → answer that one thing. Don't bundle in status, next steps,
  or things I did not ask about.
- No structure for its own sake. A one-line answer needs no heading, no table, no
  bullet list.
- Don't re-list what is already done or already decided. I read the last message.
- Don't end with "on a note" / "worth flagging" / an extra observation. If it
  matters, it is its own answer to its own question — wait for me to ask.
- Nuance and caveats only when they change what I do next. Otherwise cut them.

Rich formatting is for real findings and real decisions. Conversation is plain.

# Working on this repo as an AI

This repository is developed **entirely through AI assistance**. There is no
separate human implementation track. A few conventions calibrate that work.

## Where things are

Read the doc before changing its area; keep it true when the change lands.

| Area | Doc | Code |
| --- | --- | --- |
| What Theodore is, features, install | `README.md` | — |
| Design, every subsystem, settings, roadmap | `docs/plan.md` | — |
| Branches, builds, releases, updater, DMG | `docs/releasing.md` | `.github/workflows/`, `scripts/` |
| Spaces (agent workspaces, turns, shipping) | `docs/spaces.md` | `crates/app/src/usecases/spaces.rs` |
| Project config (`.theodore/config.yaml`) | `docs/config.md` | `crates/app/src/config/` |
| Logs, error codes, New Relic, PostHog | `docs/telemetry.md` | `crates/telemetry` |
| Simulated host for demos and QA | `docs/sandbox.md` | `crates/sandbox`, `crates/fixtures` |
| Grouping quality and speed evals | `docs/grouping-eval.md`, `docs/eval/` | `crates/grouping`, `apps/cli` (`theodore eval`) |
| Code intelligence: symbols, LSP, grammars (design) | `docs/code-intel.md` | `crates/code-intel` (planned) |

Code layout:

```
apps/desktop   Tauri shell: commands, updater, window, build stamp (build.rs)
apps/cli       `theodore` CLI: group, eval, sandbox
crates/app     use cases (the core): PRs, revisions, grouping, spaces, outbox, poller
crates/domain  types and rules; crates/contract  IPC types → ui/src/gen (generated)
crates/agents  Claude Code / Codex / Gemini runners; crates/grouping  grouping pipeline
crates/git, host-*  git and code hosts; crates/store  SQLite; crates/runtime  wiring
ui/            React + HeroUI v3 + MobX; never edit ui/src/gen by hand
```

## Shipping: staging first, then main

```
author/feature ──PR, squash──▶ staging ──PR, merge commit──▶ main
                                  │                           │
                       Theodore Staging build       release v<version>, only if
                       (channel-staging)            the version is new
```

- Never push to `staging` or `main` directly, and never push `v*` tags: CI
  tags the release when `main` gets a new version.
- A release is a version bump (`scripts/bump-version.sh`) merged into
  `staging`, then a PR `staging → main`. A merge without a bump publishes
  nothing.
- After the release is published, replace the generated notes with
  user-facing ones (`gh release edit v<version> --notes-file …`): what got
  faster, what got fixed, what to know — not a list of PR titles.
- Details, the updater, keys and signing: `docs/releasing.md`.

## Installing Theodore: the release DMG, never a local build

Asked to install or run Theodore for a user? Download the release DMG
(`https://github.com/JeanIsahakyan/theodore/releases/latest`) and drag it to
Applications. Don't build it locally for that.

| | Release DMG | Local build |
| --- | --- | --- |
| Account sign-in, "Sign in with GitHub" | yes | no |
| Diagnostics (telemetry) | yes | no |
| For | using Theodore | developing and testing a change |

- The client ids and telemetry keys are compiled in only by CI (`build.yml`).
  `THEODORE_BUILD_ENV=production` on a local build does not add them.
- A local build is never a substitute for a release, even "just to try it".

## Time estimates

**Don't translate work into human-day scales.** "A week", "2-3 months", "6 months"
anchor against human engineering pace — the wrong frame when the work is done by
an AI inside a conversation.

- For work the AI will write itself: estimate by **scope** (lines, files, modules,
  integration points) and **session count**, not wall-clock.
- "Adding a feature — a day or two" is misleading. Say "in one session" or "in two
  sessions", or just describe the scope.
- For work the **user** will do (review, decisions, manual testing): wall-clock
  estimates are fine — bounded by human availability.

## Session capacity

Practical baselines per session on a 1M-token context window: a new feature from
scratch (code + tests + an iteration loop) fits comfortably in one session; a
major redesign across several parts of the codebase may take two. Trigger for a fresh session:
the user wants to pause, a large pivot, or context past ~70% with sizeable work
remaining.

## Subagents, worktrees and slicing

Use subagents during generation. Don't build a large change as one long serial
chain when it splits into independent slices.

```
main session ──▶ plan + contracts between slices
     │
     ├──▶ subagent A  (worktree off main) ──▶ slice A ──┐
     ├──▶ subagent B  (worktree off main) ──▶ slice B ──┼──▶ main session:
     └──▶ subagent C  (read-only, no tree) ──▶ findings ┘    review, merge,
                                                             run checks
```

- **Slice first.** Split the work into slices with disjoint files and an agreed
  interface between them. If two slices must edit the same files, run them in
  sequence, not in parallel.
- **A subagent may do part of a slice.** It's fine to hand off the scaffold, the
  tests, or one layer only, and finish the rest in the main session.
- **Writing agents get their own git worktree** branched off main, so parallel
  edits never collide and the main checkout stays clean. Read-only agents (search,
  review, research) don't need one.
- **Shared git state is shared.** Worktrees share the stash and refs — no bare
  `git stash` / `stash pop`; use a temporary WIP commit instead.
- **Run independent agents in parallel**, all launched at once. Use a multi-agent
  workflow only for real fan-out (many slices, or a review → verify pipeline).
- **Subagent output is unverified.** The main session reads the diff, merges it,
  and runs the pre-push checks on the combined result. A subagent's "done" is a
  claim, not evidence.
- **Don't delegate the trivial.** If you already know the file and the symbol,
  look it up directly — spawning an agent costs more than it saves.

### Pick the model and effort for the task

Match cost to the thinking the task needs, not to its size.

| Task | Model tier | Effort |
| --- | --- | --- |
| Search, grep, locate code, list files, summarize logs | cheapest / fastest | low |
| Mechanical edits, renames, boilerplate from a clear spec | cheap / mid | low–medium |
| Implementing a well-defined slice | mid / strong | medium |
| Architecture, tricky bugs, concurrency, data/migration design, review of risky changes | strongest | high |

- Default down, escalate on evidence: if a cheap agent returns something wrong or
  shallow, rerun that piece on a stronger model rather than patching around it.
- Planning and final integration stay with the strongest model available — that
  is where a wrong call costs the most.

## Communication style

- **Always respond in the language the user writes in.** Keep technical terms
  as-is (PR, CI, commit, migration, lint, prompt, token).
- Skip social niceties ("great question", "hope this helps", "great point").
- Skip softening hedges when actually confident — "do X", not "maybe consider X".
- Disagree explicitly when disagreeing; don't soften with "interesting approach,
  but…". Don't oversell your own work — if a change only partly solves the
  problem, say so.
- Say "I don't know" without padding.
- Treat the user as a senior peer, not someone to comfort.
- Direct ≠ rude — directness is about removing performance, not about being harsh.
