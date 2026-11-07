import { describe, expect, it } from 'vitest';
import type { ChatMessage } from '../gen/contract';
import { aiTurns, feedOf, hiddenFindingsNote } from './SidePanel';

function msg(id: string, partial: Partial<ChatMessage>): ChatMessage {
  return {
    id,
    pr: 'github:acme/widgets#1',
    author: { kind: 'me' },
    text: '',
    refs: [],
    at: 0,
    failed: null,
    ...partial,
  };
}

describe('aiTurns', () => {
  const agent = (id: string, at: number, over: Partial<ChatMessage> = {}) =>
    msg(id, { author: { kind: 'agent', agent: 'claude' }, at, ...over });
  const run = (id: string, started_at: number) =>
    ({ id, started_at }) as unknown as Parameters<typeof aiTurns>[0][number];

  it('pairs each question with the replies under it', () => {
    const turns = aiTurns(
      [],
      [
        msg('q1', { text: 'why?', at: 1 }),
        agent('a1', 2),
        msg('q2', { text: 'and?', at: 3 }),
        agent('a2', 4),
        agent('a3', 5),
      ],
    );
    expect(turns.map((t) => [t.key, t.items.length])).toEqual([
      ['q1', 2],
      ['q2', 3],
    ]);
  });

  it('gives a reply the last question of yours before it', () => {
    const [turn] = aiTurns(
      [],
      [msg('q1', { text: 'why?', at: 1 }), agent('a1', 2), agent('a2', 3)],
    );
    const asked = turn!.items.map((i) =>
      i.kind === 'message' ? i.question : null,
    );
    expect(asked).toEqual(['', 'why?', 'why?']);
  });

  it('a reply with no question before it asks nothing again', () => {
    const [turn] = aiTurns([], [agent('a1', 1, { failed: 'boom' })]);
    expect(turn!.items[0]).toMatchObject({ question: '' });
  });

  it('a skill run starts its own turn, in time order', () => {
    const turns = aiTurns(
      [run('r1', 2)],
      [msg('q1', { at: 1 }), agent('a1', 3), msg('q2', { at: 4 })],
    );
    expect(turns.map((t) => t.key)).toEqual(['q1', 'r1', 'q2']);
    // The reply that landed after the run stays with the run's turn.
    expect(turns[1]!.items.map((i) => i.kind)).toEqual(['run', 'message']);
  });

  it('keys a turn by its first message, so it keeps its row', () => {
    const before = aiTurns([], [msg('q1', { at: 1 })]);
    const after = aiTurns([], [msg('q1', { at: 1 }), agent('a1', 2)]);
    expect(after[0]!.key).toBe(before[0]!.key);
  });
});

describe('hiddenFindingsNote', () => {
  it('names how many findings the repo config hid', () => {
    expect(hiddenFindingsNote({ hidden_findings: 1 })).toBe(
      '1 finding hidden by config',
    );
    expect(hiddenFindingsNote({ hidden_findings: 3 })).toBe(
      '3 findings hidden by config',
    );
  });

  it('says nothing when none were hidden', () => {
    expect(hiddenFindingsNote({ hidden_findings: 0 })).toBeNull();
    expect(hiddenFindingsNote({})).toBeNull();
  });
});

describe('feedOf', () => {
  it('interleaves skill runs and messages by time', () => {
    const run = (id: string, started_at: number) =>
      ({ id, started_at }) as unknown as Parameters<typeof feedOf>[0][number];
    const msg = (id: string, at: number) =>
      ({ id, at }) as unknown as Parameters<typeof feedOf>[1][number];
    const f = feedOf([run('r2', 30)], [msg('m1', 10), msg('m3', 40)]);
    expect(f.map((x) => (x.kind === 'run' ? x.run.id : x.message.id))).toEqual([
      'm1',
      'r2',
      'm3',
    ]);
  });
});
