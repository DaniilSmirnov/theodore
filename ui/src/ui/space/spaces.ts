/**
 * Spaces (docs/spaces.md): pure rules for the hidden build mode — whether
 * it's on, which spaces a workspace lists, what each one's sidebar glyph
 * says, and which "branch reviews" are really a space's live diff.
 */
import type {
  AgentId,
  MergeInfo,
  MergeMethod,
  PrId,
  PrSummary,
  SkillInfo,
  SkillRun,
  Setting,
  Space,
  SpaceFeedback,
  SpaceMode,
  SpaceTurn,
} from '../../gen/contract';
import { ALL, type WorkspaceId } from '../../core/workspaces';

/** The per-machine switch (Settings → Labs). */
export const BUILD_MODE_KEY = 'labs.build_mode';

/** `settings` rows are `{ key, scope, value }`; only the user scope counts. */
function userSetting(
  settings: Pick<Setting, 'key' | 'scope' | 'value'>[],
  key: string,
): Pick<Setting, 'value'> | undefined {
  return settings.find((s) => s.key === key && s.scope === 'user');
}

/** Spaces are on unless the user turned them off; the build flag forces
 *  them on. */
export function buildModeOn(
  flag: boolean,
  settings: Pick<Setting, 'key' | 'scope' | 'value'>[],
): boolean {
  return flag || userSetting(settings, BUILD_MODE_KEY)?.value !== false;
}

/** Settings → Spaces is always listed: it's where they're turned off. */
export function labsVisible(
  _flag: boolean,
  _settings: Pick<Setting, 'key' | 'scope' | 'value'>[],
): boolean {
  return true;
}

/** The `chat_message.pr` key of a space's transcript. */
export const spaceChatKey = (space: string): PrId => `space:${space}`;

/** A space review's `PrRef::host_id` is its checkpoint ref. */
export const SPACE_REF_PREFIX = 'refs/theodore/spaces/';

/** A branch review that is a space's live diff — listed with the space,
 *  not under Local branches. */
export function isSpaceReview(p: Pick<PrSummary, 'pr'>): boolean {
  return p.pr.host_id?.startsWith(SPACE_REF_PREFIX) ?? false;
}

/** Spaces the workspace lists: its project's (All: every one), archived
 *  ones left out, pinned ones first, then most recently active first. */
export function spacesIn(ws: WorkspaceId, spaces: Space[]): Space[] {
  return spaces
    .filter((s) => s.status !== 'archived')
    .filter((s) => ws === ALL || s.project === ws)
    .sort(pinnedFirst);
}

/** Pinned before unpinned; within each, most recently active first. Pin
 *  itself never touches `updated_at`, so pinning a space doesn't make it
 *  read as a new turn. */
function pinnedFirst(
  a: Pick<Space, 'pinned' | 'updated_at'>,
  b: Pick<Space, 'pinned' | 'updated_at'>,
): number {
  const pa = a.pinned ? 1 : 0;
  const pb = b.pinned ? 1 : 0;
  return pb - pa || b.updated_at - a.updated_at;
}

/** The workspace's archived spaces, latest first — the palette lists them
 *  when searched for, so one can be opened and unarchived. */
export function archivedSpacesIn(ws: WorkspaceId, spaces: Space[]): Space[] {
  return spaces
    .filter((s) => s.status === 'archived')
    .filter((s) => ws === ALL || s.project === ws)
    .sort((a, b) => b.updated_at - a.updated_at);
}

export type SpaceGlyph = 'running' | 'unseen' | 'idle' | 'failed';

type UnreadFields = Pick<Space, 'status' | 'updated_at' | 'turns'> &
  Partial<Pick<Space, 'attention_at'>>;

/** Whether something asked for you since `seenAt`: a turn ended or failed, a
 *  plan or a question came. Not any write: a PR opening or an Update from
 *  base isn't news. A space with no such event since `attention_at` was added
 *  falls back to `updated_at`, as it did. */
function hasNews(s: UnreadFields, seenAt: number | undefined): boolean {
  return s.turns > 0 && (seenAt ?? 0) < (s.attention_at ?? s.updated_at);
}

