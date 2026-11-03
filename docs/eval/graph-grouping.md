# Grouping by relatedness

**Verdict: keep it.** Validity is unchanged (every run valid on the first try,
no repairs, no "Other"). The gain is on large PRs: related files in different
directories now share a prompt and a group. Config, env, CI and docs edits
land inside the change they support instead of in a "config" group of their
own. Two copies of the same generated runtime are reviewed once, not twice.
On single-prompt PRs the difference is within run-to-run noise. Agent calls
are the same in total (10 on the large set): one more on kdb-83, one fewer on
migratro-77.

Dates: 2026-10-21. Agent: Claude CLI default model. Total agent spend for
this evaluation: about $14.

## What changed

```
before   too-big PR ─▶ split by first path segment ─▶ chunks ─▶ agent
         tiny group ─▶ merge into nearest group in the same top-level dir

after    diff (+ head files, + git history when a worktree exists)
           └─▶ relatedness graph over changed files
                 ├─ drop hub files ─▶ Louvain communities ─▶ hubs rejoin
                 │
         too-big PR ─▶ units = communities (2+ files)
                     │         + unconnected files by top-level dir
                     ├─ unit over budget ─▶ split it by top-level dir
                     └─ largest first, into the fitting chunk it is most
                        related to ─▶ chunks ─▶ agent
         every prompt ─▶ "# Related changes": rollouts, then strongest edges
         tiny group ─▶ merge into the most related same-kind group,
                       else nearest same-area group (as before)
```

`ALGO_VERSION` is 3, so stored groupings regroup on open.

### Signals

| Signal | From | Weight per file pair |
| --- | --- | --- |
| import / require / use / include / `mod` | hunk lines, head file when available | `1 / sqrt(in-degree of target)` |
| a changed line names another changed file | changed lines (`CMakeLists` → source, docs → code) | `0.8 / sqrt(times named)` |
| a name defined on a changed line, used on another file's changed line | changed lines | `1 / sqrt(users)`; plain words only with ≤ 3 users |
| the same new compound name adopted by 3+ files | changed lines, never on a deleted or unchanged line | `0.3` per pair, capped at 1 |
| a rare compound identifier (`granted_by_default`) in 2–6 files | changed lines | `0.5 / (files − 1)` |
| identical edit shape (names, strings, numbers masked) | non-skim hunks | `0.6` per pair, capped at 1.5; files whose whole change is that edit are kept in one community |
| name root after stripping role suffixes; folder name for index files | paths | `0.8 / (files − 1)`, groups of 2–8 |
| test ↔ source with the same root | paths | `+0.5` |
| co-change: Jaccard over the last 1000 commits before the base, ≥ 2 shared, commits touching > 12 files ignored | git (app only) | `1.5 × Jaccard` |
| symbol link: a hunk defines `X`, another changed hunk uses it (code intelligence, [symbol-links.md](symbol-links.md)) | code intelligence (app; eval with `--symbol-links`) | `1` per distinct symbol, capped at 2; also hunk-level, in prompts and coarsening |

Hubs are files linked by import, file-name mention or shared symbol to more than
`max(10, 2·√n)` others. Examples from the fixtures: `lib/al.lib.php` defines
`al_dispatch`, which 33 entry files call. `BaseController.php` gains the
`getPdo()` that every controller now uses. A changelog names half the PR.
Hubs sit out the Louvain run, then join the community they are most connected
to. Louvain visits nodes in index order and breaks ties toward the lowest id,
so the same diff always gives the same communities.

Code embeddings were not used: they cluster by code shape, not by feature.

### Prompt hint

Each prompt lists, in at most 2 400 chars, the rollouts first and then the
strongest pairwise links among its own files. Excerpt from vkclone-3's two
chunks:

```
# Related changes
- 34 files use the new `al_dispatch`: al_audio.php, al_fans.php, al_fave.php, … (+28 more)
- 26 files use the new `getPdo`: lib/classes/Controllers/AudioController.php, … (+20 more)
- lib/classes/Controllers/LoginController.php → lib/classes/Processors/SessionProcessor.php (`SessionProcessor`, imports)
- lib/classes/Controllers/PhotosController.php → lib/classes/Processors/PhotoProcessor.php (`adjustAlbumPhotoCount`, imports)
```

