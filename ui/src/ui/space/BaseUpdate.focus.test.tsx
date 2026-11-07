/** A local folder's base moves with a commit made in the folder, which no
 *  turn or fetch reports: coming back to the window asks again. */
import { act } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Space } from '../../gen/contract';
import { setTransport } from '../../core/transport';
import { RootStore } from '../../stores/RootStore';
import { RootStoreProvider } from '../../stores/context';
import { FakeTransport } from '../../test/fakeTransport';
import { mount, settle, type Mounted } from '../../test/dom';
import { BASE_STATUS_POLL_MS, useBaseStatus } from './BaseUpdate';

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

function Probe() {
  const s = useBaseStatus(space);
  return <span>{s ? `${s.behind} behind` : 'up to date'}</span>;
}

let transport: FakeTransport;
let m: Mounted | null = null;

beforeEach(() => {
  transport = new FakeTransport();
  setTransport(transport);
});

afterEach(() => {
  m?.unmount();
  m = null;
});

describe('Base moved', () => {
  it('is asked again when the window comes back', async () => {
    let behind = 0;
    transport.onCall('space_base_status', () => ({ base_tip: 't', behind }));
    m = await mount(
      <RootStoreProvider store={new RootStore()}>
        <Probe />
      </RootStoreProvider>,
    );
    await settle();
    expect(m.host.textContent).toBe('0 behind');

    // A commit lands on the folder's branch while you're away.
    behind = 2;
    await act(async () => {
      window.dispatchEvent(new Event('focus'));
    });
    await settle();
    expect(m.host.textContent).toBe('2 behind');
  });

  it('fetches a stale project when the page mounts, then asks again', async () => {
    let behind = 0;
    transport.onCall('space_base_status', () => ({ base_tip: 't', behind }));
    const store = new RootStore();
    store.sync.pool.put('project', 'local:app', {
      id: 'local:app',
      repo: { host: 'github', owner: 'acme', name: 'app' },
      path: null,
      status: 'ready',
      last_fetched_at: Date.now() - 60 * 60_000,
    });
    transport.onCall('refresh_project', () => {
      // The fetch brings the moved base; the core stamps the project.
      behind = 3;
      store.sync.pool.put('project', 'local:app', {
        id: 'local:app',
        repo: { host: 'github', owner: 'acme', name: 'app' },
        path: null,
        status: 'ready',
        last_fetched_at: Date.now(),
      });
      return null;
    });
    m = await mount(
      <RootStoreProvider store={store}>
        <Probe />
      </RootStoreProvider>,
    );
    await settle();
    await settle();
    expect(transport.calls.some((c) => c.cmd === 'refresh_project')).toBe(true);
    expect(m.host.textContent).toBe('3 behind');
  });

  it('asks again while the page stays open', async () => {
    vi.useFakeTimers();
    try {
      let behind = 0;
      transport.onCall('space_base_status', () => ({ base_tip: 't', behind }));
      m = await mount(
        <RootStoreProvider store={new RootStore()}>
          <Probe />
        </RootStoreProvider>,
      );
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });
      expect(m.host.textContent).toBe('0 behind');
      // Something fetched the copy without the page being told.
      behind = 1;
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BASE_STATUS_POLL_MS);
      });
      expect(m.host.textContent).toBe('1 behind');
    } finally {
      vi.useRealTimers();
    }
  });
});