/** What the sidebar shows next to a space (docs/spaces.md §3): a finished
 *  turn you haven't looked at reads as waiting for you. `seenAt` is when
 *  the space page was last open, kept locally. */
export function spaceGlyph(
  s: UnreadFields,
  seenAt: number | undefined,
): SpaceGlyph {
  if (s.status === 'running') return 'running';
  if (s.status === 'failed') return 'failed';
  if (hasNews(s, seenAt)) return 'unseen';
  return 'idle';
}

/** Whether "Mark as unread" would do anything: there's a turn to flag, and
 *  it doesn't already show as waiting for you. */
export function canMarkUnread(
  s: UnreadFields,
  seenAt: number | undefined,
): boolean {
  return s.turns > 0 && spaceGlyph(s, seenAt) !== 'unseen';
}

/** A space error as a person reads it: a refusal without the core's
 *  "invalid request:" prefix (the log keeps it). */
export function plainError(error: string): string {
  return error.replace(/^invalid request:\s*/, '');
}

type EndedFields = Pick<
  Space,
  'status' | 'name' | 'error' | 'goal' | 'title' | 'updating'
>;

/** What a failed end is called: the agent's turn on an Update from base
 *  that left conflicts is the update waiting on you, not a turn failure. */
function failedWhat(next: Pick<Space, 'updating'>): string {
  return next.updating ? 'update has conflicts left' : 'turn failed';
}

/** A turn just ended (running → anything else): what to tell you if you're
 *  not looking at the space. `null` when nothing finished. */
export function turnEnded(
  prev: Pick<Space, 'status'> | undefined,
  next: EndedFields,
): { level: 'info' | 'error'; text: string } | null {
  if (prev?.status !== 'running' || next.status === 'running') return null;
  if (next.status === 'failed') {
    return {
      level: 'error',
      text: `${spaceTitle(next)}: ${failedWhat(next)}${next.error ? ` — ${plainError(next.error)}` : ''}`,
    };
  }
  return { level: 'info', text: `${spaceTitle(next)}: turn done` };
}

/** Days after a space is done before it archives itself; 0 = never. */
export const AUTO_ARCHIVE_KEY = 'spaces.auto_archive_days';
export const DEFAULT_AUTO_ARCHIVE_DAYS = 7;

/** `spaces.auto_archive_days`, user scope; unset or invalid = 7. */
export function autoArchiveDays(
  settings: Pick<Setting, 'key' | 'scope' | 'value'>[],
): number {
  const v = userSetting(settings, AUTO_ARCHIVE_KEY)?.value;
  return typeof v === 'number' && v >= 0 && Number.isFinite(v)
    ? Math.floor(v)
    : DEFAULT_AUTO_ARCHIVE_DAYS;
}

/** Where a space is in its lifecycle (docs/spaces.md §1). `opening`: the
 *  branch is pushed and the PR is being opened (or the attempt failed —
 *  Open PR again is safe, it never opens a second one). */
export type SpaceStage =
  'drafting' | 'opening' | 'in_review' | 'merged_locally' | 'branch' | 'done';

export function spaceStage(
  s: Pick<Space, 'pr' | 'shipped' | 'done_at'>,
  prState?: PrSummary['state'],
): SpaceStage {
  if (s.shipped?.how === 'merged_locally') return 'merged_locally';
  if (s.shipped?.how === 'branch') return 'branch';
  if (s.pr) return prState === 'merged' || s.done_at ? 'done' : 'in_review';
  if (s.shipped?.how === 'pr') return 'opening';
  return 'drafting';
}

export const STAGE_LABEL: Record<SpaceStage, string> = {
  drafting: 'Drafting',
  opening: 'Opening PR',
  in_review: 'In review',
  merged_locally: 'Merged locally',
  branch: 'Kept as branch',
  done: 'Merged',
};

