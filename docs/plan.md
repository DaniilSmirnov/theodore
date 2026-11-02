# Theodore — build plan

Theodore is a standalone desktop app for code review. Local AI agents regroup a
pull request by meaning, so the reviewer reads it as logical steps, not as an
alphabetical list of files. The agents help; the reviewer decides.

---

## 1. Decisions

| # | Decision | Status |
| --- | --- | --- |
| D1 | Everything in Rust except the UI | decided |
| D2 | UI = React + HeroUI | decided |
| D3 | Shell = Tauri 2; no Node sidecar | decided |
| D4 | UI ⇄ core is local-first sync: transactions up, sync log deltas down | decided |
| D5 | No server in v1. The "backend" is the local Rust core | decided |
| D6 | Code hosts behind a `HostAdapter` with capabilities; GitHub first | decided |
| D7 | Own HTTP client for hosts (`reqwest` + `cynic`); `gh` only as a token source | decided |
| D8 | Agents = the user's installed CLIs (`claude`, `codex`, `gemini`), spawned by core | decided |
| D9 | MCP `review-tools` served by core over localhost HTTP, one token per run; stdio bridge for CLIs without HTTP MCP | decided |
| D10 | git through the git CLI, not libgit2 | decided |
| D11 | SQLite for state; OS keychain for secrets | decided |
| D12 | macOS (Apple Silicon) first | decided |
| D13 | UI state = MobX object pool + a small `resource()` cache; no TanStack Query, no router | decided |
| D14 | UI runs only inside Tauri; no browser mode, no fake core — dev and e2e use the real core with fixture adapters | decided |
| D15 | Agent per task is a setting (grouping, chat, skills, fixes), default Claude | decided |
| D16 | Auto-update from this repo's GitHub Releases, built in from the first version | decided |
| D17 | Data lives in `~/.theodore` (other builds: `~/.theodore-<env>`) | decided |
| D18 | Later, not v1: billing and the cloud features behind the Theodore account (sign-in itself is D21), Linux and Windows | decided |
| D19 | Tooling: `specta` + `tauri-specta` for TS types and typed commands, `rusqlite` for SQLite, `pnpm` for the UI | decided |
| D20 | No Apple Developer ID yet: releases are ad-hoc signed, not notarized; CI adds Developer ID signing + notarization once secrets exist | decided |
| D21 | Theodore account = WorkOS AuthKit as a public client: hosted page + PKCE, redirect to `theodore://callback` in the app bundle and to a loopback listener elsewhere, device code as fallback; no secret in the app, no server yet (§6.12) | decided |
| D22 | Telemetry = one `tracing` stream, one redaction step, sinks by setting: a local JSON-lines log always; OTLP error reports and PostHog product events only when configured and opted into (two separate choices, recorded before anything is sent), both keyed by a random install id — never the account (docs/telemetry.md) | decided |

---

## 2. Architecture

### 2.1 Layers

```
┌─ shell ───────── Tauri 2: windows, menus, updater, deep links, signing
│
├─ ui ──────────── React + HeroUI + MobX object pool
│        │  knows only the contract — no fetch, no fs
│        ▼
├─ contract ────── commands, sync actions, ephemeral events → generated TS types
│        ▼
├─ app ─────────── use cases: openPr, group, runSkill, submitReview, onPush …
│        │         sync engine (apply → sync log → deltas), outbox, scheduler
│        ▼
├─ domain ──────── pure logic, no I/O, unit-tested
│                  groups + invariants, read → viewed, anchors, interdiff,
│                  findings merge, task lifecycle, snooze
│        ▲
├─ ports ───────── traits: Host · Git · Agent · Store · Secrets · Clock
│        ▲
└─ adapters ────── host-github · git-cli · agent CLIs · mcp · sqlite · keychain
```

Rules:
- `domain` depends on nothing but std/serde. No tokio, no I/O.
- `app` depends on `domain` and `ports` only. Adapters are injected.
- The core is headless: the same `app` runs inside Tauri and inside the CLI.
  The UI runs only inside the Tauri app; there is no browser mode.

### 2.2 Processes

```
┌──────────────── Theodore.app ─────────────────┐
│ WKWebView (ui) ◀── Tauri IPC ──▶ Rust core     │
│                                   │  │  │      │
└───────────────────────────────────┼──┼──┼──────┘
                                    │  │  └──▶ GitHub API (HTTPS)
                                    │  └─────▶ git CLI ──▶ ~/.theodore/{repos,wt}
                                    ▼
                    claude / codex / gemini CLI   (cwd = PR worktree)
                                    │
                                    └──▶ http://127.0.0.1:<port>/mcp  (review-tools)
```

### 2.3 Repository layout

```
crates/
  domain/        models, invariants, diff/interdiff, anchors
  contract/      command + sync types; specta generates ui/src/gen/*.ts
  app/           use cases, sync engine, outbox, scheduler
  ports/         port traits: HostAdapter, GitPort, AgentPort, Store, Secrets, Clock
  host-http/     shared HTTP client for host adapters: auth, ETag, limits, retry
  host-github/   GraphQL (cynic) + REST, notifications polling, ETag
  host-gitlab/   M7
  linear/        read-only GraphQL client for Linear (issue linking, §6.10)
  git/           clone, worktree, diff → hunks, blobs
  agents/        CLI runners, stream parsers, process lifecycle, cost
  grouping/      grouping pipeline: pre-pass, prompt, validation, repair, eval
  mcp/           review-tools server (rmcp), stdio bridge
  store/         SQLite schema, migrations, sync log
  secrets/       keychain
  account-workos/ Theodore account over WorkOS AuthKit (§6.12)
apps/
  desktop/       Tauri app
  cli/           `theodore …` headless commands (grouping eval, debugging)
ui/              React app + sync client
docs/
```

---

## 3. Sync model (UI ⇄ core)

### 3.1 Flow

```
UI                                   core                                   GitHub
──                                   ────                                   ──────
edit ─▶ ObjectPool (instant)
   └─▶ TransactionQueue ──invoke──▶ validate (domain) ─▶ SQLite + sync log N
                                       │
   ◀── event sync{actions, N} ─────────┤
                                       └─▶ outbox ──────────────────────▶ mutation
                                              ◀── ok: mark done
                                              ◀── fail: compensating action N+1 ─▶ UI
agent / poller ──────────────────────▶ same apply path ─▶ sync log ─▶ UI
```

- Every change — user, agent, GitHub poll — becomes a `SyncAction` with a
  monotonically increasing `syncId`. The UI has one model and one inbound stream.
- UI reload: bootstrap from core, then catch up from `lastSyncId`.
- Undo/redo: an undo queue that replays inverse changes as new transactions.

### 3.2 What goes through the sync log

| Synced (mutable, persisted) | Not synced |
| --- | --- |
| PR list rows, inbox state, snooze | Diff hunks and file contents — immutable per commit, fetched by query |
| Read state per hunk, Viewed per file | Agent token stream — ephemeral `agent:delta` events; only the final message is synced |
| Groups (and regroupings) | Skill run log lines — ephemeral, last N kept in memory |
| Threads, comments, drafts | Syntax highlighting — computed in UI |
| Findings, fixes, tasks | |
| Chat messages (final), skill run status, usage | |
| Settings | |

