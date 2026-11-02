/** A file folds when it goes Viewed, as on GitHub, and by hand from the
 *  chevron in its header (#51). A Viewed flip clears a hand fold. */
import { act } from 'react';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { click, mount, type Mounted } from '../test/dom';
import { FakeTransport } from '../test/fakeTransport';
import { setTransport } from '../core/transport';
import type { FileDiff, Hunk, Revision } from '../gen/contract';
import { RootStore } from '../stores/RootStore';
import { RootStoreProvider } from '../stores/context';
import { DiffPane } from './DiffPane';

const PR = 'gh:acme/app#1';
const HEAD = 'h'.repeat(40);
const PATH = 'src/a.ts';

const hunk = (id: string, at: number, path = PATH): Hunk => ({
  id,
  path,
  old_start: at,
  old_lines: 0,
  new_start: at,
  new_lines: 1,
  header: '',
  lines: [{ kind: 'add', text: 'x', old_no: null, new_no: at }],
});

const file: FileDiff = {
  path: PATH,
  old_path: null,
  status: 'modified',
  similarity: null,
  old_mode: null,
  new_mode: null,
  flags: [],
  additions: 2,
  deletions: 0,
  hunks: [hunk('a0', 1), hunk('a1', 41)],
};

const generated: FileDiff = {
  ...file,
  path: 'pnpm-lock.yaml',
  flags: [{ kind: 'generated', reason: 'lockfile' }],
  hunks: [hunk('g0', 1, 'pnpm-lock.yaml')],
};

const rev: Revision = { base: 'b'.repeat(40), head: HEAD, files: [file] };

function seed(store: RootStore, viewed = false) {
  const pool = store.sync.pool;
  pool.put('pr', PR, {
    pr: { repo: { host: 'github', owner: 'acme', name: 'app' }, number: 1 },
    url: 'https://example.test/acme/app/pull/1',
    head_sha: HEAD,
  });
  pool.put('pr_detail', PR, { threads: [], checks: [], commits: [] });
  pool.put('grouping', PR, {
    rev: HEAD,
    groups: [
      {
        id: 'g1',
        title: 'Group g1',
        why: '',
        kind: 'normal',
        risk: null,
        check: [],
        hunks: ['a0', 'a1', 'g0'],
        outliers: [],
      },
    ],
  });
  if (viewed) {
    pool.put('file_viewed', `${PR}:${PATH}`, {
      pr: PR,
      path: PATH,
      state: 'viewed',
    });
  }
}

const noop = () => {};

const tree = (store: RootStore, r: Revision, focusHunk: string | null) => (
  <RootStoreProvider store={store}>
    <DiffPane
      pr={PR}
      rev={r}
      mode={{ mode: 'groups', group: 'g1', file: null }}
      setMode={noop}
      since={null}
      sinceOnly={false}
      baselineSha={null}
      focusHunk={focusHunk}
      setFocusHunk={noop}
      composer={null}
      setComposer={noop}
      expandHunk={null}
    />
  </RootStoreProvider>
);

async function open(
  viewed = false,
  focusHunk: string | null = null,
  r = rev,
): Promise<Mounted & { focus: (h: string | null) => Promise<void> }> {
  const store = new RootStore();
  seed(store, viewed);
  const mounted = await mount(tree(store, r, focusHunk));
  return {
    ...mounted,
    focus: (h) => act(async () => mounted.root.render(tree(store, r, h))),
  };
}

let m: Mounted | null = null;
beforeEach(() => {
  // Viewed writes through the sync queue; no transport means rollback.
  const t = new FakeTransport();
  t.onCall('transact', () => ({ status: 'ok', last_sync_id: 1 }));
  setTransport(t);
});
afterEach(() => {
  m?.unmount();
  m = null;
});

const hunks = () => m!.host.querySelectorAll('.hunk').length;
const folded = () => m!.host.querySelector('.collapsed-file');
const chevron = () => m!.host.querySelector('.fold-file')!;
const viewedToggle = () => m!.host.querySelector('.viewed-toggle')!;

describe('DiffPane folds a file', () => {
  it('shows a plain, unviewed file open', async () => {
    m = await open();
    expect(hunks()).toBe(2);
    expect(folded()).toBeNull();
    expect(chevron().getAttribute('aria-expanded')).toBe('true');
    expect(chevron().getAttribute('aria-label')).toBe('Collapse file');
  });

  it('folds when the file goes Viewed, and opens again when it does not', async () => {
    m = await open();
    await click(viewedToggle());
    expect(folded()).not.toBeNull();
    expect(folded()!.textContent).toContain('2 hunks hidden');
    expect(hunks()).toBe(0);
    expect(chevron().getAttribute('aria-expanded')).toBe('false');

    await click(viewedToggle());
    expect(folded()).toBeNull();
    expect(hunks()).toBe(2);
  });

  it('folds and unfolds by hand from the chevron, without touching Viewed', async () => {
    m = await open();
    await click(chevron());
    expect(folded()).not.toBeNull();
    expect(viewedToggle().getAttribute('aria-pressed')).not.toBe('true');
    await click(chevron());
    expect(folded()).toBeNull();
    expect(hunks()).toBe(2);
  });

  it('starts folded when the file is already Viewed', async () => {
    m = await open(true);
    expect(folded()).not.toBeNull();
    expect(hunks()).toBe(0);
    await click(m.host.querySelector('.show-file'));
    expect(folded()).toBeNull();
    expect(hunks()).toBe(2);
  });

  it('opens a Viewed file for a jump to one of its hunks', async () => {
    m = await open(true, 'a1');
    expect(folded()).toBeNull();
    expect(m.host.querySelector('#H-a1')).not.toBeNull();
    await click(chevron());
    expect(folded()).not.toBeNull();
  });

  it('keeps a generated file opened by hand across jumps', async () => {
    const g = await open(false, null, { ...rev, files: [generated] });
    m = g;
    expect(folded()).not.toBeNull();
    await click(m.host.querySelector('.show-file'));
    expect(folded()).toBeNull();
    await g.focus('g0');
    await g.focus(null);
    expect(folded()).toBeNull();
    expect(hunks()).toBe(1);
  });

  it('drops a hand override when Viewed flips', async () => {
    m = await open(true);
    await click(chevron());
    expect(folded()).toBeNull();
    await click(viewedToggle());
    expect(folded()).toBeNull();
    await click(viewedToggle());
    expect(folded()).not.toBeNull();
  });
});