/** Turns since the last ship that a Push would send. */
export function unshipped(
  s: Pick<Space, 'shipped' | 'head_sha'> & Partial<Pick<Space, 'fork_sha'>>,
): boolean {
  // A space adopted from a branch or PR starts as pushed: at that tip.
  const pushed = s.shipped?.head ?? s.fork_sha;
  return pushed != null && pushed !== s.head_sha;
}

/** What a space is called on screen: the chat agent's proposed title
 *  (GitHub #78 — a raw first line reads as a run-on sentence), else the
 *  first line of what you asked for, as you wrote it, else the space's
 *  name. The slug (`name`) is only for its branch; the goal's first line
 *  is still what Open PR / Merge locally defaults their own title to
 *  (`defaultTitle`, unrelated to this one). */
export function spaceTitle(
  s: Pick<Space, 'goal' | 'name' | 'title'> & { branch?: string },
): string {
  const proposed = s.title?.trim();
  const raw = proposed && proposed.length > 0 ? proposed : defaultTitle(s);
  // Short enough for a sidebar row or a notification: no trailing period
  // (it reads as "formatter.: turn done"), cut at a word near 60 chars.
  const t = raw.replace(/[.。!]+$/u, '');
  if (t.length <= 60) return t;
  const cut = t.slice(0, 60);
  const word = cut.lastIndexOf(' ');
  return `${(word > 30 ? cut.slice(0, word) : cut).replace(/[,;:]$/, '')}…`;
}

/** A space row's label: its PR's number in front, unless the title
 *  already opens with it (a space started from PR #113 is titled
 *  "#113 …" itself). */
export function withPrNumber(
  number: number | undefined,
  title: string,
): string {
  if (number == null) return title;
  const tag = `#${number}`;
  return new RegExp(`^${tag}(?!\\d)`).test(title) ? title : `${tag} ${title}`;
}

/** The goal's first line; with no goal the slug — or, when that is only the
 *  placeholder `space` / `space-2` (a space started from a branch with no
 *  note, before its start named it), the branch's name. */
export function defaultTitle(
  s: Pick<Space, 'goal' | 'name'> & { branch?: string },
): string {
  const line = s.goal
    .split('\n')
    .map((l) => l.trim())
    .find((l) => l.length > 0);
  if (line) return line;
  const placeholder = /^space(-\d+)?$/.test(s.name);
  return (placeholder && s.branch?.trim()) || s.name;
}

/** The title Open PR proposes: the goal's (`defaultTitle`) — or after a
 *  Continue, nothing: left empty, the core writes it from the round's net
 *  change (`ship_title.rs`), never from a sentence in the chat. */
export function shipTitle(
  s: Pick<Space, 'goal' | 'name' | 'rounds'> & { branch?: string },
): string {
  return s.rounds?.length ? '' : defaultTitle(s);
}

/** "3 files +42 −7" for the changes pane header. */
export function diffStat(
  files: { additions: number; deletions: number }[],
): string {
  const add = files.reduce((n, f) => n + f.additions, 0);
  const del = files.reduce((n, f) => n + f.deletions, 0);
  const n = files.length;
  return `${n} file${n === 1 ? '' : 's'}  +${add} −${del}`;
}

/** What a key does on a space page (outside the composer). */
export type SpaceKeyAction =
  | 'focus'
  | 'mode'
  | 'review'
  | 'run'
  | 'ship'
  | 'ship-alt'
  | 'open-pr'
  | 'changes'
  | 'archive';

/** `key` is the layout-independent letter (`latinKey`). Archive needs ⇧ —
 *  it removes the worktree, so a stray `e` must not do it. */
export function spaceKeyAction(
  key: string,
  shift: boolean,
): SpaceKeyAction | null {
  switch (key) {
    case 'c':
    case '/':
      return shift ? null : 'focus';
    case 'm':
      return shift ? null : 'mode';
    case 'd':
      // D shows / hides the changes: the conversation takes the page.
      return shift ? null : 'changes';
    case 'r':
      // ⇧R starts / stops the space's `run` command.
      return shift ? 'run' : 'review';
    case 'p':
      return shift ? 'ship-alt' : 'ship';
    case 'o':
      return shift ? null : 'open-pr';
    case 'e':
      return shift ? 'archive' : null;
    default:
      return null;
  }
}

