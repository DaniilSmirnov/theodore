/**
 * Bootstraps the pool and keeps it current: `Global` scope on startup, one
 * `Pr` scope lazily per opened PR, `sync` events applied strictly in order,
 * gaps closed with `catch_up`. `lastSyncId` only moves past what was applied.
 */
import { makeObservable, observable, runInAction } from 'mobx';
import { api } from '../core/api';
import { getTransport, type Unlisten } from '../core/transport';
import type {
  Bootstrap,
  PrId,
  SyncFrame,
  WireAction,
  WireRow,
} from '../gen/contract';
import { ObjectPool } from './ObjectPool';
import { TransactionQueue } from './TransactionQueue';
import { UndoQueue } from './UndoQueue';
import type { ModelName } from './types';

export type SyncStatus = 'idle' | 'bootstrapping' | 'online' | 'error';

export class SyncClient {
  readonly pool = new ObjectPool();
  readonly queue: TransactionQueue;
  readonly undo: UndoQueue;

  status: SyncStatus = 'idle';
  lastSyncId = 0;
  error: string | undefined;

  private readonly loadedPrs = new Set<PrId>();
  private readonly loadingPrs = new Map<PrId, Promise<void>>();
  private unlisten: Unlisten | undefined;
  /** Serializes frame handling so catch-up and live frames never interleave. */
  private chain: Promise<void> = Promise.resolve();

  constructor() {
    this.queue = new TransactionQueue(this.pool);
    this.undo = new UndoQueue(this.queue);
    makeObservable(this, {
      status: observable,
      lastSyncId: observable,
      error: observable,
    });
  }

  async start(): Promise<void> {
    this.setStatus('bootstrapping');
    try {
      const boot = await api.bootstrap({ scope: 'global' });
      runInAction(() => {
        this.applyRows(boot.rows);
        this.lastSyncId = boot.last_sync_id;
      });
      this.unlisten = await getTransport().listen<SyncFrame>(
        'sync',
        (frame) => {
          this.chain = this.chain.then(() => this.onFrame(frame));
        },
      );
      this.setStatus('online');
    } catch (e) {
      this.setError(e);
    }
  }

  /** Lazily loads one PR's models; a no-op once loaded. */
  async openPr(pr: PrId): Promise<void> {
    if (this.loadedPrs.has(pr)) return;
    const inFlight = this.loadingPrs.get(pr);
    if (inFlight) return inFlight;
    const p = api
      .bootstrap({ scope: 'pr', pr })
      .then((boot) => this.inOrder(() => this.landPr(pr, boot)));
    this.loadingPrs.set(pr, p);
    try {
      await p;
    } finally {
      this.loadingPrs.delete(pr);
    }
  }

  isPrLoaded(pr: PrId): boolean {
    return this.loadedPrs.has(pr);
  }

  /** Runs `job` between frames, never alongside one. Not for use from inside
   *  a frame: it would wait on itself. */
  private inOrder(job: () => Promise<void>): Promise<void> {
    const run = this.chain.then(job);
    this.chain = run.catch(() => {});
    return run;
  }

  /** Puts a scoped bootstrap in the pool. Its id is the global log's position
   *  while its rows are one scope's, so the log closes the difference:
   *
   *  ```text
   *  id > lastSyncId   frames up to it not applied yet, whatever their scope
   *  id < lastSyncId   frames past it applied, and the rows just overwrote them
   *  ```
   *
   *  A replay is safe: rows ahead of `id` end up the same at the log's end. */
  private async landPr(pr: PrId, boot: Bootstrap): Promise<void> {
    runInAction(() => this.applyRows(boot.rows));
    // Before the catch-up: a resync it triggers re-bootstraps what's loaded.
    this.loadedPrs.add(pr);
    const at = this.lastSyncId;
    if (boot.last_sync_id === at) return;
    try {
      await this.catchUp(
        Math.min(boot.last_sync_id, at),
        Math.max(boot.last_sync_id, at),
      );
    } catch (e) {
      this.loadedPrs.delete(pr);
      throw e;
    }
  }

