/** A space's chat renders through the same `ChatTurn` pieces as the AI
 *  panel: your turns as bubbles apart from the agent's replies, the live
 *  turn as an agent bubble, Stop in the composer. */
import { act, createRef } from 'react';
import { afterAll, afterEach, beforeAll, describe, expect, it } from 'vitest';
import { mount, settle, typeInto, type Mounted } from '../../test/dom';
import type { ChatMessage, Space } from '../../gen/contract';
import { setTransport } from '../../core/transport';
import { RootStore } from '../../stores/RootStore';
import { RootStoreProvider } from '../../stores/context';
import { FakeTransport } from '../../test/fakeTransport';
import { SpaceChat } from './SpacePage';
import {
  chatLayout,
  frames,
  installChatLayout,
  layoutRows,
  uninstallChatLayout,
  userScroll,
} from '../../test/chatLayout';
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

beforeAll(installChatLayout);
afterAll(uninstallChatLayout);

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
  transport.onCall('space_send', () => 'stored');
  transport.onCall('approve_space_plan', () => 'm2');
  transport.onCall('answer_space_question', () => null);
  setTransport(transport);
  const store = new RootStore();
  store.sync.pool.put('space', s.id, s);
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

describe('sending', () => {
  it('jumps to the bottom, though you had scrolled up', async () => {
    chatLayout.rowHeight = 300;
    try {
      const store = await render(space());
      const many = 30;
      act(() => {
        for (let i = 0; i < many; i += 1) {
          store.sync.pool.put(
            'chat_message',
            `old${i}`,
            message(`old${i}`, { text: `old ${i}`, at: 100 + i }),
          );
        }
      });
      layoutRows();
      await frames();
      const log = m!.host.querySelector<HTMLElement>('.chat-log')!;
      const end = () => log.scrollHeight - log.clientHeight;
      userScroll(log, 100);
      expect(log.scrollTop).toBe(100);

      const box = m!.host.querySelector('.chat-input textarea');
      await typeInto(box, 'go on');
      const send = m!.host.querySelector<HTMLButtonElement>(
        '.chat-input button[aria-label="Send"]',
      )!;
      await act(async () => send.click());
      layoutRows();
      await frames();
      expect(log.scrollTop).toBe(end());
      expect(m!.host.querySelector('.chat-jump')).toBeNull();
    } finally {
      chatLayout.rowHeight = 100;
    }
  });
});

describe('the changed files of a reply', () => {
  it('a chip asks the Changes pane to open on its file', async () => {
    const store = await render(space());
    act(() =>
      store.sync.pool.merge('chat_message', 'a1', {
        duration_ms: 1000,
        changed_files: [{ path: 'src/a.rs', additions: 1, deletions: 0 }],
      }),
    );
    await settle();
    const chip = m!.host.querySelector<HTMLButtonElement>('.turn-file')!;
    await act(async () => chip.click());
    expect(store.ephemeral.spaceChangesFocus.get('s1')).toEqual({
      path: 'src/a.rs',
    });
  });
});