const MODE_ORDER: SpaceMode[] = ['ask', 'plan', 'edit', 'run'];

/** `M` cycles Ask → Plan → Edit → Run → Ask. */
export function nextMode(m: SpaceMode): SpaceMode {
  return MODE_ORDER[(MODE_ORDER.indexOf(m) + 1) % MODE_ORDER.length]!;
}

/** The space a review belongs to — its host PR or its own branch review —
 *  while it can still take a turn. "Send to agent" shows only then. */
export function spaceOfReview(spaces: Space[], pr: PrId): Space | undefined {
  return spaces.find(
    (s) => s.status !== 'archived' && (s.pr === pr || s.review_pr === pr),
  );
}

// ── review with agent (docs/spaces.md §3) ───────────────────────────────────

/** The reviewer agent setting (`Settings → Spaces`'s picker lives in the
 *  Review menu itself): sticky per machine, independent of the space's own
 *  writing agent. */
export const REVIEW_AGENT_KEY = 'spaces.review_agent';

/** The built-in skill "Review with agent" runs when none is picked: a
 *  general review, the same one `/bugs` runs by hand. */
export const DEFAULT_REVIEW_SKILL = 'bugs';

/** Which skill id "Review with agent" runs: the default built-in if it's
 *  still offered (not forked away under the same id), else any other
 *  built-in, else whatever the list offers — there is always something
 *  once skills have loaded. */
export function defaultReviewSkill(
  skills: Pick<SkillInfo, 'id' | 'source'>[],
): string {
  const builtins = skills.filter((s) => s.source === 'built-in');
  return (
    builtins.find((s) => s.id === DEFAULT_REVIEW_SKILL)?.id ??
    builtins[0]?.id ??
    skills[0]?.id ??
    DEFAULT_REVIEW_SKILL
  );
}

/** The reviewer agent a space's "Review with agent" starts from: your
 *  sticky pick, else the one `agents.skills` runs by default, else the
 *  first agent on offer. */
export function reviewAgentDefault(
  settings: Pick<Setting, 'key' | 'scope' | 'value'>[],
  agents: AgentId[],
): AgentId {
  const picked = userSetting(settings, REVIEW_AGENT_KEY)?.value;
  if (typeof picked === 'string' && agents.includes(picked as AgentId)) {
    return picked as AgentId;
  }
  const skillDefaults = userSetting(settings, 'agents.skills')?.value;
  const fromSkills = Array.isArray(skillDefaults)
    ? skillDefaults.find((a): a is AgentId => agents.includes(a as AgentId))
    : undefined;
  return fromSkills ?? agents[0] ?? 'claude';
}

/** A "Review with agent" run on the space's own review (`run_skill` on
 *  `Space.review_pr`) still going — shown as "Reviewing…" without
 *  touching `Space.status`, which stays the writing agent's. */
export function reviewRunning(
  runs: readonly Pick<SkillRun, 'status'>[],
): boolean {
  return runs.some((r) => r.status === 'running');
}

// ── send all to agent (docs/spaces.md §3) ───────────────────────────────────

/** Same item, independent of object identity — `SpaceFeedback` is a plain
 *  `{ kind, id }` tag. */
function sameFeedback(a: SpaceFeedback, b: SpaceFeedback): boolean {
  return a.kind === b.kind && a.id === b.id;
}

/** The turn `item` rode as part of a "Send N to agent", `undefined` if it
 *  was never sent that way (the per-item "Send to agent" doesn't mark
 *  anything, so an item sent only that way still reads as unsent here). */
export function sentTurnOf(
  space: Pick<Space, 'sent_feedback'>,
  item: SpaceFeedback,
): number | undefined {
  return (space.sent_feedback ?? []).find((s) => sameFeedback(s.item, item))
    ?.turn;
}

/** The turn that carried `turn` has run to completion (failed or not): a
 *  finding it addressed can be dismissed in bulk, and the space is done
 *  enough to try again. */
