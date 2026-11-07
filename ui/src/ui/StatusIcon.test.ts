import { describe, expect, it } from 'vitest';
import type { PrSummary } from '../gen/contract';
import { prProblems, prStatus } from './StatusIcon';

const pr = {
  state: 'open' as const,
  checks: 'pass' as const,
  conflicts: false,
  review_decision: null,
  base_ref: 'main',
};

describe('prProblems', () => {
  it('is empty for a healthy PR', () => {
    expect(prProblems(pr)).toEqual([]);
  });

  it('lists failing checks, conflicts and requested changes in that order', () => {
    expect(
      prProblems({
        ...pr,
        checks: 'fail',
        conflicts: true,
        review_decision: 'changes_requested',
      }),
    ).toEqual(['Checks failing', 'Conflicts with main', 'Changes requested']);
  });

  it('ignores merged and closed PRs', () => {
    expect(prProblems({ ...pr, state: 'merged', checks: 'fail' })).toEqual([]);
    expect(prProblems({ ...pr, state: 'closed', conflicts: true })).toEqual([]);
  });

  it('counts a draft too', () => {
    expect(prProblems({ ...pr, state: 'draft', checks: 'fail' })).toEqual([
      'Checks failing',
    ]);
  });
});

describe('prStatus', () => {
  const full = {
    ...pr,
    approvals: 0,
    required_approvals: null,
  } as unknown as PrSummary;

  it('is ready to merge when every merge gate passes', () => {
    expect(prStatus(full)).toEqual({ tone: 'ready', label: 'Ready to merge' });
  });

  it('shows running checks before anything else that is fine', () => {
    expect(prStatus({ ...full, checks: 'pending' }).tone).toBe('pending');
  });

  it('stays open while approvals are missing', () => {
    expect(
      prStatus({ ...full, approvals: 0, required_approvals: 1 }).tone,
    ).toBe('open');
  });

  it('puts a problem above ready', () => {
    expect(prStatus({ ...full, conflicts: true })).toEqual({
      tone: 'problem',
      label: 'Conflicts with main',
    });
  });

  it('says Draft first: a draft is not up for merging, problems or not', () => {
    expect(prStatus({ ...full, state: 'draft', checks: 'fail' })).toEqual({
      tone: 'draft',
      label: 'Draft · Checks failing',
    });
    expect(prStatus({ ...full, state: 'draft' })).toEqual({
      tone: 'draft',
      label: 'Draft',
    });
  });
});
