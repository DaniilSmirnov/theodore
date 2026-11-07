# Sandbox

Run Theodore's whole review flow against a simulated, GitHub-like host: no
network, no tokens, no real repo at risk. Reviews, replies, reactions, merges
and agent fixes all land somewhere real — local bare git repos and a state
file — so every screen behaves the way it does against GitHub. Faults and
events can be injected while the app runs.

```
scenario.json ──first start──▶ <data>/sandbox/remotes/<owner>/<repo>.git   real bare repos,
      │                                                                    refs/pull/N/head
      └────────────────────────▶ <data>/sandbox/state.json                 reviews, threads,
                                                                           checks, faults
Theodore ── HostAdapter ──▶ sandbox host ──▶ state.json + bare repos
         ── git (file://) ─────────────────▶ bare repos  (clone, fetch, push)
theodore sandbox … ── control/requests ──▶ ticker (250 ms): CI, auto-merge, queue,
                   ◀─ control/responses ──              timeline, control requests
```

## Run it

```sh
theodore sandbox init ~/sandbox            # writes scenario.json + assets/
THEODORE_SANDBOX=~/sandbox theodore-desktop
# or: theodore-desktop --sandbox ~/sandbox
# dev: cd apps/desktop && THEODORE_SANDBOX=$PWD/../../crates/fixtures/data/sandbox cargo tauri dev --no-watch
```

| | normal | sandbox |
| --- | --- | --- |
| data dir | `~/.theodore` (`~/.theodore-<env>` for other builds) | `THEODORE_DATA_DIR`, else `~/.theodore-sandbox`; every build's data dir is refused |
| host | GitHub / GitLab / Bitbucket | the sandbox host (GitHub-shaped ids `github:owner/repo#n`) |
| tokens | keychain, `gh`, env | none: secrets in memory, git gets no credentials |
| git | `https://` remotes | `file://` bare repos under the data dir |
| updates | checked every 6 h | off |
| agents | agent CLIs | agent CLIs, or `"agents": "replay"` for deterministic, token-free runs |

The sidebar shows a **SANDBOX** badge and the window title starts with
`[SANDBOX]`. Settings → Sandbox lists every fault (toggle) and timeline event
(fire now).

Start over: quit the app and delete the data dir. The scenario is read on
every start, but after the first one the state file is the source of truth
(repos added to the scenario later are still built).

## Drive it from a terminal

