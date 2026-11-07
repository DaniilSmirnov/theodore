/** What you do to a turn shows at once, in the row it belongs to, before the
 *  core has answered; what arrives after takes over without a flicker. */
import { act, createRef } from 'react';
import { observer } from 'mobx-react-lite';
import { afterAll, afterEach, beforeAll, describe, expect, it } from 'vitest';
import { click, mount, settle, typeInto, type Mounted } from '../../test/dom';
import {
  chatLayout,
  frames,
  installChatLayout,
  layoutRows,
  uninstallChatLayout,
  userScroll,
} from '../../test/chatLayout';
import type { ChatMessage, Space } from '../../gen/contract';
import { setTransport } from '../../core/transport';
import { RootStore } from '../../stores/RootStore';
import { RootStoreProvider, useRootStore } from '../../stores/context';
import { FakeTransport } from '../../test/fakeTransport';
import { SpaceChat } from './SpacePage';
import { spaceChatKey } from './spaces';

beforeAll(installChatLayout);
afterAll(uninstallChatLayout);

const baseSpace: Space = {
  id: 's1',
  project: 'local:app',
  name: 's1',
  goal: '',
  branch: 'theo/s1',
  base: 'main',
  base_sha: 'b',
  head_sha: 'h',
  worktree: '/tmp/s1',
  status: 'idle',
  agent: 'claude',
  model: null,
  mode: 'edit',
  review_pr: null,
  pr: null,
  turns: 1,
  run: null,
  error: null,
  created_at: 0,
  updated_at: 0,
};

const message = (id: string, over: Partial<ChatMessage> = {}): ChatMessage => ({
  id,
  pr: spaceChatKey('s1'),
  author: { kind: 'me' },
  text: id,
  refs: [],
  at: 1,
  failed: null,
  ...over,
});
const reply = (id: string, over: Partial<ChatMessage> = {}) =>
  message(id, { author: { kind: 'agent', agent: 'claude' }, at: 2, ...over });

let m: Mounted | null = null;
let store: RootStore;
let transport: FakeTransport;

/** A promise the test settles: the core hasn't answered until then. */
function pending<T>() {
  let resolve!: (v: T) => void;
  let reject!: (e: Error) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

async function render(over: Partial<Space> = {}, messages = [message('u1')]) {
  transport = new FakeTransport();
  transport.onCall('detect_agents', () => []);
  transport.onCall('space_skills', () => []);
  setTransport(transport);
  store = new RootStore();
  store.sync.pool.put('space', 's1', { ...baseSpace, ...over });
  for (const row of messages) store.sync.pool.put('chat_message', row.id, row);
  const Page = () => (
    <RootStoreProvider store={store}>
      <Live />
    </RootStoreProvider>
  );
  m = await mount(<Page />);
  await settle();
  layoutRows();
}

const Live = observer(function Live() {
  const space = useRootStore().models.spaces.get('s1')!;
  return <SpaceChat space={space} config={null} handle={createRef()} />;
});

afterEach(() => {
  m?.unmount();
  m = null;
});

const box = () => m!.host.querySelector('.chat-input textarea');
const bubbles = () => [
  ...m!.host.querySelectorAll('.chat-message[data-variant="user"]'),
];
const dots = () => m!.host.querySelector('[aria-label="Agent is thinking"]');
const rowOf = (el: Element) => el.closest('[data-index]');
const button = (label: string) =>
  [...m!.host.querySelectorAll('button')].find(
    (b) => b.textContent?.trim() === label,
  ) ?? null;

async function typeAndSend(text: string) {
  await typeInto(box(), text);
  await act(async () => {
    box()!.dispatchEvent(
      new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }),
    );
  });
}

/** The core stored it: its row, and the space running, in one update. */
function landTurn(id: string, text: string) {
  act(() => {
    store.sync.pool.put('chat_message', id, message(id, { text, at: 5 }));
    store.sync.pool.merge('space', 's1', {
      status: 'running',
      run: `space:s1:${id}`,
    });
  });
}

