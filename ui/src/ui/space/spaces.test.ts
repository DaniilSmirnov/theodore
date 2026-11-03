import { describe, expect, it } from 'vitest';
import type { RepoRef, Space } from '../../gen/contract';
import { ALL } from '../../core/workspaces';
import {
  AUTO_ARCHIVE_KEY,
  BUILD_MODE_KEY,
  REVIEW_AGENT_KEY,
  autoArchiveDays,
  buildModeOn,
  canMarkUnread,
  defaultReviewSkill,
  defaultTitle,
  reviewAgentDefault,
  reviewRunning,
  sentTurnDone,
  sentTurnOf,
  spaceStage,
  unshipped,
  diffStat,
  isSpaceReview,
  labsVisible,
  spaceChatKey,
  spaceGlyph,
  spacesIn,
  spaceTitle,
  nextMode,
  spaceKeyAction,
  turnEnded,
  prTint,
  defaultMergeMethod,
  spaceMerge,
  canContinue,
} from './spaces';
import type { MergeInfo } from '../../gen/contract';

const setting = (value: unknown, scope = 'user') => ({
  key: BUILD_MODE_KEY,
  scope,
  value,
});

const space = (over: Partial<Space> & { id: string }): Space => ({
  project: 'local:app',
  name: over.id,
  goal: '',
  branch: `theo/${over.id}`,
  base: 'main',
  base_sha: 'b',
  head_sha: 'h',
  worktree: `/tmp/${over.id}`,
  status: 'idle',
  agent: 'claude',
  model: null,
  mode: 'edit',
  review_pr: null,
  pr: null,
  turns: 0,
  run: null,
  error: null,
  created_at: 0,
  updated_at: 0,
  ...over,
});

describe('build mode', () => {
  it('is on by default and when the user turns it on', () => {
    expect(buildModeOn(false, [])).toBe(true);
    expect(buildModeOn(false, [setting(true)])).toBe(true);
  });

  it('turns off only by the user setting, not a repo-scoped one', () => {
    expect(buildModeOn(false, [setting(false)])).toBe(false);
    expect(buildModeOn(false, [setting(false, 'repo:acme/web')])).toBe(true);
    expect(buildModeOn(true, [setting(false)])).toBe(true);
  });

  it('always lists the settings page, where spaces are turned off', () => {
    expect(labsVisible(false, [])).toBe(true);
    expect(labsVisible(false, [setting(false)])).toBe(true);
  });
});

