/** The review toolbar's submit button: on your own space PR it shows, as
 *  "Comment" (you can't approve your own PR); a space's branch review still
 *  has "Send to agent" in its place. */
import { describe, expect, it } from 'vitest';
import { submitLabel } from './Review';

const base = {
  finished: false,
  branchReview: false,
  spacePr: null as string | null,
  pr: 'github:o/r#3',
  mine: false,
  compact: false,
};

describe('submit on a review', () => {
  it('is Comment on your own space PR', () => {
    expect(submitLabel({ ...base, spacePr: 'github:o/r#3', mine: true })).toBe(
      'Comment',
    );
  });

  it('is Submit review elsewhere, and absent where it can’t apply', () => {
    expect(submitLabel(base)).toBe('Submit review');
    expect(submitLabel({ ...base, compact: true })).toBe('Submit');
    expect(submitLabel({ ...base, branchReview: true })).toBeNull();
    expect(submitLabel({ ...base, finished: true })).toBeNull();
    // A space whose review this is, but not its host PR.
    expect(submitLabel({ ...base, spacePr: 'github:o/r#9' })).toBeNull();
  });
});