describe('the space chat', () => {
  it('renders turns with the shared message component, as two bubble kinds', async () => {
    await render(space());
    const user = m!.host.querySelector('.chat-message[data-msg="u1"]')!;
    const agent = m!.host.querySelector('.chat-message[data-msg="a1"]')!;
    expect(user.getAttribute('data-variant')).toBe('user');
    expect(user.querySelector('.user-bubble')?.textContent).toBe('add a flag');
    expect(agent.getAttribute('data-variant')).toBe('agent');
    // One agent in the chat: no tag on its replies.
    expect(agent.querySelector('.agent-tag')).toBeNull();
    expect(agent.querySelector('.message-actions')).not.toBeNull();
  });

  it('names no agent while every reply is one agent, whichever it is', async () => {
    const store = await render(space({ agent: 'codex' }));
    act(() =>
      store.sync.pool.merge('chat_message', 'a1', {
        author: { kind: 'agent', agent: 'codex' },
      }),
    );
    await settle();
    expect(m!.host.querySelectorAll('.agent-tag')).toHaveLength(0);
  });

  it('names the agent on every reply once two agents have answered', async () => {
    const store = await render(space());
    act(() => {
      store.sync.pool.put(
        'chat_message',
        'u2',
        message('u2', { text: 'and now?', at: 3 }),
      );
      store.sync.pool.put(
        'chat_message',
        'a2',
        message('a2', {
          author: { kind: 'agent', agent: 'codex' },
          text: 'Done.',
          at: 4,
        }),
      );
    });
    await settle();
    const tag = (id: string) =>
      m!.host.querySelector(`.chat-message[data-msg="${id}"] .agent-tag`)
        ?.textContent;
    expect(tag('a1')).toBe('claude');
    expect(tag('a2')).toBe('codex');
  });

  it('a turn you stopped is a quiet "Stopped", not a failure card', async () => {
    const store = await render(space());
    act(() =>
      store.sync.pool.merge('chat_message', 'a1', {
        text: 'I found the cause.',
        failed: 'stopped',
        duration_ms: 1000,
      }),
    );
    await settle();
    const agent = m!.host.querySelector('.chat-message[data-msg="a1"]')!;
    expect(agent.querySelector('.agent-failed')).toBeNull();
    expect(agent.querySelector('.agent-stopped')?.textContent).toBe('Stopped');
    expect(agent.textContent).toContain('I found the cause.');
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

  it('a running turn grows in order; its stored reply takes over with nothing told twice', async () => {
    const run = 'space:s1:u2';
    const s = space({ status: 'running', run });
    const store = await render(s);
    const event = (e: Parameters<typeof store.ephemeral.handle>[0]) =>
      act(() => {
        store.ephemeral.handle(e);
        store.ephemeral.flushDeltas();
      });
    event({ type: 'agent_delta', run, agent: 'claude', text: 'Looking.' });
    event({
      type: 'run_step',
      run,
      step: { kind: 'tool', name: 'Read', target: 'a.rs', ok: null, id: 't1' },
    });
    event({ type: 'agent_delta', run, agent: 'claude', text: 'All done.' });
    const live = m!.host.querySelector('.chat-live')!;
    expect(live.querySelector('.turn-timeline')?.textContent).toContain(
      'Looking.',
    );
    expect(live.querySelector('.turn-tool')?.textContent).toContain('a.rs');

    // The reply lands while its check still runs: it holds the turn now.
    const replied: Space = {
      ...s,
      turn_log: [
        {
          message: 'u2',
          reply: 'a2',
          before: 'b',
          after: 'h',
          check: { status: 'running' },
        } as never,
      ],
    };
    act(() => {
      store.sync.pool.put(
        'chat_message',
        'a2',
        message('a2', {
          author: { kind: 'agent', agent: 'claude' },
          text: 'All done.',
          at: 3,
          activity: [
            { kind: 'text', text: 'Looking.' },
            { kind: 'tool', name: 'Read', target: 'a.rs', ok: true },
            { kind: 'tool', name: 'Bash', target: 'x', ok: true },
          ],
          duration_ms: 5000,
        }),
      );
      store.sync.pool.put('space', s.id, replied);
    });
    await act(async () =>
      m!.root.render(
        <RootStoreProvider store={store}>
          <SpaceChat space={replied} config={null} handle={createRef()} />
        </RootStoreProvider>,
      ),
    );
    expect(m!.host.querySelector('.chat-live')).toBeNull();
    const reply = m!.host.querySelector('.chat-message[data-msg="a2"]')!;
    expect(reply.querySelector('.turn-fold-line')?.textContent).toBe(
      '2 tool calls, 1 message',
    );
    expect(m!.host.querySelectorAll('.turn-tool')).toHaveLength(0);
    expect(reply.querySelector('.space-check')).not.toBeNull();
    expect(
      [...m!.host.querySelectorAll('.chat-log p')].filter(
        (p) => p.textContent === 'All done.',
      ),
    ).toHaveLength(1);
  });

  it('the last plan turn: a plan badge, Approve calls the core, Revise asks in the composer', async () => {
    await render(
      space({
        mode: 'plan',
        build_mode: 'run',
        turn_log: [
          {
            n: 1,
            message: 'u1',
            reply: 'a1',
            before: 'b',
            after: 'h',
            plan: '1. Add `--flag`',
          } as never,
        ],
      }),
    );
    const agent = m!.host.querySelector('.chat-message[data-msg="a1"]')!;
    expect(agent.querySelector('.space-plan-badge')?.textContent).toBe('plan');
    const actions = agent.querySelector('.space-plan-actions')!;
    expect(actions.textContent).toContain('implements it in Run');
    const [approve, revise] = [...actions.querySelectorAll('button')] as [
      HTMLButtonElement,
      HTMLButtonElement,
    ];
    await act(async () => approve.click());
    await settle();
    expect(
      transport.calls.find((c) => c.cmd === 'approve_space_plan'),
    ).toMatchObject({ args: { space: 's1' } });
    await act(async () => revise.click());
    await settle();
    expect(
      m!.host
        .querySelector('.chat-input textarea')
        ?.getAttribute('placeholder'),
    ).toBe('What should change in the plan?');
  });

  it('a waiting question: pick an option and send the answer', async () => {
    await render(
      space({
        status: 'running',
        run: 'space:s1:u2',
        question: {
          id: 'q1',
          run: 'space:s1:u2',
          asked_at: 0,
          questions: [
            { question: 'Which store?', options: ['SQLite', 'Postgres'] },
          ],
        },
      }),
    );
    const card = m!.host.querySelector('.space-question')!;
    expect(card.textContent).toContain('Which store?');
    const send = card.querySelector<HTMLButtonElement>('.space-answer')!;
    expect(send.disabled || send.getAttribute('aria-disabled') === 'true').toBe(
      true,
    );
    const sqlite = [
      ...card.querySelectorAll<HTMLButtonElement>('.space-option'),
    ].find((b) => b.textContent === 'SQLite')!;
    await act(async () => sqlite.click());
    await settle();
    await act(async () => send.click());
    await settle();
    expect(
      transport.calls.find((c) => c.cmd === 'answer_space_question'),
    ).toMatchObject({
      args: {
        req: {
          space: 's1',
          question: 'q1',
          answers: [{ selected: ['SQLite'], other: null }],
        },
      },
    });
  });
});
