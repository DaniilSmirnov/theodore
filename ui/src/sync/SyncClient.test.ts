import { beforeEach, describe, expect, it, vi } from 'vitest';
import { setTransport } from '../core/transport';
import { FakeTransport } from '../test/fakeTransport';
import type {
  Bootstrap,
  SyncFrame,
  WireAction,
  WireRow,
} from '../gen/contract';
import { reaction } from 'mobx';
import { SyncClient } from './SyncClient';

let transport: FakeTransport;

beforeEach(() => {
  transport = new FakeTransport();
  setTransport(transport);
});

/** Lets chained frame handling run to the end. */
const settle = () => new Promise((r) => setTimeout(r, 5));

const row = (model: string, id: string, data: unknown): WireRow => ({
  model,
  id,
  data,
  archived: false,
});

const action = (
  id: number,
  model: string,
  model_id: string,
  act: WireAction['action'],
  data: unknown,
): WireAction => ({
  id,
  model,
  model_id,
  action: act,
  data,
});

describe('SyncClient', () => {
  it('bootstraps the global scope and applies rows into the pool', async () => {
    const bootstrap: Bootstrap = {
      last_sync_id: 5,
      rows: [row('task', 't1', { title: 'Fix the thing', status: 'todo' })],
    };
    transport.queueResult('bootstrap', bootstrap);

    const client = new SyncClient();
    await client.start();

    expect(client.lastSyncId).toBe(5);
    expect(client.status).toBe('online');
    expect(client.pool.get('task', 't1')).toMatchObject({
      title: 'Fix the thing',
    });
  });

  it('applies `sync` frames strictly in arrival order', async () => {
    transport.queueResult('bootstrap', {
      last_sync_id: 0,
      rows: [],
    } satisfies Bootstrap);
    const client = new SyncClient();
    await client.start();

    const order: string[] = [];
    const frameA: SyncFrame = {
      from_sync_id: 0,
      last_sync_id: 1,
      actions: [action(1, 'task', 't1', 'I', { title: 'A' })],
    };
    const frameB: SyncFrame = {
      from_sync_id: 1,
      last_sync_id: 2,
      actions: [action(2, 'task', 't1', 'U', { title: 'B' })],
    };
    // Patch pool.put to record order without losing behaviour.
    const origPut = client.pool.put.bind(client.pool);
    client.pool.put = (...args: Parameters<typeof origPut>) => {
      order.push(String(args[2] && (args[2] as { title?: string }).title));
      return origPut(...args);
    };

    transport.emit('sync', frameA);
    transport.emit('sync', frameB);
    // Frame handling is chained but asynchronous; wait for it to settle.
    await new Promise((r) => setTimeout(r, 0));
    await new Promise((r) => setTimeout(r, 0));

    expect(order).toEqual(['A', 'B']);
    expect(client.lastSyncId).toBe(2);
    expect(client.pool.get('task', 't1')).toMatchObject({ title: 'B' });
  });

  it('fetches the gap with `catch_up` when a frame starts past our lastSyncId', async () => {
    transport.queueResult('bootstrap', {
      last_sync_id: 0,
      rows: [],
    } satisfies Bootstrap);
    const client = new SyncClient();
    await client.start();

    // We stand at 0; this frame starts at 3 — a gap of actions 1..3 was missed.
    const gapFrame: SyncFrame = {
      from_sync_id: 3,
      last_sync_id: 4,
      actions: [action(4, 'task', 't1', 'I', { title: 'latest' })],
    };
    const catchUpDelta: SyncFrame = {
      from_sync_id: 0,
      last_sync_id: 3,
      actions: [action(3, 'task', 't0', 'I', { title: 'missed' })],
    };
    transport.queueResult('catch_up', catchUpDelta);

    transport.emit('sync', gapFrame);
    await new Promise((r) => setTimeout(r, 0));
    await new Promise((r) => setTimeout(r, 0));

    expect(
      transport.calls.some((c) => c.cmd === 'catch_up' && c.args?.since === 0),
    ).toBe(true);
    expect(client.pool.get('task', 't0')).toMatchObject({ title: 'missed' });
    expect(client.pool.get('task', 't1')).toMatchObject({ title: 'latest' });
    expect(client.lastSyncId).toBe(4);
  });

  it('resyncs from scratch when catch_up reports the log was compacted past us', async () => {
    transport.queueResult('bootstrap', {
      last_sync_id: 0,
      rows: [row('task', 'stale', { title: 'will be discarded' })],
    } satisfies Bootstrap);
    const client = new SyncClient();
    await client.start();

    transport.queueResult('bootstrap', {
      last_sync_id: 0,
      rows: [row('draft', 'd1', { id: 'd1' })],
    } satisfies Bootstrap);
    await client.openPr('pr1');

    const gapFrame: SyncFrame = {
      from_sync_id: 3,
      last_sync_id: 4,
      actions: [action(4, 'task', 't1', 'I', { title: 'latest' })],
      resync_required: false,
    };
    const resyncDelta: SyncFrame = {
      from_sync_id: 0,
      last_sync_id: 10,
      actions: [],
      resync_required: true,
    };
    transport.queueResult('catch_up', resyncDelta);
    // The resync's fresh global bootstrap, then re-opening pr1.
    transport.queueResult(
      'bootstrap',
      {
        last_sync_id: 10,
        rows: [row('task', 'fresh', { title: 'from the new bootstrap' })],
      } satisfies Bootstrap,
      {
        last_sync_id: 10,
        rows: [row('draft', 'd2', { id: 'd2' })],
      } satisfies Bootstrap,
    );

    transport.emit('sync', gapFrame);
    await new Promise((r) => setTimeout(r, 0));
    await new Promise((r) => setTimeout(r, 0));
    await new Promise((r) => setTimeout(r, 0));

    // The stale pre-resync rows are gone; only the fresh bootstrap's rows remain.
    expect(client.pool.get('task', 'stale')).toBeUndefined();
    expect(client.pool.get('task', 'fresh')).toMatchObject({
      title: 'from the new bootstrap',
    });
    expect(client.pool.get('draft', 'd1')).toBeUndefined();
    expect(client.pool.get('draft', 'd2')).toBeDefined();
    // The frame that triggered catch_up never got applied directly either.
    expect(client.pool.get('task', 't1')).toBeUndefined();
    expect(client.lastSyncId).toBe(10);
    expect(client.isPrLoaded('pr1')).toBe(true);
  });

  it('never shows an empty pool while it resyncs', async () => {
    // An empty pool while online reads as "the active project is gone",
    // which drops that workspace's saved tabs (RootStore).
    const project = row('project', 'p1', { id: 'p1' });
    transport.queueResult('bootstrap', {
      last_sync_id: 0,
      rows: [project],
    } satisfies Bootstrap);
    const client = new SyncClient();
    await client.start();
    const seen: boolean[] = [];
    const stop = reaction(
      () => !!client.pool.get('project', 'p1'),
      (has) => seen.push(has),
    );

    transport.queueResult('catch_up', {
      from_sync_id: 0,
      last_sync_id: 10,
      actions: [],
      resync_required: true,
    } satisfies SyncFrame);
    transport.queueResult('bootstrap', {
      last_sync_id: 10,
      rows: [project],
    } satisfies Bootstrap);
    transport.emit('sync', {
      from_sync_id: 3,
      last_sync_id: 4,
      actions: [],
      resync_required: false,
    });
    await settle();
    stop();
    expect(seen).not.toContain(false);
    expect(client.pool.get('project', 'p1')).toBeDefined();
  });

  it('ignores a stale/duplicate frame', async () => {
    transport.queueResult('bootstrap', {
      last_sync_id: 10,
      rows: [],
    } satisfies Bootstrap);
    const client = new SyncClient();
    await client.start();

    const stale: SyncFrame = {
      from_sync_id: 5,
      last_sync_id: 8,
      actions: [action(8, 'task', 'stale', 'I', { title: 'should not apply' })],
    };
    transport.emit('sync', stale);
    await new Promise((r) => setTimeout(r, 0));

    expect(client.lastSyncId).toBe(10);
    expect(client.pool.get('task', 'stale')).toBeUndefined();
  });

  it('retry() re-bootstraps after an error and goes back online', async () => {
    transport.queueResult('bootstrap', new Error('network down'));
    const client = new SyncClient();
    await client.start();
    expect(client.status).toBe('error');

    transport.queueResult('bootstrap', {
      last_sync_id: 7,
      rows: [],
    } satisfies Bootstrap);
    await client.retry();

    expect(client.status).toBe('online');
    expect(client.lastSyncId).toBe(7);
  });

  it('retry() is a no-op while already online', async () => {
    transport.queueResult('bootstrap', {
      last_sync_id: 1,
      rows: [],
    } satisfies Bootstrap);
    const client = new SyncClient();
    await client.start();

    const bootstrapCallsBefore = transport.calls.filter(
      (c) => c.cmd === 'bootstrap',
    ).length;
    await client.retry();
    const bootstrapCallsAfter = transport.calls.filter(
      (c) => c.cmd === 'bootstrap',
    ).length;

    expect(bootstrapCallsAfter).toBe(bootstrapCallsBefore);
    expect(client.status).toBe('online');
  });

  it('bootstraps a PR scope once even when opened concurrently', async () => {
    transport.queueResult('bootstrap', {
      last_sync_id: 0,
      rows: [],
    } satisfies Bootstrap);
    const client = new SyncClient();
    await client.start();

    transport.queueResult('bootstrap', {
      last_sync_id: 0,
      rows: [row('draft', 'd1', { id: 'd1' })],
    } satisfies Bootstrap);
    await Promise.all([client.openPr('pr1'), client.openPr('pr1')]);

    const prBootstraps = transport.calls.filter(
      (c) =>
        c.cmd === 'bootstrap' &&
        (c.args?.scope as { scope: string })?.scope === 'pr',
    );
    expect(prBootstraps).toHaveLength(1);
    expect(client.isPrLoaded('pr1')).toBe(true);
  });

  // A scoped bootstrap's id is the global log's position, so jumping to it
  // would drop any undelivered frame at or below it, whatever its scope.
  it('keeps an undelivered frame at or below a scoped bootstrap id', async () => {
    transport.queueResult('bootstrap', {
      last_sync_id: 5,
      rows: [],
    } satisfies Bootstrap);
    const client = new SyncClient();
    await client.start();

    transport.queueResult('bootstrap', {
      last_sync_id: 6,
      rows: [row('draft', 'd1', { id: 'd1' })],
    } satisfies Bootstrap);
    const missed = action(6, 'task', 't1', 'I', { title: 'other scope' });
    transport.queueResult('catch_up', {
      from_sync_id: 5,
      last_sync_id: 6,
      actions: [missed],
    } satisfies SyncFrame);
    await client.openPr('pr1');

    expect(client.pool.get('task', 't1')).toMatchObject({
      title: 'other scope',
    });
    expect(client.lastSyncId).toBe(6);

    // The frame itself, arriving late, is now a duplicate.
    const put = vi.spyOn(client.pool, 'put');
    transport.emit('sync', {
      from_sync_id: 5,
      last_sync_id: 6,
      actions: [missed],
    } satisfies SyncFrame);
    await settle();
    expect(put).not.toHaveBeenCalled();
  });

  it('replays frames a slower scoped bootstrap would overwrite', async () => {
    transport.queueResult('bootstrap', {
      last_sync_id: 5,
      rows: [],
    } satisfies Bootstrap);
    const client = new SyncClient();
    await client.start();

    // Read at 5, answered only after frame 6 landed.
    let answer!: (b: Bootstrap) => void;
    transport.onCall(
      'bootstrap',
      () => new Promise<Bootstrap>((res) => (answer = res)),
    );
    const opening = client.openPr('pr1');
    await settle();

    const newer = action(6, 'draft', 'd1', 'U', { body: 'newer' });
    transport.emit('sync', {
      from_sync_id: 5,
      last_sync_id: 6,
      actions: [newer],
    } satisfies SyncFrame);
    await settle();

    transport.queueResult('catch_up', {
      from_sync_id: 5,
      last_sync_id: 6,
      actions: [newer],
    } satisfies SyncFrame);
    answer({ last_sync_id: 5, rows: [row('draft', 'd1', { body: 'older' })] });
    await opening;

    expect(client.pool.get('draft', 'd1')).toMatchObject({ body: 'newer' });
    expect(client.lastSyncId).toBe(6);
    expect(transport.calls.at(-1)).toMatchObject({
      cmd: 'catch_up',
      args: { since: 5 },
    });
  });

  it('skips catch_up when a scoped bootstrap stands where we stand', async () => {
    transport.queueResult('bootstrap', {
      last_sync_id: 5,
      rows: [],
    } satisfies Bootstrap);
    const client = new SyncClient();
    await client.start();

    transport.queueResult('bootstrap', {
      last_sync_id: 5,
      rows: [row('draft', 'd1', { id: 'd1' })],
    } satisfies Bootstrap);
    await client.openPr('pr1');

    expect(transport.calls.some((c) => c.cmd === 'catch_up')).toBe(false);
    expect(client.pool.get('draft', 'd1')).toBeDefined();
  });

  it('applies a frame arriving during a scoped bootstrap, and replays it after', async () => {
    transport.queueResult('bootstrap', {
      last_sync_id: 5,
      rows: [],
    } satisfies Bootstrap);
    const client = new SyncClient();
    await client.start();

    transport.queueResult('bootstrap', {
      last_sync_id: 6,
      rows: [row('task', 't1', { title: 'six' })],
    } satisfies Bootstrap);
    const seven = action(7, 'task', 't1', 'U', { title: 'seven' });
    transport.queueResult(
      'catch_up',
      {
        from_sync_id: 5,
        last_sync_id: 6,
        actions: [action(6, 'task', 't1', 'I', { title: 'six' })],
      } satisfies SyncFrame,
      {
        from_sync_id: 6,
        last_sync_id: 7,
        actions: [seven],
      } satisfies SyncFrame,
    );
    const opening = client.openPr('pr1');
    transport.emit('sync', {
      from_sync_id: 6,
      last_sync_id: 7,
      actions: [seven],
    } satisfies SyncFrame);
    await opening;
    await settle();

    expect(client.pool.get('task', 't1')).toMatchObject({ title: 'seven' });
    expect(client.lastSyncId).toBe(7);
  });

  it('keeps fetching when catch_up answers one batch of a longer gap', async () => {
    transport.queueResult('bootstrap', {
      last_sync_id: 0,
      rows: [],
    } satisfies Bootstrap);
    const client = new SyncClient();
    await client.start();

    transport.queueResult(
      'catch_up',
      {
        from_sync_id: 0,
        last_sync_id: 2,
        actions: [action(2, 'task', 't1', 'I', { title: 'first batch' })],
      } satisfies SyncFrame,
      {
        from_sync_id: 2,
        last_sync_id: 4,
        actions: [action(4, 'task', 't2', 'I', { title: 'second batch' })],
      } satisfies SyncFrame,
    );
    transport.emit('sync', {
      from_sync_id: 4,
      last_sync_id: 5,
      actions: [action(5, 'task', 't3', 'I', { title: 'live' })],
    } satisfies SyncFrame);
    await settle();

    expect(client.pool.get('task', 't2')).toBeDefined();
    expect(client.pool.get('task', 't3')).toBeDefined();
    expect(client.lastSyncId).toBe(5);
  });

  it('openPr rejects, unloaded, when its catch_up fails, and later frames still apply', async () => {
    transport.queueResult('bootstrap', {
      last_sync_id: 5,
      rows: [],
    } satisfies Bootstrap);
    const client = new SyncClient();
    await client.start();

    transport.queueResult('bootstrap', {
      last_sync_id: 6,
      rows: [],
    } satisfies Bootstrap);
    transport.queueResult('catch_up', new Error('ipc'));
    await expect(client.openPr('pr1')).rejects.toThrow('ipc');
    expect(client.isPrLoaded('pr1')).toBe(false);

    transport.emit('sync', {
      from_sync_id: 5,
      last_sync_id: 6,
      actions: [action(6, 'task', 't1', 'I', { title: 'next' })],
    } satisfies SyncFrame);
    await settle();
    expect(client.pool.get('task', 't1')).toBeDefined();
  });
});