describe('sending', () => {
  it('your message and a working state show before the core answers', async () => {
    await render();
    const answer = pending<string>();
    transport.onCall('space_send', () => answer.promise);
    await typeAndSend('add a flag');
    expect(bubbles().at(-1)?.textContent).toContain('add a flag');
    expect(dots()).not.toBeNull();
    expect((box() as HTMLTextAreaElement).value).toBe('');
  });

  it('a first message replaces the empty page at once', async () => {
    await render({}, []);
    expect(m!.host.querySelector('.chat-empty')).not.toBeNull();
    transport.onCall('space_send', () => pending<string>().promise);
    await typeAndSend('hello');
    expect(m!.host.querySelector('.chat-empty')).toBeNull();
    expect(bubbles()).toHaveLength(1);
    expect(dots()).not.toBeNull();
  });

  it('its stored row takes over in the same chat row, told once', async () => {
    await render();
    const answer = pending<string>();
    transport.onCall('space_send', () => answer.promise);
    await typeAndSend('add a flag');
    const shown = bubbles().at(-1)!;
    const row = rowOf(shown);

    landTurn('u2', 'add a flag');
    expect(bubbles()).toHaveLength(2);
    expect(bubbles().at(-1)).toBe(shown);
    expect(rowOf(bubbles().at(-1)!)).toBe(row);
    expect(shown.getAttribute('data-msg')).toBe('u2');
    expect(dots()).not.toBeNull();

    await act(async () => answer.resolve('u2'));
    expect(bubbles()).toHaveLength(2);
    expect(store.ephemeral.spaceSends.has('s1')).toBe(false);
  });

  it('the core answering before its row arrives changes nothing', async () => {
    await render();
    transport.onCall('space_send', () => 'u2');
    await typeAndSend('add a flag');
    await settle();
    const shown = bubbles().at(-1)!;
    landTurn('u2', 'add a flag');
    expect(bubbles()).toHaveLength(2);
    expect(bubbles().at(-1)).toBe(shown);
  });

  it('refused: gone, and the text back in the box', async () => {
    await render();
    const answer = pending<string>();
    transport.onCall('space_send', () => answer.promise);
    await typeAndSend('add a flag');
    expect(bubbles()).toHaveLength(2);
    await act(async () => answer.reject(new Error('no')));
    await settle();
    expect(bubbles()).toHaveLength(1);
    expect(dots()).toBeNull();
    expect((box() as HTMLTextAreaElement).value).toBe('add a flag');
  });

  it('the same text twice: each lands on its own', async () => {
    await render();
    transport.onCall('space_send', () => pending<string>().promise);
    await typeAndSend('again');
    await typeAndSend('again');
    landTurn('u2', 'again');
    expect(store.ephemeral.spaceSends.get('s1')).toHaveLength(1);
    act(() => {
      store.sync.pool.put(
        'chat_message',
        'u3',
        message('u3', {
          text: 'again',
          at: 6,
          queued: { seq: 1, held: false },
        }),
      );
    });
    expect(store.ephemeral.spaceSends.has('s1')).toBe(false);
  });

  it('an old message with the same text is not its row', async () => {
    await render({}, [message('u1', { text: 'again' })]);
    transport.onCall('space_send', () => pending<string>().promise);
    await typeAndSend('again');
    act(() => store.sync.pool.merge('chat_message', 'u1', { at: 2 }));
    expect(store.ephemeral.spaceSends.get('s1')).toHaveLength(1);
    expect(bubbles()).toHaveLength(2);
  });
});

