# Spaces — the stage before a pull request

A **space** is a change while it is being made. You open one in a project, say
what you want, and an agent writes the code in an isolated worktree while you
watch the diff, review it by meaning, and send comments back. When it is good,
the space becomes a pull request — its turns squashed into one commit by
you on its own branch — and Theodore's existing review, inbox and merge flow
takes over. In a local folder it is merged into the checked-out branch
instead, or kept as a branch.

On for everyone; the `labs.build_mode` setting turns them off (Settings →
Spaces, §10).

---

## 1. One object, three stages

```
              DRAFTING                         IN REVIEW                    DONE
┌──────────────────────────────┐   ┌──────────────────────────────┐
│ New space ─▶ agent turns ──┐ │   │ PR #123: Changes · Threads · │
│     ▲        (checkpoints) │ │   │ Checks, [Merge] in the space │
│     └── review · comments ◀┘ │──▶│     ▲            │           │──▶ merged ─┬─▶ archived
└──────────────────────────────┘   │     └─ threads ◀─┘ → agent   │            │
     ▲   "Open PR" ▾ draft         └──────────────────────────────┘            │
     │   (host project)                                                        │
     └─── "Continue": branch <login>/<slug>-2 on the base's fresh tip ─────────┘
         "Merge locally"   ─────────────────────────────────────────▶ merged ──▶ archived
         ▾ "Keep as branch" ────────────────────────────────────────▶ branch ──▶ archived
         (local folder)
```

The PR is the space's continuation, not a copy: `space.pr` points at it, the
space page shows its state, reviewer threads become agent tasks in the same
space, and a push from the space updates the PR.

| Stage | Space shows | Who acts | Ends when |
| --- | --- | --- | --- |
| Drafting | chat · live diff vs base | you + the agent | Open PR / Merge locally / Keep as branch |
| In review | the same, plus the PR's Threads and Checks tabs and Merge | reviewers → you → the agent | merged (here or on the host) or closed |
| Done | transcript and final diff; Continue | — | Continue (drafting again, new branch) or archived (auto, `spaces.auto_archive_days` after merge, merge locally or keep) |

## 2. The core loop

```
you                         Theodore core                              agent CLI
───                         ─────────────                              ─────────
New space "fix-login"  ──▶  worktree ~/.theodore/spaces/<id> @ main
"make the redirect…"   ──▶  chat_message(me) · status=running ──spawn─▶ edits in cwd
   ◀── streaming text ────  Ephemeral AgentDelta ◀────────────────────── stream
                            turn ends: git add -A · commit "turn 1"
                            refs/theodore/spaces/<id> ─▶ checkpoint
   ◀── live diff ─────────  branch review row (base...checkpoint), no grouping
Review ▸               ──▶  same review, grouped by meaning (existing pipeline)
comment on a line      ──▶  draft ─▶ "Send to agent" ─▶ next turn ─▶ checkpoint 2
Open PR                ──▶  squash checkpoints ─▶ one commit by you
                            push it to <login>/fix-login ─▶ outbox create_space_pr
                            space.pr = github:o/r#123 ─▶ PR row, inbox, polling
reviewer comments      ──▶  thread ─▶ "Send to agent" ─▶ turn ─▶ Push ─▶ PR updated
not what you wanted    ──▶  "Undo" ─▶ worktree + ref back to the previous checkpoint
Merge                  ──▶  merge_space ─▶ outbox merge_pr (now · auto-merge · queue)
merged                 ──▶  space.done_at ─▶ archive after spaces.auto_archive_days
Continue               ──▶  fetch base ─▶ <login>/fix-login-2 on its tip ─▶ drafting again
```

---

## 3. Where spaces live in the UI

**Decision: a "Spaces" section in every workspace's sidebar, below its
sections (Overview · Inbox · My PRs · Tasks · Skills), plus a "Drafting"
group at the top of My PRs.** Two views over one object, not
a separate feature island.

- The sidebar is "what needs you now". Agents run in parallel and finish at
  unpredictable times; their state must be visible at a glance without
  opening anything — the same job the Inbox count does for review requests.
- My PRs is "all my changes, by stage": Drafting · In review · Merged. A space
  moves down that list as it moves through its stages.
- ⌘K "New space…" and the section's `+` both open the same dialog. ⌘K alone
  would hide running work.
- In "All projects" the section lists every project's spaces, prefixed with
  the project name; "New space" asks which project.
- A local-folder workspace has no Inbox or My PRs; its Spaces section is the
  only list, and Overview → Branches shows each space's branch.

### Sidebar

```
┌ ● migratro ▾ ──── ⌘K ┐
│ Overview             │
│ Inbox              2 │
│ My PRs             1 │
│ Tasks                │
│ Skills               │
│                      │
│ Spaces             + │
│ ◐ fix-login-redirect │  running
│ ● add-csv-export     │  turn done, not seen yet
│ ○ tidy-config        │  idle
│ ✕ bump-deps          │  failed
│ ⎇ #123 rate-limits ✓ │  in review: PR number + checks
└──────────────────────┘
```

| Glyph | Meaning | Source |
| --- | --- | --- |
| ◐ | agent running | `space.status = running` |
| ● | turn finished since you last looked | `space.updated_at` > last seen (UI-local) |
| ○ | idle | `idle` |
| ✕ | last turn failed or was interrupted | `failed` |
| ⎇ #n | has a PR; tinted by its checks (green · red · amber), the merge icon once merged | `space.pr` + its `pr` row (`checks`, `state`) |

◐, ✕ and ● win over ⎇; the label keeps `#n` either way.

