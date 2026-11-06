import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import type { PrSummary } from '../gen/contract';
import { setTransport } from '../core/transport';
import { RootStore } from '../stores/RootStore';
import { RootStoreProvider } from '../stores/context';
import { click, mount, settle, typeInto, type Mounted } from '../test/dom';
import { anchor, draft, PR_ID } from '../test/factories';
import { FakeTransport } from '../test/fakeTransport';
import { Modals } from './Modals';

globalThis.CSS ??= {
  escape: (s: string) => s.replace(/[^\w-]/g, '\\$&'),
} as unknown as typeof CSS;

const summary = (host: 'github' | 'local'): PrSummary => ({
  pr: {
    repo: { host, owner: 'acme', name: 'app' },
    number: 1,
    host_id: null,
  },
  title: 't',
  author: { login: 'me', name: null, avatar_url: null },
  state: 'open',
  base_ref: 'main',
  head_ref: 'f',
  head_sha: 'abc',
  additions: 0,
  deletions: 0,
  changed_files: 0,
  reasons: ['review_requested'],
  checks: 'none',
  approvals: 0,
  required_approvals: null,
  updated_at: 0,
  url: 'https://example.test/pull/1',
});

describe('Submit review modal', () => {
  let transport: FakeTransport;
  let store: RootStore;
  let m: Mounted;

  async function open(host: 'github' | 'local' = 'github') {
    store.sync.pool.put('pr', PR_ID, summary(host));
    store.ui.go({ name: 'review', pr: PR_ID });
    store.ui.openModal('submit');
    m = await mount(
      <RootStoreProvider store={store}>
        <Modals />
      </RootStoreProvider>,
    );
    await settle();
  }

  const buttons = () => [...document.querySelectorAll('button')];
  const btn = (text: string) =>
    buttons().find((b) => b.textContent?.trim() === text);
  const radio = (label: string) =>
    [...document.querySelectorAll('label')].find((l) =>
      l.textContent?.includes(label),
    );
  const submits = () =>
    transport.calls.filter((c) => c.cmd === 'submit_review');

  beforeEach(() => {
    transport = new FakeTransport();
    transport.onCall('submit_review', () => null);
    setTransport(transport);
    store = new RootStore();
  });

  afterEach(() => {
    m?.unmount();
    document.body.innerHTML = '';
  });

  it('says there are no pending comments when the pool has none', async () => {
    await open();
    expect(document.body.textContent).toContain('No pending comments');
    expect(document.body.textContent).not.toContain('Pending comments (');
  });

  it('lists drafts with path:line and a you / agent tag', async () => {
    store.sync.pool.put(
      'draft',
      'd1',
      draft({
        id: 'd1',
        anchor: anchor({ path: 'src/a.rs', line: 7 }),
        source: { kind: 'me' },
      }),
    );
    store.sync.pool.put(
      'draft',
      'd2',
      draft({
        id: 'd2',
        anchor: anchor({ path: 'src/b.rs', line: 42 }),
        source: { kind: 'agent', agent: 'codex' },
      }),
    );
    await open();
    const text = document.body.textContent ?? '';
    expect(text).toContain('Pending comments (2)');
    expect(text).toContain('src/a.rs:7');
    expect(text).toContain('src/b.rs:42');
    expect(text).toContain('you');
    expect(document.querySelector('.agent-tag')?.textContent).toBe('codex');
    expect(text).not.toContain('No pending comments');
  });

  it('sends COMMENT by default, with the body trimmed', async () => {
    await open();
    await typeInto(document.querySelector('textarea'), '  looks fine \n');
    await click(btn('Submit'));
    await settle();
    expect(submits()).toEqual([
      {
        cmd: 'submit_review',
        args: { req: { pr: PR_ID, event: 'COMMENT', body: 'looks fine' } },
      },
    ]);
  });

  it.each([
    ['Approve', 'APPROVE'],
    ['Request changes', 'REQUEST_CHANGES'],
  ])('sends %s as %s', async (label, event) => {
    await open();
    await click(radio(label));
    await click(btn('Submit'));
    await settle();
    expect(submits()).toHaveLength(1);
    expect(submits()[0]!.args).toEqual({
      req: { pr: PR_ID, event, body: '' },
    });
  });

  it('closes the modal and leaves the review on submit', async () => {
    await open();
    await click(btn('Submit'));
    await settle();
    expect(store.ui.modal).toBeNull();
    expect(store.ui.route.name).not.toBe('review');
  });

  it('local mode: no verdict picker, always COMMENT, button says Done', async () => {
    await open('local');
    expect(document.body.textContent).toContain('Local review');
    expect(radio('Approve')).toBeUndefined();
    expect(btn('Submit')).toBeUndefined();
    await click(btn('Done'));
    await settle();
    expect(submits()[0]!.args).toEqual({
      req: { pr: PR_ID, event: 'COMMENT', body: '' },
    });
  });

  it('Cancel closes without calling submit_review', async () => {
    await open();
    await click(btn('Cancel'));
    await settle();
    expect(store.ui.modal).toBeNull();
    expect(store.ui.route).toEqual({ name: 'review', pr: PR_ID });
    expect(submits()).toHaveLength(0);
  });

  it('stays open on a failed submit, keeping drafts, comment and verdict', async () => {
    store.sync.pool.put('draft', 'd1', draft({ id: 'd1' }));
    transport.onCall('submit_review', () => {
      throw new Error('boom');
    });
    await open();
    await click(radio('Approve'));
    await typeInto(document.querySelector('textarea'), 'my verdict');
    await click(btn('Submit'));
    await settle();
    expect(submits()).toHaveLength(1);
    expect(store.models.drafts.byPr(PR_ID).map((d) => d.id)).toEqual(['d1']);
    expect(store.ui.modal).not.toBeNull();
    expect(store.ui.route).toEqual({ name: 'review', pr: PR_ID });
    expect(document.querySelector('textarea')?.value).toBe('my verdict');
    expect(btn('Submit')?.hasAttribute('disabled')).toBe(false);
  });

  it('shows Sending… and stays open until the host answers', async () => {
    let release!: () => void;
    transport.onCall(
      'submit_review',
      () =>
        new Promise<null>((r) => {
          release = () => r(null);
        }),
    );
    await open();
    await click(btn('Submit'));
    expect(submits()).toHaveLength(1);
    expect(store.ui.modal).not.toBeNull();
    expect(btn('Sending…')?.hasAttribute('disabled')).toBe(true);
    release();
    await settle();
    expect(store.ui.modal).toBeNull();
    expect(store.ui.route.name).not.toBe('review');
  });
});
