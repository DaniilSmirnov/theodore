/** A review tab whose PR row is gone (its space was archived) keeps a
 *  readable label, never the raw `github:owner/repo#…` id. */
import { describe, expect, it } from 'vitest';
import { reviewTabTitle } from './tabTitle';

describe('a review tab title', () => {
  it('keeps the last title it had once the PR row is gone', () => {
    const id = 'github:JeanIsahakyan/tamagotchi#3829142';
    expect(reviewTabTitle(id, 'tamagotchi space')).toBe('tamagotchi space');
    expect(reviewTabTitle(id, null)).toBe('tamagotchi space');
  });

  it('names the repo when it never had one', () => {
    expect(reviewTabTitle('github:o/widget#12', null)).toBe('widget #12');
    expect(reviewTabTitle('local:/tmp/app#7', null)).toBe('app #7');
    expect(reviewTabTitle('weird', null)).toBe('weird');
  });
});
