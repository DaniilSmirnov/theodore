/** Review actions against the real `RootStore` and a scripted transport:
 *  drafts (optimistic + rollback), findings, submit, open, skills, threads. */
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { setTransport } from './core/transport';
import { FakeTransport } from './test/fakeTransport';
import { RootStore } from './stores/RootStore';
import {
  addDraft,
  cancelRun,
  editDraft,
  findingToDraft,
  ignoreFinding,
  openPr,
  removeDraft,
  replyThread,
  resolveThread,
  runSkill,
  submitReview,
} from './actions';
import {
  anchor,
  draft,
  finding,
  openPrResponse,
  PR_ID,
} from './test/factories';
import { BITBUCKET_LAB, GITLAB_LAB } from './ui/settings/labs';

let transport: FakeTransport;
let store: RootStore;

const flush = () => new Promise<void>((r) => setTimeout(r, 0));
const calls = (cmd: string) => transport.calls.filter((c) => c.cmd === cmd);
const txOps = () =>
  calls('transact').map((c) => (c.args as { tx: { ops: unknown[] } }).tx.ops);

beforeEach(() => {
  transport = new FakeTransport();
  setTransport(transport);
  store = new RootStore();
});

describe('addDraft / editDraft / removeDraft', () => {
  beforeEach(() => {
    transport.onCall('transact', () => ({ status: 'ok', last_sync_id: 1 }));
  });

  it('addDraft applies optimistically and sends a create op', async () => {
    const a = anchor({ line: 7 });
    addDraft(store, PR_ID, a, 'nit');
    const [row] = store.models.drafts.byPr(PR_ID);
    expect(row).toMatchObject({
      pr: PR_ID,
      anchor: a,
      body: 'nit',
      source: { kind: 'me' },
    });
    await flush();
    expect(txOps()).toEqual([
      [{ op: 'create', model: 'draft', id: row!.id, data: row }],
    ]);
    expect(store.models.drafts.get(row!.id)).toBeDefined();
  });

  it('addDraft keeps an agent source', () => {
    addDraft(store, PR_ID, anchor(), 'x', { kind: 'agent', agent: 'claude' });
    expect(store.models.drafts.byPr(PR_ID)[0]?.source).toEqual({
      kind: 'agent',
      agent: 'claude',
    });
  });

  it('addDraft rolls back when the core rejects the transaction', async () => {
    transport.onCall('transact', () => ({ status: 'rejected', reason: 'no' }));
    addDraft(store, PR_ID, anchor(), 'nit');
    expect(store.models.drafts.byPr(PR_ID)).toHaveLength(1);
    await flush();
    expect(store.models.drafts.byPr(PR_ID)).toHaveLength(0);
  });

  it('addDraft rolls back when the transport throws', async () => {
    transport.onCall('transact', () => {
      throw new Error('offline');
    });
    addDraft(store, PR_ID, anchor(), 'nit');
    await flush();
    expect(store.models.drafts.byPr(PR_ID)).toHaveLength(0);
  });

  it('editDraft updates the body and sends an update op', async () => {
    const d = draft({ body: 'old' });
    store.sync.pool.put('draft', d.id, d);
    editDraft(store, d.id, 'new');
    expect(store.models.drafts.get(d.id)?.body).toBe('new');
    await flush();
    expect(txOps()).toEqual([
      [{ op: 'update', model: 'draft', id: d.id, data: { body: 'new' } }],
    ]);
  });

  it.each([
    ['rejected', () => ({ status: 'rejected', reason: 'no' })],
    [
      'throwing',
      () => {
        throw new Error('offline');
      },
    ],
  ])(
    'editDraft restores the old body when the transport is %s',
    async (_n, h) => {
      transport.onCall('transact', h);
      const d = draft({ body: 'old' });
      store.sync.pool.put('draft', d.id, d);
      editDraft(store, d.id, 'new');
      expect(store.models.drafts.get(d.id)?.body).toBe('new');
      await flush();
      expect(store.models.drafts.get(d.id)).toEqual(d);
    },
  );

  it('removeDraft deletes optimistically and sends a delete op', async () => {
    const d = draft();
    store.sync.pool.put('draft', d.id, d);
    removeDraft(store, d.id);
    expect(store.models.drafts.get(d.id)).toBeUndefined();
    await flush();
    expect(txOps()).toEqual([[{ op: 'delete', model: 'draft', id: d.id }]]);
    expect(store.models.drafts.get(d.id)).toBeUndefined();
  });

  it.each([
    ['rejected', () => ({ status: 'rejected', reason: 'no' })],
    [
      'throwing',
      () => {
        throw new Error('offline');
      },
    ],
  ])(
    'removeDraft restores the draft when the transport is %s',
    async (_n, h) => {
      transport.onCall('transact', h);
      const d = draft({ body: 'keep me' });
      store.sync.pool.put('draft', d.id, d);
      removeDraft(store, d.id);
      expect(store.models.drafts.get(d.id)).toBeUndefined();
      await flush();
      expect(store.models.drafts.get(d.id)).toEqual(d);
    },
  );
});

