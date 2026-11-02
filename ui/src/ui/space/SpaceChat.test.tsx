/** A space's chat renders through the same `ChatTurn` pieces as the AI
 *  panel: your turns as bubbles apart from the agent's replies, the live
 *  turn as an agent bubble, Stop in the composer. */
import { act, createRef } from 'react';
import { afterEach, describe, expect, it } from 'vitest';
import { mount, settle, type Mounted } from '../../test/dom';
import type { ChatMessage, Space } from '../../gen/contract';
import { setTransport } from '../../core/transport';
import { RootStore } from '../../stores/RootStore';
import { RootStoreProvider } from '../../stores/context';
import { FakeTransport } from '../../test/fakeTransport';
import { SpaceChat } from './SpacePage';
import { spaceChatKey } from './spaces';

Element.prototype.scrollIntoView ??= function () {};

const space = (over: Partial<Space> = {}): Space => ({
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
  ...over,
});

const message = (id: string, over: Partial<ChatMessage>): ChatMessage => ({
  id,
  pr: spaceChatKey('s1'),
  author: { kind: 'me' },
  text: '',
  refs: [],
  at: 1,
  failed: null,
  ...over,
});

let m: Mounted | null = null;
let transport: FakeTransport;

afterEach(() => {
  m?.unmount();
  m = null;
});

async function render(s: Space) {
  transport = new FakeTransport();
  transport.onCall('detect_agents', () => []);
  transport.onCall('space_skills', () => []);
  transport.onCall('space_stop', () => null);
  setTransport(transport);
  const store = new RootStore();
  store.sync.pool.put(
    'chat_message',
    'u1',
    message('u1', { text: 'add a flag' }),
  );
  store.sync.pool.put(
    'chat_message',
    'a1',
    message('a1', {
      author: { kind: 'agent', agent: 'claude' },
      text: 'Added `--flag`.',
      at: 2,
    }),
  );
  m = await mount(
    <RootStoreProvider store={store}>
      <SpaceChat space={s} config={null} handle={createRef()} />
    </RootStoreProvider>,
  );
  await settle();
  return store;
}

describe('the space chat', () => {
  it('renders turns with the shared message component, as two bubble kinds', async () => {
    await render(space());
    const user = m!.host.querySelector('.chat-message[data-msg="u1"]')!;
    const agent = m!.host.querySelector('.chat-message[data-msg="a1"]')!;
    expect(user.getAttribute('data-variant')).toBe('user');
    expect(user.querySelector('.user-bubble')?.textContent).toBe('add a flag');
    expect(agent.getAttribute('data-variant')).toBe('agent');
    expect(agent.querySelector('.agent-tag')?.textContent).toBe('claude');
    expect(agent.querySelector('.message-actions')).not.toBeNull();
  });

  it('a running turn: an agent bubble in the log, Stop in the composer', async () => {
    await render(space({ status: 'running', run: 'space:s1:u2' }));
    expect(
      m!.host.querySelector('.chat-live')?.getAttribute('data-variant'),
    ).toBe('agent');
    const inLog = [...m!.host.querySelectorAll('.chat-log button')].filter(
      (b) =>
        b.getAttribute('aria-label') === 'Stop' ||
        b.textContent?.trim() === 'Stop',
    );
    expect(inLog).toHaveLength(0);
    const stop = m!.host.querySelector<HTMLButtonElement>(
      '.chat-input button[aria-label="Stop"]',
    )!;
    await act(async () => stop.click());
    await settle();
    expect(transport.calls.find((c) => c.cmd === 'space_stop')).toMatchObject({
      args: { space: 's1' },
    });
  });
});
