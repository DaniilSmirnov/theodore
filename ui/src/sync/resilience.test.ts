import { beforeEach, describe, expect, it } from 'vitest';
import { setTransport } from '../core/transport';
import { FakeTransport } from '../test/fakeTransport';
import type {
  Bootstrap,
  SyncFrame,
  TransactionResult,
  WireAction,
} from '../gen/contract';
import { SyncClient } from './SyncClient';

let transport: FakeTransport;

beforeEach(() => {
  transport = new FakeTransport();
  setTransport(transport);
});

const act = (
  id: number,
  model: string,
  model_id: string,
  action: WireAction['action'],
  data: unknown,
): WireAction => ({ id, model, model_id, action, data });

const ticks = async (n = 4) => {
  for (let i = 0; i < n; i++) await new Promise((r) => setTimeout(r, 0));
};

async function startAt(lastSyncId: number): Promise<SyncClient> {
  transport.queueResult('bootstrap', {
    last_sync_id: lastSyncId,
    rows: [],
  } satisfies Bootstrap);
  const client = new SyncClient();
  await client.start();
  return client;
}

/** Makes the next `transact` call hang until the returned function settles it. */
function holdTransact(): (r: TransactionResult | Error) => void {
  let settle!: (r: TransactionResult | Error) => void;
  transport.onCall(
    'transact',
    () =>
      new Promise((resolve, reject) => {
        settle = (r) => (r instanceof Error ? reject(r) : resolve(r));
      }),
  );
  return (r) => settle(r);
}

const createDraft = (id: string, body: string) =>
  ({ op: 'create', model: 'draft', id, data: { id, body } }) as const;