describe('while a turn runs', () => {
  const running = { status: 'running', run: 'space:s1:u1' } as const;

  it('a new message is in the queue at once, without actions until stored', async () => {
    await render(running);
    transport.onCall('space_send', () => pending<string>().promise);
    await typeAndSend('then this');
    const row = m!.host.querySelector('.queued-message')!;
    expect(row.textContent).toContain('then this');
    expect(row.textContent).toContain('sending');
    expect(button('Cancel')).toBeNull();

    act(() =>
      store.sync.pool.put(
        'chat_message',
        'q1',
        message('q1', {
          text: 'then this',
          queued: { seq: 1, held: false },
        }),
      ),
    );
    expect(m!.host.querySelectorAll('.queued-message')).toHaveLength(1);
    expect(m!.host.querySelector('.queued-message')).toBe(row);
    expect(button('Cancel')).not.toBeNull();
    expect(row.textContent).toContain('queued');
  });

  it('Cancel takes it away at once; refused, it comes back', async () => {
    await render(running, [
      message('u1'),
      message('q1', { queued: { seq: 1, held: false } }),
    ]);
    const answer = pending<null>();
    transport.onCall('space_queued', () => answer.promise);
    await click(button('Cancel'));
    expect(m!.host.querySelector('.queued-message')).toBeNull();
    await act(async () => answer.reject(new Error('no')));
    await settle();
    expect(m!.host.querySelector('.queued-message')).not.toBeNull();
  });

  it('Stop says so on the row at once, and the reply replaces it in one update', async () => {
    await render(running);
    transport.onCall('space_stop', () => pending<null>().promise);
    const row = rowOf(bubbles()[0]!);
    await click(m!.host.querySelector('.chat-input button[aria-label="Stop"]'));
    expect(m!.host.querySelector('.chat-live')?.textContent).toContain(
      'Stopping',
    );

    act(() => {
      store.sync.pool.put(
        'chat_message',
        'a1',
        reply('a1', { text: 'Stopped here.' }),
      );
      store.sync.pool.merge('space', 's1', { status: 'idle', run: null });
    });
    expect(m!.host.querySelector('.chat-live')).toBeNull();
    expect(m!.host.querySelector('[data-msg="a1"]')).not.toBeNull();
    expect(rowOf(m!.host.querySelector('[data-msg="a1"]')!)).toBe(row);
  });

  it('text and tools show in the order they came, and rows never remount as it streams', async () => {
    await render(running);
    const run = running.run;
    const event = (e: Parameters<typeof store.ephemeral.handle>[0]) =>
      act(() => store.ephemeral.handle(e));
    event({ type: 'agent_delta', run, agent: 'claude', text: 'Looking' });
    event({ type: 'agent_delta', run, agent: 'claude', text: ' first.' });
    event({
      type: 'run_step',
      run,
      step: { kind: 'tool', name: 'Read', target: 'a.rs', ok: null, id: 't1' },
    });
    event({ type: 'agent_delta', run, agent: 'claude', text: 'Then this.' });
    act(() => store.ephemeral.flushDeltas());
    const rows = () => [
      ...m!.host.querySelectorAll('.turn-timeline > .turn-text, .turn-tool'),
    ];
    expect(rows().map((r) => r.textContent?.trim())).toEqual([
      'Looking first.',
      expect.stringContaining('a.rs'),
      'Then this.',
    ]);
    const first = rows()[0];
    event({ type: 'agent_delta', run, agent: 'claude', text: ' And more.' });
    act(() => store.ephemeral.flushDeltas());
    expect(rows()[0]).toBe(first);
    expect(rows()).toHaveLength(3);
  });
});

describe('a finished turn folded and opened', () => {
  const finished = reply('a1', {
    activity: [
      { kind: 'text', text: 'Reading.' },
      { kind: 'tool', name: 'Read', target: 'a.rs', ok: true },
      { kind: 'tool', name: 'Bash', target: 'ls', ok: true },
    ],
    duration_ms: 5000,
  });

  it('opens at once, with no panel to animate', async () => {
    await render({}, [message('u1'), finished]);
    expect(m!.host.querySelector('.turn-tool')).toBeNull();
    await click(m!.host.querySelector('.turn-fold-line'));
    expect(m!.host.querySelector('.turn-tool')).not.toBeNull();
    expect(m!.host.querySelector('.disclosure__content')).toBeNull();
  });

  it('stays open after scrolling out of view and back', async () => {
    const turns = Array.from({ length: 60 }, (_, i) => [
      message(`x${i}`, { at: i * 2 }),
      reply(`y${i}`, {
        at: i * 2 + 1,
        activity: [
          { kind: 'tool', name: 'Bash', target: 'a', ok: true },
          { kind: 'tool', name: 'Bash', target: 'b', ok: true },
          { kind: 'tool', name: 'Bash', target: 'c', ok: true },
        ],
        duration_ms: 1000,
      }),
    ]).flat();
    chatLayout.rowHeight = 300;
    await render({}, turns);
    await frames();
    const log = m!.host.querySelector<HTMLElement>('.chat-log')!;
    const reply59 = () => m!.host.querySelector('[data-msg="y59"]');
    await click(reply59()!.querySelector('.turn-fold-line'));
    await frames();
    expect(reply59()?.querySelector('.turn-tool')).not.toBeNull();

    userScroll(log, 0);
    await frames();
    expect(reply59()).toBeNull();
    userScroll(log, log.scrollHeight);
    await frames();
    expect(reply59()?.querySelector('.turn-tool')).not.toBeNull();
    chatLayout.rowHeight = 100;
  });
});