describe('findings', () => {
  beforeEach(() => {
    transport.onCall('transact', () => ({ status: 'ok', last_sync_id: 1 }));
  });

  it('findingToDraft creates a titled draft and marks the finding drafted', async () => {
    const f = finding({ title: 'Bug', body: 'It panics.' });
    store.sync.pool.put('finding', f.id, f);
    findingToDraft(store, PR_ID, f);

    const [d] = store.models.drafts.byPr(PR_ID);
    expect(d).toMatchObject({
      body: '**Bug** — It panics.',
      anchor: f.anchor,
      source: { kind: 'me' },
    });
    expect(store.models.findings.get(f.id)?.state).toBe('drafted');
    await flush();
    expect(txOps()).toEqual([
      [{ op: 'create', model: 'draft', id: d!.id, data: d }],
      [
        {
          op: 'update',
          model: 'finding',
          id: f.id,
          data: { state: 'drafted' },
        },
      ],
    ]);
  });

  it('ignoreFinding marks it ignored', async () => {
    const f = finding();
    store.sync.pool.put('finding', f.id, f);
    ignoreFinding(store, f.id);
    expect(store.models.findings.get(f.id)?.state).toBe('ignored');
    await flush();
    expect(txOps()).toEqual([
      [
        {
          op: 'update',
          model: 'finding',
          id: f.id,
          data: { state: 'ignored' },
        },
      ],
    ]);
  });
});

describe('submitReview', () => {
  it('sends submit_review with the request', async () => {
    transport.onCall('submit_review', () => null);
    await submitReview(store, PR_ID, 'APPROVE', 'lgtm');
    expect(transport.calls).toEqual([
      {
        cmd: 'submit_review',
        args: { req: { pr: PR_ID, event: 'APPROVE', body: 'lgtm' } },
      },
    ]);
  });

  it('rejects when the transport rejects and leaves drafts alone', async () => {
    transport.onCall('submit_review', () => {
      throw new Error('boom');
    });
    const d = draft();
    store.sync.pool.put('draft', d.id, d);
    await expect(submitReview(store, PR_ID, 'COMMENT', 'x')).rejects.toThrow(
      'boom',
    );
    expect(store.models.drafts.byPr(PR_ID)).toEqual([d]);
    expect(calls('transact')).toHaveLength(0);
  });
});

describe('openPr', () => {
  const URL = 'https://github.com/acme/app/pull/1';

  beforeEach(() => {
    transport.onCall('open_pr', () => openPrResponse());
    transport.onCall('bootstrap', () => ({ rows: [], last_sync_id: 0 }));
  });

  it('opens the review, navigates to it and returns the pr', async () => {
    const go = vi.spyOn(store.ui, 'go');
    expect(await openPr(store, URL)).toBe(PR_ID);
    expect(calls('open_pr')).toEqual([
      { cmd: 'open_pr', args: { req: { url: URL } } },
    ]);
    expect(calls('bootstrap')[0]?.args).toEqual({
      scope: { scope: 'pr', pr: PR_ID },
    });
    expect(go).toHaveBeenCalledWith({ name: 'review', pr: PR_ID });
    expect(store.sync.isPrLoaded(PR_ID)).toBe(true);
  });

  it('turns a blank tab into the review when asked to replace', async () => {
    const go = vi.spyOn(store.ui, 'go');
    const replaceTab = vi.spyOn(store.ui, 'replaceTab');
    const blank = { name: 'new', id: 'tab-1' } as const;
    expect(await openPr(store, URL, blank)).toBe(PR_ID);
    expect(replaceTab).toHaveBeenCalledWith(blank, {
      name: 'review',
      pr: PR_ID,
    });
    expect(go).not.toHaveBeenCalled();
  });

  it('returns null and does not navigate when open_pr is rejected', async () => {
    transport.onCall('open_pr', () => {
      throw new Error('no access');
    });
    const go = vi.spyOn(store.ui, 'go');
    expect(await openPr(store, URL)).toBeNull();
    expect(go).not.toHaveBeenCalled();
    expect(calls('bootstrap')).toHaveLength(0);
  });

  it('returns null and does not navigate when loading the pr fails', async () => {
    transport.onCall('bootstrap', () => {
      throw new Error('down');
    });
    const go = vi.spyOn(store.ui, 'go');
    expect(await openPr(store, URL)).toBeNull();
    expect(go).not.toHaveBeenCalled();
  });

  it.each([
    ['GitLab', 'https://gitlab.com/acme/app/-/merge_requests/3'],
    ['Bitbucket', 'https://bitbucket.org/acme/app/pull-requests/3'],
  ])('%s link with the host off warns and calls nothing', async (name, url) => {
    const go = vi.spyOn(store.ui, 'go');
    expect(await openPr(store, url)).toBeNull();
    expect(transport.calls).toEqual([]);
    expect(go).not.toHaveBeenCalled();
    expect(store.ephemeral.toasts.at(-1)).toMatchObject({
      level: 'warn',
      text: `${name} links open once ${name} is on in Settings → Labs.`,
    });
  });

  it.each([
    [GITLAB_LAB, 'https://gitlab.com/acme/app/-/merge_requests/3'],
    [BITBUCKET_LAB, 'https://bitbucket.org/acme/app/pull-requests/3'],
  ])('opens a link once %s is on', async (lab, url) => {
    store.sync.pool.put('setting', `user:${lab}`, {
      key: lab,
      scope: 'user',
      value: true,
    });
    expect(await openPr(store, url)).toBe(PR_ID);
    expect(calls('open_pr')[0]?.args).toEqual({ req: { url } });
  });
});

