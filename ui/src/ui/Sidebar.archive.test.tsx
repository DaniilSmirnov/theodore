/** The Archive button on a sidebar space row (docs/spaces.md §3): only on
 *  space rows, archives without opening or selecting the row. */
import { act } from 'react';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import type { Space } from '../gen/contract';
import { setTransport } from '../core/transport';
import { RootStore } from '../stores/RootStore';
import { RootStoreProvider } from '../stores/context';
import { FakeTransport } from '../test/fakeTransport';
import { mount, settle, type Mounted } from '../test/dom';
import { Sidebar } from './Sidebar';

const space = (over: Partial<Space> & { id: string }): Space => ({
  project: 'local:app',
  name: over.id,
  goal: '',
  branch: `theo/${over.id}`,
  base: 'main',
  base_sha: 'b',
  head_sha: 'h',
  worktree: `/tmp/${over.id}`,
  status: 'idle',
  agent: 'claude',
  model: null,
  mode: 'edit',
  review_pr: null,
  pr: null,
  turns: 0,
  run: null,
  error: null,
  created_at: 0,
  updated_at: 0,
  ...over,
});

describe('sidebar Archive button', () => {
  let transport: FakeTransport;
  let store: RootStore;
  let view: Mounted;

  beforeEach(async () => {
    localStorage.clear();
    transport = new FakeTransport();
    transport.onCall('archive_space', () => null);
    setTransport(transport);
    store = new RootStore();
    store.sync.pool.put('space', 'a', space({ id: 'a', updated_at: 2 }));
    store.sync.pool.put(
      'space',
      'b',
      space({ id: 'b', updated_at: 1, status: 'running' }),
    );
    view = await mount(
      <RootStoreProvider store={store}>
        <Sidebar />
      </RootStoreProvider>,
    );
  });

  afterEach(() => view.unmount());

  const rows = () => [
    ...view.host.querySelectorAll<HTMLElement>('[role=option]'),
  ];
  const rowOf = (text: string) => rows().find((r) => r.textContent === text)!;
  const archiveBtn = (row: HTMLElement) =>
    row.querySelector<HTMLButtonElement>('button[aria-label="Archive space"]');
  const archiveCalls = () =>
    transport.calls.filter((c) => c.cmd === 'archive_space');

  it('is on space rows only', () => {
    const withBtn = rows().filter((r) => archiveBtn(r));
    expect(withBtn.map((r) => r.textContent)).toEqual(['a', 'b']);
    expect(archiveBtn(rowOf('New space'))).toBeNull();
    expect(archiveBtn(rowOf('Inbox'))).toBeNull();
  });

  it('archives that space and does not open or select the row', async () => {
    const before = JSON.stringify(store.ui.route);
    const btn = archiveBtn(rowOf('a'))!;
    expect(btn.disabled).toBe(false);
    await act(async () => btn.click());
    await settle();
    expect(archiveCalls().map((c) => c.args)).toEqual([{ space: 'a' }]);
    expect(JSON.stringify(store.ui.route)).toBe(before);
    expect(rowOf('a').getAttribute('aria-current')).toBeNull();
  });

  it('stays on its row for pointer and keyboard presses too', async () => {
    const before = JSON.stringify(store.ui.route);
    const btn = archiveBtn(rowOf('a'))!;
    await act(async () => {
      const opts = { bubbles: true, cancelable: true, pointerType: 'mouse' };
      btn.dispatchEvent(
        new PointerEvent('pointerdown', { ...opts, button: 0 }),
      );
      btn.dispatchEvent(new PointerEvent('pointerup', { ...opts, button: 0 }));
      btn.dispatchEvent(new MouseEvent('click', opts));
    });
    await act(async () => {
      btn.dispatchEvent(
        new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }),
      );
      btn.dispatchEvent(
        new KeyboardEvent('keyup', { key: 'Enter', bubbles: true }),
      );
    });
    await settle();
    expect(archiveCalls().length).toBeGreaterThan(0);
    expect(JSON.stringify(store.ui.route)).toBe(before);
  });

  it('is disabled while a turn runs', async () => {
    const btn = archiveBtn(rowOf('b'))!;
    expect(btn.disabled).toBe(true);
    await act(async () => btn.click());
    expect(archiveCalls()).toEqual([]);
  });

  it('steps back only when the archived space is the open one', async () => {
    await act(async () => store.ui.go({ name: 'space', id: 'a' }));
    const open = JSON.stringify(store.ui.route);
    store.sync.pool.put('space', 'c', space({ id: 'c', updated_at: 0 }));
    await settle();
    // Archiving another space leaves the open page alone.
    await act(async () => archiveBtn(rowOf('c'))!.click());
    await settle();
    expect(JSON.stringify(store.ui.route)).toBe(open);
    await act(async () => archiveBtn(rowOf('a'))!.click());
    await settle();
    expect(store.ui.route).not.toEqual({ name: 'space', id: 'a' });
  });

  it('control: pressing the row itself opens the space', async () => {
    await act(async () => rowOf('a').click());
    expect(store.ui.route).toEqual({ name: 'space', id: 'a' });
  });
});