describe('sync resilience', () => {
  it('a gap triggers catch_up and the optimistic draft is neither duplicated nor lost when the canonical row arrives', async () => {
    const client = await startAt(0);
    const resolve = holdTransact();

    const tx = client.queue.transact([createDraft('d1', 'optimistic')]);
    expect(client.pool.get('draft', 'd1')).toMatchObject({
      body: 'optimistic',
    });

    transport.queueResult('catch_up', {
      from_sync_id: 0,
      last_sync_id: 3,
      actions: [act(3, 'task', 't0', 'I', { title: 'missed' })],
    } satisfies SyncFrame);
    transport.emit('sync', {
      from_sync_id: 3,
      last_sync_id: 4,
      actions: [act(4, 'draft', 'd1', 'I', { id: 'd1', body: 'canonical' })],
    } satisfies SyncFrame);
    await ticks();

    expect(transport.calls.some((c) => c.cmd === 'catch_up')).toBe(true);
    expect(client.lastSyncId).toBe(4);
    expect(client.pool.count('draft')).toBe(1);
    expect(client.pool.get('draft', 'd1')).toMatchObject({ body: 'canonical' });
    expect(client.pool.get('task', 't0')).toMatchObject({ title: 'missed' });

    resolve({ status: 'ok', last_sync_id: 4 });
    expect(await tx).toMatchObject({ status: 'ok' });
    expect(client.pool.count('draft')).toBe(1);
    expect(client.pool.get('draft', 'd1')).toMatchObject({ body: 'canonical' });
  });

  it('a rejected transact rolls back its draft but keeps an unrelated frame applied meanwhile', async () => {
    const client = await startAt(0);
    const resolve = holdTransact();

    const tx = client.queue.transact([createDraft('d1', 'doomed')]);
    transport.emit('sync', {
      from_sync_id: 0,
      last_sync_id: 1,
      actions: [act(1, 'task', 't1', 'I', { title: 'unrelated' })],
    } satisfies SyncFrame);
    await ticks();
    expect(client.pool.get('task', 't1')).toMatchObject({ title: 'unrelated' });
    expect(client.pool.get('draft', 'd1')).toBeDefined();

    resolve({ status: 'rejected', reason: 'stale' });
    expect(await tx).toEqual({ status: 'rejected', reason: 'stale' });

    expect(client.pool.get('draft', 'd1')).toBeUndefined();
    expect(client.pool.get('task', 't1')).toMatchObject({ title: 'unrelated' });
    expect(client.lastSyncId).toBe(1);
  });

  it('pending lists in-flight transactions and empties after success and after failure', async () => {
    const client = await startAt(0);
    const resolve = holdTransact();

    const ok = client.queue.transact([createDraft('d1', 'a')]);
    expect(client.queue.pending).toHaveLength(1);
    await ticks(1);
    resolve({ status: 'ok', last_sync_id: 1 });
    await ok;
    expect(client.queue.pending).toHaveLength(0);

    const rejected = client.queue.transact([createDraft('d2', 'b')]);
    expect(client.queue.pending).toHaveLength(1);
    await ticks(1);
    resolve({ status: 'rejected', reason: 'no' });
    await rejected;
    expect(client.queue.pending).toHaveLength(0);

    const crashed = client.queue.transact([createDraft('d3', 'c')]);
    expect(client.queue.pending).toHaveLength(1);
    await ticks(1);
    resolve(new Error('core crashed'));
    expect(await crashed).toMatchObject({ status: 'rejected' });
    expect(client.queue.pending).toHaveLength(0);
    expect(client.pool.get('draft', 'd1')).toBeDefined();
    expect(client.pool.get('draft', 'd2')).toBeUndefined();
    expect(client.pool.get('draft', 'd3')).toBeUndefined();
  });

  it('a stale frame arriving after a catch_up does not regress state', async () => {
    const client = await startAt(0);
    transport.queueResult('catch_up', {
      from_sync_id: 0,
      last_sync_id: 3,
      actions: [act(3, 'task', 't0', 'I', { title: 'fresh' })],
    } satisfies SyncFrame);
    transport.emit('sync', {
      from_sync_id: 3,
      last_sync_id: 4,
      actions: [act(4, 'task', 't1', 'I', { title: 'live' })],
    } satisfies SyncFrame);
    await ticks();
    expect(client.lastSyncId).toBe(4);

    transport.emit('sync', {
      from_sync_id: 1,
      last_sync_id: 3,
      actions: [
        act(2, 'task', 't0', 'U', { title: 'old' }),
        act(3, 'task', 't1', 'D', null),
      ],
    } satisfies SyncFrame);
    await ticks();

    expect(client.lastSyncId).toBe(4);
    expect(client.pool.get('task', 't0')).toMatchObject({ title: 'fresh' });
    expect(client.pool.get('task', 't1')).toMatchObject({ title: 'live' });
    expect(client.status).toBe('online');
  });

  it('drops the gap frame itself when catch_up already carried us past it', async () => {
    const client = await startAt(0);
    transport.queueResult('catch_up', {
      from_sync_id: 0,
      last_sync_id: 6,
      actions: [act(6, 'task', 't1', 'I', { title: 'newest' })],
    } satisfies SyncFrame);
    transport.emit('sync', {
      from_sync_id: 3,
      last_sync_id: 4,
      actions: [act(4, 'task', 't1', 'U', { title: 'older' })],
    } satisfies SyncFrame);
    await ticks();

    expect(client.lastSyncId).toBe(6);
    expect(client.pool.get('task', 't1')).toMatchObject({ title: 'newest' });
  });

  it('a failing catch_up puts the client in error without applying the gap frame', async () => {
    const client = await startAt(0);
    transport.queueResult('catch_up', new Error('offline'));
    transport.emit('sync', {
      from_sync_id: 3,
      last_sync_id: 4,
      actions: [act(4, 'task', 't1', 'I', { title: 'x' })],
    } satisfies SyncFrame);
    await ticks();

    expect(client.status).toBe('error');
    expect(client.lastSyncId).toBe(0);
    expect(client.pool.get('task', 't1')).toBeUndefined();
  });
});
