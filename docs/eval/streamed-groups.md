# Streamed groups

**Verdict: stream, on by default for Claude.** Each group is shown the moment
the model closes its JSON object, at no extra token cost. On kdb#83 (28k
lines, 3 chunks), the first group shows in ~5.6 s instead of ~26 s, at the
same 132k input tokens. On PRs that fit one prompt, the first group shows in
~4.7 s instead of ~10.8 s. Validity is unchanged.

Date: 2026-10-27. Model: `claude-sonnet-5-5` at low effort (the grouping
defaults). Claude Code 2.1.312.

## How it works

```
claude -p … --output-format stream-json --verbose --include-partial-messages
   stream_event / content_block_delta / text_delta ─▶ AgentEvent::TextDelta
                                                        │
GroupStream (crates/grouping/src/stream.rs)  ◀──────────┘
   prose / ```json ─▶ `"groups": [` ─▶ depth-0 `{` … `}` ─▶ RawGroup
                                                        │
Board (pipeline.rs) ◀───────────────────────────────────┘
   shown = hidden group ⊕ per chunk, in chunk order:
           its final groups once landed, else the groups its answer wrote
   ─▶ OnProgress(Stage::Streamed | Stage::Chunk), grouping `in_progress`
chunk lands (validated, repaired) ─▶ its final groups replace its streamed ones
all chunks ─▶ merge pass ─▶ returned grouping (final, not in_progress)
```

- `AgentRequest::stream_text` turns on `TextDelta` events, on top of the
  per-message `Text` events. Claude gets `--include-partial-messages`; Gemini's
  `stream-json` is deltas already. Codex ignores it: `codex exec --json` has
  no text deltas.
- `GroupingOptions::stream`: the app sets it for Claude and Gemini runs
  (`crates/runtime/src/grouper.rs`). The CLI leaves it off unless
  `theodore eval --stream`, so evals stay comparable with earlier runs.
- Only a chunk's first answer streams. A repair call does not: the chunk's
  streamed groups stay up until the repaired answer lands.
- A streamed group keeps only its own chunk's hunks, each hunk once. Ids are
  unique across chunks (`slug-2`) and the same from streamed to final.
- Streaming coarsens per chunk, as the chunk lands, so a group shown after its
  chunk landed is never merged away by another chunk.
- The app writes each provisional grouping as the PR's row (coalesced when
  they come faster than they are written), then the final one. A failed run
  puts back the row it replaced; with none, its row stays `in_progress` and
  is regrouped on the next open.
- `GroupingReport::first_group_ms` goes to the `grouping done` log line and
  the `grouping` span. `eval --save` writes `first_group_ms`, `done_ms`,
  `stream.{group_ms,answer_ms}` and `previews.{shown,kept,changed}`.

## kdb#83 (292 visible hunks, 3 chunks, 3 in parallel)

| variant | runs | first group, median (range) | done | input tok | output tok | valid 1st try | groups |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| (a) no stream | 3 | 26.4 s (26.2–30.7) | 26.4 s (26.2–30.7) | 132–157k | 6.6–7.4k | 2/3 | 12–13 |
| (b) stream | 6 | **5.6 s (5.3–6.8)** | 27.7 s (25.8–31.2) | 132k | 6.1–6.9k | 5/6 | 11–13 |

How the groups arrive (b):

```
first  5.3–6.8 s
half   14.2–15.7 s
last   23.0–28.1 s  (last answer)
done   25.8–31.2 s  (+ merge pass)
```

- Every group shown already has its `why`, `risk` and `check`.
- Groups arrive in chunk order; the merge pass reorders them ~3 s after the
  last answer. The UI keeps a group the user opened or read in its slot.
- Previews survived: 34/34 kept with the same hunks in the final grouping.
- The one invalid first try in (a) and in (b) was a chunk needing a repair
  call, not streaming-related: the prompts are identical.

## One-prompt PRs (13: shipped fixtures + real set, one chunk each)

| fixture | (a) done | groups | (b) first | (b) done | groups | streamed |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| bun_39523 | 10.8 s | 4 | 6.0 s | 12.5 s | 4 | 4 |
| django_22073 | 11.5 s | 5 | 4.2 s | 10.7 s | 5 | 5 |
| django_22109 | 8.2 s | 3 | 5.6 s | 9.2 s | 4 | 4 |
| next_88225 | 6.8 s | 2 | 4.9 s | 6.4 s | 2 | 2 |
| pr211 | 6.3 s | 4 | 3.4 s | 7.2 s | 4 | 3 |
| pr479 | 7.1 s | 3 | 3.7 s | 7.6 s | 3 | 3 |
| pr482 | 11.2 s | 6 | 5.0 s | 11.5 s | 6 | 5 |
| ra_23173 | 23.6 s | 11 | 5.6 s | 26.8 s | 11 | 11 |
| ra_23452 | 10.1 s | 5 | 4.0 s | 10.6 s | 5 | 5 |
| ruff_18961 | 11.1 s | 5 | 5.2 s | 10.7 s | 6 | 5 |
| ruff_28362 | 5.8 s | 3 | 4.7 s | 6.7 s | 3 | 2 |
| tokio_8587 | 15.2 s | 6 | 4.7 s | 13.9 s | 6 | 6 |
| ts_64846 | 10.8 s | 5 | 4.5 s | 10.0 s | 5 | 5 |

| run | first group, median (range) | done, median (range) | valid 1st try | groups | input tok | output tok |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| (a) | = done | 10.8 s (5.8–23.6) | 13/13 | 62 | 360k | 15.5k |
| (b) | 4.7 s (3.4–6.0) | 10.6 s (6.4–26.8) | 13/13 | 64 | 360k | 16.0k |
| (b) again | 4.9 s (3.7–7.7) | 10.5 s (6.3–23.1) | 13/13 | 59 | 360k | 15.6k |

- "Streamed" is one less than the final count where the PR has a prepass
  `hidden` group (lockfiles, generated files): it never comes from the model.
- Previews in the second (b) run: 55/55 kept with the same hunks.

## Port check (the shipped code, same day)

The numbers above came from the prototype branch. Re-measured on the
shipped pipeline (`theodore eval /tmp/geval/sets/big --model
claude-sonnet-5-5 [--stream] --save …`):

| run | first group | done | input tok | output tok | valid 1st try | groups | previews kept |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| stream 1 | 7.1 s | 35.7 s | 132k | 7.2k | yes | 12 | 12/12 |
| stream 2 | 7.9 s | 95.1 s | 158k | 8.0k | no (1 repair) | 12 | 11/12 |
| no stream | 30.3 s | 30.3 s | 132k | 6.7k | yes | 12 | — |

- Same gain: first group at ~24% of the no-stream run's time (7.1 / 30.3 s),
  as before (5.6 / 26.4 s). The whole set ran ~15% slower than the
  prototype's day, the no-stream run included.
- Stream 2: every chunk's answer was in by 25 s; one chunk's repair call and
  the merge pass took the rest (to 95 s). Its streamed groups stayed up the
  whole time, and one of them changed hunks when the repair landed. Repair
  latency is not streaming-related, but streaming is what keeps it from being
  a blank pane.

## Two-phase grouping was measured and rejected

Partitioning first (a lean answer: id, title, kind, hunks) and writing the
explanations in a second, batched describe pass, on kdb#83:

| | stream (shipped) | two phases | two phases + stream |
| --- | ---: | ---: | ---: |
| first group shown | ~5.6 s | ~10.4 s | ~3.7 s |
| every hunk in a group | ~28 s | ~21 s | ~20 s |
| every group explained | ~28 s | ~35 s | ~36 s |
| input tokens | 132k | ~350k | ~350k |

Two phases cost ~2.7x the tokens, finish later, and show groups without
their explanation for most of the run. Streaming one phase gets the first
group in sooner than two phases alone, fully explained. Not shipped.

## Not verified

- A streamed run that hits a repair was measured once (stream 2 above): one
  preview of twelve changed when the repaired answer landed.
- A PR fragmented enough that per-chunk coarsening merges groups on landing:
  the streamed groups it merges are replaced when their chunk lands (in unit
  tests only).
- Claude Code and Gemini stream (Gemini from its documented format, not yet
  run against a real binary). A Codex run shows the grouping when it is done,
  as before.