### 3.3 Sync design choices

| Choice | Why |
| --- | --- |
| No client-side database | core is local; bootstrap over IPC is fast |
| Tauri IPC and events as the only transport | one process boundary, no network stack between UI and core |
| Subscription per open PR | PR models load lazily, only when opened |
| Single window | no cross-tab locking needed |
| Sync log + apply path in Rust (`app` + `store`) on SQLite | core is the single source of truth |
| MobX models, object pool, transaction queue, undo queue in `ui/src/sync` | instant UI, per-object re-render |
| `resource(key, load)` for immutable data (hunks, file contents): dedup, loading/error, LRU | data keyed by sha never needs invalidation, so no query library |
| Navigation = tab state in the pool, no router | a desktop app has no address bar |

### 3.4 External truth

GitHub owns threads, comments, Viewed, reviews, checks. Core applies the local
change first, sends it through the outbox, and reconciles:
- success → nothing to do (or merge server ids into the model);
- user-visible failure → compensating action + toast;
- conflict on poll (someone else changed it) → GitHub wins, UI gets the delta.

Offline / 401 / rate limit → outbox pauses; reads come from SQLite. A host
refusal (validation, forbidden, not found, a malformed answer) fails the
write at once with the host's message (§6.7).

Offline, the app stays quiet: one probe watches for the host, nothing else
calls it.

```
online ──a read fails on the network──▶ offline banner
                                          │ PR / inbox / space / project polls: skipped
                                          │   (local branch reviews still poll)
                                          │ probe (changed_since) at +30 s
                                          ├─ fails ─▶ again at +60 s, +120 s, … ≤ 5 min
                                          └─ answers ─▶ online ─▶ skipped polls run now
```

