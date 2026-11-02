/** A question with no diff-line chip to pin it to — the composer's plain
 *  send, or a hunk's fixed "Ask agent": pending at once (the question, the
 *  agent's streaming text), cleared once the real reply lands. One per PR —
 *  a later ask replaces an earlier one still running. */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { setTransport } from './core/transport';
import { FakeTransport } from './test/fakeTransport';
import { RootStore } from './stores/RootStore';
import { askPending, askRunId } from './actions';
import type { ChatMessage } from './gen/contract';

const PR = 'gh:acme/app#1';

let transport: FakeTransport;

beforeEach(() => {
  transport = new FakeTransport();
  setTransport(transport);
  transport.onCall('cancel_run', () => null);
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

function msg(id: string, at: number, who: 'me' | 'claude'): ChatMessage {
  return {
    id,
    pr: PR,
    author: who === 'me' ? { kind: 'me' } : { kind: 'agent', agent: who },
    text: id,
    refs: [],
    at,
    failed: null,
  };
}

const put = (store: RootStore, m: ChatMessage) =>
  store.sync.pool.put('chat_message', m.id, m);

describe('askPending', () => {
  it('opens the aside and tracks the run until the reply lands', async () => {
    const store = new RootStore();
    vi.setSystemTime(1000);
    transport.queueResult('ask', 'q1');

    await askPending(store, {
      pr: PR,
      question: 'about the hunk at src/a.ts:12',
      about: null,
      group: null,
      agent: null,
    });

    expect(store.ui.asideShown).toBe(true);
    expect(store.ephemeral.chatPending.get(PR)).toMatchObject({
      run: askRunId('q1'),
      question: 'about the hunk at src/a.ts:12',
      agent: null,
    });
    // The AI panel is told to show the chat's bottom, like a fresh send.
    expect(store.ephemeral.askFocus).toMatchObject({ pr: PR, message: null });

    // The streaming text a `LineAskBubble`-style view would read.
    store.ephemeral.handle({
      type: 'agent_delta',
      run: askRunId('q1'),
      agent: 'claude',
      text: 'looking…',
    });
    expect(store.ephemeral.agentText.get(askRunId('q1'))).toBe('looking…');

    put(store, msg('q1', 900, 'me'));
    expect(store.ephemeral.chatPending.has(PR)).toBe(true);
    put(store, msg('r1', 1100, 'claude'));
    expect(store.ephemeral.chatPending.has(PR)).toBe(false);
  });

  it('does not set anything pending when `ask` is rejected', async () => {
    const store = new RootStore();
    transport.queueResult('ask', new Error('boom'));

    await expect(
      askPending(store, {
        pr: PR,
        question: 'x',
        about: null,
        group: null,
        agent: null,
      }),
    ).rejects.toThrow();
    expect(store.ephemeral.chatPending.has(PR)).toBe(false);
  });

  it('a later ask replaces the pending one; the earlier run answering does not clear it', async () => {
    const store = new RootStore();
    transport.queueResult('ask', 'q2', 'q3');

    vi.setSystemTime(1000);
    await askPending(store, {
      pr: PR,
      question: 'first',
      about: null,
      group: null,
      agent: null,
    });
    vi.setSystemTime(2000);
    await askPending(store, {
      pr: PR,
      question: 'second',
      about: null,
      group: null,
      agent: null,
    });
    expect(store.ephemeral.chatPending.get(PR)).toMatchObject({
      run: askRunId('q3'),
      question: 'second',
    });

    // q2's own reply lands — it isn't the one being tracked any more.
    put(store, msg('q2', 1500, 'me'));
    put(store, msg('r2', 1600, 'claude'));
    expect(store.ephemeral.chatPending.get(PR)).toMatchObject({
      question: 'second',
    });
  });
});
