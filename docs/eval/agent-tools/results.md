# Agent tools eval: results, 2026-11-02

Does an agent do better with the code-intelligence tools (docs/code-intel.md
§7) than with Grep and Read alone? Harness: `theodore eval-tools`
(README.md here); tasks: `tasks.yaml` (TASKS.md: 16 tasks + 1 smoke, Rust /
TypeScript / Python, answer and edit). Claude Code 2.1.312, `sonnet`, same
prompt, permissions and tools in every condition; `on` adds the code tools
and their system-prompt line. Spend: **$9.43**.

## Runs

| run | condition | repeats | note |
| --- | --- | --- | --- |
| run-2 | off, on | 2 | on = tools attached the way the app attached them (deferred) |
| run-3 | on, `--eager-tools` | 2, stopped at 12/34 | `ENABLE_TOOL_SEARCH=false`: every MCP tool up front |
| run-4 | on, `alwaysLoad` | 1 | Theodore's server entry `alwaysLoad: true` (#126) |

## Results

| condition | passed | runs that called a code tool | $ for the 17 tasks |
| --- | --- | --- | --- |
| off | 34/34 | — | 1.54 (median per task, summed) |
| on, deferred | 34/34 | **0 / 34** | ≈ 1.54 |
| on, eager | 10/10 scored | 6 / 12 | — (Rust tasks only) |
| on, `alwaysLoad` | 17/17 | 5 / 17 | 1.72 (+12 %) |

Code-tool calls under `alwaysLoad`, by task:

| task | off $ | alwaysLoad $ | code-tool calls |
| --- | ---: | ---: | --- |
| rust-trace-language-remove | 0.144 | 0.143 | find_symbol 1, get_symbol 4 |
| rust-reexport-sandbox-data-dir | 0.085 | 0.078 | 2 |
| rust-callers-code-intel-status | 0.106 | 0.116 | find_symbol 1 |
| py-callers-rich-markup-render | 0.093 | 0.097 | 1 |
| py-rename-rich-markup-render | 0.132 | 0.116 | 1 |
| the other 12 | | | none |

## What it says

```
deferred (as shipped)  ─▶ tools behind ToolSearch ─▶ agent has Grep ─▶ never looks   (0/34)
alwaysLoad             ─▶ tools in the prompt     ─▶ used where tracing helps        (5/17)
```

- **As shipped, agents never used the code tools.** Claude Code defers MCP
  tools behind ToolSearch; with Grep and Read at hand the agent never went
  looking. Fixed by `alwaysLoad` on Theodore's server only (#126): the
  user's own servers stay deferred, unlike `ENABLE_TOOL_SEARCH=false`.
- **No quality difference on this suite**: sonnet passes every task with
  Grep alone, so the suite is at its ceiling and only cost separates the
  conditions.
- **Cost +12 %** with `alwaysLoad`, from one repeat. Part of it is the
  first-run cache write of a longer prompt (smoke task: $0.127 vs $0.072,
  no tool used), so it is an upper bound.

Two run-3 runs are not scored: their check script was missing because the
checkout the harness reads checks from changed branch mid-run. The harness
should copy `checks/` into the run's output dir at start.

## Next

- Harder tasks, where Grep misleads more often: larger repos, deeper
  re-export chains, cross-crate trait dispatch, renames with shadowing.
- A weaker model (`haiku`) on the same suite: where Grep alone fails, the
  tools should show up as pass rate, not only cost.
- More repeats for cost: one repeat per task is within noise.
- Make the harness copy `checks/` at start, so a checkout change can't break a run.
