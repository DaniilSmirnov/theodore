/** "Set up with agent" stays reachable once the chat has turns: a row above
 *  the composer while the project has no `spaces:` commands, dismissible. */
import { createRef } from 'react';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { click, mount, settle, type Mounted } from '../../test/dom';
import type { ChatMessage, Space, SpaceConfigView } from '../../gen/contract';
import { setTransport } from '../../core/transport';
import { RootStore } from '../../stores/RootStore';
import { RootStoreProvider } from '../../stores/context';
import { FakeTransport } from '../../test/fakeTransport';
import { SpaceChat } from './SpacePage';
import { spaceChatKey } from './spaces';

Element.prototype.scrollIntoView ??= function () {};

const space = (over: Partial<Space> = {}): Space => ({
  id: 's1',
  project: 'github:o/r',
  name: 's1',
  goal: 'make a game',
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

const NO_COMMANDS: SpaceConfigView = {
  commands: [],
  ports: [],
  repo_commands: null,
  denied: false,
};

let m: Mounted | null = null;
let transport: FakeTransport;

beforeEach(() => localStorage.clear());
afterEach(() => {
  m?.unmount();
  m = null;
});

async function render(s: Space, config: SpaceConfigView | null) {
  transport = new FakeTransport();
  transport.onCall('detect_agents', () => []);
  transport.onCall('space_skills', () => []);
  transport.onCall('space_setup_assist', () => s);
  setTransport(transport);
  const store = new RootStore();
  store.sync.pool.put('space', s.id, s);
  store.sync.pool.put('chat_message', 'u1', message('u1', { text: 'go' }));
  store.sync.pool.put(
    'chat_message',
    'a1',
    message('a1', {
      author: { kind: 'agent', agent: 'claude' },
      text: 'Done.',
      at: 2,
    }),
  );
  m = await mount(
    <RootStoreProvider store={store}>
      <SpaceChat space={s} config={config} handle={createRef()} />
    </RootStoreProvider>,
  );
  await settle();
}

describe('Set up with agent', () => {
  it('shows above the composer once the chat has turns', async () => {
    await render(space(), NO_COMMANDS);
    const btn = m!.host.querySelector<HTMLButtonElement>(
      '.space-setup-assist',
    );
    expect(btn).not.toBeNull();
    await click(btn);
    await settle();
    expect(
      transport.calls.find((c) => c.cmd === 'space_setup_assist'),
    ).toMatchObject({ args: { space: 's1' } });
  });

  it('is gone once dismissed, and stays gone for the project', async () => {
    await render(space(), NO_COMMANDS);
    await click(
      m!.host.querySelector('.space-setup-assist-hint [aria-label="Dismiss"]'),
    );
    expect(m!.host.querySelector('.space-setup-assist')).toBeNull();
    m!.unmount();
    await render(space({ id: 's1' }), NO_COMMANDS);
    expect(m!.host.querySelector('.space-setup-assist')).toBeNull();
  });

  it('is not offered when the project has commands, or while a turn runs', async () => {
    await render(space(), {
      ...NO_COMMANDS,
      commands: [
        { kind: 'run', script: 'npm start', layer: 'repo', allowed: true },
      ],
    });
    expect(m!.host.querySelector('.space-setup-assist')).toBeNull();
    m!.unmount();
    await render(space({ status: 'running', run: 'space:s1:u2' }), NO_COMMANDS);
    expect(m!.host.querySelector('.space-setup-assist')).toBeNull();
  });
});