export function sentTurnDone(
  space: Pick<Space, 'status' | 'turns'>,
  turn: number,
): boolean {
  return space.status !== 'running' && space.turns >= turn;
}

export type UndoState =
  | { ok: true; turn: SpaceTurn }
  | { ok: false; turn: SpaceTurn | null; reason: string };

/** What "Undo last turn" would undo — the last turn not undone yet — or
 *  why it can't. Mirrors the core's rules (`rewindable`): not while
 *  running, not archived, not once the turn shipped. */
export function undoState(s: RewindFields): UndoState {
  const log = s.turn_log ?? [];
  const i = lastLive(log);
  const turn = i >= 0 ? log[i]! : null;
  const reason = rewindReason(s, i);
  return reason == null && turn
    ? { ok: true, turn }
    : { ok: false, turn, reason: reason ?? 'No turn to undo' };
}

type RewindFields = Pick<
  Space,
  'status' | 'turn_log' | 'head_sha' | 'shipped' | 'base_sha'
> &
  Partial<Pick<Space, 'updating'>>;

function lastLive(log: SpaceTurn[]): number {
  let i = log.length - 1;
  while (i >= 0 && log[i]!.undone) i--;
  return i;
}

/** Why the space can't go back to where `turn_log[i]` started (it and
 *  every later turn undone), or `null` when it can — the core's
 *  `rewindable`. */
function rewindReason(s: RewindFields, i: number): string | null {
  const log = s.turn_log ?? [];
  if (s.status === 'running') return 'The agent is still working';
  if (s.status === 'archived') return 'Archived';
  if (s.updating) return 'Finish or abort the update from base first';
  const last = lastLive(log);
  if (i < 0 || last < 0) return 'No turn to undo';
  if (log[last]!.after !== s.head_sha) {
    return 'The space moved since its last turn';
  }
  const shipped = s.shipped;
  if (shipped) {
    const earlier = log
      .slice(0, i)
      .filter((t) => !t.undone)
      .flatMap((t) => [t.before, t.after]);
    const beforeTurn =
      shipped.head === log[i]!.before ||
      shipped.head === s.base_sha ||
      earlier.includes(shipped.head);
    if (!beforeTurn) {
      const what =
        shipped.how === 'pr'
          ? 'Already pushed'
          : shipped.how === 'merged_locally'
            ? 'Already merged'
            : 'Already written as a branch';
      return `${what} — ask the agent to revert it instead`;
    }
  }
  return null;
}

/** "Back to here" on the reply of a live turn that isn't the last one:
 *  `null` when there is nothing after it to undo; else whether it can go
 *  back (`reason` says why not). */
export function rewindState(
  s: RewindFields,
  reply: string,
): { ok: boolean; reason: string | null } | null {
  const log = s.turn_log ?? [];
  const k = log.findIndex((t) => t.reply === reply && !t.undone);
  if (k < 0) return null;
  const i = log.findIndex((t, j) => j > k && !t.undone);
  if (i < 0) return null;
  const reason = rewindReason(s, i);
  return { ok: reason == null, reason };
}

/** The last agent turn not undone, which "Revert the last turn's change"
 *  restores a file from; `null` when there is none. */
export function lastAgentTurn(s: Pick<Space, 'turn_log'>): SpaceTurn | null {
  const log = s.turn_log ?? [];
  for (let i = log.length - 1; i >= 0; i--) {
    const t = log[i]!;
    if (!t.undone && !(t.reverted ?? []).length) return t;
  }
  return null;
}

/** Agent turns the space stands on: `turns` minus the undone ones (a
 *  Revert of yours isn't a turn). "after turn N" in the changes pane. */
export function liveTurns(s: Pick<Space, 'turns' | 'turn_log'>): number {
  const undone = (s.turn_log ?? []).filter(
    (t) => t.undone && !(t.reverted ?? []).length,
  ).length;
  return Math.max(0, s.turns - undone);
}

/** Message ids (yours and the agent's) of the space's undone turns. */
export function undoneMessages(s: Pick<Space, 'turn_log'>): Set<string> {
  return new Set(
    (s.turn_log ?? [])
      .filter((t) => t.undone)
      .flatMap((t) => [t.message, t.reply, ...(t.steered ?? [])]),
  );
}