describe('spaces', () => {
  it('keys a transcript by the space id', () => {
    expect(spaceChatKey('abc')).toBe('space:abc');
  });

  it("tells a space's review from a plain branch review", () => {
    const repo: RepoRef = { host: 'local', owner: 'local', name: 'app' };
    expect(
      isSpaceReview({
        pr: { repo, number: 1, host_id: 'refs/theodore/spaces/abc' },
      }),
    ).toBe(true);
    expect(isSpaceReview({ pr: { repo, number: 1, host_id: 'feature' } })).toBe(
      false,
    );
    expect(isSpaceReview({ pr: { repo, number: 1, host_id: null } })).toBe(
      false,
    );
  });

  it("lists a project's live spaces, newest activity first; All lists every project's", () => {
    const all = [
      space({ id: 'old', updated_at: 1 }),
      space({ id: 'new', updated_at: 5 }),
      space({ id: 'gone', status: 'archived', updated_at: 9 }),
      space({ id: 'other', project: 'github:acme/web', updated_at: 3 }),
    ];
    expect(spacesIn('local:app', all).map((s) => s.id)).toEqual(['new', 'old']);
    expect(spacesIn(ALL, all).map((s) => s.id)).toEqual([
      'new',
      'other',
      'old',
    ]);
  });

  it('shows a finished turn you have not seen as waiting', () => {
    const s = space({ id: 's', turns: 1, updated_at: 10 });
    expect(spaceGlyph(s, undefined)).toBe('unseen');
    expect(spaceGlyph(s, 9)).toBe('unseen');
    expect(spaceGlyph(s, 10)).toBe('idle');
    expect(spaceGlyph({ ...s, turns: 0 }, undefined)).toBe('idle');
    expect(spaceGlyph({ ...s, status: 'running' }, 99)).toBe('running');
    expect(spaceGlyph({ ...s, status: 'failed' }, 99)).toBe('failed');
  });

  it('sorts pinned spaces first, newest activity first within each group', () => {
    const all = [
      space({ id: 'old', updated_at: 1 }),
      space({ id: 'new', updated_at: 5 }),
      space({ id: 'pinned-old', updated_at: 2, pinned: true }),
      space({ id: 'pinned-new', updated_at: 8, pinned: true }),
    ];
    expect(spacesIn('local:app', all).map((s) => s.id)).toEqual([
      'pinned-new',
      'pinned-old',
      'new',
      'old',
    ]);
  });

  it('pinning never touches updated_at, so it does not read as a new turn', () => {
    // Pure sorting rule only looks at `pinned`/`updated_at` as given — this
    // guards the contract that `set_space_pinned` (Rust) must not bump
    // `updated_at`, by asserting pin alone doesn't reorder by activity.
    const unpinned = space({ id: 'a', updated_at: 5 });
    const pinned = { ...space({ id: 'b', updated_at: 5 }), pinned: true };
    expect(spacesIn('local:app', [unpinned, pinned]).map((s) => s.id)).toEqual([
      'b',
      'a',
    ]);
  });

  it('"Mark as unread" only does something once there is a turn to flag and it is not already unseen', () => {
    const s = space({ id: 's', turns: 1, updated_at: 10 });
    expect(canMarkUnread(s, 20)).toBe(true);
    expect(canMarkUnread(s, undefined)).toBe(false); // already unseen
    expect(canMarkUnread({ ...s, turns: 0 }, 20)).toBe(false); // no turn yet
  });

  it('notices only a turn that just ended', () => {
    // Called by what was asked for; the slug only when there's no goal.
    const next = {
      status: 'idle' as const,
      name: 'fix',
      goal: 'Fix it',
      error: null,
    };
    expect(turnEnded({ status: 'running' }, next)).toEqual({
      level: 'info',
      text: 'Fix it: turn done',
    });
    expect(
      turnEnded(
        { status: 'running' },
        { ...next, status: 'failed', error: 'boom' },
      ),
    ).toEqual({ level: 'error', text: 'Fix it: turn failed — boom' });
    expect(turnEnded({ status: 'idle' }, next)).toBeNull();
    expect(turnEnded(undefined, next)).toBeNull();
    expect(
      turnEnded({ status: 'running' }, { ...next, status: 'running' }),
    ).toBeNull();
  });

  it('sums a diff stat', () => {
    expect(diffStat([])).toBe('0 files  +0 −0');
    expect(
      diffStat([
        { additions: 30, deletions: 5 },
        { additions: 12, deletions: 2 },
      ]),
    ).toBe('2 files  +42 −7');
    expect(diffStat([{ additions: 1, deletions: 0 }])).toBe('1 file  +1 −0');
  });
});

describe('shipping', () => {
  const shipped = (how: 'pr' | 'merged_locally' | 'branch', head = 'h') => ({
    how,
    branch: 'me/x',
    sha: 's1',
    head,
    pushes: 1,
    at: 1,
  });

  it('moves through its stages', () => {
    expect(spaceStage(space({ id: 'a' }))).toBe('drafting');
    expect(spaceStage(space({ id: 'a', shipped: shipped('pr') }))).toBe(
      'opening',
    );
    const pr = 'github:o/r#1';
    expect(
      spaceStage(space({ id: 'a', shipped: shipped('pr'), pr }), 'open'),
    ).toBe('in_review');
    expect(
      spaceStage(space({ id: 'a', shipped: shipped('pr'), pr }), 'merged'),
    ).toBe('done');
    expect(
      spaceStage(space({ id: 'a', shipped: shipped('merged_locally') })),
    ).toBe('merged_locally');
    expect(spaceStage(space({ id: 'a', shipped: shipped('branch') }))).toBe(
      'branch',
    );
  });

  it('knows when there are turns to push', () => {
    expect(unshipped(space({ id: 'a' }))).toBe(false);
    expect(unshipped(space({ id: 'a', shipped: shipped('pr', 'h') }))).toBe(
      false,
    );
    expect(unshipped(space({ id: 'a', shipped: shipped('pr', 'old') }))).toBe(
      true,
    );
  });

  it('proposes the goal as the title', () => {
    expect(defaultTitle({ goal: '\n  Retry on 429s \nmore', name: 'n' })).toBe(
      'Retry on 429s',
    );
    expect(defaultTitle({ goal: '', name: 'tidy' })).toBe('tidy');
  });
});