**Pin / mark as unread** (finding a space again, GitHub #77): right-click
a row, or type "pin"/"unread" in ⌘K.

```
┌ Spaces             + │
│ ● fix-login-redirect │  pinned (small pin mark) — sorts first
│ ◐ add-csv-export     │  running
│ ● tidy-config        │  turn done, not seen yet
└──────────────────────┘
  right-click ▸ Pin/Unpin · Mark as unread
```

| | Field | Written by |
| --- | --- | --- |
| Pin | `space.pinned` (synced) | `set_space_pinned` only — never a client transaction, like the rest of `space` |
| Mark as unread | the UI-local seen mark (`seen.ts`) | forgets the space was seen, so it reads ● again and counts toward the dock badge, without waiting for a new turn |

Pinning never touches `updated_at`, so pinning alone doesn't make a space
read as a new turn. Pinned spaces sort first, then by activity, in both
the sidebar and ⌘K.

**⌘F searches the current list** (GitHub #77): the PR list on Inbox / My
PRs (title, number, author, repo, branch), or this section everywhere
else build mode is on (name, title, goal, branch). Fuzzy-ish substring
match; Esc clears the query, then closes; ↑↓ moves, ⏎ opens. Never on
`review` or `space` (a diff/text view) — the key is left for the
webview's own find there; `settings` has no sidebar to search either.

### New space

```
┌ New space ──────────────────────────────────────┐
│ Project    migratro                        ▾    │
│ What should change?                             │
│ ┌─────────────────────────────────────────────┐ │
│ │ Retry the export job on 429s with backoff   │ │
│ └─────────────────────────────────────────────┘ │
│ Name       retry-export-429   (from the goal)   │
│ Branch     ada/retry-export-429   from  main ▾  │
│ Agent      claude ▾   Mode  Edit ▾              │
│                              [Cancel] [Start ⏎] │
└─────────────────────────────────────────────────┘
```

"Start" creates the worktree and sends the goal as the first message — the
first turn starts immediately. Name and branch are derived from the goal and
editable.

| Goal | Name |
| --- | --- |
| Retry the export job on 429s | `retry-the-export-job-on` (first five words) |
| исправить логин | `ispravit-login` (Cyrillic transliterated) |
| Crème brûlée für Straße | `creme-brulee-fur-strasse` (accents dropped) |
| 修复登录 | `space` (other scripts only break words) |
| a second "исправить логин" in the project | `ispravit-login-2`, and so its branch |

That slug is only the instant placeholder. Without delaying turn 1, the
chat agent (`agents.chat`, its fastest, cheapest model — Claude's `haiku`)
proposes a better one from the goal, in the background:

```
create_space ─▶ slug name, turn 1 sent ──▶ page opens at once
                      │
                      └─▶ space_naming::apply (background)
                              chat agent, cheap model ─▶ "NAME: … / TITLE: …"
                              name/branch ─▶ applied, deduped          (unless you typed one, or it shipped)
                              title       ─▶ applied                   (always, once a valid answer lands)
                              anything else (offline, no CLI, a bad
                              answer) ─▶ nothing changes, silently
```

`Space::title` is what the sidebar, palette, tab and notifications show
(`spaceTitle`) instead of the goal's raw first line — the fix for chats
that otherwise read as a run-on sentence ("For LIN-145, I dont agree with
the proposed solution…", GitHub #78). Open PR / Merge locally's own title
field still defaults to the goal's first line (`defaultTitle`), on
purpose: that one is the commit message, not the chat's name.

#### Start from an issue, a branch, or a PR

A "Start from" chip next to the goal switches what the space starts on,
instead of a fresh `<login>/<slug>` forked from the goal
(`CreateSpaceRequest.start_from` / `SpaceStart`):

```
goal (default)  ──▶ fork <login>/<slug> off the project's default branch
issue            ──▶ get_issue; goal := title + body + a link; number kept
                     for ship_space's PR body ("Closes #<number>")
branch           ──▶ resolve the branch's tip; worktree starts there
pr               ──▶ get_pr; same as branch, plus Space.pr set right away
                     (opens "In review", not "Drafting")
```

| | branch / name | worktree starts at | shipping |
| --- | --- | --- | --- |
| goal | `<login>/<slug>`, new | `base_sha` | Open PR: pushes the new branch |
| issue | `<login>/<slug>`, new | `base_sha` | same, body says "Closes #n" |
| branch | the adopted branch, kept | its own tip (`fork_sha`) | pushes back to it, never a fresh branch |
| pr | the PR's branch, kept | its own tip (`fork_sha`) | Push (the PR already exists) |

An issue needs a host that can read its own issues
(`Capabilities::issues`; GitHub and GitLab, not Bitbucket) — the picker
lists the viewer's own open ones (`my_open_issues`), or a pasted URL /
`#<number>` resolves directly (`get_issue`). The goal box becomes an
optional note on top of what's picked or typed, rather than required.

### Space page — drafting

Chat left, changes right. The diff is the whole branch vs its base, the same
`DiffPane` the review uses, refreshed after every turn.

```
┌ ◐ fix-login-redirect ── ada/fix-login-redirect ← main ── Drafting ── $0.42 / $5.00 ┐
│ claude                             [Review] [Open PR ▾] [Archive]                │
├──────────── chat ──────────────────┬──────────── changes ────────────────────────┤
│ you  The login page should send    │ 3 files  +42 −7   vs main · after turn 2    │
│      people back where they were…  │ ▾ src/auth/redirect.ts               +30 −5  │
│                                    │   @@ -10,6 +10,12 @@                         │
│ claude  I added a `next` param…    │   + const next = safeNext(params.get(…      │
│   ▸ 6 tool calls          [Undo]   │ ▾ src/routes/login.tsx               +12 −2  │
│   ◆ turn 1 · a1b2c3 · 2 files      │   …                                         │
│                                    │                                             │
│ you  Also reject absolute URLs.    │                                             │
│ claude  ⠋ editing redirect.ts…     │                                             │
│                                    │                                             │
│ ┌────────────────────────────────┐ │                                             │
│ │ Ask the agent…  @file  /skill  │ │                                             │
│ └──────────────────── [Stop ⎋] ──┘ │                                             │
└────────────────────────────────────┴─────────────────────────────────────────────┘
```

`/skill` in a space's chat runs your Claude skills (`~/.claude/skills`, the
repo's `.claude/skills`) as that turn's instructions; review built-ins aren't
offered there. The skill's `SKILL.md` goes into the agent's prompt with the
rest of the message as the task (none → "apply it to the work in this
space"); the chat keeps the text as typed. Same id in both places → yours
runs. An unknown `/word` is sent as is.

**Attachments**: a message can carry files — images, PDFs, anything up to
25 MB each. `save_attachment` stores one under
`<data dir>/attachments/<id>/<name>` and returns its id; `space_send` (and
the AI panel's `ask`) take the ids. Your message keeps them
(`ChatMessage.attachments`, its text as typed); `attachment_data` gives an
image back as a `data:` URL for the transcript.

```
save_attachment ──▶ id ──▶ space_send { attachments: [id] }
                              ├─ your message: text as typed + attachments
                              └─ prompt: <text or skill expansion>
                                         Attached files (read them; …):
                                         - <abs path> (<mime>, <name>)
```

| Agent | Can open them by |
| --- | --- |
| Claude Code | `--add-dir <attachment dir>`, then its Read tool (images, PDFs) |
| Codex | images as `-i <path>`; other files by path (its sandbox reads anywhere) |
| Gemini | the path in the prompt only |

Later turns name an earlier message's files — `(attached: shot.png)` — but
don't send them again.

**Undo last turn**: the Undo button on the agent's last reply (and ⌘K "Undo
last turn") moves the worktree and the checkpoint ref back to where that
turn started. The turn stays in the chat, dimmed and marked *undone*, and the
next turns' prompts leave it out. Pressing it again walks back one more turn.
Refused while a turn runs, on an archived space, and once the turn shipped
(pushed, merged locally, or kept as a branch) — then ask the agent to revert
it instead.

```
turn 1 ─▶ ckpt A ─▶ turn 2 ─▶ ckpt B          Undo: worktree + ref → A
                                │              turn 2 kept, marked undone
                                └─ shipped? ── refused
```

**Update from base**: when the base branch has commits the space doesn't,
the header shows *Base moved: 3 commits behind · Update* (counted from the
copy on this Mac, re-read after each turn and each project fetch; no
network). Update merges the base into the space (§6): clean, the space is on
the new base at once; on conflicts a banner *Resolving conflicts with main:
a.ts, b.ts* replaces the button and the agent gets a turn to resolve them.

```
┌ ○ retry-429 ──────── [⎇ Base moved: 3 commits behind · Update] [Review] [Open PR ▾] ┐
                    after Update, on conflicts:
┌ ○ retry-429 ──────────────────────────────────────────── [Review] [Open PR ▾] ──────┐
│ ⚠ Resolving conflicts with main: src/retry.rs. Theodore commits    [Complete merge] │
│   the merge once no conflict markers are left.                     [Abort]          │
```

| Banner action | Does |
| --- | --- |
| (the agent's turn ends) | Theodore checks the files; none has markers left → it commits the merge |
| Complete merge | the same check now, for files fixed by hand; says which files still have markers |
| Abort | `merge --abort`, worktree and ref back to where the update started, old base |

While it resolves: Ship and Undo are refused ("finish or abort the update
from base first"), a plain message is a normal turn whose end tries the
merge again. Undo of the turn that completed the merge takes the merge back
too, base included; a clean update is not a turn, so Undo stops at it. The
PR's own "Update branch" on the host is a separate action and stays as is.

### Space page — in review

Once `space.pr` is set the header gains the PR and Merge, and the right
pane gains tabs: **Changes · Threads · Checks**. Everything a PR needs
happens here; "PR" still opens the normal review.

```
┌ ○ rate-limits ── In review · #123 ───────── [Review] [Push] [Merge ▾] [PR] ──────┐
├──────────── chat ──────────────────┬── Changes │ Threads 2 │ ⎇ Checks ───────────┤
│ …                                  │ ● anna · limiter.ts:41                      │
│ ◆ turn 4 · 9f8e7d · pushed         │   "Should this be per-token, not global?"   │
│ ◆ turn 5 · 1c2d3e · not pushed     │   [Reply…] [Send to agent] [Resolve]        │
│                                    │ ● ci-bot · README.md:12                     │
│                                    │   [Send to agent] …                         │
└────────────────────────────────────┴─────────────────────────────────────────────┘
```

| Tab | Shows | Reuses |
| --- | --- | --- |
| Changes | the live diff, as while drafting | `DiffPane` |
| Threads | the PR's threads and drafts: Reply · Resolve · Send to agent; unresolved count on the tab | the review side panel's `Threads` |
| Checks | review decision + approvals, mergeable state (`merge_info`), conflicting files (`conflicts`) with Update branch, then every check with Re-run (`rerun_check`) | `Checks`, `ReviewerStrip` |

**Merge** (`merge_space`, through `merge_pr` and the outbox):

| PR state (`merge_info`) | Merge does |
| --- | --- |
| clean / unstable, mergeable | merges now |
| the base has a merge queue | adds it to the queue |
| blocked / behind / unknown, the repo allows auto-merge | turns on auto-merge |
| conflicts, a draft, auto-merge already on, in the queue | disabled, says why |
| turns not pushed yet | refused: Push first, so the merge has them |

The method is the host's default (GitHub `viewerDefaultMergeMethod`: your
last one, else the repo's) when the repo allows it, else the first allowed
one; ▾ picks another allowed method or opens the full merge dialog.

**Continue** — once the PR merged, a banner offers it instead of only
waiting for auto-archive (§7):

```
#123 merged ─▶ [Continue] ─▶ fetch main ─▶ branch ada/rate-limits-2 on main's tip
                              turns + chat stay · pr/shipped/done_at clear
                              next turn's prompt: "the previous PR (#123) merged…"
```

**Send to agent** sits next to a thread's Resolve, a draft's edit/delete and
a finding's Add to review — in the diff and in the side panel — but only when
the PR is the space's (`space.pr`) or is the space's own branch review
(`space.review_pr`). It starts the space's next turn with a message that
quotes the comment and where it is:

```
Address this review feedback on the change.

Review thread on src/limiter.ts:41:
anna wrote:
> Should this be per-token, not global?

Change the code where the feedback applies. If you disagree with a point,
leave the code and say why. End with what you changed.
```

Several items (`space_send_feedback` takes a list) are numbered in one
message. Removed lines read `path:7 (old side, removed lines)`; outdated
comments say so; `<!-- … -->` metadata is dropped. The draft or thread itself
is left as it was.

**Fix CI**: "Fix with agent" next to the Checks tab heading, once a check on
`space.pr` has failed. It fetches each failing check's log (GitHub Actions
today; a host without one gets a bare link) and starts the space's next turn
with the list, each check's log attached or linked, and the project's
`spaces.check` command if one is configured:

```
space.pr ─▶ fresh checks ─▶ failing ones ─▶ check_log (GitHub Actions)
                                                │
                              save_attachment ◀─┴─▶ no log: name + url
                                    │
                                    ▼
                              space_send (one turn, attachments)
```

Refused without a PR, with nothing failing, or while a turn is running — same
rules as any other turn.

### Review of a space

"Review ▸" opens the normal Review tab for the space — groups by meaning, read
marks and progress, findings, skills. One difference while drafting:

```
┌ Review · fix-login-redirect (your space) ───────────────────────────────────────┐
│ Groups           │ diff …                                         │ AI · Tasks   │
│ 1 Redirect guard │ + if (!isSameOrigin(next)) return '/'          │              │
│ 2 Login wiring   │   💬 "also handle //evil.com" (draft)           │              │
├──────────────────┴────────────────────────────────────────────────┴──────────────┤
│ 3 drafts · 1 finding               [Send 4 to agent ▸]   (replaces Submit review) │
└──────────────────────────────────────────────────────────────────────────────────┘
```

Today each draft and finding has its own "Send to agent" (above). The
footer that sends them all as one turn is planned, and so is tracking each
item as a `task` assigned to the space's agent that moves to `review` when
the turn ends.

### Notifications

| Event | Space open | Elsewhere in the app | App in background |
| --- | --- | --- | --- |
| turn finished | diff refreshes | toast "fix-login: turn done" [Open] + ● | notification "fix-login: turn finished" |
| turn failed | error in chat | error toast [Open] + ✕ | notification "fix-login: turn failed" + the error |
| PR checks / reviews | header + tabs | existing inbox | existing |

"In the background" = the window isn't focused. The notification respects
`spaces.notify` (Settings → Spaces, default on); the shell sends it
(`notify_space`), so the window keeps only the plugin's permission check.
Clicking it brings the window forward and opens the space it was about:

```text
turn ends, window away ─▶ notify_space(id, title, body)
  Theodore.app on macOS ─▶ thread waits on the notification ─▶ click
  │    ─▶ focus window ─▶ `open-space` id ─▶ space opens            ⇒ true
  dev run / bare binary / other OS ─▶ notification plugin, no click ⇒ false
       ─▶ next window focus opens the space of the last notification
```

| | Clicks reported | Why |
| --- | --- | --- |
| `Theodore.app`, macOS | yes | posted under the app's own bundle id, `mac-notification-sys` waits for the click |
| dev run, bare binary | no | posted as Terminal; the click goes to Terminal |
| Linux, Windows | no | the plugin path, unchanged |

On macOS a notification waits on a thread of its own until it's clicked,
closed or cleared from Notification Center (the thread then ends); one left
in Notification Center keeps its thread until the app quits. At most 16
wait at once; past that a notification is only shown and falls back to the
next window focus. The dock badge counts the spaces whose last
turn ended after you last looked at them (finished or failed); a space only
counts as seen while the window has focus.

---

### Setup, run, check, archive

A space can be set up for its project by the `spaces:` section of the project
config — files copied in, free ports, env, and `setup` / `run` / `check` /
`archive` commands. Nothing configured gets `.env*` and 2 ports anyway, so a
space works with zero config; a project with no commands at all offers "Set
up with agent" instead, and a failed setup offers "Fix with agent" next to
Retry. See `docs/config.md`.

### Terminal

An interactive shell in the space's worktree, next to the agent's turns —
for `git log`, a one-off script, poking at something the agent just wrote.
Toggled from the header (and `⌃\``), it opens as a drawer under the page;
`+` starts another tab, × closes one.

```
⌃`   ──▶ drawer ── [Terminal 1] [Terminal 2] [+] ────────────────┐
           tab = one xterm.js view over one real PTY             │
           $SHELL -l, cwd = worktree, env = the space's own ◀────┘
```

- **Shell**: `$SHELL -l` (the user's login shell), so `.zshrc`/`.bashrc`
  aliases and prompt apply, same as a terminal app.
- **Env**: the parent process's env minus Theodore's own `THEODORE_*`
  secrets, plus the space's variables and `spaces.env` — the same
  `space_command_env` a space's `setup`/`run`/`check` commands get
  (docs/config.md).
- **Several per space, outlive the page.** Each tab is a real PTY the core
  owns, not the component; navigating away from the space page and back
  re-lists and re-attaches whatever is still running, scrollback included.
  Closed on archive (before the worktree is removed) and on a clean app
  quit — never left running past the space, or past Theodore itself.
- **Bounded, twice.** The core keeps a raw tail (ANSI intact) per terminal
  to reseed the view on reattach, and a separate ANSI-stripped line tail
  (`space_terminal_tail`, 2,000 lines) for a later agent tool to read as
  plain text — not wired up yet.
- **Flow control**: output is batched (16 ms or 64 KB, whichever first)
  before it reaches the UI, so a chatty shell can't flood the event bus.
- Follows the app's light/dark tokens, not a terminal palette of its own.

## 4. What makes it Theodore's

- **Review is the product.** The agent's diff is reviewed with the same
  grouping, read state, findings and skills as a teammate's PR. A
  space's review is an ordinary branch review (`open_branch` plumbing) whose
  head is the space's checkpoint.
- **Comments are instructions.** Drafts, findings and reviewer threads go to
  the space's agent as its next turn, quoted with their `path:line`. The
  loop "read → comment → agent fixes → read what changed" runs on the
  review you already use; tracking each as a `task` is planned.
- **Stages, not modes.** Writing code and reviewing it are one lifecycle;
  nothing is exported from one tool into another.

---

## 5. Agent control

| Mode | The agent may | Port mapping |
| --- | --- | --- |
| Ask | read, skills, subagents, todos, web, MCP; no writes or commands | `AgentMode::ReadOnly` |
| Edit (default) | + edit files in the space | `AgentMode::Edit` |
| Run | + any shell command (tests, installs), no prompts | `AgentMode::Run`: Claude `--permission-mode bypassPermissions`; Codex `workspace-write` with network access; Gemini `--approval-mode yolo`. No sandbox of Theodore's own (§12 #4) |

Mode is picked next to Send (Ask · Edit · Run) and sticky per space. Edit
is unchanged from before: Claude's Edit already pre-approves Bash, Codex's
`workspace-write` already runs commands in its own sandbox; Run lifts the
remaining prompts and limits.

- **Stop** cancels the run (SIGTERM → 5 s → SIGKILL, existing lifecycle), then
  checkpoints whatever was edited as "turn N (stopped)". Nothing is lost;
  messages queued behind it are held, not sent (§8).
- **Resume** = send another message. The space keeps one agent session and
  each turn continues it (below), so the agent still has the files it read,
  the commands it ran and its todo list; a different agent picks up from the
  replayed transcript.
- **Switch agent or model** per message (picker in the header).
- **Context**: `@path` mentions (stored as refs), paste a PR link to attach
  its description and diff stats, `/skill` runs a skill as the turn's
  instructions. The space's own commands come as MCP tools (below); a
  `get_space_context` (goal, diff stat, open tasks) is v2.
- **Scripts**: repo settings `spaces.setup` (v2; once, after create — e.g.
  `pnpm i`) and `spaces.check` (after each turn; a failing check is fed into
  the next turn). Output streams as run log lines.
- **Safety**:
  - The agent's cwd is `~/.theodore/spaces/<id>`, a worktree of Theodore's own
    cache. The user's checkout and its `.git` are never written. A local
    folder project uses the existing mirror.
  - Theodore never pushes, opens a PR or writes into a local folder unless
    you ask: a click, or asking the agent, which calls the `ship` tool
    (below). Either way it is the same ship, with the same guarantees.
  - The prompt tells the agent to stay in its directory, to ship only
    through `ship`, and never to run `git push`, `gh pr create` or switch
    branches itself. After each turn Theodore points the space ref at
    whatever `HEAD` is; a `HEAD` that no longer descends from the base is
    flagged instead of silently recorded.
  - Theodore's secrets are stripped from the agent env (existing).
  - Every git command Theodore runs in the worktree first checks that git
    resolves exactly that worktree (§6 "Worktree guard"); otherwise nothing
    runs, so a lost `.git` never turns into `add -A` / `reset --hard` on a
    parent repository.
  - File-edit tools stay inside the worktree and the turn's attachment dirs
    (Edit and Run; Ask can't edit at all):

    | Agent | How | Verified |
    | --- | --- | --- |
    | Claude Code | no blanket `Edit`/`Write` allow; `--settings` with `Edit(//<dir>/**)` rules (Edit) and a `PreToolUse` hook on `Edit\|Write\|MultiEdit\|NotebookEdit` (Edit and Run): Theodore's own binary run as `--theodore-edit-guard <dir>…` exits 2 for a path outside, symlinks resolved, `..` refused. The hook holds under `bypassPermissions` too | live, 2.1.312 |
    | Codex | its own `workspace-write` sandbox: writes only in cwd (plus `/tmp` and `$TMPDIR`), for shell and patches alike | flags only |
    | Gemini | its file tools refuse paths outside the workspace (the CLI's own check); nothing added | not installed here |

  - Not an OS sandbox: in Edit and Run, a shell command (Claude's Bash,
    Gemini's shell) can still write outside its directory. Later: run agents
    under a macOS sandbox profile that only allows writes inside the space
    (open question 4).

### One session per space

Each run reports its CLI session id (Claude Code's `session_id`, Codex's
`thread_id`); the space stores it as `agent_session` (agent, id, worktree).
The next turn continues it and sends only the new message: skill expansion,
attachments, a failed `check` and the `ultracode` keyword wrap it as before,
but no transcript.

```
turn 1 ─▶ claude -p "<goal>"                         ─▶ session S ─▶ stored
turn 2 ─▶ claude -p "<message>" --resume S           ─▶ session S
          codex exec … resume S "<message>"
gone?  ─▶ "No conversation found" / "no rollout found"
          └─▶ once more, new session + replayed transcript ─▶ session S'
```

| Next turn | Continues the session? |
| --- | --- |
| same agent, same worktree | yes; the model and mode may change (flags are per run) |
| another agent | no: new session from the replayed transcript (last 12 messages) |
| Gemini | never: it has no resume, every turn replays the transcript |
| after Undo last turn | no: the session holds the undone turn; the replay leaves it out |
| after unarchive | yes (the worktree path is the same); no if the checkpoints were gone |
| the resumed run failed for another reason | the session stays for the next turn |

- Claude Code's system prompt (`--append-system-prompt`) and permissions go
  on every turn. Codex's thread already opens with the system prompt (folded
  into its first message), so a resumed turn doesn't repeat it.
- A turn that ran without a session (Gemini, or a switch) clears the stored
  one: the conversation moved on without it.
- Review runs (AI panel, grouping, skills, fixes) don't keep sessions.

**The replayed transcript, once a space outgrows it.** The last 12 messages
carry the conversation across a switch, but a space with more history than
that would otherwise lose everything before them. The first time a turn
can't resume hits that case, the chat agent's cheap model summarizes
whatever falls outside the window (goal, decisions, files touched, what's
left) once; the summary is cached on the space (`handoff_summary`) and
prepended ahead of the replayed window on every switch after that — no
second call:

```
history > 12 messages, can't resume
  cached already?      ──yes──▶ prepend it
  no  ──▶ chat agent, cheap model ─▶ summary ─▶ cached ─▶ prepend it
          fails (offline, no CLI) ─▶ today's plain replay, nothing cached
```

A short space (≤ 12 messages total) never asks for one.

### Tools the agent gets

Every turn, in every mode, gets the `space-tools` MCP server: the space's
configured commands, never a shell of the agent's choosing.

| Tool | Does |
| --- | --- |
| `run_check` | the project's `check`, now: exit code + last lines |
| `start_app` / `stop_app` / `app_status` | the `run` command: start, stop, running?, ports, `http://localhost:<port>` |
| `read_logs` | the tail of `setup`, `run`, `check` or `archive` output (bounded) |
| `space_info` | name, branch, base, worktree, ports, env variable names (not values), which commands are set |
| `ship` | Open PR / Open as draft / Push / Merge locally / Keep as branch, as the Ship menu does (§6): PR number and URL, or why it was refused |
| `get_space_diff` | the change vs the merge-base with the base, uncommitted edits included; one `file`, or `stat` for +/− per file |
| `get_review_comments` | drafts, findings and threads on the space's review and its PR: `path:line`, who (you, an agent, a reviewer), state |
| `add_review_comment` | a finding on a line of the space's review: shows in Review, "Send to agent" sends it back |
| `set_base` | a new base branch: fetched, the diff is vs the merge-base from then on |
| `update_progress` | the turn's plan as steps (pending · active · done) above the composer |

```
turn ─▶ token for this space ─▶ agent ─▶ run_check ─▶ check ─▶ result ─▶ agent fixes ─▶ …
  │                                  ├─▶ start_app ─▶ run   ─▶ URLs   ─▶ agent tries it
  │                                  └─▶ ship ─▶ ship_space (as the turn) ─▶ PR #n ─▶ agent tells you
  └─ agent exits ─▶ token revoked, progress cleared
```

- The system prompt names them: use `run_check` after changes, `start_app`
  to try the app, `ship` only when you ask to push, open a PR or merge.
- `ship` from the turn runs while that turn holds the space; any other
  caller still waits for the turn. It checkpoints the edits so far first,
  so they ship too; the turn's own checkpoint follows when it ends.
- `get_space_diff` snapshots the worktree through a temporary index: no
  ref moves, the worktree's own index stays as it was.
- `set_base` keeps the worktree and the turns; a PR that is already open
  keeps its target (the tool says so). Undo doesn't move the base back.
- The tracker is the turn's: each call replaces it, no steps clears it,
  and it goes when the turn ends.
- Trust holds: a repo command not allowed yet is refused with "not allowed
  by the user yet" and never runs.
- The turn lists its tool calls while it runs and after (this session),
  Theodore's own by what they do.
- Review chat, skills and fixes on a space's PR also get the read-only
  `read_logs` and `app_status` next to `review-tools`.

Details, limits and errors: `docs/config.md` §7.

---

## 6. Git model

```
~/.theodore/repos/<host>/<owner>/<repo>.git    bare cache
   ├─ refs/heads/*                ◀── force-fetched from the remote, pruned
   └─ refs/theodore/spaces/<id>   ──▶ last checkpoint   (Theodore's own ref)
~/.theodore/spaces/<id>/          detached worktree at the checkpoint + live edits

turn ends ─▶ git add -A ─▶ commit "turn N" ─▶ update-ref refs/theodore/spaces/<id> HEAD
Update    ─▶ fetch ─▶ checkpoint ─▶ git merge <base tip> in the worktree ─▶ base_sha = tip
Ship      ─▶ commit-tree <checkpoint tree> -p <base> (author: you) ─▶ refs/theodore/shipped/<id>
Open PR   ─▶ git push origin <shipped>:refs/heads/<login>/<slug>   (never forced)
Push      ─▶ commit-tree <checkpoint tree> -p <last shipped> [-p <base tip merged since>] ─▶ push (fast-forward)
Merge     ─▶ (local folder) fetch <cache> <shipped ref> ─▶ git merge into its checked-out branch
Keep      ─▶ (local folder) fetch <cache> <shipped ref>:refs/heads/space/<slug>
Keep      ─▶ (local folder) git -C <folder> fetch <cache> <ref>:refs/heads/<branch>
Continue  ─▶ ls-remote + fetch <base> ─▶ tip ─▶ (turns not pushed) commit-tree 3-way onto tip
          ─▶ reset --hard + clean -fd the worktree, update-ref refs/theodore/spaces/<id>
```

Why detached plus a private ref, not a real branch in the cache:

| A branch under `refs/heads/` would… | Because |
| --- | --- |
| break every project refresh | `fetch +refs/heads/*:refs/heads/*` refuses to update a branch checked out in a worktree |
| be deleted before it is pushed | the same fetch runs with `--prune` |

Every turn is a commit, so the diff is commit-to-commit (cacheable, same as a
PR's revisions), "undo last turn" is a ref move plus `reset --hard` and
`clean -fd` in the worktree (ignored files such as installed deps stay), and the agent's own
`git commit`s are simply kept. Checkpoints are authored by Theodore and never
leave the cache; what ships is one squashed commit authored by you (§12 #2).

**Worktree guard** (checkpoint, Undo, the busy check, reopen, archive):

```
git in ~/.theodore/spaces/<id>   GIT_CEILING_DIRECTORIES=~/.theodore/spaces, no inherited GIT_DIR
  ├─ real dir, `.git` is a file, rev-parse --show-toplevel == the dir (symlinks resolved),
  │  common dir under ~/.theodore/repos                         ─▶ run the command
  └─ anything else (`.git` gone, a `git init` in its place, …)  ─▶ NotAWorktree, nothing run
```

The ceiling alone already makes git fail instead of walking up into a
git-controlled home; the guard says why. A directory that fails it is never
deleted: reopening the space (next turn, Undo, unarchive) or archiving it
moves it to `~/.theodore/spaces/.moved-aside/<id>-<millis>` and recreates the
worktree from `refs/theodore/spaces/<id>`.

**Busy worktree**: before a checkpoint or Undo Theodore asks git what is
unfinished there (`GitPort::worktree_busy`, one `WorktreeBusy`):

| Found (`rev-parse --git-path`, `ls-files -u`) | Busy |
| --- | --- |
| `rebase-merge/`, `rebase-apply/` | rebase (also `git am`) |
| `MERGE_HEAD` | merge |
| `CHERRY_PICK_HEAD` | cherry-pick |
| `REVERT_HEAD` | revert |
| unmerged index entries, none of the above | unresolved conflicts |

| While busy | |
| --- | --- |
| a turn ends | answer kept, no checkpoint, no `turn_log` entry; space `failed` with "checkpoint skipped: rebase in progress" |
| Undo | refused: "can't undo: … finish or abort it first" |
| Archive, Ship | refused with the same reason; nothing half-done is committed |
| next turn after it's finished | checkpoints as usual |

That is for an operation nobody told Theodore about (the agent ran `git
rebase` itself). A merge Theodore started for Update from base is recorded
on the space (`space.updating`), and its flow owns the end of it.

**Update from base** (owner, 2026-10-31: merge the base in, never rebase or
force-push, so pushed history and reviewers' comments stay put; shipping
still squashes):

```
Update ─▶ fetch the base ─▶ tip == base_sha? ─▶ "already up to date"
  └─▶ checkpoint "edits before updating from main" (= from)
      └─▶ git merge --no-edit --ff <tip>   (Theodore's identity, no hooks, merge.conflictStyle=diff3)
            ├─ clean ─────▶ merge commit = checkpoint "merge main" ─▶ base_sha = tip ─▶ diff vs tip
            └─ conflicts ─▶ merge left in progress, space.updating = { base_tip, from, conflicts, started_at }
                              └─▶ agent turn (Edit; an Ask space switches to Edit): the files,
                                  "resolve the markers, keep both intents, run_check, leave git to Theodore"
                                  turn ends ─▶ space_merge_finish instead of the checkpoint:
                                    ├─ no file it listed has a marker line ─▶ add -A, commit = checkpoint,
                                    │     base_sha = tip, updating cleared, turn_log.base_before = old base
                                    ├─ markers left ─▶ no checkpoint, failed "conflicts left in b.ts; …"
                                    ├─ the agent committed the merge itself ─▶ that commit is checkpointed
                                    └─ the agent aborted it ─▶ checkpoint, old base, updating cleared
            Abort ─▶ merge --abort, reset --hard <from>, clean -fd, ref → from, updating cleared
```

- A file counts as resolved when no line starts with `<<<<<<<`, `|||||||`
  or `>>>>>>>` (or it is deleted): the agent needn't `git add`, which its
  sandbox may not allow (the index lives in the cache).
- Shipping after an update: the base is a real parent of the space's head,
  so the squashed commit stays correct.

  | Ship | the new commit |
  | --- | --- |
  | Open PR / first Merge locally | parent = the new `base_sha`, tree = the merged checkpoint: one commit vs the new base |
  | Push (PR open) | parents = last push + the base tip merged since (dropped when already in its history), tree = the merged checkpoint: fast-forward, and the host's PR diff (vs the merge base) is still the change alone |
  | Merge locally again | the same second parent |

**Shipping, step by step**

- *Author*: `user.name` / `user.email` as git resolves them (a local
  folder's own config first, then the cache's, i.e. your global one); on
  GitHub without either, your login with its noreply address. Unsigned:
  Theodore can't answer a passphrase prompt. Message: the title (editable
  in the confirm popover, default the goal's first line), then the goal
  when it says more.
- *Open PR*: the commit's parent is the base. If the branch name is
  already taken on the host, nothing is pushed. After the push the outbox
  item `create_space_pr` asks the host for an open PR from that branch
  first and only then creates one; a create the host refuses as a
  duplicate is looked up again. So a retry — after a timeout, a crash, or
  clicking Open PR again after a failure — never opens a second PR, and a
  retry with no new turns pushes nothing.
- *PR body*: the repo's PR template when the change has one, else the
  goal (when it says more than the title); then a link back to the space.

  ```
  template? first of .github/pull_request_template.md · .github/PULL_REQUEST_TEMPLATE.md
            · docs/pull_request_template.md · pull_request_template.md   (at the checkpoint)
    yes ─▶ the space's agent fills it in (read-only, Claude on haiku, 60 s,
    │      from the goal + changed files) ─▶ kept only if every heading survived
    │      └─ failed / timed out / dropped headings ─▶ goal + the template as is
    no  ─▶ the goal
  then  ─▶ <!-- theodore-space:<id> -->
           <sub>From a Theodore space: theodore://space/<id></sub>
  ```

  The footer is added once: an existing one (from the marker on) is
  replaced, never repeated. The link uses the build's own scheme
  (`theodore-staging://…`); opening it brings the window forward and
  opens the space (`apps/desktop/src/deep_link.rs`). It's plain text, not
  a markdown link: hosts don't make custom schemes clickable.
- *Continue* (§7): the new round's branch is the old one plus `-2` (a
  continued branch counts on: `-3`, `-4`), skipping a name another space,
  an earlier round or the host already has. Its base is the base branch's
  tip fetched now; turns after the last push (not in the merged PR) are
  replayed onto it three-way, and refused if that conflicts.
- *Push*: one more commit on top of the last pushed one, so reviewers'
  history and comments stay put. No force push, ever:

  | PR branch on the host | the new commit |
  | --- | --- |
  | where the last push left it | parent = last push, tree = latest checkpoint |
  | moved (someone pushed to it) | parent = their tip, tree = their tip + the turns since the last push (three-way, no worktree); refused if that conflicts |
  | gone | parent = last push, pushed again |

- *Merge locally*: refused, folder untouched, on a dirty working tree
  (untracked files count), a detached `HEAD`, or a merge that would
  conflict (checked first with `git merge-tree`, before the index or any
  file changes). Otherwise a normal `git merge` (fast-forward when it can
  be) with the user's own hooks; a merge that still fails is aborted. The
  objects come over with `fetch --no-write-fetch-head`: no `FETCH_HEAD`,
  no extra ref. A second merge after more turns brings only those turns.
- *Keep as branch*: writes `refs/heads/space/<slug>` in the folder and
  nothing else; refused if that branch exists.

---

## 7. Lifecycle and data

New synced model `space` (core-owned, written only by commands):

| Field | |
| --- | --- |
| `id`, `project`, `name`, `goal` | `goal` is the first message |
| `branch`, `base`, `base_sha`, `head_sha` | `branch` (`<login>/<slug>`, `space/<slug>` without a login) is pushed on Open PR; `head_sha` = last checkpoint |
| `worktree` | absolute path |
| `status` | `idle · running · failed · archived` |
| `agent`, `model`, `mode` | sticky per space |
| `review_pr` | the branch review the diff and Review use |
| `pr` | the host PR, once opened — the link from drafting to review |
| `usage`, `turns`, `error`, `created_at`, `updated_at` | |
| `shipped` | the last ship: `how` (`pr · merged_locally · branch`), `branch`, `sha` (the squashed commit), `head` (the checkpoint it came from — later turns are "not pushed"), `pushes`, `at` |
| `done_at` | set when the PR merges (seen by the poller's spaces sweep) or on Merge locally / Keep as branch; cleared by Continue |
| `turn_log` | per turn: your message and the reply (`chat_message` ids), messages steered into it (`steered`), checkpoint `before` / `after`, `undone`; `base_before` when the turn completed an Update from base (Undo puts that base back) |
| `title` | the chat agent's proposed title (§3 "New space"); `None` until it lands |
| `handoff_summary` | the earlier conversation, summarized once and cached (§5 "One session per space") |
| `progress` | the running turn's tracker (`update_progress`): `steps` (label, `pending · active · done`), `note`, `at`; `null` once the turn ends |
| `updating` | an Update from base resolving its conflicts: `base_tip`, `from` (the checkpoint it started at), `conflicts`, `started_at` (§6) |
| `rounds` | earlier rounds of a continued space, oldest first: the merged `pr`, its `branch`, `base_sha`, the last pushed `head_sha`, `at` |
| `continue_note` | after Continue: one line the next turn's prompt opens with, then cleared |

| Data | Where |
| --- | --- |
| Transcript | `chat_message` rows with `pr = "space:<id>"` — the chat model, bootstrap and UI already work on it. A message waiting for its turn carries `queued` (§8) |
| Streaming text, tool lines | `Ephemeral::AgentDelta` / `RunLog` with run id `space:<id>:<msg>`; not persisted |
| A turn | on the space row: `status`, `turns`, `head_sha`, and an entry in `turn_log` (what undo moves back to). Later: `space_run` rows (agent, mode, usage, session id) for per-turn diffs and native resume |
| Code | the worktree and `refs/theodore/spaces/<id>` |

**Archive** checkpoints any uncommitted edits, removes the worktree, keeps the
ref and the rows. The space page's Archive button does it at any time (not
while a turn runs). **Unarchive** (the archived banner's button, or ⌘K) recreates the
worktree from the ref and reopens the live diff; archived spaces are found
by searching ⌘K. A done space starts its auto-archive countdown again from
the unarchive. If the ref is gone the worktree starts over at the base and
the space says so (`failed`, "the checkpoints were gone").
**Auto-archive**: once a minute the poller re-fetches each in-review space's
PR, sets `done_at` when it has merged, and archives spaces done for
`spaces.auto_archive_days` (default 7, `0` = never; Settings → Spaces). A PR
closed without merging leaves the space alone. Space worktrees live outside
`~/.theodore/wt`, so the PR worktree sweep never touches them.

**Continue after merge** (`continue_space`): a merged space goes on as the
next round instead of waiting to be archived.

```
merged #123 ─▶ Continue ─┬─ running / archived / PR not merged / offline ─▶ refused
                         └─▶ fetch the base ─▶ tip
                              worktree + ref ─▶ tip (+ turns not pushed, replayed)
                              branch ─▶ ada/rate-limits-2
                              rounds += { #123, ada/rate-limits, … }
                              pr · shipped · done_at ─▶ cleared   (drafting again)
                              continue_note ─▶ the next turn's prompt:
                                "the previous pull request (#123, branch …) was
                                 merged into main … continues on ada/rate-limits-2"
```

Turns, transcript, the agent session and the review id stay. Undo can't
go back past Continue (the space moved since its last turn). With `pr`
and `done_at` cleared the sweep has nothing to archive; a sweep that
found the old PR merged while Continue ran re-reads the row under the
ship lock and leaves it alone.

| Once the PR merged | |
| --- | --- |
| Update from base (§6) | off: "Base moved" hides and the core refuses; Continue is the way onto the new base |
| An Update from base still resolving | Continue waits: complete or abort it first |

Not built yet: **Delete** (archive plus deleting the ref and the chat rows);
**GC** (archived spaces whose branch was never pushed lose their ref after
`spaces.gc_days`, 30); removing a project archiving its spaces first.

**Crash recovery**

| Found on start | Action |
| --- | --- |
| `status = running` | → `failed` "interrupted: Theodore was closed" |
| queued messages | kept, held: each waits for its Send (§8) |
| dirty worktree | kept; the next turn (or archive) checkpoints it first |
| worktree dir missing | recreated from `refs/theodore/spaces/<id>` |
| dir there, but not the space's worktree (`.git` gone or foreign) | git refuses to run in it (§6 "Worktree guard"); on reopen or archive it is moved to `spaces/.moved-aside/`, never deleted, and recreated from the ref |
| a rebase, merge or conflict left in the worktree | no checkpoint or Undo until it is finished or aborted (§6 "Busy worktree") |
| `updating` set (an Update from base was resolving) | kept: the merge is still in the worktree; Complete merge or Abort from the banner, or send the agent a message |
| ref missing too | `failed`, transcript kept, "Restart from base" offered |

---

## 8. Concurrency

One run per space at a time. A send while it runs is **queued**: stored as
your `chat_message` with `queued` (`seq`, the agent / model / mode picked,
`held`) and shown under the running turn, not in the transcript yet.
Attachments go with it; a `/skill` is expanded when it runs, from the
worktree as it is then.

```
send ─▶ running? ── no ──▶ turn now
           └─ yes ─▶ queued #n ──▶ turn ends ─┬─ on its own ─▶ #1 runs, then #2 …
                                              └─ stopped · failed · restart
                                                   └─▶ held: Send · Edit · Cancel
```

| Turn ended | The queue |
| --- | --- |
| on its own (a failing `check` included: the next turn hears it) | the first waiting message runs as the next turn, then the next |
| stopped (also during its `check`) | held: nothing is sent, nothing dropped |
| failed, or a queued one couldn't start | held |
| Theodore restarted | held (§7 Crash recovery) |

- **Edit** changes a queued message's text, **Cancel** deletes it; both
  only while it is queued.
- **Send** on a held message runs it now when the space is free; while a
  turn runs it goes back in line. Held ones never send on their own, and a
  new message while some are held runs at once.
- A queued message's picks win over the space's when it runs; `None` means
  whatever the space has then. Its turn is a normal one: undo, session
  resume and the replayed transcript treat it like any other.
- **Undo** and **Archive** leave the queue as it is. An archived space sends
  nothing (Send is refused); after Unarchive the held messages can be sent.
- A per-space lock covers "send now or queue" and "turn ends → next queued
  starts", so a send never slips in between and the order holds.

**Send now (steer).** For a Claude Code turn the composer also offers
*Send now* (⌘⏎; ⏎ still queues): the message goes into the running turn and
the agent takes it at its next step. Claude Code reads messages from stdin
while it runs (`--input-format stream-json --replay-user-messages`, checked
on 2.1.312): one written mid-run joins the turn after the current tool call
and is echoed back when taken; one that lands just as the turn ends starts
another turn in the same process and session. The run ends — stdin closed —
once every message was taken and answered; the reply is all the answers.

```
claude -p --input-format stream-json …   stdin ◀─ goal
   tool call … ◀────────────────────────── stdin ◀─ "also handle 404s"  (Send now)
   echo: taken ─▶ … ─▶ result ─▶ all taken? ─▶ close stdin ─▶ exit ─▶ checkpoint
```

| | Send now |
| --- | --- |
| Claude Code, same agent and mode, no attachments | into the running turn; listed in the turn's `steered`, undone with it |
| Codex, Gemini (one prompt per process) | queued |
| another agent / mode, attachments (`--add-dir` is per process) | queued |
| the run just ended, or hasn't started taking input yet | queued |

The runner exposes it as an optional capability (`AgentPort::run_steerable`
returns a `Steer` handle or none). A message written but never taken within
30 s of the run's last result closes stdin anyway, so a run never waits on
input forever.

Spaces run in parallel, each in its own worktree. v2: `spaces.max_parallel`
(default 3) queues the rest. Git writes into the shared bare cache (fetch,
`update-ref`, `worktree add/remove`) and into one space's worktree
(checkpoint, reset) each take a per-path lock (plan §7 "One writer at a
time"), so a turn's checkpoint never races a project refresh.

**Offline.** A space of a host project needs the network only to Open PR /
Push. Creating it, sending, undo, archive and Review use the clone already
on this Mac when the remote can't be reached (plan §7 "Offline"); only a
project never cloned answers "You're offline and this project hasn't been
downloaded to this Mac yet".

## 9. Commands

| Command | Does |
| --- | --- |
| `create_space` | worktree + ref at the base, `space` row; sends `goal` as turn 1 when given |
| `list_spaces` | rows of one project or all (the UI reads the synced rows) |
| `space_send` | chat row, then a turn in the worktree; checkpoint; refresh the diff. While a turn runs: a queued chat row (§8) |
| `space_send_now` | into the running Claude turn when it can take it (§8 "Send now"), else as `space_send` |
| `space_queued` | `send` · `cancel` · `edit` one queued message |
| `space_stop` | cancels the running turn; the queue is held |
| `save_attachment` | stores a file (≤ 25 MB) for a message; `attachment_data` reads it back as a `data:` URL |
| `space_skills` | skills `/<id>` can run: `~/.claude/skills` plus the worktree's `.claude/skills`, no built-ins |
| `space_review` | opens (or refreshes) the space's branch review; `group: true` also groups it |
| `archive_space` | checkpoint, remove the worktree, keep the ref |
| `unarchive_space` | recreate the worktree from the ref, reopen the live diff |
| `undo_space_turn` | worktree and ref back to the last live turn's `before`; the turn is marked undone |
| `set_space_pinned` | `space.pinned`; pinned spaces sort first (sidebar, ⌘K) — never touches `updated_at` |
| `space_send_feedback` | threads / drafts / findings of the space's PR or own review, quoted as one turn |
| `space_base_status` | the base's tip in the copy on this Mac and how many commits the space is behind it; no network |
| `update_space_from_base` | merge the base's tip into the space (§6); while `updating`, complete that merge if no markers are left |
| `abort_space_update` | `merge --abort`, back to `updating.from` on the old base |
| `ship_space` | `open_pr` · `open_draft_pr` · `push` (host project) or `merge_locally` · `keep_branch` (local folder), with an optional title (§6) |
| `space_setup_assist` | "Set up with agent": no `spaces:` commands at all — a turn asking the agent to look at the repo and write them (`docs/config.md` §2, §8) |
| `space_fix_setup` | "Fix with agent": setup failed — a turn with the command, its exit status and the log tail, asking the agent to fix the cause |
| `open_space_terminal` | a new PTY in the worktree (§3 "Terminal"), `$SHELL -l`, given `cols`/`rows` |
| `write_space_terminal` | bytes typed or pasted into it |
| `resize_space_terminal` | a new `cols`/`rows` (`SIGWINCH`) |
| `close_space_terminal` | kills its process group |
| `list_space_terminals` | a space's open (or just-exited) terminals, each with scrollback to reseed the view |
| `space_fix_ci` | "Fix with agent" on the Checks tab: the space PR's failing checks' logs (or links), as one turn |
| `merge_space` | merges the space's PR: an optional method, else the host's default; now, auto-merge or the merge queue (§3); refused with turns not pushed |
| `continue_space` | after the PR merged: a new branch on the base's fresh tip, the round kept (§7) |

## 10. The flag

| | on (default) | turned off |
| --- | --- | --- |
| Sidebar section, route, palette items, keys | present | absent |
| Settings → Spaces | present, switch on | present, switch off |
| Core commands | used | registered, unused |

On = `FEATURES.buildMode || setting labs.build_mode != false` (user scope):
no row, or `true`, means on. The flag forces them on and locks the switch.

---

## 11. Phasing

| Phase | Scope |
| --- | --- |
| **Built** | `space` model; create / send / stop / review / archive; transcript in chat rows; checkpoint per turn; sidebar section; space page with chat + diff; Review opens the grouped review; crash → failed. Shipping: squash into one commit by you; Open PR / Open as draft (`HostAdapter::create_pr` + `find_open_pr` on GitHub, GitLab, Bitbucket, sandbox and fixtures, through the outbox, idempotent by head branch) and Push (never forced); branch `<login>/<slug>`; Merge locally and Keep as branch for local folders; modes Ask / Edit / Run; Archive button and `spaces.auto_archive_days`. "Send to agent" on a space PR's / own review's threads, drafts and findings; Undo last turn (`turn_log`); Unarchive; one agent session per space (Claude Code `--resume`, Codex `exec resume`); background notifications and the dock badge (`spaces.notify`); names transliterated from Cyrillic and accented goals, `-2` for repeats; a chat-agent-proposed name and title in the background, never delaying turn 1 (`space_naming`); a cached, once-generated handoff summary for a space that outgrows the replayed window (`space_handoff`); `setup` / `run` / `check` / `archive` from the project config (`docs/config.md`) with logs and Open app; `copy` and `ports` default to `.env*` and 2 with zero config; "Set up with agent" and "Fix with agent"; `copy` and `ports` default to `.env*` and 2 with zero config; "Set up with agent" and "Fix with agent"; the agent's `space-tools` for the change: `ship`, `get_space_diff`, `get_review_comments`, `add_review_comment`, `set_base`, `update_progress` (the tracker above the composer) |
| **v2** | My PRs "Drafting" group and the sidebar's `⎇ #n` glyph; "Send all to agent" footer and feedback tracked as tasks; Threads / Checks tabs on the space page; `space_run` rows and per-turn diffs; Delete and ref GC; queue + `max_parallel`; `copy` and `ports` default to `.env*` and 2 with zero config; "Set up with agent" and "Fix with agent" |
| **v2** | My PRs "Drafting" group and the sidebar's `⎇ #n` glyph; "Send all to agent" footer and feedback tracked as tasks; Threads / Checks tabs on the space page; `space_run` rows and per-turn diffs; Delete and ref GC; queue + `max_parallel`; `copy` and `ports` default to `.env*` and 2 with zero config; "Set up with agent" and "Fix with agent" |
| **v2** | My PRs "Drafting" group and the sidebar's `⎇ #n` glyph; "Send all to agent" footer and feedback tracked as tasks; Threads / Checks tabs on the space page; `space_run` rows and per-turn diffs; Delete and ref GC; queue + `max_parallel`; interactive terminal tabs (§3 "Terminal") over a real PTY, surviving navigation, closed on archive / app quit |
| **v2** | My PRs "Drafting" group and the sidebar's `⎇ #n` glyph; "Send all to agent" footer and feedback tracked as tasks; Threads / Checks tabs on the space page; `space_run` rows and per-turn diffs; Delete and ref GC; queue + `max_parallel`; an agent tool over `space_terminal_tail`; `copy` and `ports` default to `.env*` and 2 with zero config; "Set up with agent" and "Fix with agent" |
| **v2** | My PRs "Drafting" group and the sidebar's `⎇ #n` glyph; "Send all to agent" footer and feedback tracked as tasks; Threads / Checks tabs on the space page; `space_run` rows and per-turn diffs; Delete and ref GC; queue + `max_parallel`; sends while running are queued (held after Stop / a failure / a restart; Send · Edit · Cancel) and Send now steers a running Claude Code turn |
| **v2** | My PRs "Drafting" group and the sidebar's `⎇ #n` glyph; "Send all to agent" footer and feedback tracked as tasks; Threads / Checks tabs on the space page; `space_run` rows and per-turn diffs; Delete and ref GC; `max_parallel`; `copy` and `ports` default to `.env*` and 2 with zero config; "Set up with agent" and "Fix with agent"; Update from base (merge, never rebase): "Base moved: N commits behind", conflicts resolved by an agent turn, Complete merge / Abort; `copy` and `ports` default to `.env*` and 2 with zero config; "Set up with agent" and "Fix with agent" for setup; "Fix with agent" on the Checks tab, sending a failing check's log (GitHub Actions) or link as one turn |
| **v2** | My PRs "Drafting" group and the sidebar's `⎇ #n` glyph; "Send all to agent" footer and feedback tracked as tasks; Threads / Checks tabs on the space page; `space_run` rows and per-turn diffs; Delete and ref GC; queue + `max_parallel`; `copy` and `ports` default to `.env*` and 2 with zero config; "Set up with agent" and "Fix with agent"; the agent's `space-tools` for the change: `ship`, `get_space_diff`, `get_review_comments`, `add_review_comment`, `set_base`, `update_progress` (the tracker above the composer); interactive terminal tabs (§3 "Terminal") over a real PTY, surviving navigation, closed on archive / app quit; sends while running are queued (held after Stop / a failure / a restart; Send · Edit · Cancel) and Send now steers a running Claude Code turn; Update from base (merge, never rebase): "Base moved: N commits behind", conflicts resolved by an agent turn, Complete merge / Abort; in review on the space page: Changes · Threads · Checks tabs, Merge (`merge_space`: host default method, auto-merge, merge queue), Continue after merge (`continue_space`, `rounds`), the sidebar's `⎇ #n` glyph; Open PR fills the repo's PR template and links back (`theodore://space/<id>`) |
| **v2** | My PRs "Drafting" group; "Send all to agent" footer and feedback tracked as tasks; `space_run` rows and per-turn diffs; Delete and ref GC; `max_parallel`; an agent tool over `space_terminal_tail`; Continue for a space merged locally or kept as a branch |
| **Later** | OS sandbox for agents; live diff during a turn (snapshot the working tree through a temporary index); signed shipped commits; spaces started from an issue or a failing check; several agents racing one goal, pick the best diff |

## 12. Decisions (owner, 2026-10-21)

| # | Question | Decision |
| --- | --- | --- |
| 1 | Branch name on ship | `<login>/<slug>` (the host login; `space/<slug>` when there is none) |
| 2 | Checkpoint commits on ship | squashed into one commit, authored by you, message from the goal |
| 3 | Draft or ready | **Open PR** opens a ready PR; its dropdown offers *Open as draft* (and any other modes) |
| 4 | Run mode safety | no extra sandbox: Run mode trusts the agent CLI as configured |
| 5 | Local folder end state | **Merge locally**: a normal merge of the squashed commit into the folder's checked-out branch, instead of a PR; *Keep as branch* stays as the alternative |
| 6 | Budget | none: budgets, and showing spend at all, were dropped by the owner on 2026-10-22. Nothing is limited, no cost is shown, and an old `budget.*` setting is ignored |
| 7 | Archive | a setting `spaces.auto_archive_days` (default 7, `0` = never) plus an **Archive** button on the space at any time |
