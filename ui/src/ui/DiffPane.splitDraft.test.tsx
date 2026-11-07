/** Split view: a context line is on both sides but is one anchor, so a
 *  draft on it shows once, on the new side — not once per side. */
import { afterAll, afterEach, beforeAll, describe, expect, it } from 'vitest';
import { mount, type Mounted } from '../test/dom';
import type { Anchor, FileDiff, Revision } from '../gen/contract';
import { RootStore } from '../stores/RootStore';
import { RootStoreProvider } from '../stores/context';
import { DiffPane } from './DiffPane';

const PR = 'gh:acme/app#1';
const HEAD = 'h'.repeat(40);

const file: FileDiff = {
  path: 'notes.md',
  old_path: null,
  status: 'modified',
  similarity: null,
  old_mode: null,
  new_mode: null,
  flags: [],
  additions: 1,
  deletions: 0,
  hunks: [
    {
      id: 'h1',
      path: 'notes.md',
      old_start: 1,
      old_lines: 1,
      new_start: 1,
      new_lines: 2,
      header: '',
      lines: [
        { kind: 'add', text: 'hello world', old_no: null, new_no: 1 },
        { kind: 'ctx', text: 'main v3', old_no: 1, new_no: 2 },
      ],
    },
  ],
};

const rev: Revision = { base: 'b'.repeat(40), head: HEAD, files: [file] };

const anchor = (line: number): Anchor => ({
  rev: HEAD,
  path: 'notes.md',
  side: 'new',
  line,
  start_line: null,
  start_side: null,
});

// The pane falls back to unified below 760px; jsdom measures 0.
const width = Object.getOwnPropertyDescriptor(
  HTMLElement.prototype,
  'clientWidth',
);
beforeAll(() => {
  Object.defineProperty(HTMLElement.prototype, 'clientWidth', {
    configurable: true,
    get: () => 1400,
  });
});
afterAll(() => {
  if (width) Object.defineProperty(HTMLElement.prototype, 'clientWidth', width);
});

let m: Mounted | null = null;
afterEach(() => {
  m?.unmount();
  m = null;
});

const noop = () => {};

describe('split diff drafts', () => {
  it('shows a draft on a context line once, on the right', async () => {
    const store = new RootStore();
    store.ui.diffView = 'split';
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
          hunks: ['h1'],
          outliers: [],
        },
      ],
    });
    pool.put('draft', 'd1', {
      id: 'd1',
      pr: PR,
      anchor: anchor(2),
      body: 'Also keep main v3 as the last line.',
      source: 'human',
    });
    pool.put('draft', 'd2', {
      id: 'd2',
      pr: PR,
      anchor: anchor(1),
      body: 'Rename hello world.',
      source: 'human',
    });
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
    expect(m.host.querySelector('.split-lines')).not.toBeNull();
    const drafts = [...m.host.querySelectorAll('.draft')];
    expect(drafts.map((d) => d.textContent?.includes('main v3'))).toEqual([
      false,
      true,
    ]);
    const rows = [...m.host.querySelectorAll('.split-row')];
    const ctx = rows[rows.length - 1]!;
    const [left, right] = [...ctx.querySelectorAll('.split-col')];
    expect(left!.querySelector('.draft')).toBeNull();
    expect(right!.querySelector('.draft')).not.toBeNull();

    // In a half-width column the card's actions wrap under its label
    // rather than run off its edge; the label itself never breaks.
    const card = right!.querySelector('.draft')!;
    const actions = card.querySelector('.draft-actions')!;
    expect(actions.textContent).toContain('delete');
    expect(actions.parentElement!.className).toContain('flex-wrap');
    expect(card.querySelector('.draft-label')!.className).toContain(
      'whitespace-nowrap',
    );

    // A narrow pane wraps the file header's controls under the name
    // instead of cutting them off; the name keeps room (layout is CSS).
    const header = m.host.querySelector('.file-h')!;
    expect(header.className).toContain('flex-wrap');
    expect(header.querySelector('.file-name')!.className).toContain(
      'min-w-[6rem]',
    );
  });
});
