import { describe, expect, it } from 'vitest';
import type { Space, SpaceTurn } from '../../gen/contract';
import { ALL } from '../../core/workspaces';
import {
  NOTIFY_KEY,
  archivedSpacesIn,
  nativeNotice,
  notifyOn,
  spaceOfReview,
  undoState,
  undoneMessages,
  waitingSpaces,
  questionAsked,
  plansByReply,
  actionablePlan,
} from './spaces';

const space = (over: Partial<Space> & { id: string }): Space => ({
  project: 'local:app',
  name: over.id,
  goal: '',
  branch: `space/${over.id}`,
  base: 'main',
  base_sha: 'base',
  head_sha: 'base',
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

const turn = (n: number, before: string, after: string, undone = false) =>
  ({
    message: `m${n}`,
    reply: `r${n}`,
    before,
    after,
    undone,
  }) satisfies SpaceTurn;

const twoTurns = [turn(1, 'base', 't1'), turn(2, 't1', 't2')];

describe('archived spaces', () => {
  it('are listed per workspace, latest first, apart from live ones', () => {
    const spaces = [
      space({ id: 'old', status: 'archived', updated_at: 1 }),
      space({ id: 'new', status: 'archived', updated_at: 2 }),
      space({ id: 'live', updated_at: 3 }),
      space({ id: 'other', status: 'archived', project: 'local:other' }),
    ];
    expect(archivedSpacesIn('local:app', spaces).map((s) => s.id)).toEqual([
      'new',
      'old',
    ]);
    expect(archivedSpacesIn(ALL, spaces)).toHaveLength(3);
  });
});

describe('send to agent', () => {
  const spaces = [
    space({ id: 'a', review_pr: 'local:x#1' }),
    space({ id: 'b', pr: 'github:o/r#7', review_pr: 'local:x#2' }),
    space({ id: 'c', pr: 'github:o/r#9', status: 'archived' }),
  ];

  it('finds the space by its PR or its own review', () => {
    expect(spaceOfReview(spaces, 'local:x#1')?.id).toBe('a');
    expect(spaceOfReview(spaces, 'github:o/r#7')?.id).toBe('b');
    expect(spaceOfReview(spaces, 'local:x#2')?.id).toBe('b');
  });

  it('is absent on other PRs and on archived spaces', () => {
    expect(spaceOfReview(spaces, 'github:o/r#1')).toBeUndefined();
    expect(spaceOfReview(spaces, 'github:o/r#9')).toBeUndefined();
  });
});

describe('undo last turn', () => {
  it('undoes the last turn not undone yet', () => {
    const s = space({ id: 's', head_sha: 't2', turn_log: twoTurns });
    expect(undoState(s)).toEqual({ ok: true, turn: twoTurns[1] });
    const after = space({
      id: 's',
      head_sha: 't1',
      turn_log: [twoTurns[0]!, { ...twoTurns[1]!, undone: true }],
    });
    expect(undoState(after)).toEqual({ ok: true, turn: twoTurns[0] });
  });

  it('refuses while running, archived, or with nothing to undo', () => {
    const base = { head_sha: 't2', turn_log: twoTurns };
    const reason = (s: Space) => {
      const u = undoState(s);
      return u.ok ? null : u.reason;
    };
    expect(reason(space({ id: 's', ...base, status: 'running' }))).toMatch(
      /still working/,
    );
    expect(reason(space({ id: 's', ...base, status: 'archived' }))).toBe(
      'Archived',
    );
    expect(reason(space({ id: 's' }))).toBe('No turn to undo');
    expect(reason(space({ id: 's', ...base, head_sha: 'elsewhere' }))).toMatch(
      /moved/,
    );
  });

  it('refuses a shipped turn but not one after the ship', () => {
    const shipped = (head: string, how: 'pr' | 'branch' = 'pr') => ({
      how,
      branch: 'space/s',
      sha: 'squash',
      head,
      pushes: 1,
      at: 0,
    });
    const at = (head: string, how?: 'pr' | 'branch') =>
      undoState(
        space({
          id: 's',
          head_sha: 't2',
          turn_log: twoTurns,
          shipped: shipped(head, how),
        }),
      );
    expect(at('t2')).toMatchObject({ ok: false, reason: /Already pushed/ });
    expect(at('t2', 'branch')).toMatchObject({
      ok: false,
      reason: /written as a branch/,
    });
    expect(at('t1').ok).toBe(true);
    expect(at('base').ok).toBe(true);
    expect(at('unknown').ok).toBe(false);
  });

  it('marks undone turns in the transcript', () => {
    const s = space({
      id: 's',
      turn_log: [twoTurns[0]!, { ...twoTurns[1]!, undone: true }],
    });
    expect([...undoneMessages(s)].sort()).toEqual(['m2', 'r2']);
    expect(undoneMessages(space({ id: 'x' })).size).toBe(0);
  });
});

describe('notifications', () => {
  it('are on unless turned off', () => {
    expect(notifyOn([])).toBe(true);
    expect(notifyOn([{ key: NOTIFY_KEY, scope: 'user', value: false }])).toBe(
      false,
    );
    expect(notifyOn([{ key: NOTIFY_KEY, scope: 'user', value: true }])).toBe(
      true,
    );
  });

  it('say a turn finished or failed, only when one ended', () => {
    const running = { status: 'running' as const };
    expect(
      nativeNotice(running, {
        status: 'idle',
        name: 'fix-login',
        goal: 'Fix the login redirect',
        error: null,
      }),
    ).toEqual({ title: 'Fix the login redirect: turn finished' });
    expect(
      nativeNotice(running, {
        status: 'failed',
        name: 'fix',
        goal: '',
        error: 'boom',
      }),
    ).toEqual({ title: 'fix: turn failed', body: 'boom' });
    expect(
      nativeNotice(
        { status: 'idle' },
        { status: 'idle', name: 'x', goal: '', error: null },
      ),
    ).toBeNull();
    expect(
      nativeNotice(undefined, { ...running, name: 'x', goal: '', error: null }),
    ).toBe(null);
  });

  it('badge the spaces with a turn you have not seen', () => {
    const spaces = [
      space({ id: 'done', turns: 1, updated_at: 10 }),
      space({ id: 'failed', status: 'failed', turns: 2, updated_at: 10 }),
      space({ id: 'seen', turns: 1, updated_at: 10 }),
      space({ id: 'running', status: 'running', turns: 1, updated_at: 10 }),
      space({ id: 'fresh', turns: 0, updated_at: 10 }),
      space({ id: 'gone', status: 'archived', turns: 1, updated_at: 10 }),
    ];
    const seen = (id: string) => (id === 'seen' ? 10 : undefined);
    expect(waitingSpaces(spaces, seen)).toBe(2);
    // A turn waiting on your answer counts too.
    const asking = space({
      id: 'asking',
      status: 'running',
      turns: 1,
      question: { id: 'q', run: 'r', questions: [], asked_at: 0 },
    });
    expect(waitingSpaces([...spaces, asking], seen)).toBe(3);
  });

  it('a new question is a notice once, with its first question', () => {
    const q = {
      id: 'q1',
      run: 'r',
      questions: [{ question: 'Which store?', options: ['a', 'b'] }],
      asked_at: 0,
    };
    const next = { name: 'fix', goal: '', question: q };
    expect(questionAsked({ question: null }, next)).toEqual({
      title: 'fix: question',
      body: 'Which store?',
    });
    expect(questionAsked({ question: q }, next)).toBeNull();
    expect(questionAsked(undefined, { ...next, question: null })).toBeNull();
  });

  it('only the last, live plan turn gets Approve / Revise', () => {
    const t = (reply: string, plan: string | null, undone = false) =>
      ({ reply, plan, undone }) as never;
    const log = [t('a1', '1. old'), t('a2', null), t('a3', '1. new')];
    expect(plansByReply(log)).toEqual(
      new Map([
        ['a1', '1. old'],
        ['a3', '1. new'],
      ]),
    );
    expect(actionablePlan({ status: 'idle', turn_log: log })).toBe('a3');
    expect(actionablePlan({ status: 'running', turn_log: log })).toBeNull();
    expect(
      actionablePlan({ status: 'idle', turn_log: [...log, t('a4', null)] }),
    ).toBeNull();
    expect(
      actionablePlan({
        status: 'idle',
        turn_log: [t('a1', '1. p'), t('a2', null, true)],
      }),
    ).toBe('a1');
  });
});
