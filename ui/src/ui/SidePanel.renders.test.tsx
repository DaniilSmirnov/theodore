/** The AI panel's log is virtual, one row per turn: a long thread mounts only
 *  the turns near the view, typing in the composer leaves the rows alone, and
 *  a row only re-renders for a message of its own. */
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
import { mount, settle, typeInto, type Mounted } from '../test/dom';
import {
  frames,
  installChatLayout,
  layoutRows,
  uninstallChatLayout,
} from '../test/chatLayout';
import type { ChatMessage } from '../gen/contract';
import { setTransport } from '../core/transport';
import { RootStore } from '../stores/RootStore';
import { RootStoreProvider } from '../stores/context';
import { FakeTransport } from '../test/fakeTransport';
import { SidePanel } from './SidePanel';

const renders = vi.hoisted(() => new Map<string, number>());

vi.mock('./ChatTurn', async (original) => {
  const real = await original<typeof import('./ChatTurn')>();
  return {
    ...real,
    ChatMessage: (props: Parameters<typeof real.ChatMessage>[0]) => {
      renders.set(props.m.id, (renders.get(props.m.id) ?? 0) + 1);
      return real.ChatMessage(props);
    },
  };
});

const PR = 'gh:acme/app#1';

const message = (id: string, over: Partial<ChatMessage>): ChatMessage => ({
  id,
  pr: PR,
  author: { kind: 'me' },
  text: id,
  refs: [],
  at: 1,
  failed: null,
  ...over,
});

const reply = (id: string, at: number, over: Partial<ChatMessage> = {}) =>
  message(id, { author: { kind: 'agent', agent: 'claude' }, at, ...over });

beforeAll(installChatLayout);
afterAll(uninstallChatLayout);

let m: Mounted | null = null;
let store: RootStore;
let transport: FakeTransport;

/** `turns` questions with a reply each, oldest first, before mounting. */
async function open(turns: number) {
  transport = new FakeTransport();
  transport.onCall('detect_agents', () => []);
  transport.onCall('list_skills', () => []);
  transport.onCall('ask', () => 'q-new');
  setTransport(transport);
  store = new RootStore();
  for (let i = 0; i < turns; i += 1) {
    store.sync.pool.put(
      'chat_message',
      `q${i}`,
      message(`q${i}`, { at: 10 + i * 2 }),
    );
    store.sync.pool.put(
      'chat_message',
      `a${i}`,
      reply(`a${i}`, 11 + i * 2, {
        refs: [
          {
            rev: 'h',
            path: 'src/a.ts',
            line: i + 1,
            side: 'new',
            start_line: null,
            start_side: null,
          },
        ],
      }),
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
  renders.clear();
}

afterEach(() => {
  m?.unmount();
  m = null;
});

beforeEach(() => renders.clear());

const mountedMessages = () => m!.host.querySelectorAll('.chat-message');

describe('a long thread', () => {
  it('mounts only the turns near the view, ending at the latest', async () => {
    await open(100);
    expect(mountedMessages().length).toBeLessThan(24);
    expect(m!.host.querySelector('[data-msg="a99"]')).not.toBeNull();
    expect(m!.host.querySelector('[data-msg="q0"]')).toBeNull();
  });

  it('keeps its chips: a reply shows where it points, and a click jumps', async () => {
    await open(3);
    const chip = m!.host.querySelector('[data-msg="a2"] .ref-jump');
    expect(chip?.textContent).toContain('a.ts:3');
  });
});

describe('a message row', () => {
  beforeEach(() => open(4));

  it('does not re-render as you type in the composer', async () => {
    const box = m!.host.querySelector('.chat-input textarea');
    await typeInto(box, 'h');
    await typeInto(box, 'he');
    await typeInto(box, 'hel');
    expect((box as HTMLTextAreaElement).value).toBe('hel');
    expect(renders.size).toBe(0);
  });

  it('does not re-render when another turn gets a message', async () => {
    await act(async () =>
      store.sync.pool.put('chat_message', 'q4', message('q4', { at: 100 })),
    );
    expect(renders.has('q4')).toBe(true);
    expect([...renders.keys()].filter((id) => id !== 'q4')).toEqual([]);
  });

  it('re-renders for a change to its turn, and only its turn', async () => {
    await act(async () =>
      store.sync.pool.put(
        'chat_message',
        'a3',
        reply('a3', 17, { text: 'edited' }),
      ),
    );
    expect(renders.get('a3')).toBe(1);
    expect(renders.has('a2')).toBe(false);
    expect(renders.has('q2')).toBe(false);
  });

  it('a reply joins the turn of its question, in the same row', async () => {
    await act(async () =>
      store.sync.pool.put('chat_message', 'a3b', reply('a3b', 18)),
    );
    const q = m!.host.querySelector('[data-msg="q3"]')!;
    const second = m!.host.querySelector('[data-msg="a3b"]')!;
    expect(second.closest('[data-index]')).toBe(q.closest('[data-index]'));
  });
});

describe('Ask again', () => {
  it('asks the question again of the agent that answered', async () => {
    await open(2);
    const again = [...m!.host.querySelectorAll('[data-msg="a1"] button')].find(
      (b) => b.textContent?.trim() === 'Ask again',
    );
    expect(again).toBeDefined();
    await act(async () => (again as HTMLButtonElement).click());
    await settle();
    expect(transport.calls.find((c) => c.cmd === 'ask')).toMatchObject({
      args: { req: { pr: PR, question: 'q1', agent: 'claude' } },
    });
  });
});
