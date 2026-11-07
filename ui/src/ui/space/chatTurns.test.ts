import { describe, expect, it } from 'vitest';
import type { ChatMessage } from '../../gen/contract';
import { chatRows, estimateRowHeight, pairTurns } from './chatTurns';
import { splitQueue } from './queue';

const me = (id: string, at = 1): ChatMessage => ({
  id,
  pr: 'space:s',
  author: { kind: 'me' },
  text: id,
  refs: [],
  at,
  failed: null,
});
const agent = (id: string, at = 1): ChatMessage => ({
  ...me(id, at),
  author: { kind: 'agent', agent: 'claude' },
});
const ids = (turns: ReturnType<typeof pairTurns>) =>
  turns.map((t) => [t.key, t.messages.map((m) => m.id)]);

describe('pairTurns', () => {
  it('pairs each of your messages with the replies under it', () => {
    expect(
      ids(pairTurns([me('u1'), agent('a1'), me('u2'), agent('a2')])),
    ).toEqual([
      ['u1', ['u1', 'a1']],
      ['u2', ['u2', 'a2']],
    ]);
  });

  it('a message still waiting for its reply is its own turn', () => {
    expect(ids(pairTurns([me('u1'), agent('a1'), me('u2')]))).toEqual([
      ['u1', ['u1', 'a1']],
      ['u2', ['u2']],
    ]);
  });

  it('two of your messages in a row: the reply goes with the last', () => {
    expect(ids(pairTurns([me('u1'), me('u2'), agent('a1')]))).toEqual([
      ['u1', ['u1']],
      ['u2', ['u2', 'a1']],
    ]);
  });

  it('a reply with no message before it starts its own turn', () => {
    expect(ids(pairTurns([agent('a0'), agent('a1'), me('u1')]))).toEqual([
      ['a0', ['a0', 'a1']],
      ['u1', ['u1']],
    ]);
  });

  it('a message sent from here keeps the key it was shown under', () => {
    const turns = pairTurns([me('u1'), agent('a1')], (id) =>
      id === 'u1' ? 'sending-1' : id,
    );
    expect(turns[0]?.key).toBe('sending-1');
  });

  it('nothing said: no turns', () => {
    expect(pairTurns([])).toEqual([]);
  });

  it('keeps the order the queue rules give it', () => {
    const queued: ChatMessage = {
      ...me('q1', 0),
      queued: { seq: 1, held: false } as never,
    };
    const { sent } = splitQueue([agent('a1', 2), queued, me('u1', 1)]);
    expect(ids(pairTurns(sent))).toEqual([['u1', ['u1', 'a1']]]);
  });
});

describe('chatRows', () => {
  const turns = pairTurns([me('u1'), agent('a1'), me('u2')]);
  const keys = (rows: ReturnType<typeof chatRows>) => rows.map((r) => r.key);

  it('head, turns, tail, in that order', () => {
    expect(
      keys(chatRows(turns, { head: true, live: false, tail: true })),
    ).toEqual(['head', 'u1', 'u2', 'tail']);
    expect(
      keys(chatRows(turns, { head: false, live: false, tail: false })),
    ).toEqual(['u1', 'u2']);
  });

  it('the live reply goes on the last turn, nowhere else', () => {
    const rows = chatRows(turns, { head: false, live: true, tail: false });
    expect(rows.map((r) => r.kind === 'turn' && r.live)).toEqual([false, true]);
  });

  it('a running turn before its message: a row of its own', () => {
    expect(keys(chatRows([], { head: false, live: true, tail: true }))).toEqual(
      ['live', 'tail'],
    );
  });
});

describe('estimateRowHeight', () => {
  it('grows with the text', () => {
    const short = chatRows(pairTurns([me('u1')]), {
      head: false,
      live: false,
      tail: false,
    })[0]!;
    const long = chatRows(
      pairTurns([{ ...me('u1'), text: 'x'.repeat(5000) }]),
      { head: false, live: false, tail: false },
    )[0]!;
    expect(estimateRowHeight(long)).toBeGreaterThan(estimateRowHeight(short));
  });
});
