/** A hunk header's "Ask agent" (DiffPane's context menu) used to call `ask`
 *  straight through, bypassing the pending bubble the chat composer shows —
 *  nothing appeared in the AI panel until the whole reply landed. It now
 *  goes through the same `askPending` path: the aside opens, a pending
 *  bubble with the question shows at once, the agent's streaming text fills
 *  it in, and the real reply (a synced `ChatMessage`) replaces it. */
import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { askRunId } from '../actions';
import { setTransport } from '../core/transport';
import { RootStore } from '../stores/RootStore';
import { RootStoreProvider } from '../stores/context';
import { FakeTransport } from '../test/fakeTransport';
import { Harness, PR, revision, seed } from '../test/bigPr';
import { SidePanel } from './SidePanel';

// jsdom lays nothing out: the chat's scroll-to-bottom has nowhere to go.
Element.prototype.scrollIntoView ??= function () {};

let transport: FakeTransport;
let store: RootStore;
let host: HTMLDivElement;
let root: Root;
let resolveAsk: (id: string) => void;

beforeEach(async () => {
  transport = new FakeTransport();
  transport.onCall('detect_agents', () => []);
  transport.onCall('list_skills', () => []);
  transport.onCall('ask', () => new Promise<string>((r) => (resolveAsk = r)));
  setTransport(transport);
  const rev = revision();
  store = new RootStore();
  seed(store, rev);
  host = document.createElement('div');
  document.body.appendChild(host);
  root = createRoot(host);
  await act(async () =>
    root.render(
      <RootStoreProvider store={store}>
        <Harness rev={rev} onReady={() => {}} />
        <SidePanel pr={PR} />
      </RootStoreProvider>,
    ),
  );
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
});

const hunk = () => host.querySelector('#H-f0h0')!;

const askAgentFromHunkMenu = async () => {
  await act(async () => {
    hunk().firstElementChild!.dispatchEvent(
      new MouseEvent('contextmenu', { bubbles: true, cancelable: true }),
    );
  });
  const item = store.ui.contextMenu!.items.find((i) => i.id === 'ask-agent')!;
  await act(async () => item.onAction!());
};

const pendingBubble = () => {
  const all = host.querySelectorAll('.chat .msg.agent');
  return all.length ? all[all.length - 1]! : null;
};

describe('a hunk header\'s "Ask agent"', () => {
  it('opens the aside with a pending bubble, no command fired yet', async () => {
    await act(async () => store.ui.toggleAside(false));
    expect(store.ui.asideShown).toBe(false);
    await askAgentFromHunkMenu();
    expect(store.ui.asideShown).toBe(true);
    expect(transport.calls.some((c) => c.cmd === 'ask')).toBe(true);
    expect(transport.calls.find((c) => c.cmd === 'ask')).toMatchObject({
      args: {
        req: { pr: PR, question: 'about the hunk at src/mod0/file0.ts:1' },
      },
    });
  });

  it('shows the question streaming in, then gives way to the real reply', async () => {
    await askAgentFromHunkMenu();
    await act(async () => resolveAsk('q1'));

    // Dots while nothing has streamed yet.
    expect(
      pendingBubble()?.querySelector('[aria-label="Agent is thinking"]'),
    ).not.toBeNull();

    await act(async () => {
      store.ephemeral.handle({
        type: 'agent_delta',
        run: askRunId('q1'),
        agent: 'claude',
        text: 'this hunk changes the…',
      });
    });
    expect(pendingBubble()?.textContent).toContain('this hunk changes the…');

    await act(async () => {
      store.sync.pool.put('chat_message', 'q1', {
        id: 'q1',
        pr: PR,
        author: { kind: 'me' },
        text: 'about the hunk at src/mod0/file0.ts:1',
        refs: [],
        at: Date.now(),
        failed: null,
      });
      store.sync.pool.put('chat_message', 'r1', {
        id: 'r1',
        pr: PR,
        author: { kind: 'agent', agent: 'claude' },
        text: 'it renames the helper',
        refs: [],
        at: Date.now() + 1,
        failed: null,
      });
    });

    // The pending bubble is gone — the real messages took its place.
    expect(store.ephemeral.chatPending.has(PR)).toBe(false);
    expect(host.querySelector('.chat')!.textContent).toContain(
      'it renames the helper',
    );
    expect(host.querySelectorAll('.chat .msg.agent').length).toBe(1);
  });
});
