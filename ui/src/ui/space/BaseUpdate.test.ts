import { describe, expect, it } from 'vitest';
import type { SpaceUpdate } from '../../gen/contract';
import { baseMovedLabel, resolvingText } from './BaseUpdate';

const updating = (conflicts: string[]): SpaceUpdate => ({
  base_tip: 't',
  from: 'h',
  conflicts,
  started_at: 0,
});

describe('update from base', () => {
  it('says how far the base moved, and only when it did', () => {
    const idle = { status: 'idle' as const, updating: null };
    expect(baseMovedLabel(idle, { base_tip: 't', behind: 3 })).toBe(
      'Base moved: 3 commits behind',
    );
    expect(baseMovedLabel(idle, { base_tip: 't', behind: 1 })).toBe(
      'Base moved: 1 commit behind',
    );
    expect(baseMovedLabel(idle, { base_tip: 't', behind: 0 })).toBeNull();
    expect(baseMovedLabel(idle, null)).toBeNull();
    expect(
      baseMovedLabel(
        { status: 'idle', updating: updating(['a']) },
        { base_tip: 't', behind: 2 },
      ),
    ).toBeNull();
    expect(
      baseMovedLabel(
        { status: 'archived', updating: null },
        { base_tip: 't', behind: 2 },
      ),
    ).toBeNull();
  });

  it('names the conflicted files while resolving', () => {
    expect(
      resolvingText({ status: 'idle', base: 'main', updating: null }),
    ).toBeNull();
    expect(
      resolvingText({
        status: 'running',
        base: 'main',
        updating: updating(['a.ts']),
      }),
    ).toBe('The agent is resolving conflicts with main: a.ts');
    expect(
      resolvingText({
        status: 'failed',
        base: 'dev',
        updating: updating(['a', 'b', 'c', 'd', 'e']),
      }),
    ).toBe('Resolving conflicts with dev: a, b, c and 2 more');
  });
});
