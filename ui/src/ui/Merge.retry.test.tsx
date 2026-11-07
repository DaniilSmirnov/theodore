/** The merge dialog under host errors: a sentence instead of the raw
 *  `host: host server error: …`, no toast, and it recovers on its own
 *  while open (asked again on a backoff, like the header's merge state). */
import { act } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { MergeInfo } from '../gen/contract';
import { setCommandFailureHandler } from '../core/api';
import { setTransport } from '../core/transport';
import { RootStore } from '../stores/RootStore';
import { RootStoreProvider } from '../stores/context';
import { FakeTransport } from '../test/fakeTransport';
import { mount, type Mounted } from '../test/dom';
import { MergeDialog } from './Merge';
import { mergeInfoRetryMs } from './mergeInfo';

// jsdom has no `CSS.escape`; an open react-aria modal calls it.
(globalThis as { CSS?: { escape?: (s: string) => string } }).CSS ??= {};
globalThis.CSS.escape ??= (s: string) =>
  s.replace(/[^a-zA-Z0-9_-]/g, (c) => `\\${c}`);

const PR = 'github:acme/app#1';

const info = {
  state: 'clean',
  allowed_methods: ['squash'],
  default_method: 'squash',
  auto_merge_allowed: false,
  auto_merge_enabled: false,
  merge_queue: false,
  in_queue: false,
} as unknown as MergeInfo;

let transport: FakeTransport;
let store: RootStore;
let m: Mounted | null = null;
const toast = vi.fn();

beforeEach(() => {
  vi.useFakeTimers();
  transport = new FakeTransport();
  setTransport(transport);
  setCommandFailureHandler(toast);
  store = new RootStore();
  store.sync.pool.put('pr', PR, {
    pr: { repo: { host: 'github', owner: 'acme', name: 'app' }, number: 1 },
    title: 'Add notes',
    state: 'open',
    approvals: 0,
    required_approvals: null,
    review_decision: null,
    conflicts: false,
  });
  store.sync.pool.put('pr_detail', PR, {
    threads: [],
    commits: [],
    checks: [],
  });
});

afterEach(() => {
  m?.unmount();
  m = null;
  toast.mockReset();
  vi.useRealTimers();
});

describe('the merge dialog under host errors', () => {
  it('says a sentence, retries with backoff and recovers while open', async () => {
    let calls = 0;
    transport.onCall('merge_info', () => {
      calls += 1;
      if (calls <= 2)
        throw {
          code: 'host',
          message: 'host: host server error: sandbox: 502 bad gateway',
        };
      return info;
    });
    m = await mount(
      <RootStoreProvider store={store}>
        <MergeDialog pr={PR} onClose={() => {}} />
      </RootStoreProvider>,
    );
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    const text = () => document.body.textContent ?? '';
    expect(text()).toContain('GitHub is having trouble right now');
    expect(text()).not.toContain('host server error');
    expect(toast).not.toHaveBeenCalled();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(mergeInfoRetryMs(0));
    });
    expect(calls).toBe(2);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(mergeInfoRetryMs(1));
    });
    expect(calls).toBe(3);
    expect(text()).not.toContain('having trouble');
    expect(document.body.querySelector('.do-merge')).not.toBeNull();
  });
});
