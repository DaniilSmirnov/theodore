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
  transport.onCall('approve_space_plan', () => 'm2');
  transport.onCall('answer_space_question', () => null);
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
