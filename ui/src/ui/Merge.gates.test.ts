/** What the merge dialog lists between a PR and its merge, and which of it
 *  actually stops the merge: only what the host itself refuses. */
import { describe, expect, it } from 'vitest';
import type { MergeInfo } from '../gen/contract';
import { RootStore } from '../stores/RootStore';
import { blockingGates, gatesFor } from './Merge';

const PR = 'github:acme/app#1';

function seed(state: 'open' | 'draft', checks: string[]): RootStore {
  const store = new RootStore();
  store.sync.pool.put('pr', PR, {
    pr: { repo: { host: 'github', owner: 'acme', name: 'app' }, number: 1 },
    state,
    approvals: 0,
    required_approvals: null,
    review_decision: null,
    conflicts: false,
  });
  store.sync.pool.put('pr_detail', PR, {
    threads: [],
    commits: [],
    checks: checks.map((status, i) => ({ id: `c${i}`, name: `c${i}`, status })),
  });
  return store;
}

const info = (state: MergeInfo['state']) => ({ state }) as MergeInfo;

describe('merge gates', () => {
  it('one failed check on an unprotected repo: listed once, merge allowed', () => {
    const store = seed('open', ['fail']);
    const gates = gatesFor(store, PR, info('unstable'));
    const labels = gates.filter((g) => !g.ok).map((g) => g.label);
    expect(labels).toEqual(['1 check failing']);
    expect(labels.join()).not.toContain('still running');
    expect(blockingGates(gates)).toEqual([]);
  });

  it('a failed check the host requires blocks', () => {
    const store = seed('open', ['fail']);
    const gates = gatesFor(store, PR, info('blocked'));
    expect(blockingGates(gates).map((g) => g.label)).toContain(
      '1 check failing',
    );
  });

  it('a draft blocks until it is ready for review', () => {
    const store = seed('draft', ['pass']);
    const gates = gatesFor(store, PR, info('clean'));
    expect(blockingGates(gates).map((g) => g.label)).toEqual([
      'Draft — needs "Ready for review"',
    ]);
  });
});
