/** The AI panel's chat shares its look with a space's chat (`ChatTurn`):
 *  your messages as bubbles apart from the agent's replies, and Stop in
 *  the composer — never on a bubble. */
import { act } from 'react';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { mount, settle, type Mounted } from '../test/dom';
import type { ChatMessage, SkillRun } from '../gen/contract';
import { askPending, askRunId } from '../actions';
import { setTransport } from '../core/transport';
import { RootStore } from '../stores/RootStore';
import { RootStoreProvider } from '../stores/context';
import { FakeTransport } from '../test/fakeTransport';
import { SidePanel, newestRun } from './SidePanel';

// jsdom lays nothing out: the chat's scroll-to-bottom has nowhere to go.
Element.prototype.scrollIntoView ??= function () {};

const PR = 'gh:acme/app#1';

let transport: FakeTransport;
let store: RootStore;
let m: Mounted;

beforeEach(async () => {
  transport = new FakeTransport();
  transport.onCall('detect_agents', () => []);
  transport.onCall('list_skills', () => []);
  transport.onCall('ask', () => 'q1');
  transport.onCall('cancel_run', () => null);
  setTransport(transport);
  store = new RootStore();
  m = await mount(
    <RootStoreProvider store={store}>
      <SidePanel pr={PR} />
    </RootStoreProvider>,
  );
});

afterEach(() => m.unmount());

const message = (id: string, over: Partial<ChatMessage>): ChatMessage => ({
  id,
  pr: PR,
  author: { kind: 'me' },
  text: '',
  refs: [],
  at: Date.now(),
  failed: null,
  ...over,
});

const put = (row: ChatMessage) =>
  act(async () => store.sync.pool.put('chat_message', row.id, row));

const composerButton = (label: string) =>
  m.host.querySelector<HTMLButtonElement>(
    `.chat-input button[aria-label="${label}"]`,
  );

/** Any Stop control inside the message log. */
const stopInLog = () =>
  [...m.host.querySelectorAll('.chat-log button')].filter(
    (b) =>
      b.getAttribute('aria-label') === 'Stop' ||
      b.textContent?.trim() === 'Stop',
  );

const startPending = async () => {
  await act(async () =>
    askPending(store, {
      pr: PR,
      question: 'why?',
      about: null,
      group: null,
      agent: 'claude',
    }),
  );
};

