/** A group's "What to check" list (#48): the item text is drag-selectable
 *  for copy even though `Checkbox.Content` is `select-none` so a click
 *  anywhere in the row still toggles the box. */
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { act } from 'react';
import { mount, type Mounted } from '../test/dom';
import type { FileDiff, Group, Revision } from '../gen/contract';
import { RootStore } from '../stores/RootStore';
import { RootStoreProvider } from '../stores/context';
import { DiffPane } from './DiffPane';
import { setTransport } from '../core/transport';
import { FakeTransport } from '../test/fakeTransport';

const PR = 'gh:acme/app#1';
const HEAD = 'h'.repeat(40);

const file = (path: string, ids: string[]): FileDiff => ({
  path,
  old_path: null,
  status: 'modified',
  similarity: null,
  old_mode: null,
  new_mode: null,
  flags: [],
  additions: ids.length,
  deletions: 0,
  hunks: ids.map((id, i) => ({
    id,
    path,
    old_start: 1 + i * 40,
    old_lines: 0,
    new_start: 1 + i * 40,
    new_lines: 1,
    header: '',
    lines: [{ kind: 'add', text: id, old_no: null, new_no: 1 + i * 40 }],
  })),
});

const rev: Revision = {
  base: 'b'.repeat(40),
  head: HEAD,
  files: [file('src/a.ts', ['a0'])],
};

const g: Group = {
  id: 'g1',
  title: 'Group g1',
  why: 'Explains g1.',
  kind: 'normal',
  risk: null,
  check: ['Verify the error path is covered'],
  hunks: ['a0'],
  outliers: [],
};

function seed(): RootStore {
  const store = new RootStore();
  store.sync.pool.put('pr', PR, {
    pr: { repo: { host: 'github', owner: 'acme', name: 'app' }, number: 1 },
    url: 'https://example.test/acme/app/pull/1',
    head_sha: HEAD,
  });
  store.sync.pool.put('pr_detail', PR, {
    threads: [],
    checks: [],
    commits: [],
  });
  store.sync.pool.put('grouping', PR, { rev: HEAD, groups: [g] });
  return store;
}

const noop = () => {};

let m: Mounted | null = null;
let transport = new FakeTransport();
beforeEach(() => {
  // A tick is a transaction: core accepts it.
  transport = new FakeTransport();
  transport.onCall('transact', () => ({ status: 'ok', last_sync_id: 1 }));
  setTransport(transport);
});
afterEach(() => {
  m?.unmount();
  m = null;
});

describe('GroupCard "What to check" list', () => {
  it('toggles on a click on the text, not at the end of a drag', async () => {
    const store = seed();
    m = await mount(
      <RootStoreProvider store={store}>
        <DiffPane
          pr={PR}
          rev={rev}
          mode={{ mode: 'groups', group: 'g1', file: null }}
          setMode={noop}
          since={null}
          sinceOnly={false}
          baselineSha={null}
          focusHunk={null}
          setFocusHunk={noop}
          composer={null}
          setComposer={noop}
          expandHunk={null}
        />
      </RootStoreProvider>,
    );
    const text = () => m!.host.querySelector('.gcard span.select-text')!;
    const input = () =>
      m!.host.querySelector<HTMLInputElement>('.gcard input[type="checkbox"]')!;
    const click = () =>
      act(async () => {
        text().dispatchEvent(new MouseEvent('click', { bubbles: true }));
      });

    // A drag leaves a selection behind: no toggle, and the selection stays.
    const selection = window.getSelection()!;
    selection.selectAllChildren(text());
    await click();
    expect(input().checked).toBe(false);
    selection.removeAllRanges();

    await click();
    expect(input().checked).toBe(true);
  });

  it('keeps item text selectable, and the control still toggles it', async () => {
    const store = seed();
    m = await mount(
      <RootStoreProvider store={store}>
        <DiffPane
          pr={PR}
          rev={rev}
          mode={{ mode: 'groups', group: 'g1', file: null }}
          setMode={noop}
          since={null}
          sinceOnly={false}
          baselineSha={null}
          focusHunk={null}
          setFocusHunk={noop}
          composer={null}
          setComposer={noop}
          expandHunk={null}
        />
      </RootStoreProvider>,
    );
    const item = m.host.querySelector('.gcard span.select-text');
    expect(item).not.toBeNull();
    expect(item!.textContent).toBe('Verify the error path is covered');

    const input = m.host.querySelector<HTMLInputElement>(
      '.gcard input[type="checkbox"]',
    )!;
    expect(input.checked).toBe(false);
    await act(async () => {
      input.dispatchEvent(
        new MouseEvent('click', { bubbles: true, cancelable: true, detail: 1 }),
      );
    });
    expect(input.checked).toBe(true);
  });
});

describe('"What to check" ticks', () => {
  const pane = (store: RootStore) => (
    <RootStoreProvider store={store}>
      <DiffPane
        pr={PR}
        rev={rev}
        mode={{ mode: 'groups', group: 'g1', file: null }}
        setMode={noop}
        since={null}
        sinceOnly={false}
        baselineSha={null}
        focusHunk={null}
        setFocusHunk={noop}
        composer={null}
        setComposer={noop}
        expandHunk={null}
      />
    </RootStoreProvider>
  );
  const input = () =>
    m!.host.querySelector<HTMLInputElement>('.gcard input[type="checkbox"]')!;
  const tick = () =>
    act(async () => {
      m!.host
        .querySelector('.gcard span.select-text')!
        .dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });

  it('survive the pane remounting (a tab switch) as a synced row', async () => {
    const store = seed();
    m = await mount(pane(store));
    await tick();
    expect(input().checked).toBe(true);
    expect(
      transport.calls.some(
        (c) =>
          c.cmd === 'transact' &&
          JSON.stringify(c.args).includes('"model":"check_tick"'),
      ),
    ).toBe(true);

    m.unmount();
    m = await mount(pane(store));
    expect(input().checked).toBe(true);

    await tick();
    expect(input().checked).toBe(false);
    m.unmount();
    m = await mount(pane(store));
    expect(input().checked).toBe(false);
  });
});
