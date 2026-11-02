/** Live run output: a run's streamed text re-renders only its own view,
 *  and a fix task shows what its agent writes and does while it runs. */
import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { observer } from 'mobx-react-lite';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { setTransport } from '../core/transport';
import { delegateTask } from '../actions';
import { RootStore } from '../stores/RootStore';
import { RootStoreProvider } from '../stores/context';
import { FakeTransport } from '../test/fakeTransport';
import { LiveText, TaskLive, tailLines } from './RunLive';

let transport: FakeTransport;
let store: RootStore;
let host: HTMLDivElement;
let root: Root;

beforeEach(() => {
  transport = new FakeTransport();
  setTransport(transport);
  store = new RootStore();
  host = document.createElement('div');
  document.body.appendChild(host);
  root = createRoot(host);
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
});

const delta = (run: string, text: string) =>
  act(() =>
    store.ephemeral.handle({ type: 'agent_delta', run, agent: 'claude', text }),
  );
const log = (run: string, line: string) =>
  act(() =>
    store.ephemeral.handle({ type: 'run_log', run, agent: 'claude', line }),
  );

async function render(node: React.ReactNode) {
  await act(async () =>
    root.render(<RootStoreProvider store={store}>{node}</RootStoreProvider>),
  );
}

describe('LiveText', () => {
  it('shows the fallback, then the text as it streams, without re-rendering the view around it', async () => {
    let outer = 0;
    const Chat = observer(function Chat() {
      outer += 1;
      return <LiveText run="r1" fallback={<i className="dots" />} />;
    });
    await render(<Chat />);
    expect(host.querySelector('.dots')).not.toBeNull();

    delta('r1', 'Hel');
    delta('r1', 'lo');
    delta('r2', 'other run');
    expect(host.querySelector('.live-text')?.textContent).toBe('Hello');
    expect(outer).toBe(1);
  });
});

describe('TaskLive', () => {
  it('shows nothing for a task with no run this session', async () => {
    await render(<TaskLive task="t1" />);
    expect(host.querySelector('.task-live')).toBeNull();
  });

  it("shows the run's last lines and steps, the whole text on a click", async () => {
    transport.onCall('delegate_task', () => 'fix:1');
    await render(<TaskLive task="t1" />);
    await act(async () => {
      expect(await delegateTask(store, 't1', 'claude')).toBe('fix:1');
    });
    expect(host.querySelector('.task-live')).toBeNull();

    delta('fix:1', 'one\ntwo\n\nthree\n');
    delta('fix:1', 'four');
    log('fix:1', 'tool: Edit · src/lib.rs');
    const text = () =>
      host.querySelector<HTMLButtonElement>('.task-live-text')!;
    expect(text().textContent).toBe('two\nthree\nfour');
    expect(host.querySelector('.task-live')!.textContent).toContain('Edit');

    await act(async () => text().click());
    expect(text().getAttribute('aria-expanded')).toBe('true');
    expect(text().textContent).toBe('one\ntwo\n\nthree\nfour');
  });

  it('a new fix of the task drops what the last one showed', async () => {
    transport.queueResult('delegate_task', 'fix:1', 'fix:2');
    await render(<TaskLive task="t1" />);
    await act(async () => void (await delegateTask(store, 't1')));
    delta('fix:1', 'first try');
    await act(async () => void (await delegateTask(store, 't1')));
    expect(store.ephemeral.agentText.has('fix:1')).toBe(false);
    expect(host.querySelector('.task-live')).toBeNull();
    delta('fix:2', 'second try');
    expect(host.querySelector('.task-live-text')!.textContent).toBe(
      'second try',
    );
  });
});

describe('tailLines', () => {
  it('keeps the last non-blank lines', () => {
    expect(tailLines('a\n\nb\nc\n', 2)).toBe('b\nc');
    expect(tailLines('', 3)).toBe('');
  });
});
