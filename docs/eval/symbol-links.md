# Symbol links from code intelligence

**Verdict: keep it, as a hint. No measurable quality change.** Every run is
valid on the first try. Claude already orders "defined here" before "used
there" without the links; with them, it never put a use's group before its
definition's (4 of 47 on migratro-77 without), but one run per variant is
inside run-to-run noise (an earlier baseline, skel-v2, also had 0).
Grouping latency is unchanged: the links come from a ≤ 2 s side task that
runs alongside the history scan.

Dates: 2026-11-02. Agent: Claude CLI default model. Agent spend: $1.96.

## What changed

```
Core::grouping_context
  ├─ head files (as before)
  ├─ co-change history ─────────────┐ tokio::join, both best effort
  └─ symbol_links (≤ 2 s) ──────────┘
       changed file ─▶ file_symbols ─▶ symbol covering an added line
                                       (name on an added line first, then
                                        innermost; container gives way)
                    ─▶ references(name) ─▶ hit on another hunk's added line
                    ─▶ SymbolLink { from: defining hunk, to: using hunk, symbol }
        ▼
RepoContext.symbol_links ─▶ RelGraph
  ├─ file edge "uses `X`", 1 per distinct symbol, capped at 2 (hubs count it)
  ├─ prompt "# Symbol links": one line per defining hunk and symbol
  └─ coarsening / incremental placement: +1 per link between a tiny group's
     hunk and a group's (breaks ties between groups of the same file)
```

- Caps (`SymbolLinkLimits::default`): 2 s wall, 60 files, 40 symbols
  (round-robin across hunks), 8 links per symbol, 200 links, 4 port calls in
  flight. What is found by the deadline is kept; the calls still running are
  aborted.
- Skipped names: under 3 chars, a list of common method names (`new`,
  `get`, `render`, …), modules, and fields/variables without a compound
  name. The syntax layer matches by name, so `new` would link everything.
- A worktree not open yet (the PR's open races its first grouping) is
  asked again 3 times, 100 ms apart. Code intelligence off: nothing is
  asked, the context is as before.
- Prompt line: `- c1 `evict_older` → a1, c2` under "Found by code
  intelligence: the first hunk defines or changes `name`, the hunks after →
  use it. Put them in one group, or the defining hunk in an earlier group
  than its uses." At most 30 lines, 1 600 chars, 6 users per line. Only
  links whose both hunks are in the prompt.
- `ALGO_VERSION` stays 3: stored groupings aren't worse.

## Eval without repositories

Fixtures carry only diffs. `theodore eval --symbol-links` writes a sparse
checkout per fixture (each changed file holds its hunks' new-side lines at
their own line numbers, blank elsewhere), opens it with the real
`CodeIntel` facade (syntax layer, no language servers) and calls the same
`symbol_links` as the app, with a 60 s budget instead of 2 s (a fresh index
per fixture). Every link joins two changed hunks, so both ends are in the
checkout; definitions whose name line is outside the hunk are lost.

| set | fixtures with links | links | time per fixture |
| --- | ---: | ---: | ---: |
| real (10 OSS PRs) | 4 | 11 | 1.5–11 s |
| big (3) | 1 (migratro-77) | 47 | 3–13 s |

Zero links: PHP has no grammar (vkclone-3), kdb-83 (Swift/C++) found none
(not investigated), and most small real PRs change one definition used
nowhere else in the diff.

Example links (real set):

```
ts_64846   timingDump.ts `fromEnvironment` → async/client.ts, sync/client.ts
ra_23173   base-db/src/input.rs `is_proc_macro` → hir-def/src/nameres/collector.rs (×2)
ruff_18961 pylint/settings.rs `matches_value` → magic_value_comparison.rs
```

## Results

Offline, against the stored `claude-default` groupings: 27% of the real
set's links (3/11) and 49% of migratro-77's (23/47) had both hunks in one
group. The rest were split in reading order — TypeScript#64846 reviews the
module, then its use in each client, as three groups, which the M1 eval
rated 5/5. So a link is not "same group"; the hint says "same group or
definition first".

How each run treated the links (`docs/eval/scripts/link_adherence.py`):

| run | links | same group | defining group first | defining group later | groups (linked PRs) |
| --- | ---: | ---: | ---: | ---: | ---: |
| real: links-off | 11 | 3 | 8 | 0 | 25 |
| real: links-on | 11 | 4 | 7 | 0 | 27 |
| migratro-77: claude-default (2026-10-21) | 47 | 23 | 20 | 4 | 14 |
| migratro-77: skel-v2 (2026-10-22) | 47 | 27 | 20 | 0 | 13 |
| migratro-77: links-off | 47 | 27 | 16 | 4 | 15 |
| migratro-77: links-on | 47 | 30 | 17 | 0 | 13 |

The `theodore eval` table, real set:

| run | valid 1st try | groups | wall (s) | cost ($) |
| --- | ---: | ---: | ---: | ---: |
| links-off | 10/10 | 50 | 149 | 0.87 |
| links-on | 10/10 | 52 | 127 | 0.48 |

Cost and wall time are not comparable: the second run hit the CLI's prompt
cache (six of the ten prompts were byte-identical). Agent calls are the
same (10).

## Limits

- One run per variant on 5 linked PRs. Read "never later with links" as
  likely, not proven.
- Only the syntax layer was measured (review worktrees don't start language
  servers by default). With an LSP the links are exact, not by name.
- In the app the index may be cold when a PR is first opened: the 2 s
  budget can end before any reference answers, and that grouping simply has
  no links. Eval needed 1.5–13 s per fixture on a fresh index.
- Cancelled port calls stop waiting, but the syntax layer's blocking work
  (a text search over unindexed files) may still finish in the background.

## Reproduce

```
cargo build -p theodore-cli --release
cd ~/.theodore-eval/cwd
theodore eval ~/.theodore-eval/sets/real --save ~/.theodore-eval/runs/links-off/real
theodore eval ~/.theodore-eval/sets/real --symbol-links --links-dir ~/.theodore-eval/links \
  --save ~/.theodore-eval/runs/links-on/real
theodore eval ~/.theodore-eval/sets/big --graph-only --symbol-links \
  --links-dir ~/.theodore-eval/links-big --compare ~/.theodore-eval/runs/claude-default/big
python3 docs/eval/scripts/link_adherence.py ~/.theodore-eval/links/links.json \
  ~/.theodore-eval/runs real links-off links-on
```

The grammars come from the release pinned in
`crates/code-intel-toolchain/manifest/grammars.json`; a build whose pins
don't match the published assets indexes nothing ("checksum mismatch" in
the `--symbol-links` output), and every fixture gets 0 links.
