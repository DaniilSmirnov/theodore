/** "Merge: Checking…" doesn't stay until you leave the tab: a failed
 *  `merge_info` (a host answering 502) is asked again on a backoff. */
import { act } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { MergeInfo } from '../../gen/contract';
import { setTransport } from '../../core/transport';
import { FakeTransport } from '../../test/fakeTransport';
import { mount, type Mounted } from '../../test/dom';
import { mergeInfoRetryMs, useMergeInfo } from '../mergeInfo';

const PR = 'gh:acme/app#1';

const info: MergeInfo = {
  state: 'clean',
  allowed_methods: ['squash'],
  default_method: 'squash',
  auto_merge_allowed: false,
  auto_merge_enabled: false,
  merge_queue: false,
  in_queue: false,
} as unknown as MergeInfo;

function Probe() {
  const got = useMergeInfo(PR, 'v1');
  return <span>{got ? got.state : 'Checking…'}</span>;
}

let transport: FakeTransport;
let m: Mounted | null = null;

beforeEach(() => {
  vi.useFakeTimers();
  transport = new FakeTransport();
  setTransport(transport);
});

afterEach(() => {
  m?.unmount();
  m = null;
  vi.useRealTimers();
});

describe('useMergeInfo', () => {
  it('asks again after a failure instead of checking forever', async () => {
    let calls = 0;
    transport.onCall('merge_info', () => {
      calls += 1;
      if (calls === 1) throw { code: 'host.server_error', message: '502' };
      return info;
    });
    m = await mount(<Probe />);
    expect(m.host.textContent).toBe('Checking…');
    await act(async () => {
      await vi.advanceTimersByTimeAsync(mergeInfoRetryMs(0));
    });
    expect(calls).toBe(2);
    expect(m.host.textContent).toBe('clean');
  });

  it('backs off, capped', () => {
    expect(mergeInfoRetryMs(0)).toBe(2_000);
    expect(mergeInfoRetryMs(1)).toBe(4_000);
    expect(mergeInfoRetryMs(10)).toBe(30_000);
  });
});
