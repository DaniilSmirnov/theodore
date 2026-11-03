# Agent tools eval: do the code tools help?

`theodore eval-tools` runs the same tasks with the code tools
(docs/code-intel.md §7) and without, and compares them (§10 S3: which
layer is on by default, whether Grep stays).

**It spends money.** Every run is a real `claude -p` session billed to
your account. `--budget-usd` (default 30) stops scheduling new runs once
the runs so far cost that much; a run already started finishes. Start with
`--dry-run`, then `--only <id> --repeat 1`.

```
tasks.yaml ─▶ validate ─▶ plan: task × repeat × condition (off/on interleaved)
  per run: spent ≥ budget? ─▶ stop
           bare clone (cache) ─▶ fresh worktree at rev
           on: open the facade ─▶ warm: indexed, servers ready   (warm-up timed apart)
               ─▶ code-tools MCP + the app's system-prompt line
           claude -p, the app's flags ─▶ stream-json ─▶ tokens, cost, turns, tools
           check ─▶ 0/1 ─▶ runs.jsonl ─▶ worktree removed
  summary.md · summary.json
```

## Run it

```sh
cargo build -p theodore-cli
B=target/debug/theodore

$B eval-tools --dry-run                                   # the plan and the flags, no agent
$B eval-tools --only refs-open-workspace --repeat 1 --model haiku --budget-usd 2
$B eval-tools --repeat 2 --model sonnet --budget-usd 30   # the suite, both conditions
```

| Flag | Default | |
| --- | --- | --- |
| `--tasks` | `docs/eval/agent-tools/tasks.yaml` | the suite |
| `--only a,b` | all | task ids |
| `--repeat` | 2 | runs per (task, condition) |
| `--conditions` | `off,on` | `on` alone for a smoke run |
| `--model`, `--effort` | the CLI's | passed to `claude` |
| `--budget-usd` | 30 | no new run past this |
| `--out` | `target/eval-tools/<UTC stamp>` | results |
| `--transport` | `http` | `stdio`: below |
| `--warm-timeout-s` | 600 | `on` starts the agent anyway after this |
| `--install-servers` | off | download missing language servers first |
| `--keep-worktrees` | off | |
| `--cache-dir` | `<data dir>/eval-tools` | repos, worktrees, `target/<task>`, `code-intel/` |

Claude Code only: its stream-json carries the cost and the cache split.

## What is identical, what differs

| | off | on |
| --- | --- | --- |
| prompt, model, effort, cwd (fresh worktree at `rev`) | same | same |
| mode | answer: read-only (Read, Grep, Glob…; no Bash, no edits) · edit: a space's Edit mode (`acceptEdits` in the worktree, Bash) | same |
| `--strict-mcp-config` (no user MCP servers) | yes | yes |
| `CARGO_TARGET_DIR` | `<cache>/target/<task>` | same |
| MCP server | none | `code-tools`: `find_symbol`, `get_symbol`, `references`, `definition`, `diagnostics`, `code_status`; edit tasks also `replace_symbol_body`, `insert_after_symbol`, `rename_symbol` |
| system prompt | — | the app's line about the tools (`theodore_app::code_prompt`) |

The tools are the app's own: the same definitions, result shapes and caps
(`crates/mcp`), the same queries and edits (`theodore_app::CodeRoot`).

Transport:

- `http` (default): this process holds the facade, warms it, and serves the
  tools on localhost the way the app serves `space-tools`. Language servers
  stay warm for the agent.
- `stdio`: the agent starts `theodore code-tools-mcp` itself. The index is
  warm (shared cache), but language servers start again inside the agent's
  time.

`theodore code-tools-mcp --root <dir> [--trust trusted|review] [--edit]` is
also usable on its own, for any MCP client.

## The task file

```yaml
- id: refs-open-workspace            # unique, a-z 0-9 - _
  repo: https://github.com/JeanIsahakyan/theodore   # or a path relative to this repo's root
  rev: <full commit sha>
  kind: answer | edit
  prompt: |
    …
  check:
    type: contains | regex | command
    value: "…"        # contains (case-sensitive) / regex: against the final answer
    run: "bash docs/eval/agent-tools/checks/x.sh"     # command: exit 0 = pass
  timeout_s: 600      # optional
```

- An edit task is scored by a command. A command runs with `bash -c` in the
  worktree after the agent, with `THEODORE_EVAL_WORKTREE`,
  `THEODORE_EVAL_ROOT` (this repo), `THEODORE_EVAL_ANSWER_FILE` (the final
  answer) and `CARGO_TARGET_DIR` set.
- Paths in `run` under this directory resolve to this repo's copy, even when
  the worktree (the task's repo at `rev`) has an older one.
- The file is validated whole before anything runs; every problem is listed.

## What is measured

Per run (`runs.jsonl`, one JSON object a line; transcripts in
`transcripts/<run>.jsonl`):

| Field | From |
| --- | --- |
| `score` 0/1 | the check; a run that errored or timed out scores 0 |
| `input_tokens`, `cache_creation_tokens`, `cache_read_tokens`, `output_tokens` | the `result` event's `usage` |
| `cost_usd` | `total_cost_usd`; none for a killed run |
| `num_turns`, `agent_duration_ms` | the `result` event |
| `duration_s` | wall time of the agent process, warm-up excluded |
| `tool_calls` | `tool_use` blocks by name (`Grep`, `Read`, `mcp__code-tools__find_symbol`, …) |
| `warm_s`, `warm_ready` | `on`: indexing and servers before the clock |
| `diff_stat` | edit tasks: what the agent changed |

`summary.md`: per condition pass rate, median and mean tokens, cost, turns,
time, warm-up, mean tool calls per run; per task the medians and on − off
deltas.

## Limits

- Claude Code only. Codex and Gemini report no cost per run.
- A killed run has no `result`: its tokens are the sum of its messages'
  usage, its cost is unknown and doesn't count toward the budget.
- Runs are sequential. Repeat at least 2: one run per condition is noise.
- `on` warms once per run, on a fresh worktree: the index is cached by blob,
  language servers start again each time (`warm_s` shows it).