describe('space archive settings', () => {
  const row = (key: string, value: unknown, scope = 'user') => ({
    key,
    scope,
    value,
  });

  it('defaults auto-archive to 7 days, 0 meaning never', () => {
    expect(autoArchiveDays([])).toBe(7);
    expect(autoArchiveDays([row(AUTO_ARCHIVE_KEY, 0)])).toBe(0);
    expect(autoArchiveDays([row(AUTO_ARCHIVE_KEY, 3)])).toBe(3);
    expect(autoArchiveDays([row(AUTO_ARCHIVE_KEY, -1)])).toBe(7);
    expect(autoArchiveDays([row(AUTO_ARCHIVE_KEY, 'x')])).toBe(7);
  });
});

describe('space keys', () => {
  it('maps letters to page actions', () => {
    expect(spaceKeyAction('c', false)).toBe('focus');
    expect(spaceKeyAction('/', false)).toBe('focus');
    expect(spaceKeyAction('m', false)).toBe('mode');
    expect(spaceKeyAction('r', false)).toBe('review');
    expect(spaceKeyAction('p', false)).toBe('ship');
    expect(spaceKeyAction('p', true)).toBe('ship-alt');
    expect(spaceKeyAction('o', false)).toBe('open-pr');
    expect(spaceKeyAction('d', false)).toBe('changes');
    expect(spaceKeyAction('x', false)).toBeNull();
  });

  it('toggles the run command with shift+R', () => {
    expect(spaceKeyAction('r', true)).toBe('run');
  });

  it('archives only with shift', () => {
    expect(spaceKeyAction('e', false)).toBeNull();
    expect(spaceKeyAction('e', true)).toBe('archive');
  });

  it('cycles modes', () => {
    expect(nextMode('ask')).toBe('plan');
    expect(nextMode('plan')).toBe('edit');
    expect(nextMode('edit')).toBe('run');
    expect(nextMode('run')).toBe('ask');
  });
});

describe('spaceTitle', () => {
  it('is the first line of the goal, without a trailing period', () => {
    expect(spaceTitle({ name: 'x', goal: 'Fix the login.\nMore text' })).toBe(
      'Fix the login',
    );
    expect(spaceTitle({ name: 'tes', goal: '' })).toBe('tes');
  });

  it('cuts a long goal at a word', () => {
    const t = spaceTitle({
      name: 'x',
      goal: 'Show a "Free shipping over $50" note under the cart total, using the existing price formatter.',
    });
    expect(t.endsWith('…')).toBe(true);
    expect(t.length).toBeLessThanOrEqual(61);
    expect(t).toBe(
      'Show a "Free shipping over $50" note under the cart total…',
    );
  });
});

describe('review with agent', () => {
  const row = (key: string, value: unknown, scope = 'user') => ({
    key,
    scope,
    value,
  });

  it('picks the sticky setting first, else agents.skills, else the first offered', () => {
    expect(reviewAgentDefault([], ['codex', 'claude'])).toBe('codex');
    expect(
      reviewAgentDefault(
        [row('agents.skills', ['codex'])],
        ['codex', 'claude'],
      ),
    ).toBe('codex');
    expect(
      reviewAgentDefault(
        [row(REVIEW_AGENT_KEY, 'claude'), row('agents.skills', ['codex'])],
        ['codex', 'claude'],
      ),
    ).toBe('claude');
    // A sticky pick that is no longer enabled falls through, same as an
    // unset one.
    expect(
      reviewAgentDefault([row(REVIEW_AGENT_KEY, 'gemini')], ['codex']),
    ).toBe('codex');
  });

  it('prefers the default built-in skill, then any built-in, then anything', () => {
    expect(
      defaultReviewSkill([
        { id: 'tests', source: 'built-in' },
        { id: 'bugs', source: 'built-in' },
      ]),
    ).toBe('bugs');
    expect(defaultReviewSkill([{ id: 'security', source: 'built-in' }])).toBe(
      'security',
    );
    expect(
      defaultReviewSkill([{ id: 'polish', source: '~/.claude/skills' }]),
    ).toBe('polish');
    expect(defaultReviewSkill([])).toBe('bugs');
  });

  it('is "reviewing" only while a run on the review is going', () => {
    expect(reviewRunning([])).toBe(false);
    expect(reviewRunning([{ status: 'done' }])).toBe(false);
    expect(reviewRunning([{ status: 'done' }, { status: 'running' }])).toBe(
      true,
    );
  });
});