The app must be running (`status` also works when it isn't). Same
`THEODORE_DATA_DIR` as the app, or `--data-dir`.

```sh
theodore sandbox status
theodore sandbox push 101                                   # generated one-line edit
theodore sandbox push 101 --file src/a.ts --content 'x\ny'  # explicit files
theodore sandbox comment 110 src/pages/Orders.tsx:7 "Cancel on unmount?" --as lee
theodore sandbox check 109 ci/test pass                     # pending | pass | fail
theodore sandbox fault rate_limit on --value 120            # reset in 120 s
theodore sandbox request-review 117                         # from you (or --user / --team)
theodore sandbox merge 106 --as lee                         # merged by someone else
theodore sandbox fire push-101                              # a timeline event, now
```

PRs are `101` or `acme/storefront#101`.

**A long chat without tokens.** Create a space in the sandbox, quit the app,
then seed it and reopen:

```sh
theodore sandbox seed-chat "<space name or id>" --turns 300
```

It writes N stored turns (markdown, code, tables, tool steps of varying length)
into `theodore.db`. It refuses a data dir that isn't a sandbox's, and a sandbox
whose app is running.

## Faults

| name | effect | `--value` |
| --- | --- | --- |
| `latency` | every host call waits | ms (default 400) |
| `offline` | host calls fail to connect; git remotes point nowhere | |
| `auth` | every call answers 401 (token expired) | |
| `rate_limit` | 403 rate limited until a reset time, then lifts itself | seconds (default 60) |
| `server_errors` | a share of calls answer 502 | rate 0..1 (default 0.3) |
| `not_found` | listed PRs answer 404 | a PR number to add |
| `slow_merge_status` | merge status takes long | ms (default 4000) |
| `merge_queue` | direct merges refused; queued PRs merge after `ci.queue_after` | |
| `required_approvals` | every PR needs N approvals to merge | N (default 2) |

`faults.latency_by_call` in the scenario adds a fixed delay to one call
(`get_pr`, `search`, `merge_status`, …) regardless of the toggles.

## What the host does

```
review submitted ──▶ review + threads (marker-tagged, idempotent) ──▶ viewer leaves "requested"
push to head ─────▶ refs/pull/N/head moves ──▶ threads shift or go outdated,
                    Viewed files dismissed, checks pending ──(ci.push_after)──▶ ci.push_result
merge ────────────▶ merge / squash / rebase commit on the base branch (real git),
                    other PRs on that base get a new ETag
re-run check ─────▶ pending ──(ci.rerun_after)──▶ ci.rerun_result
auto-merge ───────▶ merges once checks pass and approvals are met
```

GitHub rules it keeps: no approving or requesting changes on your own PR, a
review body is required for comment / request-changes without inline comments,
inline comments only on files in the diff, only your own comments can be
edited or deleted, a draft or conflicting PR can't merge, a review request
can't go to the author. Inbox sections are derived from state: review
requested (you or your team), assigned, mentioned (`@you`), authored, and
updated after your last review.

## Scenario format

`scenario.json` (see `crates/fixtures/data/sandbox` for a full example):

```jsonc
{
  "name": "default",
  "viewer": "sam",                                   // who Theodore is signed in as
  "users": [{ "login": "sam", "name": "Sam Rivera", "avatar": "data:image/svg+xml;base64,…", "bot": false }],
  "teams": [{ "slug": "web-core", "members": ["sam", "mira"] }],
  "repos": [{
    "owner": "acme", "name": "storefront", "default_branch": "main",
    "hosted": true,                                  // false: only for a local folder project
    "require_up_to_date": false,                     // true: a PR behind its base is "Behind"
    "branches": [                                    // in order; `from` must come earlier
      { "name": "main", "commits": [{ "message": "Initial", "author": "mira", "ago": "4w",
          "files": [{ "path": "src/a.ts", "text": "…" }] }] },
      { "name": "feature", "from": "main", "commits": [{ "message": "…", "author": "lee", "ago": "2d",
          "files": [
            { "path": "img/logo.png", "asset": "assets/logo.png" },  // or "base64": "…"
            { "path": "src/b.ts", "rename_from": "src/a.ts" },       // keeps content unless given
            { "path": "old.txt", "delete": true }
          ] }] },
      { "name": "main", "commits": [ … ] }          // same name again: main moves on
    ]
  }],
  "prs": [{
    "repo": "acme/storefront", "number": 101, "title": "ABC-101 …", "author": "mira",
    "head": "feature", "base": "main", "state": "open",        // open | draft | merged | closed
    "reviewers": [{ "user": "sam" }, { "team": "web-core" },     // requested
                  { "user": "lee", "state": "approved", "ago": "1d" }],
    "checks": [{ "name": "ci/test", "status": "pending", "after": "45s", "then": "pass" }],
    "threads": [{ "path": "src/b.ts", "line": 3, "resolved": false, "outdated": false,
                  "comments": [{ "author": "lee", "body": "…", "reactions": { "+1": ["mira"] } }] }],
    "required_approvals": 1, "labels": [], "assignees": []
  }],
  "projects": [{ "repo": "acme/storefront" }, { "local": "handbook", "from": "acme/handbook" }],
  "agents": "real",                                  // or "replay"
  "auto_skills": false,                              // replay only: run review.auto_skills
  "faults": { "latency": false, "latency_ms": 400, "not_found_prs": [107] },
  "ci": { "push_after": "8s", "push_result": "pass", "rerun_after": "8s", "queue_after": "8s" },
  "timeline": [
    { "id": "push-101", "at": "45s", "action": { "do": "push", "pr": 101,
        "files": [{ "path": "src/a.ts", "text": "…" }], "message": "…" } },
    { "id": "manual-one", "action": { "do": "check", "pr": 109, "name": "lint", "status": "pass" } }
  ]
}
```

Durations: `250ms`, `30s`, `15m`, `2h`, `3d`, `1w`. `ago` is relative to the
first start; `at` is relative to each app start, and an event fires once
(`fire` fires it again). Timeline actions: `push`, `comment`, `reply`,
`review`, `request_review`, `merge`, `close`, `check`, `fault`.

## Simulated vs not

| real | simulated |
| --- | --- |
| git: clone, fetch, diff, merge-tree, worktrees, blobs, grep, push | the host API, its search and ETags |
| the core, store, outbox, poller, grouping pipeline | CI (timed transitions) |
| agent CLIs (unless `replay`) | other people (timeline and CLI) |

Not simulated: GitLab/Bitbucket hosts, notifications as a separate API (the
inbox probe is a version counter), branch deletion on merge and stack
retargeting, the device-flow login, Linear.
