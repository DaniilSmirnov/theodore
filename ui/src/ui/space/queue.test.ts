import { describe, expect, it } from 'vitest';
import type { ChatMessage } from '../../gen/contract';
import { canSendNow, heldReason, splitQueue } from './queue';

const msg = (
  id: string,
  at: number,
  queued?: { seq: number; held?: boolean },
): ChatMessage => ({
  id,
  pr: 'space:s',
  author: { kind: 'me' },
  text: id,
  refs: [],
  at,
  queued: queued ? { seq: queued.seq, held: queued.held ?? false } : null,
});

describe('the queue', () => {
  it('keeps queued messages out of the transcript, in send order', () => {
    const { sent, queued } = splitQueue([
      msg('b', 3, { seq: 2 }),
      msg('reply', 2),
      msg('a', 9, { seq: 1 }),
      msg('goal', 1),
    ]);
    expect(sent.map((m) => m.id)).toEqual(['goal', 'reply']);
    expect(queued.map((m) => m.id)).toEqual(['a', 'b']);
  });

  it('says why it waits only when something is held', () => {
    const waiting = [msg('a', 1, { seq: 1 })];
    expect(heldReason({ status: 'running' }, waiting)).toBeNull();
    const held = [msg('a', 1, { seq: 1, held: true })];
    expect(heldReason({ status: 'idle' }, held)).toMatch(/Send or drop/);
    expect(heldReason({ status: 'archived' }, held)).toMatch(/Unarchive/);
  });
});

describe('send now', () => {
  const running = {
    status: 'running',
    agent: 'claude',
    mode: 'edit',
  } as const;
  const same = { agent: 'claude', mode: 'edit', files: 0 } as const;

  it('is offered for a running Claude turn and an unchanged message', () => {
    expect(canSendNow(running, same)).toBe(true);
  });

  it('falls back to the queue for anything the run cannot take', () => {
    expect(canSendNow({ ...running, status: 'idle' }, same)).toBe(false);
    expect(canSendNow({ ...running, agent: 'codex' }, same)).toBe(false);
    expect(canSendNow(running, { ...same, mode: 'run' })).toBe(false);
    expect(canSendNow(running, { ...same, agent: 'gemini' })).toBe(false);
    expect(canSendNow(running, { ...same, files: 1 })).toBe(false);
  });
});