It ends with "Links are between files, not hunks: a purely mechanical hunk in
a linked file can still go to its own skim group." That sentence was added
after v3 (see below).

### Repository context in the app

`Core::grouping_context` reads the head contents of changed text files from
the worktree (≤ 512 KB each, ≤ 24 MB total, no binary/generated/LFS files, no
`..` paths). It also calls `GitPort::co_change_commits(worktree, base, paths,
1000, 12)`, which runs `git log --no-merges --name-only` from the PR base.
The `Grouper` gets both as a `RepoContext`. The eval fixtures carry only
diffs, so **none of the agent runs below used head files or co-change**. Those
two signals are unit-tested only.

## Runs

| label | code |
| --- | --- |
| claude-default, claude-default-2 | before (directory chunking), two runs to show run-to-run variance |
| graph-v1 | graph, chunks packed first-fit |
| graph-v2 | + file-name mention edges, chunks packed by affinity |
| graph-v3 | + stricter "new name" rule (compound, never seen before) |
| graph-v4 | + hint sentence about mechanical hunks (final); big set only |

Cost is not comparable across runs. The CLI's prompt cache made repeated
prompts cheap (vkclone-3 cost $0.22 in v2 and v3 against $0.67 cold). Agent
calls are the honest cost measure.

### Large PRs (3: migratro-77, kdb-83, vkclone-3)

| run | valid 1st try | agent calls | Other hunks | groups | in target | groups ≤2 hunks | docs/config-only groups | files split over groups | wall (s) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| claude-default | 3/3 | 10 | 0 | 37 | 3/3 | 0 | 3 | 17 | 243 |
| claude-default-2 | 3/3 | 10 | 0 | 33 | 3/3 | 0 | 2 | 17 | 270 |
| graph-v1 | 3/3 | 10 | 0 | 37 | 3/3 | 2 | 0 | 9 | 214 |
| graph-v2 | 3/3 | 10 | 0 | 38 | 2/3 | 3 | 1 | 11 | 209 |
| graph-v3 | 3/3 | 10 | 0 | 38 | 3/3 | 4 | 2 | 9 | 217 |
| **graph-v4** | 3/3 | 10 | 0 | 40 | 3/3 | 2 | 0 | 21 | 203 |

Agent calls per PR: kdb-83 3 → 4 (three chunks instead of one oversized
`Blank/` chunk plus the rest); migratro-77 4 → 3; vkclone-3 3 → 3. "in
target" counts visible groups within the size-scaled target range (8–15
here). v2 had 16 visible groups on kdb-83. "files split over groups" jumps
in v4 because vkclone-3 got its formatting skim group back (0 → 10 split
files, the same 10 as both baselines).

### Single-prompt PRs (real set, 10 OSS PRs)

No chunking happens here, so only the hint and coarsening can matter.

| run | valid 1st try | agent calls | Other hunks | groups | in target | groups ≤2 hunks | docs/config-only groups | wall (s) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| claude-default | 10/10 | 10 | 0 | 48 | 10/10 | 13 | 8 | 181 |
| graph-v1 | 10/10 | 10 | 0 | 46 | 10/10 | 12 | 9 | 172 |
| graph-v2 | 10/10 | 10 | 0 | 47 | 10/10 | 11 | 10 | 166 |
| graph-v3 | 10/10 | 10 | 0 | 45 | 10/10 | 10 | 8 | 179 |

The group count differs by 0–1 per PR, which is noise. v4 was not run on this
set; its only change from v3 is one sentence in the hint.

## Offline check: graph communities against Claude's groups

`theodore eval <set> --graph-only --compare <run dir>` builds the graph and the
chunks with no agent call. It scores partitions against a saved grouping
(here `claude-default`) by pairwise "same group" precision/recall over hunks.
"x" counts only pairs in different files: a file-level partition keeps a
file's hunks together for free. "dir" is the old rule, one block per
top-level directory. Pooled over all pairs in each set:

