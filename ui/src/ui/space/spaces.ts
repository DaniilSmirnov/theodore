/**
 * Spaces (docs/spaces.md): pure rules for the hidden build mode — whether
 * it's on, which spaces a workspace lists, what each one's sidebar glyph
 * says, and which "branch reviews" are really a space's live diff.
 */
import type {
  AgentId,
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
 *  ones left out, most recently active first. */
export function spacesIn(ws: WorkspaceId, spaces: Space[]): Space[] {
  return spaces
    .filter((s) => s.status !== 'archived')
    .filter((s) => ws === ALL || s.project === ws)
    .sort((a, b) => b.updated_at - a.updated_at);
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

/** What the sidebar shows next to a space (docs/spaces.md §3): a finished
 *  turn you haven't looked at reads as waiting for you. `seenAt` is when
 *  the space page was last open, kept locally. */
export function spaceGlyph(
  s: Pick<Space, 'status' | 'updated_at' | 'turns'>,
  seenAt: number | undefined,
): SpaceGlyph {
  if (s.status === 'running') return 'running';
  if (s.status === 'failed') return 'failed';
  if (s.turns > 0 && (seenAt ?? 0) < s.updated_at) return 'unseen';
  return 'idle';
}

/** A turn just ended (running → anything else): what to tell you if you're
 *  not looking at the space. `null` when nothing finished. */
export function turnEnded(
  prev: Pick<Space, 'status'> | undefined,
  next: Pick<Space, 'status' | 'name' | 'error' | 'goal' | 'title'>,
): { level: 'info' | 'error'; text: string } | null {
  if (prev?.status !== 'running' || next.status === 'running') return null;
  if (next.status === 'failed') {
    return {
      level: 'error',
      text: `${spaceTitle(next)}: turn failed${next.error ? ` — ${next.error}` : ''}`,
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
export function unshipped(s: Pick<Space, 'shipped' | 'head_sha'>): boolean {
  return s.shipped != null && s.shipped.head !== s.head_sha;
}

/** What a space is called on screen: the chat agent's proposed title
 *  (GitHub #78 — a raw first line reads as a run-on sentence), else the
 *  first line of what you asked for, as you wrote it, else the space's
 *  name. The slug (`name`) is only for its branch; the goal's first line
 *  is still what Open PR / Merge locally defaults their own title to
 *  (`defaultTitle`, unrelated to this one). */
export function spaceTitle(s: Pick<Space, 'goal' | 'name' | 'title'>): string {
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

export function defaultTitle(s: Pick<Space, 'goal' | 'name'>): string {
  const line = s.goal
    .split('\n')
    .map((l) => l.trim())
    .find((l) => l.length > 0);
  return line ?? s.name;
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

const MODE_ORDER: SpaceMode[] = ['ask', 'edit', 'run'];

/** `M` cycles Ask → Edit → Run → Ask. */
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
 *  why it can't. Mirrors the core's rules (`undoable_turn`): not while
 *  running, not archived, not once the turn shipped. */
export function undoState(
  s: Pick<Space, 'status' | 'turn_log' | 'head_sha' | 'shipped' | 'base_sha'>,
): UndoState {
  const log = s.turn_log ?? [];
  let i = log.length - 1;
  while (i >= 0 && log[i]!.undone) i--;
  const turn = i >= 0 ? log[i]! : null;
  const no = (reason: string): UndoState => ({ ok: false, turn, reason });
  if (s.status === 'running') return no('The agent is still working');
  if (s.status === 'archived') return no('Archived');
  if (!turn) return no('No turn to undo');
  if (turn.after !== s.head_sha) {
    return no('The space moved since its last turn');
  }
  const shipped = s.shipped;
  if (shipped) {
    const earlier = log
      .slice(0, i)
      .filter((t) => !t.undone)
      .flatMap((t) => [t.before, t.after]);
    const beforeTurn =
      shipped.head === turn.before ||
      shipped.head === s.base_sha ||
      earlier.includes(shipped.head);
    if (!beforeTurn) {
      const what =
        shipped.how === 'pr'
          ? 'Already pushed'
          : shipped.how === 'merged_locally'
            ? 'Already merged'
            : 'Already written as a branch';
      return no(`${what} — ask the agent to revert it instead`);
    }
  }
  return { ok: true, turn };
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
  next: Pick<Space, 'status' | 'name' | 'error' | 'goal' | 'title'>,
): { title: string; body?: string } | null {
  if (prev?.status !== 'running' || next.status === 'running') return null;
  if (next.status === 'failed') {
    const title = `${spaceTitle(next)}: turn failed`;
    return next.error ? { title, body: next.error } : { title };
  }
  return { title: `${spaceTitle(next)}: turn finished` };
}

/** The dock badge: spaces whose last turn ended after you last looked
 *  (finished or failed), archived ones aside. */
export function waitingSpaces(
  spaces: Pick<Space, 'id' | 'status' | 'updated_at' | 'turns'>[],
  seen: (id: string) => number | undefined,
): number {
  return spaces.filter(
    (s) =>
      (s.status === 'idle' || s.status === 'failed') &&
      s.turns > 0 &&
      (seen(s.id) ?? 0) < s.updated_at,
  ).length;
}