describe('the AI panel chat', () => {
  it('renders your message and the reply as different bubbles', async () => {
    await put(message('u1', { text: 'why is this here?' }));
    await put(
      message('a1', {
        author: { kind: 'agent', agent: 'claude' },
        text: 'it caches the result',
        at: Date.now() + 1,
      }),
    );
    const user = m.host.querySelector('.chat-message[data-msg="u1"]')!;
    const agent = m.host.querySelector('.chat-message[data-msg="a1"]')!;
    expect(user.getAttribute('data-variant')).toBe('user');
    expect(agent.getAttribute('data-variant')).toBe('agent');
    expect(user.querySelector('.user-bubble')?.textContent).toBe(
      'why is this here?',
    );
    expect(agent.querySelector('.user-bubble')).toBeNull();
    expect(agent.querySelector('.agent-tag')?.textContent).toBe('claude');
  });

  it('a pending question shows as your bubble, the reply as the agent’s', async () => {
    await startPending();
    const user = m.host.querySelector('.chat-log [data-variant="user"]');
    expect(user?.textContent).toContain('why?');
    const live = m.host.querySelector('.chat-pending');
    expect(live?.getAttribute('data-variant')).toBe('agent');
    // Once the question is stored, it isn't shown twice.
    await put(message('q1', { text: 'why?' }));
    expect(
      m.host.querySelectorAll('.chat-log [data-variant="user"]'),
    ).toHaveLength(1);
  });

  it('Stop sits in the composer while a question runs and cancels it', async () => {
    expect(composerButton('Send')).not.toBeNull();
    expect(composerButton('Stop')).toBeNull();
    await startPending();
    expect(composerButton('Send')).toBeNull();
    expect(stopInLog()).toHaveLength(0);
    await act(async () => composerButton('Stop')!.click());
    await settle();
    expect(transport.calls.find((c) => c.cmd === 'cancel_run')).toMatchObject({
      args: { run: askRunId('q1') },
    });
    expect(store.ephemeral.chatPending.has(PR)).toBe(false);
    expect(composerButton('Send')).not.toBeNull();
  });

  it('⌘. stops the running question too', async () => {
    await startPending();
    await act(async () => {
      window.dispatchEvent(
        new KeyboardEvent('keydown', { key: '.', metaKey: true }),
      );
    });
    await settle();
    expect(transport.calls.some((c) => c.cmd === 'cancel_run')).toBe(true);
    expect(store.ephemeral.chatPending.has(PR)).toBe(false);
  });

  it('no bubble has a Stop button: a skill run, a line ask, a stalled ask', async () => {
    const run: SkillRun = {
      id: 'run1',
      pr: PR,
      skill: 'security',
      agents: ['claude'],
      status: 'running',
      error: null,
      started_at: Date.now(),
    };
    await act(async () => store.sync.pool.put('skill_run', run.id, run));
    await act(async () =>
      store.ephemeral.setLineAsk('k1', {
        pr: PR,
        anchor: {
          rev: 'h',
          path: 'a.ts',
          line: 3,
          side: 'new',
          start_line: null,
          start_side: null,
        },
        question: 'what does this do?',
        agent: 'claude',
        status: 'asking',
        message: 'm9',
        reply: null,
        startedAt: Date.now() + 5,
      }),
    );
    await startPending();
    await act(async () =>
      store.ephemeral.handle({
        type: 'run_stalled',
        run: askRunId('q1'),
        agent: 'claude',
      }),
    );
    expect(m.host.querySelector('.chat-log .agent-stalled')).not.toBeNull();
    // Retry stays, as a stalled hint's companion — not a Stop.
    expect(
      [...m.host.querySelectorAll('.chat-log button')].some(
        (b) => b.textContent?.trim() === 'Retry',
      ),
    ).toBe(true);
    expect(stopInLog()).toHaveLength(0);
    expect(
      m.host.querySelectorAll('.chat-input button[aria-label="Stop"]'),
    ).toHaveLength(1);
  });

  it('a running skill run stops from the composer', async () => {
    const run: SkillRun = {
      id: 'run1',
      pr: PR,
      skill: 'security',
      agents: ['claude'],
      status: 'running',
      error: null,
      started_at: Date.now(),
    };
    await act(async () => store.sync.pool.put('skill_run', run.id, run));
    await act(async () => composerButton('Stop')!.click());
    await settle();
    expect(transport.calls.find((c) => c.cmd === 'cancel_run')).toMatchObject({
      args: { run: 'run1' },
    });
  });
});

describe('Stop with several runs going', () => {
  it('stops the newest: a line ask asked after the pending question', async () => {
    await startPending();
    await act(async () =>
      store.ephemeral.setLineAsk('k1', {
        pr: PR,
        anchor: {
          rev: 'h',
          path: 'src/a.ts',
          line: 3,
          side: 'new',
          start_line: null,
          start_side: null,
        },
        question: 'what does this do?',
        agent: 'claude',
        status: 'asking',
        message: 'm9',
        reply: null,
        startedAt: Date.now() + 60_000,
      }),
    );
    // The line's question shows as your bubble, with its file:line chip.
    const asked = [
      ...m.host.querySelectorAll('.chat-log [data-variant="user"]'),
    ].find((el) => el.textContent?.includes('what does this do?'));
    expect(asked?.querySelector('.ref')?.textContent).toContain('a.ts:3');
    await act(async () => composerButton('Stop')!.click());
    await settle();
    expect(transport.calls.find((c) => c.cmd === 'cancel_run')).toMatchObject({
      args: { run: askRunId('m9') },
    });
    expect(store.ephemeral.lineAsks.has('k1')).toBe(false);
    // The pending question is next: Stop stays.
    expect(store.ephemeral.chatPending.has(PR)).toBe(true);
    expect(composerButton('Stop')).not.toBeNull();
  });
});

describe('newestRun', () => {
  it('picks the run started last', () => {
    expect(newestRun([{ at: 1 }, { at: 5 }, { at: 3 }])).toEqual({ at: 5 });
    expect(newestRun([])).toBeNull();
  });
});
