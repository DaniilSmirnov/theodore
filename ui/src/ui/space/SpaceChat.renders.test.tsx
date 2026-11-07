/** Typing and space updates leave the message rows alone: only the composer
 *  re-renders with what you type, and a row only with its own message. */
import { act, createRef } from 'react';
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
import { mount, settle, typeInto, type Mounted } from '../../test/dom';
import type { ChatMessage, Space } from '../../gen/contract';
import { setTransport } from '../../core/transport';
import { RootStore } from '../../stores/RootStore';
import { RootStoreProvider } from '../../stores/context';
import { FakeTransport } from '../../test/fakeTransport';
import { SpaceChat } from './SpacePage';
import { installChatLayout, uninstallChatLayout } from '../../test/chatLayout';
import { spaceChatKey } from './spaces';

const renders = vi.hoisted(() => new Map<string, number>());

vi.mock('../ChatTurn', async (original) => {
  const real = await original<typeof import('../ChatTurn')>();
  return {
    ...real,
    ChatMessage: (props: Parameters<typeof real.ChatMessage>[0]) => {
      renders.set(props.m.id, (renders.get(props.m.id) ?? 0) + 1);
      return real.ChatMessage(props);
    },
  };
});

const space: Space = {
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

const message = (id: string, over: Partial<ChatMessage>): ChatMessage => ({
  id,
  pr: spaceChatKey('s1'),
  author: { kind: 'me' },
  text: id,
  refs: [],
  at: 1,
  failed: null,
  ...over,
});

beforeAll(installChatLayout);
afterAll(uninstallChatLayout);

let m: Mounted | null = null;
let store: RootStore;

beforeEach(async () => {
  renders.clear();
  const transport = new FakeTransport();
  transport.onCall('detect_agents', () => []);
  transport.onCall('space_skills', () => []);
  setTransport(transport);
  store = new RootStore();
  store.sync.pool.put('space', 's1', space);
  store.sync.pool.put('chat_message', 'u1', message('u1', {}));
  store.sync.pool.put(
    'chat_message',
    'a1',
    message('a1', { author: { kind: 'agent', agent: 'claude' }, at: 2 }),
  );
  m = await mount(
    <RootStoreProvider store={store}>
      <SpaceChat space={space} config={null} handle={createRef()} />
    </RootStoreProvider>,
  );
  await settle();
  renders.clear();
});

afterEach(() => {
  m?.unmount();
  m = null;
});

describe('a message row', () => {
  it('does not re-render as you type in the composer', async () => {
    const box = m!.host.querySelector('.chat-input textarea');
    await typeInto(box, 'h');
    await typeInto(box, 'he');
    await typeInto(box, 'hel');
    expect((box as HTMLTextAreaElement).value).toBe('hel');
    expect(renders.size).toBe(0);
  });

  it('does not re-render for a space change that says nothing about it', () => {
    act(() => {
      store.sync.pool.merge('space', 's1', {
        updated_at: 5,
        progress: { items: [] },
      });
    });
    expect(renders.size).toBe(0);
  });

  it('does not re-render when a changed file chip opens the Changes pane', () => {
    act(() => store.ephemeral.openSpaceChanges('s1', 'src/a.rs'));
    expect(renders.size).toBe(0);
  });

  it('re-renders for a change to itself, and only itself', () => {
    act(() => {
      store.sync.pool.put(
        'chat_message',
        'a1',
        message('a1', {
          author: { kind: 'agent', agent: 'claude' },
          at: 2,
          text: 'edited',
        }),
      );
    });
    expect(renders.get('a1')).toBe(1);
    expect(renders.has('u1')).toBe(false);
  });

  it('re-renders when the space changes what it says about it', () => {
    act(() => {
      store.sync.pool.merge('space', 's1', {
        turn_log: [
          { message: 'u1', reply: 'a1', before: 'b', after: 'h', undone: true },
        ],
      });
    });
    expect(renders.get('a1')).toBe(1);
    expect(
      m!.host.querySelector('.chat-message[data-msg="a1"]')?.className,
    ).toContain('space-undone');
  });
});