| set | graph P / R | graph xP / xR | dir P / R | dir xP / xR |
| --- | --- | --- | --- | --- |
| big (3) | 27% / 77% | 21% / 71% | 18% / 96% | 15% / 94% |
| real (10) | 44% / 92% | 26% / 81% | 18% / 98% | 8% / 96% |
| small (3) | 21% / 64% | 22% / 64% | 18% / 82% | 19% / 82% |

When the graph says two hunks in different files belong together, Claude
agrees 1.4× (big) to 3× (real) as often as it does for "same top-level
directory". The graph's recall is lower: Claude also groups files that share
no visible signal. That gap is why communities only pick chunk boundaries and
the agent still decides the groups.

Chunk recall (Claude-default same-group pairs kept in one prompt by the new
chunking): kdb-83 73%, migratro-77 76%, vkclone-3 100%. This is biased
against the graph: those reference groups were made under directory
chunking, so they can never span directory chunks.

## What the groupings look like

**kdb-83** (Swift client + C++ engine + codegen):

- The Swift TL runtime exists twice: the codegen template
  (`tools/codegen/swift/runtime/…`) and the copy vendored into the app
  (`Blank/…/Runtime/…`), with identical edits. Both baseline runs put the
  two copies in different chunks and reviewed them twice ("Swift runtime:
  use loadUnaligned" and again "Swift RPC runtime: unaligned loads, …"). All
  four graph runs pair each file with its copy: "Swift RPC runtime: unaligned
  loads + close handler/timeout (Reader, Aes, Connection × 2)".
- The generated Swift client (`Client.swift`, `Constants.swift`,
  `Types.swift`) links to the C++ bindings through the shared protocol
  constants (`TL_API_*`); v1–v3 grouped it with the schema, v4 hid it as
  generated, as the baselines did.
- v1 and v2 had a small leftovers group ("API shard/scatter codegen target,
  api docs redirect, AES test signature update"): files with no links were
  packed into whatever chunk had room. File-name links (`tl-pipeline.cmake`
  names the sharding sources) and affinity packing pulled those files back
  next to their peers. In v4 `regen-clients.sh` and the Xcode entitlement
  still ride along with a Swift runtime group.

**migratro-77** (auth ↔ core registry sync, 77 files):

- Baseline: separate "Config, CI and Docker wiring …" and "Wire
  CORE_TO_AUTH_SYNC_SECRET … into scripts and ops docs" groups (both runs),
  plus a docs/spec/changelog group.
- graph-v4: `.env.*`, `ci.yml`, `ci-local.sh` and `dev-all.sh` sit in
  "Registry→auth client sync: app_client table, sync secret and config
  wiring". `Dockerfile.unified`, `.env.example`, `config-catalog.md` and
  `production-image.md` sit in "Shell login tagline from VITE_PRODUCT_TAGLINE".
  Specs ride with their features: `19-tenancy-and-accounts.md` with the
  canInvite gate, `04-identity-and-tokens.md` with hop narrowing. The env keys
  are the links (`CORE_TO_AUTH_SYNC_SECRET`, `VITE_PRODUCT_TAGLINE` defined in
  `.env.example`, read in `config.ts`).
- Weak spot: v1 produced one mixed group ("Org admin requires persona
  canInvite; shell tagline from env"). Later runs didn't repeat it.

**vkclone-3** (33 entry files switched to `al_dispatch()`):

- All runs, before and after, keep the 33 `al_dispatch` edits in one group.
  The graph now forces it: those files' whole change is the same edit, so
  they are one unit, together with `al.lib.php` (a hub that rejoins them).
  This holds even though each entry file also imports its own controller.
- Regression found and fixed: in v1–v3 the separate "Formatting: array() →
  [], aligned keys" skim group vanished. The old hint said "keep linked hunks
  in one group", and the formatting hunks sit in linked controller files. The
  v4 hint sentence brought it back ("Formatting: array syntax and alignment",
  26 hunks). That is one run, so treat it as likely, not proven.

## What didn't work, and limits

- **Raw symbol links glued everything on kdb-83.** The first build counted
  every defined name: `extension Color` in 15 views, `State`, `HONK` in a
  comment. That gave 2 943 edges, 63 hubs out of 114 files, and one SwiftUI
  blob. Fixed by: no definitions from comments, prose or function locals;
  `extension` doesn't define; plain names link at most 3 users; only compound
  names count as new rollouts.
- **Pure Louvain splits a rollout.** A 5-file same-edit clique lost to the
  import edges pairing each entry file with its controller (modularity really
  does prefer that). Hence the forced bundle for files whose whole change is
  one edit.
- **More chunks cost a call.** kdb-83 went from 2 chunks (one far over budget)
  to 3 within budget.
- **File-level, not hunk-level.** A file whose hunks belong to two features
  (an `.env` file with two features' keys, `main.ts` wiring several things)
  gets one community. The agent still splits it ("files split over groups"
  rose on migratro-77), but chunking can't.
- **Not measured end to end:** head-file imports and co-change history. The
  fixtures have no repository.
- **One run per variant**, and Claude varies run to run (the two baselines
  differ by 4 groups on the big set). Read the qualitative effects that repeat
  across v1–v4 (copies paired, config folded in, rollout intact) as solid. Read
  single-run effects as indicative.

## What's left

- ~~One-line skeleton per file for large PRs~~: built, see "Follow-ups" below.
- Hunk-level nodes, so a file serving two features can be split before
  chunking.
- An eval set with repositories attached, to measure head-file imports and
  co-change.
- ~~Incremental regroup doesn't use the graph~~: built, see "Follow-ups".

## Reproduce

```
cargo build -p theodore-cli --release
cd ~/.theodore-eval/cwd
theodore eval ~/.theodore-eval/sets/big  --save ~/.theodore-eval/runs/<label>/big
theodore eval ~/.theodore-eval/sets/real --save ~/.theodore-eval/runs/<label>/real
theodore eval ~/.theodore-eval/sets/big --graph-only --compare ~/.theodore-eval/runs/claude-default/big [--show]
python3 docs/eval/scripts/graph_compare.py ~/.theodore-eval/runs ~/.theodore-eval/sets big claude-default <label>
python3 docs/eval/scripts/metrics.py ~/.theodore-eval/runs ~/.theodore-eval/sets claude-default <label>
```

## Follow-ups: file skeletons and graph-aware regroup

Dates: 2026-10-22. Agent: Claude CLI default model. Agent spend: $5.15 (two
runs of the big set).

**Skeletons: neutral.** Every run is still valid on the first try, with no
"Other" hunks and the same agent calls. Group counts and split files stay
within run-to-run noise. Cost rises about 10%. The cross-chunk duplicate it
was meant to fix (the Swift runtime reviewed in two groups on kdb-83) is
still there. It stays on: it costs no extra call and it gives each part
real context. The numbers don't prove it helps.

**Regroup: same-file suggestions only.** Replaying pushes on the eval PRs
showed that links to other files don't predict where Claude puts a hunk.
Links within the same file do: the suggested group was Claude's group for
93-99% of the hunks it covers. Cross-file links are used only for the
merge rule, and only when the regroup is over the size target.

`ALGO_VERSION` stays 3. Groupings stored now are as good as before, so a
fresh regroup isn't worth an agent call per PR. A bump would also stop
`Core::regroup` from continuing a stored grouping after a push: it only
continues from rows made by the current version.

### File skeletons for large PRs

```
too big ─▶ chunks (unchanged: the skeleton is not reserved when packing)
            │
            └─ each chunk's prompt, after "Related changes":
               "# The rest of this PR"
                 ## Part k of n (label): N file(s)
                 - path (new, +40 -0): adds …; edits …; removes …; routes …;
                   calls …; sql …; hooks …
                 - … +19 more file(s) in dir (8), dir (5), +3 dirs
```

- One line per file, regex-extracted (`crates/grouping/src/skeleton.rs`),
  capped at 180 chars. Each list is capped too: 5 names, 3 routes, 4 calls,
  4 tables, 3 hooks.
- The section fills the room the chunk leaves under the budget: between
  2 000 and 8 000 chars, never more than a sixth of the budget. When it
  doesn't all fit, the files most related to this chunk are listed first.
  The rest are counted by folder.
- **edits** = the definition a change sits in (`Class.method`). With head
  contents from the app, it comes from the file itself. Without them, from
  the hunk header and context lines. The eval runs used diffs only.
- Languages: TS/JS, Python, Rust, Go, PHP, Swift, C# (unit-tested per
  language). Routes cover `router.get('/x')`, `Route::get`, `HandleFunc`,
  `MapGet`, `.route("/x", …)`, decorators and attributes, Vapor, Django
  `path()`. SQL tables are only read from lines that look like SQL, so a
  Python `from x import y` is not a table.

Lines from the fixtures:

```
apps/auth/src/registry/app-client-store.ts (new, +94 -0): adds AppClientRow, AppClientStore, applyIfNewer, loadAll; calls pool.query; sql app_client
apps/core-api/test/persona-permissions.e2e-spec.ts (+111 -2): routes GET /v1/workspaces/${ws}, GET /v1/me/workspaces, GET /v1/workspaces/ws_cli/members +1; calls auth.mintToken …
lib/classes/Processors/FriendProcessor.php (+17 -0): adds searchFriends; edits FriendProcessor; calls $this.fetchAll; sql friends, users
lib/classes/Controllers/SettingsController.php (+56 -48): edits SettingsController, SettingsController.newpassAction, SettingsController.aRestoreAction …
engines/api/api-handler.h (+1386 -10): adds signup, login, markRead, addMember, removeMember +26; edits ApiHandler, connection; calls rpc.send_error, tl.save_to_buffer …
```

The section adds 1 850 to 8 000 chars per chunk. Input tokens rise 4-6%.

| run | valid 1st try | agent calls | Other hunks | groups | in target | groups ≤2 hunks | docs/config-only groups | files split over groups | cost ($) | wall (s) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| claude-default | 3/3 | 10 | 0 | 37 | 3/3 | 0 | 3 | 17 | 2.32 | 243 |
| claude-default-2 | 3/3 | 10 | 0 | 33 | 3/3 | 0 | 2 | 17 | 2.41 | 270 |
| graph-v4 | 3/3 | 10 | 0 | 40 | 3/3 | 2 | 0 | 21 | 2.34 | 203 |
| skel-v1 | 3/3 | 10 | 0 | 38 | 3/3 | 2 | 0 | 17 | 2.60 | 220 |
| skel-v2 | 3/3 | 10 | 0 | 39 | 3/3 | 3 | 1 | 22 | 2.55 | 198 |

`skel-v1` is the skeleton alone. `skel-v2` also replaces the part prompt's
"do not worry about the rest of the PR" with a pointer to the section. The
chunks are identical to graph-v4 in both runs. Both runs predate one fix
(import lines were read as enclosing definitions, so `edits Pool` showed up
on a few TS files).

What the groupings show:

- **kdb-83**: the Swift runtime still appears as two groups, one per chunk
  (v4: 23 + 13 hunks; skel-v1: 11 + 26; skel-v2: 23 + 14). A chunk's agent
  now sees the other copy listed but still can't group it. The merge pass
  only orders groups, so the overlap survives. skel-v1 split the
  `sendMessage` signature updates three ways, one of them a 1-hunk group.
  skel-v2 folded the probe-driver updates into the E2E tests and tools
  group (10; v4 had 5 + 5 + 7 in three groups).
- **migratro-77**: both skeleton runs pull the `app_client` migration (5)
  out of the config group. They leave "Sync secret, exchange rate limit and
  NODE_ENV wiring" (14-15 hunks): the same `.env`, CI, script and
  `config.ts` files v4 had in "Registry→auth client sync: app_client table,
  sync secret and config wiring" (18). v4's group was the same config plus
  the migration and its two test helpers, so this is a split, not new
  fragmentation.
  skel-v2 added a 1-hunk changelog skim group.
- **vkclone-3**: same shape as v4 in both runs. The 33 `al_dispatch` files
  stay together (skel-v1 marks the group skim). Controllers are grouped by
  the kind of change, as before.

What didn't work: reserving the section's room when packing chunks. It
moved kdb-83 from 3 to 4 chunks (one more agent call) and lowered chunk
recall from 73% to 71% (migratro-77: 76% to 71%). That's why the section
now only uses the room left over.

### Graph in incremental regroup

```
push ─▶ unchanged hunks keep their group
        new / changed hunk ─┬─ Changed: was in group G ────────▶ suggest G
                            └─ only G holds hunks of its file ──▶ suggest G
        prompt: each existing group lists its files (≤ 6)
                each suggested hunk: "Suggested group: G (reason)"
agent ─▶ assignments + new groups
         ├─ hunk left out with a suggestion ─▶ G (no repair call; not
         │                                     counted valid first try)
         └─ over the size target: a new 1-hunk group joins the existing
            same-kind group it is most related to (coarsen's relatedness,
            ≥ 1.0), until the target is met. Existing ids never change.
```

The first version did what was asked: suggest the one existing group whose
files the hunk's file is strongly linked to (edge ≥ 1.0), and merge a new
1-hunk group into a strongly linked existing group every time. A push
replay showed both rules were unreliable, so they were narrowed.

Push replay: `theodore eval <set> --graph-only --compare <run>` now plays
back pushes against a saved grouping. Each file in turn, then each hunk of
a multi-hunk file in turn, is treated as new. It checks whether the
suggested group is where the saved run put that hunk. No agent calls.
Against graph-v4:

| rule | file | kdb-83 | migratro-77 | vkclone-3 |
| --- | --- | --- | --- | --- |
| strong link (≥ 1.0) to exactly one group | whole file new | 23% covered, 66% right | 19%, 53% | 17%, **3%** |
| same | one hunk new | 35%, 97% | 22%, 88% | 4%, 100% |
| cross-file link only, ≥ 2.0 | one hunk new | 13%, 100% | 20%, 57% | 33%, **16%** |
| **only group holding the same file** (kept) | one hunk new | 93%, 99% | 69%, 94% | 41%, 93% |
| merge a new 1-hunk group via a ≥ 1.0 link, always | whole file new | 76% merged, 50% right | 83%, 54% | 82%, 42% |

Against claude-default, the kept rule scores 91% / 99% (kdb-83), 80% / 99%
(migratro-77) and 41% / 93% (vkclone-3).

Cross-file links fail where Claude groups by the kind of change rather than
by feature. On vkclone-3 each entry file imports its controller, but Claude
puts the entry file in the `al_dispatch` rollout group and the controller in
a "prepared statements" group. That is the file-level limit from the first
evaluation again. A whole new file therefore gets no suggestion. The agent
places it, seeing each group's file list.

Changed hunks (`HunkChange::Changed { previous }`) are suggested the group
of their previous version. No caller in the app passes `Changed` yet: the
runtime grouper only marks hunks unchanged or new. The replay doesn't cover
this path either; only the unit tests do.

Deterministic tests (`crates/grouping/src/incremental.rs`) use a revision
and a push: billing (route + service) and search groups, then a new service
hunk, a new test file, an unrelated note and an edited search hunk. They
check:

- suggestions: same file → billing, edited hunk → its old group, the new
  test file and the note → none, a file split over two groups → none;
- the prompt lists each group's files and each suggestion;
- a hunk the agent drops lands in its suggestion without a repair call;
- over the target, one of three new 1-hunk groups merges into billing,
  which meets the target, so the other two stay;
- never merged: a skim group into a normal one, an unrelated group, a
  2-hunk group, or anything within the target.

### What's left

- Let the merge pass merge, not just order: the Swift runtime split on
  kdb-83 needs a cross-chunk merge, and the skeleton can't do that.
- Hunk-level graph nodes (unchanged from above). This is what limits both
  cross-file suggestions and chunking.
- Run the skeleton on the real set, and with head files from a real
  worktree. Neither is measured: the fixtures carry diffs only.
- Pass `Changed` hunks from the app's runtime grouper (it only knows the
  previous grouping, not the previous revision).

### Reproduce (follow-ups)

```
theodore eval ~/.theodore-eval/sets/big --save ~/.theodore-eval/runs/skel-v2/big
theodore eval ~/.theodore-eval/sets/big --graph-only --compare ~/.theodore-eval/runs/graph-v4/big --show
python3 docs/eval/scripts/graph_compare.py ~/.theodore-eval/runs ~/.theodore-eval/sets big claude-default claude-default-2 graph-v4 skel-v1 skel-v2
```
