/** A line ask's "Answered · View" asks the AI panel to show a message. The
 *  log is virtual, so the row may not be mounted: the panel scrolls to it
 *  through the virtual list, then the message glows. */
import { act } from 'react';
import {
  afterAll,
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from 'vitest';
import { mount, settle, type Mounted } from '../test/dom';
import {
  frames,
  installChatLayout,
  layoutRows,
  uninstallChatLayout,
  userScroll,
} from '../test/chatLayout';
import type { ChatMessage } from '../gen/contract';
import { setTransport } from '../core/transport';
import { RootStore } from '../stores/RootStore';
import { RootStoreProvider } from '../stores/context';
import { FakeTransport } from '../test/fakeTransport';
import { SidePanel } from './SidePanel';

const PR = 'gh:acme/app#2';

const message = (id: string, at: number, mine: boolean): ChatMessage => ({
  id,
  pr: PR,
  author: mine ? { kind: 'me' } : { kind: 'agent', agent: 'claude' },
  text: id,
  refs: [],
  at,
  failed: null,
});

beforeAll(installChatLayout);
afterAll(uninstallChatLayout);

let m: Mounted;
let store: RootStore;

beforeEach(async () => {
  const transport = new FakeTransport();
  transport.onCall('detect_agents', () => []);
  transport.onCall('list_skills', () => []);
  setTransport(transport);
  store = new RootStore();
  for (let i = 0; i < 100; i += 1) {
    store.sync.pool.put(
      'chat_message',
      `q${i}`,
      message(`q${i}`, 10 + i * 2, true),
    );
    store.sync.pool.put(
      'chat_message',
      `a${i}`,
      message(`a${i}`, 11 + i * 2, false),
    );
  }
  m = await mount(
    <RootStoreProvider store={store}>
      <SidePanel pr={PR} />
    </RootStoreProvider>,
  );
  await settle();
  layoutRows();
  await frames();
});

afterEach(() => {
  m.unmount();
  vi.useRealTimers();
});

const log = () => m.host.querySelector<HTMLElement>('.chat-log')!;
const msg = (id: string) => m.host.querySelector(`[data-msg="${id}"]`);
const pill = () => m.host.querySelector('.chat-jump');

const focus = async (id: string | null) => {
  await act(async () => store.ephemeral.focusAsk(PR, id));
  layoutRows();
  await frames();
};

describe('showing a message from a line ask', () => {
  it('scrolls to a row that is not mounted, and the message glows', async () => {
    expect(msg('a3')).toBeNull();
    await focus('a3');
    expect(msg('a3')).not.toBeNull();
    expect(msg('a3')!.classList.contains('ask-flash')).toBe(true);
    expect(msg('a4')?.classList.contains('ask-flash') ?? false).toBe(false);
    expect(store.ephemeral.askFocus).toBeNull();
    // It's in the view, and the way back is offered.
    const row = msg('a3')!.closest('[data-index]') as HTMLElement;
    const top = Number(
      /translateY\((-?[\d.]+)px\)/.exec(row.style.transform)![1],
    );
    expect(top).toBeGreaterThan(log().scrollTop - 100);
    expect(top).toBeLessThan(log().scrollTop + log().clientHeight);
    expect(pill()).not.toBeNull();
  });

  it('a mounted row glows too, and a repeat focus glows again', async () => {
    await focus('a99');
    const first = msg('a99')!;
    expect(first.classList.contains('ask-flash')).toBe(true);
    await focus('a99');
    // A new node, so the animation starts over.
    expect(msg('a99')).not.toBe(first);
    expect(msg('a99')!.classList.contains('ask-flash')).toBe(true);
  });

  it('the glow ends, and the row does not glow again when it scrolls back', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
    await focus('a3');
    expect(msg('a3')!.classList.contains('ask-flash')).toBe(true);
    await act(async () => {
      vi.advanceTimersByTime(2000);
    });
    expect(msg('a3')!.classList.contains('ask-flash')).toBe(false);
  });

  it('a message that has not synced yet: the bottom, and it waits for it', async () => {
    userScroll(log(), 100);
    await frames();
    expect(pill()).not.toBeNull();
    await focus('q-new');
    expect(log().scrollTop).toBe(log().scrollHeight - log().clientHeight);
    expect(store.ephemeral.askFocus?.message).toBe('q-new');

    await act(async () =>
      store.sync.pool.put('chat_message', 'q-new', message('q-new', 500, true)),
    );
    layoutRows();
    await frames();
    expect(store.ephemeral.askFocus).toBeNull();
    expect(msg('q-new')?.classList.contains('ask-flash')).toBe(true);
  });

  it('no message: the bottom of the log', async () => {
    userScroll(log(), 100);
    await frames();
    await focus(null);
    expect(log().scrollTop).toBe(log().scrollHeight - log().clientHeight);
    expect(pill()).toBeNull();
    expect(store.ephemeral.askFocus).toBeNull();
  });
});