  private applyRows(rows: WireRow[]): void {
    for (const r of rows) {
      this.pool.put(r.model as ModelName, r.id, r.data, r.archived);
    }
  }

  private async onFrame(frame: SyncFrame): Promise<void> {
    try {
      if (frame.from_sync_id > this.lastSyncId) {
        // A frame arrived past where we stand: something was missed, fetch it.
        await this.catchUp(this.lastSyncId, frame.from_sync_id);
      }
      if (frame.last_sync_id <= this.lastSyncId) return; // stale/duplicate
      runInAction(() => {
        this.applyActions(frame.actions);
        this.lastSyncId = frame.last_sync_id;
      });
    } catch (e) {
      this.setError(e);
    }
  }

  /** Applies the log from `since` until it reaches `until`; `catch_up`
   *  answers in batches. */
  private async catchUp(since: number, until: number): Promise<void> {
    let from = since;
    for (;;) {
      const delta = await api.catchUp(from);
      if (delta.resync_required) {
        // The sync log was compacted past where we stood: `delta.actions` is
        // empty and can't close the gap incrementally. Bootstrap from
        // scratch instead of applying it.
        await this.resync();
        return;
      }
      runInAction(() => {
        this.applyActions(delta.actions);
        this.lastSyncId = Math.max(this.lastSyncId, delta.last_sync_id);
      });
      if (!delta.actions.length || delta.last_sync_id >= until) return;
      from = delta.last_sync_id;
    }
  }

  /** Discards the pool and re-bootstraps every scope we had loaded — the
   *  same shape as `start()`, but without tearing down the frame listener
   *  (the connection itself is still fine; only our local copy is stale). */
  private async resync(): Promise<void> {
    const openPrs = [...this.loadedPrs];
    const boot = await api.bootstrap({ scope: 'global' });
    // Cleared and refilled in one action: an empty pool while online, even
    // for the bootstrap's round trip, reads as "the active project is
    // gone" and drops that workspace's saved tabs (RootStore).
    runInAction(() => {
      this.pool.clear();
      this.loadedPrs.clear();
      this.applyRows(boot.rows);
      this.lastSyncId = boot.last_sync_id;
    });
    // Runs inside a frame: `landPr` directly, not `openPr`.
    for (const pr of openPrs) {
      await this.landPr(pr, await api.bootstrap({ scope: 'pr', pr }));
    }
  }

  private applyActions(actions: WireAction[]): void {
    for (const a of actions) this.applyAction(a);
  }

  private applyAction(a: WireAction): void {
    const model = a.model as ModelName;
    switch (a.action) {
      case 'I':
        this.pool.put(model, a.model_id, a.data, false);
        return;
      case 'U':
        this.pool.put(
          model,
          a.model_id,
          a.data,
          this.pool.isArchived(model, a.model_id),
        );
        return;
      case 'C':
        this.pool.merge(
          model,
          a.model_id,
          (a.data as Record<string, unknown>) ?? {},
        );
        return;
      case 'A':
        this.pool.put(
          model,
          a.model_id,
          a.data ?? this.pool.get(model, a.model_id),
          true,
        );
        return;
      case 'D':
        this.pool.remove(model, a.model_id);
        return;
    }
  }

  private setStatus(s: SyncStatus): void {
    runInAction(() => {
      this.status = s;
      this.error = undefined;
    });
  }

  private setError(e: unknown): void {
    runInAction(() => {
      this.status = 'error';
      this.error = e instanceof Error ? e.message : String(e);
    });
  }

  /** Manual reconnect after `status` went to `error` (e.g. an offline
   *  banner's "Retry") — tears down the old frame listener and bootstraps
   *  again. A no-op while already connecting or connected. */
  async retry(): Promise<void> {
    if (this.status === 'online' || this.status === 'bootstrapping') return;
    this.unlisten?.();
    this.unlisten = undefined;
    await this.start();
  }

  dispose(): void {
    this.unlisten?.();
  }
}
