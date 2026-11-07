/** Review with agent (docs/spaces.md §3): an item a past send carried says
 *  so, asks before running a second turn for it, and the footer counts it
 *  as sent. */
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import type { Space } from '../../gen/contract';
import { setTransport } from '../../core/transport';
import { RootStore } from '../../stores/RootStore';
import { RootStoreProvider } from '../../stores/context';
import { FakeTransport } from '../../test/fakeTransport';
import { click, mount, settle, type Mounted } from '../../test/dom';
import { SendAllToAgentFooter } from './SendAllToAgent';
import { SendToAgent } from './SendToAgent';

const PR = 'local:app#space-s1';
const HEAD = 'h'.repeat(40);

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
  review_pr: PR,
  pr: null,
  turns: 3,
  run: null,
  error: null,
  created_at: 0,
  updated_at: 0,
  sent_feedback: [{ item: { kind: 'draft', id: 'd1' }, turn: 3 }],
};

const draft = (id: string, line: number) => ({
  id,
  pr: PR,
  anchor: {
    rev: HEAD,
    path: 'notes.md',
    side: 'new',
    line,
    start_line: null,
    start_side: null,
  },
  body: `draft ${id}`,
  source: 'human',
});

let transport: FakeTransport;
let store: RootStore;
let m: Mounted | null = null;

beforeEach(() => {
  localStorage.clear();
  transport = new FakeTransport();
  transport.onCall('space_send_feedback', () => 'msg');
  setTransport(transport);
  store = new RootStore();
  store.sync.pool.put('space', 's1', space);
  store.sync.pool.put('draft', 'd1', draft('d1', 1));
  store.sync.pool.put('draft', 'd2', draft('d2', 2));
});

afterEach(() => {
  m?.unmount();
  m = null;
});

const sends = () =>
  transport.calls.filter((c) => c.cmd === 'space_send_feedback');

describe('Send to agent on a sent item', () => {
  it('says which turn carried it and asks before sending it again', async () => {
    m = await mount(
      <RootStoreProvider store={store}>
        <SendToAgent pr={PR} item={{ kind: 'draft', id: 'd1' }} />
      </RootStoreProvider>,
    );
    expect(m.host.querySelector('.sent-to-agent')?.textContent).toBe(
      'sent to agent · turn 3',
    );
    await click(m.host.querySelector('button.send-to-agent'));
    await settle();
    expect(sends()).toHaveLength(0);
    expect(m.host.textContent).toContain('Send again?');
    await click(m.host.querySelector('button.send-to-agent-again'));
    await settle();
    expect(sends().map((c) => c.args)).toEqual([
      { req: { space: 's1', items: [{ kind: 'draft', id: 'd1' }] } },
    ]);
  });

  it('sends an unsent item at once, with no mark', async () => {
    m = await mount(
      <RootStoreProvider store={store}>
        <SendToAgent pr={PR} item={{ kind: 'draft', id: 'd2' }} />
      </RootStoreProvider>,
    );
    expect(m.host.querySelector('.sent-to-agent')).toBeNull();
    await click(m.host.querySelector('button.send-to-agent'));
    await settle();
    expect(sends()).toHaveLength(1);
  });
});

describe('the Send N footer', () => {
  it('counts an individually sent item as sent, not as new', async () => {
    m = await mount(
      <RootStoreProvider store={store}>
        <SendAllToAgentFooter pr={PR} />
      </RootStoreProvider>,
    );
    const footer = m.host.querySelector('.send-all-to-agent')!;
    expect(footer.textContent).toContain('1 draft · 1 sent');
    expect(footer.textContent).toContain('Send 1 to agent');
  });
});
