import { describe, expect, it } from 'vitest';

import { methodHint } from './Merge';

describe('methodHint', () => {
  it("names the PR's own base, not main, for a stacked PR", () => {
    expect(methodHint('squash', 'feature/auth')).toBe(
      'one commit on feature/auth; branch history is squashed',
    );
    expect(methodHint('rebase', 'feature/auth')).toBe(
      'branch commits on top of feature/auth, no merge commit',
    );
    expect(methodHint('merge', 'develop')).toBe(
      'all commits + a merge commit on develop',
    );
  });

  it('falls back to words when the base is unknown', () => {
    expect(methodHint('squash', '')).toBe(
      'one commit on the base branch; branch history is squashed',
    );
  });
});