| Offline thing | Behaviour |
| --- | --- |
| Account refresh | retried after 30 s, doubling up to 10 min; one warning, then debug |
| Account 4xx | signs out only on the provider's own JSON error (`invalid_grant`); a captive portal's or proxy's 4xx counts as a network failure |
| Codex model list | "not installed" is debug and stands for 10 min; a real failure warns once per process |
| Agent run | never blocked by host health (the agent's API may still answer). A connection error in the CLI's error or stderr ends it as `agent.offline` (info): "Couldn't reach Claude's servers — check your connection". A run gone quiet after one (stuck retrying) is stopped at the stall mark, not the 30-min cap |

---

## 4. Domain model

```
Repo 1─* Pr ─* PrRevision (head sha) ─* File ─* Hunk ─(group)─▶ Group
              │                               ▲
              │                               └─ HunkRead (per user, per hunk)
              ├─* Thread ─* Comment          Anchor = revision + path + side + line[..line]
              ├─* Draft ─────────────────────▶ Anchor
              ├─* Finding ─▶ Anchor, agents[], skill, severity, Fix?
              ├─* Task ─▶ Anchor?, Thread?, assignee (human | agent)
              ├─* SkillRun, ChatMessage, Usage
              └── Snooze, Baseline (last reviewed revision)
```

Key rules (all in `domain`, all unit-tested):

| Rule | Detail |
| --- | --- |
| Hunk in exactly one group | Grouping output is rejected unless it is a partition of the hunk ids |
| Viewed from reads | A file is Viewed only when every hunk of it is read, across groups |
| Push dismisses | A changed hunk loses its read mark; its file goes `DISMISSED` |
| Anchor is host-neutral | `rev`, `path`, `side` (old/new), `line`, `start?`; adapters map it to the host's position format (§6.3) |
| Anchors move | After a push, anchors are remapped through the old→new diff; a deleted line makes the anchor outdated |
| Findings merge | Same file + overlapping lines + similar title → one finding, `agents[]` grows |
| Task lifecycle | `todo → progress → review → done`; accept resolves the linked thread |
| Fix target | Own or local PR → commit + push; someone else's PR → suggestion draft |

Hunk identity across revisions: `(path, normalized content hash)`, with a
fallback match by overlapping new-side range. Needed for read state, groups and
anchors to survive pushes.

---

## 5. Grouping (core risk)

This is the part the product stands on, and the least proven. Build and
measure it first, before any UI work.

### 5.1 Pipeline

```
diff ─▶ hunks with ids
   ─▶ deterministic pre-pass
        • hidden: lockfiles, linguist-generated, vendored, snapshots
        • skim candidates: pure renames, formatting-only, import-only
   ─▶ agent call (PR title, body, commits, hunk list) ─▶ JSON
   ─▶ validate (partition, known ids, order, required fields)
        ├─ ok ─▶ groups
        └─ errors ─▶ one repair call with the error list
                       └─ still bad ─▶ leftovers go to group "Other"
```

Output schema per group: `id` (stable slug), `title`, `why`, `kind`
(`normal | skim | hidden`), `risk?`, `check[]`, `hunks[]`, `outliers[]`, plus a
global reading order.

Groups show as the agent writes them (streamed grouping, Claude Code only;
docs/eval/streamed-groups.md: first group in ~5.6 s instead of ~26 s on a
28k-line PR, same tokens):

```
answer text deltas ─▶ each group's JSON object closes ─▶ shown at once
part (chunk) lands ─▶ validated, repaired if needed ─▶ replaces its streamed groups
                      └─ a repair keeps the streamed groups up until it lands
all parts ─▶ merge pass orders them ─▶ final grouping
                      └─ a group the user opened or read keeps its place
```

- Every grouping written mid-run carries `in_progress`. A run that fails puts
  the grouping it replaced back; with none, its groups stay marked unfinished
  ("Grouping stopped before it finished") and the PR is grouped again on the
  next open.
- Group ids are unique across parts (`slug-2`) and the same from the streamed
  group to the final one. Coarsening runs per part, so a group shown once its
  part landed is never merged away by another part.
- Codex and Gemini answer whole: their runs show the grouping when it is done.
- Grouping in two phases (partition, then describe) was measured and rejected:
  first group ~10.4 s at ~2.7× the tokens.

### 5.2 Large PRs

- Hunk list sent as headers + a few lines each; the agent reads full code with
  its own read/grep tools in the PR worktree when it needs to.
- Above a size threshold: split by top-level directory, group each part, then a
  merge pass over group summaries.

### 5.3 Re-grouping after a push

Unchanged hunks keep their group. Only new and changed hunks are sent, together
with the existing groups, and the agent assigns them or proposes new groups.
Group ids stay stable.

### 5.4 Eval harness (`apps/cli`)

```
theodore group <pr-url> [--agent claude] [--json] [--save fixtures/]
theodore eval fixtures/            # replays saved PRs, prints a table
```

| Metric | Target for M1 exit |
| --- | --- |
| Valid partition on first call | ≥ 90 % |
| Valid after repair | 100 % (with "Other" fallback) |
| Latency, PR ≤ 50 files | ≤ 60 s |
| Cost per PR | recorded by the eval harness; the app shows no cost and has no budget (owner, 2026-10-22) |
| Human rating 1–5 on 10 real PRs | ≥ 4 average |

---

## 6. Code hosts

### 6.1 Crates

```
app ──▶ host::HostAdapter (traits, host-neutral models, capabilities)
            │
            ├─ host-github     GraphQL (cynic, typed against the schema) + REST
            ├─ host-gitlab     REST v4 (+ GraphQL where it is simpler)   — M7
            └─ host-bitbucket  REST 2.0                                 — M7.1
            │
            └─ host::http   one client for GitHub and GitLab:
                 auth injection · conditional GET (caller's ETag) · rate limiter ·
                 retry/backoff · timeouts (connect 10 s, request 60 s) ·
                 token redaction in logs
```

`host-bitbucket` owns a second, smaller HTTP client rather than sharing
`host::http`: that one only ever sends `Authorization: Bearer`, and
Bitbucket's app-password flow needs Basic auth instead (§6.8). It uses the
same timeouts.

The caller owns the ETag: `get_conditional(path, None)` is a plain GET and
always answers with a body (the poller passes the ETag it stored per PR;
opening a PR passes none). If an open still gets "not modified", it drops
the PR's ETag and fetches once more before giving up.

`reqwest` + `cynic`, no `octocrab`: we need full control over ETags, limits and
retries, and would use a fraction of it.

### 6.2 Adapter

Split by area, not one monolith:

```
HostAdapter
  ├─ capabilities() ─▶ { viewed, pending_review, suggestions, merge_queue, auto_merge, … }
  ├─ inbox      changed_since(cursor) · search(section)
  ├─ pulls      get(ref, etag) · files · commits · checks
  ├─ reviews    submit(ReviewDraft) · set_viewed
  ├─ threads    reply · resolve · edit · delete · react
  └─ merge      merge · enqueue · auto_merge · update_branch
```

- A missing feature returns `Unsupported`; the UI hides it up front from
  `capabilities()`.
- The interface is validated against GitLab before it is frozen (M7), not
  derived from GitHub alone.

### 6.3 Host-neutral models

The adapter translates both ways; the domain never sees host formats.

| Domain | GitHub | GitLab | Bitbucket |
| --- | --- | --- | --- |
| `Anchor{rev, path, side, line, start?}` | `commit_id, path, side, line, start_line, start_side` | `position{base_sha, start_sha, head_sha, old/new_path, old/new_line}` | `inline{path, from, to}` |
| `PrRef` | node id + `owner/repo#n` | project id + iid | workspace/repo + id |

Host ids are opaque strings inside the domain.

### 6.4 Capabilities by host

GitLab and Bitbucket are behind Settings → Labs (`labs.gitlab`,
`labs.bitbucket`), off by default; GitHub is always on.

| Feature | GitHub | GitLab | Bitbucket Cloud |
| --- | --- | --- | --- |
| Viewed per file on the server | `markFileAsViewed` | no (kept locally) | no (kept locally) |
| Batched review (drafts → submit) | pending review | draft notes + bulk publish | no — comments post immediately, approve/request-changes is a separate call |
| Resolve thread | yes | yes (discussions) | yes |
| Suggestions | yes | yes | no |
| Merge queue | yes | merge trains | no |
| Checks | checks + statuses | pipelines | build statuses |

Where the host has no server-side Viewed, it is stored locally and does not
sync with the host's web UI.

### 6.5 GitHub operations

| Area | Calls |
| --- | --- |
| Read | GraphQL `pullRequest{files{viewerViewedState} reviewThreads reviewRequests commits statusCheckRollup}`, `search` for inbox sections |
| Viewed | `markFileAsViewed`, `unmarkFileAsViewed` |
| Review | one `addPullRequestReview` with all `threads` inline → `submitPullRequestReview` |
| Threads | `addPullRequestReviewThreadReply`, `resolveReviewThread`, `unresolveReviewThread`; REST edit/delete/react |
| Suggestions | `createCommitOnBranch` («Apply N suggestions») |
| People | `requestReviews`, REST remove requested reviewer |
| Merge | `mergePullRequest`, `enqueuePullRequest`, `enable/disablePullRequestAutoMerge`, `updatePullRequestBranch`, `markPullRequestReadyForReview` |
| Checks | REST rerequest check run, commit status |

Sending all threads in one mutation avoids N calls and the secondary limit on
content creation.

### 6.6 Change detection (no webhooks)

```
GitHub:    GET /notifications  (If-Modified-Since, honours X-Poll-Interval)
              └─ something changed ─▶ GraphQL for the changed PRs only
           open PR: GET /pulls/{n} with ETag every 10–30 s (304s are free)
GitLab:    ?updated_after=<cursor>
Bitbucket: re-lists the viewer's involved PRs every poll — no indexed
           "updated after" filter cheap enough to use as a real delta probe
```

GraphQL is POST, so it has no conditional requests: polling the inbox through
GraphQL `search` is expensive, notifications are nearly free.

The notifications API needs a classic token (`gh`, OAuth app, classic PAT) with
`notifications` or `repo` scope; fine-grained PATs and GitHub App tokens don't
get it. For those, fall back to GraphQL `search` on a slow interval.

### 6.7 Outbox, idempotency, errors

- Every host write goes through the outbox as resumable steps. Where the host
  has no batch (Bitbucket), comments are posted one by one and the outbox
  resumes from the last confirmed step.
- No idempotency keys on GitHub: each comment body carries
  `<!-- theodore:<draft-id> -->` (invisible when rendered). After a timeout the
  outbox looks for it before retrying, so nothing is posted twice.
- One error type for all adapters:
  `Auth · Forbidden · RateLimited{reset_at} · NotFound · Conflict · Validation · Network · Unsupported · Protocol`.
  The outbox decides from it: wait, retry, or roll back with a compensating action.

  | Error | Outbox |
  | --- | --- |
  | `Network` (incl. 5xx after host-http's own retries, timeouts) | retry: 2 s doubling to 5 min for 12 attempts (~30 min), then every 3 h |
  | `RateLimited` | retry at `reset_at`, rate-limit banner |
  | ↳ both | give up after 20 attempts (~a day) |
  | `Auth` (token rejected) | pause 30 s, token banner, until a reconnect |
  | `Validation` · `Forbidden` · `NotFound` · `Conflict` · `Protocol` · `Unsupported` | fail now |

  Failing = compensating action, one error toast with the host's message,
  item `failed` with `last_error`. A failed item is not pending, so it no
  longer holds off "Restart to update".
- GraphQL: a mutation answered with `errors` fails with GitHub's message
  even when `data` came back too (`{addPullRequestReview: null}` +
  `UNPROCESSABLE` "Can not approve your own pull request"):
  `UNPROCESSABLE` → `Validation`, `FORBIDDEN` → `Forbidden`, `NOT_FOUND` →
  `NotFound`. A query keeps partial `data` and logs the errors with their
  op and path.

### 6.8 Auth (no server)

| Host | Method |
| --- | --- |
| GitHub | `gh auth token` if present → else OAuth device flow (client id only) → else PAT |
| GitLab | OAuth + PKCE or PAT |
| Bitbucket Cloud | app password (Basic, `username:app_password`) or an access token (Bearer) |

Tokens live in the keychain. Never in `.git/config`, never in process arguments,
never in the webview.

#### GitHub device-flow client id

The "Sign in with GitHub" button (Onboarding and Settings → Account) runs
GitHub's OAuth device flow — no pasted token, no `gh` required — but it needs
an OAuth app's client id. The owner's GitHub OAuth App (device flow enabled,
no client secret needed) has its client id in the repo variable
`THEODORE_GITHUB_CLIENT_ID`, compiled into every CI build. Checked in this
order:

1. The `THEODORE_GITHUB_CLIENT_ID` environment variable at run time.
2. The `host.github.client_id` setting (a `transact` on the `setting`
   model, scope `user`).
3. The client id compiled in (`option_env!`, from the build's environment).

With none set (a local dev build), `host_status`'s `device_login_available` is `false`, the
button is hidden, and sign-in falls back to `gh`'s own login or a pasted
token — nothing else about auth changes.

### 6.9 Tests

- One contract test suite, run against every adapter on recorded HTTP fixtures
  (wiremock).
- Opt-in live smoke test against a sandbox repo.

### 6.10 Linear (read-only)

Behind Settings → Labs (`labs.linear`), off by default.

A separate integration, not a `HostAdapter`: Linear isn't a code host, and its
personal API keys authenticate with a bare `Authorization: <key>` header (no
`Bearer` prefix) — the opposite of every host adapter's convention — so the
`linear` crate talks to `reqwest` directly instead of building on `host::http`.

```
app ──▶ linear::LinearClient ──▶ https://api.linear.app/graphql
            │
            ├─ viewer()          validates a token, returns the user + workspace
            └─ issue(identifier) one issue by its key ("ABC-123")
```

- **Linking.** PR titles, head branch names and bodies are scanned for issue
  keys (`[A-Z][A-Z0-9]+-\d+`); every match is looked up and returned by
  `linear_issues`. With no token stored this returns an empty list, never an
  error — the feature is additive, not a gate.
- **Caching.** Results are kept in memory per PR for about five minutes:
  Peek and the Review header both ask on every render, and Linear's API has
  no conditional-GET story the way GitHub's ETags do.
- **Token.** `set_linear_token` validates the key against `viewer` before
  storing it (mirrors `set_host_token`); `linear_status` reports
  connected/user/workspace for Settings → Integrations.
- **Scope.** Read-only. Creating or updating a Linear issue from Theodore is
  future work, not covered here.

### 6.11 Bitbucket

`PrRef.repo.owner`/`.name` are the workspace and repo slug directly — unlike
GitLab's nested namespaces, Bitbucket slugs are always one path segment, so
there is no project-id lookup to cache.

Bitbucket's `/pullrequests/{id}` endpoint returns a flat list with no
"discussion" object; a thread is reconstructed by walking each comment's
`parent` id up to its root, and the root's `resolution` field (present once a
thread is resolved) carries the thread's resolved state. Comments anchor to a
diff with `inline{path, to, from}` — one line per comment, so a multi-line
`Anchor` collapses to its end point on write and never round-trips back as a
range.

`merge_strategy` is one of `merge_commit` / `squash` / `fast_forward`;
`MergeMethod::Rebase` maps to `fast_forward`, the closest analogue to a true
rebase-and-merge Bitbucket has (it fails outright if the branches diverged,
rather than rebasing).

Capabilities: `resolve_threads` only. No server-side Viewed, no pending/draft
review, no suggestions, no merge queue, no auto-merge, no reactions — the UI
hides those from `capabilities()` the same way it does for any other host
gap. `merge_status`, reviewer management and draft-state toggling stay
`Unsupported` for now; `viewer`, `search`/`search_repo`, `get_pr`,
`submit_review`, the thread operations, `list_repos` and `merge` are real.

### 6.12 Theodore account

A second identity: who you are to Theodore itself, for the cloud features to
come (sync, teams, licensing, enterprise SSO). Wherever it's configured,
signing in is required to use the app (see "Sign-in required" below). It is
not a code host and never touches the host tokens of §6.8; signing in or out
of one leaves the other alone. Identity provider: WorkOS AuthKit, used as a
**public client** — no client secret or API key ships in the app, and there
is no Theodore server yet.

#### What a public client can do today

| Flow | Without a secret | Notes |
| --- | --- | --- |
| Hosted AuthKit page + PKCE, code exchange with `code_verifier` | yes | the secret is required only when no verifier is sent |
| Device authorization (RFC 8628) | yes | `POST /user_management/authorize/device` with `client_id` only |
| Refresh token grant | yes | `client_id` + `refresh_token`; the refresh token rotates and is single-use |
| JWKS (`/sso/jwks/<client_id>`), logout redirect | yes | public endpoints |
| Magic Auth API (send a code, then verify it) | **no** | needs the API key |
| Email + password grant | **no** | needs the API key |

So "type your email in the app, get a code, type the code in the app" can't
be done client-only. The code is emailed and entered on the hosted AuthKit
page, and that page then hands the sign-in back to the app.

#### Chosen flow

```
Settings → Account / Onboarding
  email ──▶ account_sign_in_start{browser, email}
              core: PKCE S256 + state; bind 127.0.0.1:<port> (loopback only)
              UI:   open /user_management/authorize?…&login_hint=<email>
                         │
  browser: AuthKit ─▶ "we emailed you a code" ─▶ code ─▶ 302
                         │
         ┌───────────────┴────────────────┐
  app bundle                         dev run / bare binary
  theodore://callback?code&state     127.0.0.1:<port>/callback?code&state
  macOS "Open Theodore?" ─▶ app      the running app's listener
         └───────────────┬────────────────┘
                 state matches? ──no──▶ ignored, keep waiting
                         │ yes
  POST /user_management/authenticate  code + code_verifier + client_id
                         │
  verify JWT (JWKS, RS256, iss, exp, sub) ─▶ keychain ─▶ event account_status
  loopback tab: "You're signed in"          UI: card flips, window comes forward
```

- **Primary: the browser flow above.** It's the shortest path to "email → code
  → in" client-only. With Magic Auth as the only method enabled, the email the
  user typed (sent as `login_hint`) takes them straight to the code screen.
  The other methods enabled in the dashboard (Google, SSO, passkeys) show up
  on the same page through "Other sign-in options".
- **Secondary: device code ("Sign in on another device").** The app shows a
  code and a link, and the user signs in on any browser or device. This is the
  "sign in on the web, the app picks it up" path. It needs no redirect at
  all, so it still works when the loopback port is blocked.
- **Redirect: the app's URL scheme in a bundle, loopback elsewhere.**

  | Where the app runs | Redirect | How it gets back |
  | --- | --- | --- |
  | Installed production app (`…/Theodore.app/Contents/MacOS/`) | `theodore://callback` | the browser hands it to macOS ("Open Theodore?"), Theodore comes to the front |
  | Installed staging app (`Theodore Staging.app`) | `theodore-staging://callback` | the same, to the staging app only |
  | Installed nightly app (`Theodore Nightly.app`) | `theodore-nightly://callback` | the same, to the nightly app only |
  | `cargo tauri dev`, a bare `target/release/theodore-app`, any dev build | `http://127.0.0.1:47823/callback` | the running app's loopback listener; the tab shows "You're signed in" |
  | A redirect is configured (env or setting) | that one, always | loopback if `http`, else the app scheme |

  Each build has its own scheme, so a redirect never reaches another
  build installed beside it (docs/releasing.md "One identity per build";
  `crates/runtime/src/identity.rs`).

  - The desktop shell decides: it tells the core
    (`set_account_app_redirect`) when its executable sits inside an app
    bundle, the only place macOS routes the scheme to. The scheme is
    registered in the bundle's `Info.plist` from `tauri.conf.json`
    (`plugins.deep-link`), with `tauri.<env>.conf.json` merged in for
    staging and nightly.
  - With the scheme, no port is bound. The deep-link plugin hands
    `theodore[-<env>]://callback?…` to the running app, which passes it to
    `Core::account_sign_in_callback`: same `state` check and code exchange
    as the loopback, same outcome for `account_sign_in_poll`, so the UI has
    one flow. A URL that arrives with no browser sign-in waiting (e.g. the
    link launched the app) is ignored.
  - The loopback stays because a URL scheme only resolves for an installed
    bundle, and AuthKit allows `http://127.0.0.1` redirects in production
    for native clients.
  - Other `theodore://` hosts stay free for "open PR in Theodore" (§11).
- **CSRF.** Each attempt has a fresh 256-bit `state`. A redirect with a
  missing or different `state` has no effect and the sign-in keeps waiting
  for the real one (the loopback tab gets an "out of date" page). The
  listener binds only `127.0.0.1` and answers only `GET <path>`. A sign-in
  stops after 10 minutes, on cancel, or once it completes. The code and
  tokens are never logged.
- **Session.**
  - The session lives in one keychain entry, `theodore.account`: a JSON object
    with the access token, refresh token, `exp`, `sid` and profile.
  - A refresher wakes 60 s before `exp`, which defaults to 5 minutes, and
    rotates the tokens. On a network error it retries every ~30 s.
  - A rejected refresh means the session ended (revoked, or the maximum
    session length was reached): Theodore signs out and emits `account_status`.
  - Sign-out deletes the keychain entry, then calls
    `/user_management/sessions/logout?session_id=<sid>` in the background,
    best effort.
- **Validation.**
  - The access token's signature is checked against the JWKS. The JWKS is
    cached and refetched when the token names an unknown `kid`.
  - Claims checked: `iss` (`https://api.workos.com`, or a custom auth domain),
    `exp` with 60 s leeway, `client_id` when present, and `sub` against the
    returned user.
  - If the JWKS can't be reached at all, the claims are still checked. The
    token came straight from the provider over TLS, so the signature is
    defense in depth there.

#### Sign-in required

Wherever the account is configured (a client id from the env, the setting or
the build), nothing runs until someone is signed in. The UI shows only the
sign-in, and the core enforces it too.

| Where | Gate |
| --- | --- |
| Fixture and sandbox modes, the CLI (`LocalAccount`) | none: exempt |
| A build with no client id anywhere (a plain `cargo run`, `cargo tauri dev`) | none: no account, the app works as before |
| Configured, signed in (a stored session) | open |
| Configured, signed out | closed |

```
closed ─▶ UI     the window is the sign-in screen: logo + sign-in stages,
                 centred; no sidebar, tabs, shortcuts, palette or menu
                 actions. First launch: Onboarding's first step, no Skip;
                 GitHub, agents and projects follow once signed in.
          shell  every command off the allow-list answers
                 {code: "account_required"} (desktop gate.rs)
          core   the outbox, the poller, the start-up inbox refresh and
                 clones hold (account_hold); nothing reaches a host
sign in ─▶ open: the app comes back where it was; held work starts
```

- **Allowed while signed out**: `bootstrap`, `catch_up`, `app_info`,
  `check_update`, `install_update`, `account_status`,
  `account_sign_in_start` / `_poll` / `_cancel`, `account_sign_out`,
  `focus_window`. Every other command is gated, new ones included (deny by
  default; the list and its test live in `apps/desktop/src/gate.rs`).
- **Offline.** A stored session keeps the app usable with no network: a
  refresh that fails on the network, a timeout, a 5xx or a 4xx without the
  provider's JSON error keeps the session, and the refresher retries after
  30 s, doubling up to 10 min. Only a definitive rejection (the provider's
  error: the refresh token revoked or no longer valid) signs out. The gate then closes
  and says "Your session ended — sign in again".
- **Signing out** (Settings → Account) closes the gate too. Only the
  session goes: the store, projects, drafts and worktrees stay for the next
  sign-in, and nothing is deleted.
- **Dev builds.** With no client id, there's no gate: `cargo run` and
  `cargo tauri dev` work as before. Setting `THEODORE_WORKOS_CLIENT_ID`
  (env) or `account.workos.client_id` (setting) turns the gate on in a dev
  run too.

#### Where it lives

```
ui  SignInGate (whole window, signed out) · AccountStep (Onboarding, first)
    AccountCard (Settings → Account, top) · accountGate.ts (routing, pure)
     └─ useAccountSignIn ─▶ reduceFlow (pure, vitest)
commands  account_status · account_sign_in_start · account_sign_in_poll ·
          account_sign_in_cancel · account_sign_out · focus_window (shell)
desktop  gate.rs             allow-list; every other command asks the gate
app  usecases/account.rs   config, gate, state check, keychain, refresher, events
ports  AccountPort          LocalAccount = no provider (fixtures, sandbox, CLI)
account-workos  WorkosAccount   PKCE, loopback listener, device code, refresh, JWKS
desktop  deep_link.rs       the build's scheme handler, bundle check ─▶ app redirect
```

The browser method's outcome is held in core memory. `account_sign_in_poll`
reads it, polled once a second, with no network call. For the device
method, each poll is one token request at the server's interval, and
`slow_down` adds 5 s. Every change of state is also pushed as the
`account_status` ephemeral event.

#### Configuration

Checked in this order; with no client id the whole feature is hidden (no
card, no onboarding step, no sign-in gate). Fixture and sandbox modes always
hide it.

| What | Env | Setting (scope `user`) | Default |
| --- | --- | --- | --- |
| Client id | `THEODORE_WORKOS_CLIENT_ID` | `account.workos.client_id` | the one compiled in, else none: feature hidden |
| Redirect URI | `THEODORE_WORKOS_REDIRECT_URI` | `account.workos.redirect_uri` | the build's own in the app bundle (`theodore://callback`, `theodore-staging://…`, `theodore-nightly://…`), else `http://127.0.0.1:47823/callback` |
| API base | `THEODORE_WORKOS_API_BASE` | — | `https://api.workos.com` |
| Extra accepted `iss` | `THEODORE_WORKOS_ISSUER` | — | none (set it with a custom auth domain) |

Accepted issuers: the API base itself, or `<API base>/user_management/client_…`
with any client id. An app signing in through one of the environment's
Applications gets tokens whose `iss` names the environment's client id, not
the app's; the signature, checked with the keys fetched for the app's client
id, is what ties the token to the environment. A `client_id` claim, when
present, must be the app's or the one the issuer names.

A configured redirect always wins. An `http` one must be on `127.0.0.1`,
`[::1]` or `localhost`, with an explicit port; any other scheme is treated as
the app's URL scheme, so only the build's own `theodore[-<env>]://callback`
makes sense there. Port
`0` binds any free port. Use it only with a
wildcard-port redirect registered (`http://127.0.0.1:*/callback`); WorkOS
documents wildcard ports for loopback, but not yet for `127.0.0.1` in
production.

#### WorkOS dashboard setup (owner)

1. **Create the application.** Create or pick the environment (staging first,
   then production). Under Applications, use the AuthKit application and copy
   its **Client ID** (`client_…`). The API key is never needed by the app.
2. **Redirects** — register the app's and the loopback:
   - production: `theodore://callback`, used by the installed app;
   - staging: `theodore-staging://callback` and
     `theodore-nightly://callback`, used by the staging and nightly apps
     (docs/releasing.md "WorkOS redirects to register");
   - both: `http://127.0.0.1:47823/callback`, used by `cargo tauri dev`, a
     bare binary and any dev build;
   - optional: also add `http://127.0.0.1:*/callback`, if you later want
     port `0`;
   - leave the default redirect as-is: a wildcard can't be the default.

   A sign-in whose redirect isn't registered fails on the AuthKit page with
   an invalid redirect error.
3. **Authentication methods**:
   - enable **Magic Auth** (email code); it gives the "email → code → in"
     experience;
   - optional: Google / Microsoft OAuth, passkeys, SSO for enterprise
     connections;
   - email + password is fine too, but can't be entered in-app until the
     server exists.
4. **CLI Auth / device authorization.** Enable it if the environment shows a
   toggle; it backs "Sign in on another device". Nothing else needs
   configuring, since the code page is on the AuthKit domain.
5. **Sessions.** Keep the access token duration at 5 min. Pick the maximum
   session length and the inactivity timeout: they decide how long Theodore
   stays signed in without a fresh sign-in.
6. **Sign-out redirect.** Set any page, e.g. the marketing site. The
   background logout call never follows it.
7. **CORS / allowed origins.** Not needed: every call comes from the Rust
   core, never from the webview.
8. **Configure the app.** Build it with `THEODORE_WORKOS_CLIENT_ID=client_…`
   set: the id is compiled in (`option_env!` in `theodore-runtime`), the
   lowest-priority source. The release workflow takes it from the
   repository variable of the same name, and `scripts/nightly-build.sh`
   from its environment (docs/releasing.md). At run time the env var, then
   the `account.workos.client_id` setting, still win over it: that's how a
   build points at staging. No client id is ever committed.

#### Needs the future server

- In-app email + code entry, with no browser at all. The server calls the
  Magic Auth API with the secret: it sends the code, then verifies the code
  the app forwards. Email + password works the same way. The port doesn't
  change: a server-backed adapter adds `send_code` / `verify_code` next to
  `start_browser`, and the UI's email field already exists.
- Anything that needs the API key: user and org management, revoking
  sessions by user, webhooks for "signed out elsewhere".
- Licensing and entitlements: the access token's `entitlements` /
  `feature_flags` claims are read by the server, not trusted from the client.

---

## 7. Git

```
~/.theodore/
  repos/<host>/<owner>/<repo>.git      bare clone
  wt/<host>-<owner>-<repo>-<n>/        worktree per PR at pull/<n>/head
  theodore.db                          SQLite
```

- All via `git` CLI under `tokio::process`, with `GIT_TERMINAL_PROMPT=0` and a
  credential helper that reads the token from core (`git -c credential.helper=…`).
- `diff --no-color --find-renames -U<n> base...head` parsed into files and hunks;
  binary, LFS, submodule and generated files flagged, not dropped.
- Size limits per file and per PR; oversize files collapsed with a reason.
- Never: hard reset of user branches, deleting unmerged branches, writing tokens
  to disk.
- Worktree GC: drop worktrees of closed PRs after N days; locked while an agent
  runs in it.
- **One writer at a time.** A process-wide async lock per bare repo (fetch,
  clone, `update-ref`, `worktree add/remove/prune`) and per worktree
  (checkout, reset, clean), keyed by canonical path; a worktree's lock is
  always taken before its repo's. Without it, the poller's push handler and
  the UI's `get_revision` fetching into one bare repo lose with `cannot lock
  ref … is at NEW but expected OLD`, and two checkouts into one worktree
  with `index.lock: File exists`.

  ```text
  poller  fetch ─┐                 ┌─▶ fetch ─▶ ok
  UI      fetch ─┴─▶ lock(repo) ───┴─▶ waits ─▶ fetch ─▶ ok (or reuses one that started after it asked)
  ```

- **One fetch per open.** `fetch_pr` right after `ensure_repo` fetches only
  the PR ref; on its own (the poller after a push) it fetches the branches and
  the PR ref in one `git fetch`, so a moved base is there for the diff too.
- **The PR worktree is the head.** Agents, grouping, skills and the repo
  config read it as the PR's current head; an older revision (the review's
  "since last visit" baseline) is diffed in the bare repo and never checks
  out there.
- **Offline.** A fetch git fails with no network ("Could not resolve host",
  "Failed to connect", "Connection timed out", "Operation timed out",
  "Network is unreachable", "unable to access" without an HTTP status) is
  `GitError::Network` (`git.network`, UI code `offline`), never raw stderr.
  A host project works from the clone already on this Mac:

  | Clone on disk | Network | Result |
  | --- | --- | --- |
  | yes | yes | fetched, then used |
  | yes | no (or the host already known offline: no fetch tried) | used as is, logged "You're offline — using the copy on this Mac"; a refresh keeps `last_fetched_at` |
  | no | no | `app.offline`: "You're offline and this project hasn't been downloaded to this Mac yet" |

  So spaces (create, send, undo, archive, review), branches and the
  project refresh keep working offline. A project added offline stays
  `cloning` with that message in `error` and retries the clone with backoff
  (15 s doubling to 5 min) — never the red `error` state.

---

## 8. Agents

### 8.1 Runners

| Agent | Invocation | Stream |
| --- | --- | --- |
| Claude Code | `claude -p --output-format stream-json --verbose --mcp-config <json>` | JSON lines |
| Codex | `codex exec --json` (+ MCP config) | JSON lines |
| Gemini CLI | `gemini -p --output-format stream-json` (+ MCP settings) | JSON lines |

Exact flags and MCP-over-HTTP support per CLI are verified in M1/M4. If a CLI
only speaks stdio MCP, it gets `theodore mcp-bridge --url … --token …` — our own
binary as a stdio→HTTP proxy.

Each runner normalizes its stream into one event type:
`Text · Thinking · ToolCall · ToolResult · Usage · Done · Error`.

Model and effort (§8.5 `agents.<id>.model` / `.effort`) per CLI:

| Agent | Model | Effort | Picker list |
| --- | --- | --- | --- |
| Claude Code | `--model <id>` | `--effort low…max`; `ultracode` = `max`, and in a space the turn's prompt opens with `ultracode` (Claude Code's multi-agent mode) | Claude Code's own catalog cache (`~/.claude/cache/model-catalog/*-cc.json`: every version, older ones under "Earlier versions", each with its levels); else the aliases `fable`, `opus`, `sonnet`, `haiku` |
| Codex | `-m <slug>` | `-c model_reasoning_effort="<level>"` | `codex debug models`, `visibility: list`, by `priority` |
| Gemini CLI | `-m <name>` | — | none |

### 8.2 Lifecycle

```
spawn (cwd = worktree) ─▶ stream ─▶ Done
   │                                      │
   ├─ cancel ─▶ SIGTERM ─▶ 5 s ─▶ SIGKILL │
   ├─ silent 90 s ─▶ stalled (UI: restart / switch agent)
   └─ exit ≠ 0 ─▶ failed with stderr tail
```

- Credentials: each CLI uses its own login. Core does not read or copy them.
- Reading code: agents use their own read/grep tools in the worktree; MCP adds
  only what the worktree cannot give (PR context, groups, base files, findings).
- Env: Theodore's own secrets are removed from the child env; the agent's own
  auth variables (e.g. `ANTHROPIC_API_KEY`) are passed through.
- Grouping, regroup and merge run on Claude's `sonnet` (Codex/Gemini keep their model setting) and always at `--effort low` (Codex `model_reasoning_effort="low"`), whatever `agents.<id>.effort` says: at high or max Claude explores the repo with Grep/Read instead of grouping the diff it was given (kdb#83, 28k lines: 65 s at low, 75 s at the CLI default, 11+ min at max). Eval at low, 13 fixtures + kdb#83: Sonnet 13/13 valid, 146 s, kdb 36 s; Opus 13/13, 141 s, 55 s; Haiku 11/13 with 2 repairs, 293 s, 92 s and twice the groups.
- Permissions: review runs are read-only: reading, skills, subagents, todos, the web and MCP, as in Claude Code itself, but no writes and no commands (`Bash`, `Edit`, `Write` denied), so nothing reaches the PR without you.
  Fix runs may edit, only inside the PR worktree.
- The user's own MCP servers (`agents.user_mcp`, on by default, issue #16),
  in **spaces only** — review runs (grouping, chat, skills, fixes) never
  get them: Claude space turns load the user's Claude Code servers (`~/.claude.json`, the
  repo's `.mcp.json`, claude.ai connectors) next to Theodore's, and their
  tools (`mcp__*`) are pre-approved; the read-only denylist still applies.
  Off: `--strict-mcp-config`, only Theodore's own server. Codex and Gemini
  have no such flag and ignore the setting: Codex's `-c` overrides add to
  `~/.codex/config.toml`, so its servers load either way; Gemini's
  settings-file merge is unverified.
- Pinned minimum CLI versions; stream parsers covered by recorded fixtures.

### 8.3 MCP `review-tools`

| Tool | Returns / does |
| --- | --- |
| `get_pr_context()` | title, body, commits, files, groups |
| `get_group(id)` | group with its hunks |
| `get_base_file(path)` | file at the base revision (the worktree has only head) |
| `add_finding(file, line, severity, title, body)` | creates a finding → sync log → UI |
| `propose_patch(file, before, after, note)` | attaches a fix to a finding |

### 8.4 Skills, chat

- Skills: `SKILL.md` from `~/.claude/skills` and the repo's `.claude/skills`.
  Claude loads them natively; other agents get the text in the prompt.
- A skill can run on several agents in parallel; findings merge (§4).
- Chat: questions about the PR, a group, or a line; replies stream as ephemeral
  events and are stored when complete.
- Attachments: a question can carry files (images, PDFs, anything), same as
  a space's chat (docs/spaces.md "Attachments"). The stored question keeps
  them; the agent gets their paths listed after the question.
- Cost: the app neither shows nor limits what agent runs cost (budgets and
  the spend display were dropped by the owner, 2026-10-22). The eval
  harness (`theodore eval`) still reports cost per grouping run.

### 8.5 Agent selection

Which agent does what is a setting, layered user → repo:

| Setting | Default | Notes |
| --- | --- | --- |
| `agents.grouping` | `claude` | one agent |
| `agents.chat` | `claude` | switchable per message in the UI |
| `agents.skills` | `[claude]` | a list: skills can run on several agents at once |
| `agents.fix` | `claude` | one agent; edits only in the PR worktree |
| `agents.<id>.enabled`, `.path`, `.model`, `.effort` | detected on onboarding; model and effort unset = the CLI's own | CLI path, model and effort override. Effort: Claude `--effort`, Codex `-c model_reasoning_effort="…"`, Gemini none. The picker shows no "default": an agent without a model or effort set gets, and keeps, the one shown. Its models (`agent_models`): Claude Code's catalog cache (else its aliases), Codex's from `codex debug models` (listed ones, by priority; read once per run of the app), none for Gemini |
| `agents.user_mcp` | `true` | user scope only: Claude runs may use the user's own MCP servers (§8.2) |

`theodore group --agent <id>` overrides the setting for evals. M1 ships the
Claude runner; the others follow in M4 behind the same runner trait.

---

## 9. Storage and secrets

- SQLite via `rusqlite` (WAL), migrations in `store`, applied on start; one DB
  per user profile.
- Tables: models from §4 + `sync_log(id, model, model_id, action, data)` +
  `outbox(id, op, payload, attempts, status, last_error)`.
- Keychain: one item for all Theodore secrets (one prompt). On macOS through
  `/usr/bin/security` so the prompt survives rebuilds; secret passed on stdin,
  not as an argument.

---

## 10. UI

| Part | Plan |
| --- | --- |
| Screens | Inbox + peek, My PRs, All PRs (list ↔ board), Tasks, Review (groups │ diff │ AI/tasks/threads), Settings, Onboarding |
| Look | HeroUI, own colour tokens, Shiki highlighting, Geist fonts, Gravity UI icons |
| Types | generated from the Rust contract, never hand-written |
| State | MobX object pool fed by the sync stream; `resource()` for immutable data; no business logic in the UI |
| Actions | every action is a transaction or a command; nothing is faked with timers |
| Dev and e2e | the real Rust core with fixture adapters (§12.1); no fake core in TS |

Every component subscribes to the objects it reads, not to the whole state.

---

## 11. Shell, packaging, release

- Tauri 2, one main window, `titleBarStyle: Overlay`.
- Strict CSP (no remote scripts; Shiki and fonts bundled). Capabilities scoped
  to the commands the UI needs.
- Deep link `theodore://` (`theodore-staging://`, `theodore-nightly://`
  for those builds), registered in the bundle's `Info.plist`
  (`tauri-plugin-deep-link`). `theodore://callback` carries the account
  sign-in redirect (§6.12); other hosts are free for "open PR in Theodore".
- One identity per build: staging, nightly and dev builds have their own
  name, bundle id, data dir, keychain item and scheme, so they run beside
  production (docs/releasing.md "One identity per build").

### 11.1 Updates (from the first version)

The updater ships in M0, so every build ever released can update itself.

```
merge to main @ 0.3.0 (no v0.3.0 yet)
               ──▶ CI release workflow
                    build .app ─▶ sign ─▶ [notarize] ─▶ DMG ─▶ sign ─▶ [notarize]
                    .app.tar.gz + .sig (updater key)
                    latest.json {version, notes, url, signature}
                    └─▶ GitHub Release v0.3.0 on JeanIsahakyan/theodore (public)

app ──▶ on launch + every 6 h
        GET github.com/JeanIsahakyan/theodore/releases/latest/download/latest.json
        newer? ─▶ download in background ─▶ verify signature
               ─▶ "Restart to update" (never forced)
               ─▶ restart waits until: no agent running, no outbox item
                  pending (failed ones don't count)
```

| Topic | Rule |
| --- | --- |
| Mechanism | `tauri-plugin-updater` + `tauri-plugin-process`; manifest from GitHub Releases, no server |
| Signing key | generated once, private key + password in CI secrets, public key in `tauri.conf.json`. **Never rotated**: a new key orphans every installed build |
| Channels | `stable` = `/releases/latest/` (GitHub skips pre-releases there); `beta` = a fixed pre-release tag `channel-beta` whose `latest.json` is overwritten. Setting `updates.channel`. `staging` = pre-release `channel-staging`, rebuilt on every push to `staging`: the only channel of a staging build, never offered to any other |
| Versioning | semver; the tag must equal the version in `Cargo.toml` and `tauri.conf.json` (CI check) |
| Tags | never re-tag a published release; ship a new patch instead |
| Data | SQLite migrations forward-only; DB copied to `theodore.db.bak-<version>` before migrating; downgrade not supported |
| UX | Settings → About: version, channel, "Check now", release notes of the pending update |
| Failure | download or signature failure → keep the current version, retry next check, show nothing scary |
| Test | every release workflow run also tests the update path: install N-1, update to N, app starts |

Platform: `darwin-aarch64` only in v1; the manifest format already allows more
platforms later.

### 11.2 Signing without an Apple Developer ID (now)

| Step | Now (ad-hoc) | With Developer ID (later) |
| --- | --- | --- |
| Code signature | `codesign -s -` (ad-hoc, required on Apple Silicon) | Developer ID Application + hardened runtime |
| Notarization | none | `notarytool` + staple, app and DMG |
| First install | Gatekeeper blocks it: System Settings → Privacy & Security → "Open Anyway", or `xattr -dr com.apple.quarantine /Applications/Theodore.app` | opens normally |
| Updates | work: the updater downloads outside the browser, so no quarantine flag, and checks its own signature | same |
| Keychain prompt after update | none: secrets go through Apple-signed `/usr/bin/security`, not our changing signature | same |

- The release workflow has the Developer ID and notarization steps from day one,
  skipped while the secrets are absent. Adding the secrets turns them on; no
  other change.
- Switching to Developer ID later does not break the update chain: the updater
  trusts its own key, not the Apple identity.
- README gets an "Install" section with the first-launch steps until then.

---

## 12. Quality gates

| Part | Checks |
| --- | --- |
| Rust | `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test` |
| UI | `tsc --noEmit`, lint, Prettier, unit tests |
| E2E | the Tauri app on the real core with fixture adapters (§12.1) |
| Contract | generated TS is up to date (CI diff check) |
| Grouping | `theodore eval fixtures/` — no regression in validity |
| Agents | stream parser fixture tests per CLI version |

All wired into lefthook pre-push and CI.

### 12.1 Fixture adapters

Dev runs and e2e use the real `app` + `domain` with adapters swapped by config:

```
Tauri app ──▶ app + domain (real)
               ├─ host:   fixture — PRs, threads, checks from JSON
               ├─ git:    recorded repos in fixtures/
               ├─ agents: replay of recorded CLI streams
               └─ clock:  controlled time (snooze, polling, stalls)
```

Demo events (author push, checks green, agent crash) are fixture triggers, not
UI code. E2E drives the app through `tauri-driver` (WebDriver). It has no macOS
driver for WKWebView, so e2e runs on Linux CI.

---

## 13. Milestones

```
M0 scaffold ─▶ M1 grouping spike ─▶ M2 core + sync + UI ─▶ M3 GitHub ─▶ M4 agents
                                                               │
                         M7 GitLab ◀─ M6 ship ◀─ M5 re-review ◀┘
```

| M | Scope | Exit | Sessions |
| --- | --- | --- | --- |
| M0 | Cargo workspace, `ui/` package, Tauri hello window, contract codegen, checks in lefthook, updater + release workflow | `cargo test` + `tsc` green; window opens; v0.0.1 → v0.0.2 updates itself from GitHub Releases | 1–2 |
| M1 | `domain` diff model, `git` (clone, worktree, diff), minimal GitHub read (token from `gh`), minimal Claude runner (JSON in the final message, no MCP yet), grouping + validation, `theodore group/eval` | §5.4 targets on 10 real PRs | 1–2 |
| M2 | `store`, sync engine, `app` skeleton, Tauri wiring, UI with MobX object pool, fixture adapters | real PR opens read-only in the app, grouped | 2 |
| M3 | Inbox sections, Viewed sync, threads, drafts → submit, notifications + ETag polling, outbox, offline | full review round-trip on a real PR | 1–2 |
| M4 | MCP server, skill runs on several agents, findings merge, chat, fixes, tasks | skill → findings → draft/task/fix works on claude + codex | 1–2 |
| M5 | Push detection, interdiff, DISMISSED, incremental regroup, anchor remap, snooze by event | author push handled without losing reads or comments | 1 |
| M6 | Onboarding, layered settings UI, keychain, beta channel, big-PR performance | signed DMG installs and updates on a clean Mac | 1–2 |
| M7 | GitLab adapter; freeze `HostAdapter` | same review round-trip on GitLab | 1 |
| M7.1 | Bitbucket Cloud adapter (§6.10) | same review round-trip on Bitbucket | 1 |

Total: ~11–14 sessions to v1 on GitHub, +1 for GitLab, +1 for Bitbucket.

Later: code intelligence — symbols, references, LSP diagnostics (`docs/code-intel.md`), Linear integration, merge queue UI, org settings
via repo file, Windows/Linux, accounts and billing, uploading problem reports.

Spaces — the stage before a pull request, where an agent writes the change in
its own worktree and you review it by meaning before it becomes a PR — are
designed in `docs/spaces.md` and ship hidden behind `FEATURES.buildMode`.

---

## 14. Risks

| Risk | Mitigation |
| --- | --- |
| Grouping quality, latency or cost is not good enough | M1 is a gate: measure before building the rest |
| CLI output formats change | pinned minimum versions, fixture tests, one normalizer per CLI |
| A CLI lacks HTTP MCP | stdio bridge in our binary |
| GitHub rate limits with many PRs | notifications + ETag (304s are free), adaptive poll intervals, one-mutation reviews |
| Huge PRs (10k+ lines) freeze the UI | virtualized diff, size limits, lazy hunk loading |
| Anchors drift after force-push | remap via diff; fall back to "outdated" like GitHub |
| Agents touching files they shouldn't | read-only allowlist for review, edits only in the worktree |
| No WebDriver for WKWebView on macOS | e2e on Linux CI (WebKitGTK); manual smoke on macOS per release |

---

## 15. Open questions

None right now.
