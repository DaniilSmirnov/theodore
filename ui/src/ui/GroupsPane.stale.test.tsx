/** A grouping older than the open revision (a push removed some of its
 *  hunks) must render — not crash on the hunks the revision no longer has —
 *  and count only what's left. */
import { afterEach, describe, expect, it } from 'vitest';
import { mount, type Mounted } from '../test/dom';
import type { FileDiff, Revision } from '../gen/contract';
import { RootStore } from '../stores/RootStore';
import { RootStoreProvider } from '../stores/context';
import { GroupsPane } from './GroupsPane';

const PR = 'gh:acme/app#1';
const HEAD = 'h'.repeat(40);

const file = (path: string, hunk: string): FileDiff => ({
  path,
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
      id: hunk,
      path,
      old_start: 1,
      old_lines: 0,
      new_start: 1,
      new_lines: 1,
      header: '',
      lines: [{ kind: 'add', text: 'x', old_no: null, new_no: 1 }],
    },
  ],
});

const rev: Revision = {
  base: 'b'.repeat(40),
  head: HEAD,
  files: [file('src/a.ts', 'ha'), file('src/b.ts', 'hb')],
};

function seed(store: RootStore) {
  const pool = store.sync.pool;
  pool.put('pr', PR, {
    pr: { repo: { host: 'github', owner: 'acme', name: 'app' }, number: 1 },
    url: 'https://example.test/acme/app/pull/1',
    head_sha: HEAD,
  });
  const g = (id: string, hunks: string[], outliers: string[] = []) => ({
    id,
    title: `Group ${id}`,
    why: '',
    kind: 'normal',
    risk: null,
    check: [],
    hunks,
    outliers,
  });
  pool.put('grouping', PR, {
    rev: 'o'.repeat(40),
    groups: [
      // `gone` was pushed away; it's also the group's only outlier.
      g('g1', ['ha', 'gone', 'hb'], ['gone']),
      // Every hunk of this one is gone — the group goes too.
      g('g2', ['gone2']),
    ],
  });
}

let m: Mounted | null = null;
afterEach(() => {
  m?.unmount();
  m = null;
});

describe('GroupsPane with a grouping older than the revision', () => {
  it('renders, drops missing hunks and empty groups, counts the rest', async () => {
    const store = new RootStore();
    seed(store);
    m = await mount(
      <RootStoreProvider store={store}>
        <GroupsPane
          pr={PR}
          rev={rev}
          mode={{ mode: 'groups', group: 'g1', file: null }}
          setMode={() => {}}
          since={null}
          sinceOnly={false}
        />
      </RootStoreProvider>,
    );
    const titles = [...m.host.querySelectorAll('.gtitle')].map(
      (e) => e.textContent,
    );
    expect(titles).toEqual(['Group g1']);
    const head = m.host.querySelector('.grp-h')!.textContent;
    expect(head).toContain('2 files');
    expect(head).toContain('+2');
    expect(head).toContain('0/2');
    expect(m.host.textContent).toContain('0 of 1 group read');
    expect(m.host.textContent).not.toContain('off-pattern');
    expect(m.host.textContent).not.toContain('undefined');
  });

  it('badges a file tree row whose file is in no group', async () => {
    const store = new RootStore();
    seed(store);
    store.sync.pool.put('grouping', PR, {
      rev: 'o'.repeat(40),
      groups: [g1Only],
    });
    m = await mount(
      <RootStoreProvider store={store}>
        <GroupsPane
          pr={PR}
          rev={rev}
          mode={{ mode: 'files', group: null, file: null }}
          setMode={() => {}}
          since={null}
          sinceOnly={false}
        />
      </RootStoreProvider>,
    );
    const row = (path: string) =>
      [...m!.host.querySelectorAll('.file-tree [role="option"]')].find((r) =>
        r.textContent?.includes(path),
      );
    expect(row('a.ts')?.querySelector('.tree-ungrouped')).toBeNull();
    expect(row('b.ts')?.querySelector('.tree-ungrouped')).not.toBeNull();
  });
});

const g1Only = {
  id: 'g1',
  title: 'Group g1',
  why: '',
  kind: 'normal',
  risk: null,
  check: [],
  hunks: ['ha'],
  outliers: [],
};