describe('runSkill / cancelRun', () => {
  it('runSkill sends run_skill and returns the run id', async () => {
    transport.onCall('run_skill', () => 'run-1');
    expect(await runSkill(store, PR_ID, 'review', ['claude'])).toBe('run-1');
    expect(transport.calls).toEqual([
      {
        cmd: 'run_skill',
        args: { req: { pr: PR_ID, skill: 'review', agents: ['claude'] } },
      },
    ]);
  });

  it('runSkill defaults to no agents', async () => {
    transport.onCall('run_skill', () => 'run-2');
    await runSkill(store, PR_ID, 'review');
    expect(calls('run_skill')[0]?.args).toEqual({
      req: { pr: PR_ID, skill: 'review', agents: [] },
    });
  });

  it('cancelRun cancels and clears the run state', async () => {
    transport.onCall('cancel_run', () => null);
    store.ephemeral.handle({
      type: 'agent_delta',
      run: 'run-1',
      agent: 'claude',
      text: 'hi',
    });
    store.ephemeral.handle({
      type: 'run_log',
      run: 'run-1',
      agent: 'claude',
      line: 'l',
    });
    expect(store.ephemeral.agentText.get('run-1')).toBe('hi');
    await cancelRun(store, 'run-1');
    expect(transport.calls).toEqual([
      { cmd: 'cancel_run', args: { run: 'run-1' } },
    ]);
    expect(store.ephemeral.agentText.has('run-1')).toBe(false);
    expect(store.ephemeral.runLog.has('run-1')).toBe(false);
  });

  it('cancelRun keeps the run when cancel_run fails', async () => {
    transport.onCall('cancel_run', () => {
      throw new Error('gone');
    });
    store.ephemeral.handle({
      type: 'agent_delta',
      run: 'run-1',
      agent: 'claude',
      text: 'hi',
    });
    await expect(cancelRun(store, 'run-1')).rejects.toThrow('gone');
    expect(store.ephemeral.agentText.get('run-1')).toBe('hi');
  });
});

describe('replyThread / resolveThread', () => {
  it('send the request and resolve', async () => {
    transport.onCall('reply_thread', () => null);
    transport.onCall('resolve_thread', () => null);
    await replyThread(store, PR_ID, 't1', 'thanks');
    await resolveThread(store, PR_ID, 't1', true);
    expect(transport.calls).toEqual([
      {
        cmd: 'reply_thread',
        args: { req: { pr: PR_ID, thread: 't1', body: 'thanks' } },
      },
      {
        cmd: 'resolve_thread',
        args: { req: { pr: PR_ID, thread: 't1', resolved: true } },
      },
    ]);
  });

  it('swallow a rejection instead of throwing', async () => {
    const boom = () => {
      throw { code: 'host', message: 'nope' };
    };
    transport.onCall('reply_thread', boom);
    transport.onCall('resolve_thread', boom);
    await expect(replyThread(store, PR_ID, 't1', 'x')).resolves.toBeUndefined();
    await expect(
      resolveThread(store, PR_ID, 't1', false),
    ).resolves.toBeUndefined();
  });
});