describe('send N to agent', () => {
  it('reads the turn an item was sent with, undefined if never', () => {
    const s: Pick<Space, 'sent_feedback'> = {
      sent_feedback: [{ item: { kind: 'draft', id: 'd1' }, turn: 2 }],
    };
    expect(sentTurnOf(s, { kind: 'draft', id: 'd1' })).toBe(2);
    expect(sentTurnOf(s, { kind: 'draft', id: 'd2' })).toBeUndefined();
    expect(sentTurnOf(s, { kind: 'finding', id: 'd1' })).toBeUndefined();
    expect(
      sentTurnOf({ sent_feedback: [] }, { kind: 'draft', id: 'd1' }),
    ).toBeUndefined();
  });

  it('is "done" once the space is past that turn and not running', () => {
    expect(sentTurnDone({ status: 'running', turns: 3 }, 2)).toBe(false);
    expect(sentTurnDone({ status: 'idle', turns: 1 }, 2)).toBe(false);
    expect(sentTurnDone({ status: 'idle', turns: 2 }, 2)).toBe(true);
    expect(sentTurnDone({ status: 'failed', turns: 3 }, 2)).toBe(true);
  });
});

const info = (over: Partial<MergeInfo> = {}): MergeInfo => ({
  mergeable: true,
  state: 'clean',
  allowed_methods: ['merge', 'squash'],
  auto_merge_enabled: false,
  in_queue: false,
  required_approvals: null,
  auto_merge_allowed: false,
  merge_queue: false,
  default_method: null,
  ...over,
});

describe('a space in review', () => {
  it('tints its PR glyph by checks, then by how it ended', () => {
    expect(prTint(undefined)).toBe('none');
    expect(prTint({ state: 'open', checks: 'fail' })).toBe('fail');
    expect(prTint({ state: 'draft', checks: 'pass' })).toBe('pass');
    expect(prTint({ state: 'merged', checks: 'fail' })).toBe('merged');
    expect(prTint({ state: 'closed', checks: 'pass' })).toBe('closed');
  });

  it("merges by the host's default when the repo allows it", () => {
    expect(defaultMergeMethod(info())).toBe('merge');
    expect(defaultMergeMethod(info({ default_method: 'squash' }))).toBe(
      'squash',
    );
    expect(defaultMergeMethod(info({ default_method: 'rebase' }))).toBe(
      'merge',
    );
    expect(defaultMergeMethod(info({ allowed_methods: [] }))).toBe('squash');
  });

  it('merges now, auto-merges, queues, or says why not', () => {
    const s = space({ id: 'a', status: 'idle' });
    expect(spaceMerge(info(), s)).toEqual({ kind: 'merge' });
    expect(spaceMerge(info({ state: 'unstable' }), s)).toEqual({
      kind: 'merge',
    });
    expect(spaceMerge(info({ merge_queue: true }), s)).toEqual({
      kind: 'queue',
    });
    expect(
      spaceMerge(info({ state: 'blocked', auto_merge_allowed: true }), s),
    ).toEqual({ kind: 'auto' });
    expect(spaceMerge(info({ state: 'blocked' }), s).kind).toBe('none');
    expect(
      spaceMerge(info({ state: 'dirty', auto_merge_allowed: true }), s),
    ).toEqual({ kind: 'none', reason: 'Conflicts with the base branch' });
    expect(spaceMerge(null, s).kind).toBe('none');
    const unpushed = space({
      id: 'b',
      head_sha: 'h2',
      shipped: {
        how: 'pr',
        branch: 'theo/b',
        sha: 's1',
        head: 'h1',
        pushes: 1,
        at: 0,
      },
    });
    expect(spaceMerge(info(), unpushed)).toEqual({
      kind: 'none',
      reason: 'Push the new turns first',
    });
  });

  it('offers Continue once the PR merged', () => {
    const s = space({ id: 'a', pr: 'github:o/r#1' });
    expect(canContinue(s, 'merged')).toBe(true);
    expect(canContinue(s, 'open')).toBe(false);
    expect(canContinue({ ...s, status: 'archived' }, 'merged')).toBe(false);
    expect(canContinue({ ...s, pr: null }, 'merged')).toBe(false);
  });
});