/** Native "turn finished" notifications (Settings → Labs); on unless the
 *  user turned them off. */
export const NOTIFY_KEY = 'spaces.notify';

export function notifyOn(
  settings: Pick<Setting, 'key' | 'scope' | 'value'>[],
): boolean {
  return userSetting(settings, NOTIFY_KEY)?.value !== false;
}

/** The native notification for a turn that just ended, `null` when none
 *  did: "<space>: turn finished" / "<space>: turn failed". */
export function nativeNotice(
  prev: Pick<Space, 'status'> | undefined,
  next: EndedFields,
): { title: string; body?: string } | null {
  if (prev?.status !== 'running' || next.status === 'running') return null;
  if (next.status === 'failed') {
    const title = `${spaceTitle(next)}: ${failedWhat(next)}`;
    return next.error ? { title, body: plainError(next.error) } : { title };
  }
  return { title: `${spaceTitle(next)}: turn finished` };
}

/** The agent just asked you something (`ask_user`): the notice, `null`
 *  when no new question came — "<space>: question" and the first one. */
export function questionAsked(
  prev: Pick<Space, 'question'> | undefined,
  next: Pick<Space, 'question' | 'name' | 'goal'>,
): { title: string; body: string } | null {
  const q = next.question;
  if (!q || prev?.question?.id === q.id) return null;
  return {
    title: `${spaceTitle(next)}: question`,
    body: q.questions[0]?.question ?? '',
  };
}

/** The dock badge: spaces whose last turn ended after you last looked
 *  (finished or failed), and spaces whose agent waits on your answer;
 *  archived ones aside. */
export function waitingSpaces(
  spaces: (UnreadFields & Pick<Space, 'id' | 'question'>)[],
  seen: (id: string) => number | undefined,
): number {
  return spaces.filter(
    (s) =>
      (s.status === 'running' && !!s.question) ||
      ((s.status === 'idle' || s.status === 'failed') &&
        hasNews(s, seen(s.id))),
  ).length;
}

/** The sidebar's `⎇ #n` tint (docs/spaces.md §3): the PR's checks while
 *  it's open, else how it ended. */
export type PrTint = 'pass' | 'fail' | 'pending' | 'none' | 'merged' | 'closed';

export function prTint(
  pr: Pick<PrSummary, 'state' | 'checks'> | undefined,
): PrTint {
  if (!pr) return 'none';
  if (pr.state === 'merged') return 'merged';
  if (pr.state === 'closed') return 'closed';
  return pr.checks;
}

/** The method Merge uses: the host's default if the repo allows it, else
 *  the first allowed one (the same rule as `merge_space`). */
export function defaultMergeMethod(info: MergeInfo): MergeMethod {
  const allowed = info.allowed_methods;
  const d = info.default_method;
  if (d && (allowed.length === 0 || allowed.includes(d))) return d;
  return allowed[0] ?? 'squash';
}

/** What the space header's Merge does now, or why it can't (mirrors
 *  `merge_space` in the core). */
export type SpaceMerge =
  { kind: 'merge' | 'auto' | 'queue' } | { kind: 'none'; reason: string };

/** The Checks tab's Merge line, agreeing with what blocks Merge: a draft
 *  or conflicts on the PR row win over a `merge_info` that still says
 *  clean. `labels`: the merge dialog's state wording. */
export function mergeLine(
  pr: Pick<PrSummary, 'state' | 'conflicts'>,
  info: MergeInfo | null,
  labels: Record<MergeInfo['state'], string>,
): { ok: boolean | null; text: string } {
  if (pr.state === 'merged') return { ok: true, text: 'Merged' };
  if (pr.state === 'closed')
    return { ok: false, text: 'Closed without merging' };
  if (pr.state === 'draft') return { ok: false, text: DRAFT_REASON };
  if (pr.conflicts || info?.state === 'dirty')
    return { ok: false, text: labels.dirty };
  if (!info) return { ok: null, text: 'Checking…' };
  const extra = `${info.auto_merge_enabled ? ' · auto-merge on' : ''}${info.in_queue ? ' · in the merge queue' : ''}`;
  return {
    ok: info.state === 'clean' && info.mergeable !== false,
    text: `${labels[info.state]}${extra}`,
  };
}

/** How often a space page asks for its PR (`refresh_space_pr`): often
 *  while checks run, now and then otherwise, never once it's done. */
export function prPollMs(
  pr: Pick<PrSummary, 'state' | 'checks'> | undefined,
): number | null {
  if (pr?.state === 'merged' || pr?.state === 'closed') return null;
  return !pr || pr.checks === 'pending' ? 10_000 : 30_000;
}

const DRAFT_REASON = 'A draft: mark it ready for review first';
const CONFLICT_REASON = 'Conflicts with the base branch';

export function spaceMerge(
  info: MergeInfo | null,
  s: Pick<Space, 'shipped' | 'head_sha' | 'status'> &
    Partial<Pick<Space, 'fork_sha'>>,
  /** The PR row: its draft state and conflicts can be newer than `info`. */
  pr?: Pick<PrSummary, 'state' | 'conflicts'>,
): SpaceMerge {
  if (s.status === 'running')
    return { kind: 'none', reason: 'The agent is still working' };
  if (pr?.state === 'draft') return { kind: 'none', reason: DRAFT_REASON };
  if ((s.shipped == null || s.shipped.how === 'pr') && unshipped(s))
    return { kind: 'none', reason: 'Push the new turns first' };
  if (pr?.conflicts) return { kind: 'none', reason: CONFLICT_REASON };
  if (!info) return { kind: 'none', reason: 'Checking whether it can merge…' };
  if (info.in_queue) return { kind: 'none', reason: 'In the merge queue' };
  if (info.auto_merge_enabled)
    return {
      kind: 'none',
      reason: 'Auto-merge is on: it merges once checks and reviews pass',
    };
  if (info.merge_queue) return { kind: 'queue' };
  const ready =
    info.mergeable !== false &&
    (info.state === 'clean' || info.state === 'unstable');
  if (ready) return { kind: 'merge' };
  if (info.state === 'dirty')
    return { kind: 'none', reason: 'Conflicts with the base branch' };
  if (info.auto_merge_allowed) return { kind: 'auto' };
  return {
    kind: 'none',
    reason:
      info.state === 'blocked'
        ? "Required checks or reviews haven't passed"
        : info.state === 'behind'
          ? 'Behind the base branch'
          : 'The host is still checking',
  };
}

/** "Continue" is offered once the space's PR merged (docs/spaces.md §7). */
export function canContinue(
  s: Pick<Space, 'pr' | 'status'>,
  prState: PrSummary['state'] | undefined,
): boolean {
  return (
    s.pr != null &&
    s.status !== 'running' &&
    s.status !== 'archived' &&
    prState === 'merged'
  );
}

/** Plan turns' plans, by the reply that carries them. */
export function plansByReply(
  log: Pick<SpaceTurn, 'reply' | 'plan' | 'undone'>[] | undefined,
): Map<string, string> {
  return new Map(
    (log ?? [])
      .filter((t) => !t.undone && t.plan)
      .map((t) => [t.reply, t.plan!]),
  );
}

/** The reply whose plan Approve / Revise act on: the last turn's, while
 *  the space can take a turn. `null` otherwise. */
export function actionablePlan(
  s: Pick<Space, 'status' | 'turn_log'>,
): string | null {
  if (s.status === 'running' || s.status === 'archived') return null;
  const log = (s.turn_log ?? []).filter((t) => !t.undone);
  const last = log[log.length - 1];
  return last?.plan ? last.reply : null;
}

/** The drawers under a space page. */
export type Drawer = 'logs' | 'terminal';

/** One drawer at a time: opening one closes the other, its own toggle
 *  closes it. */
export function toggleDrawer(
  current: Drawer | null,
  which: Drawer,
): Drawer | null {
  return current === which ? null : which;
}
