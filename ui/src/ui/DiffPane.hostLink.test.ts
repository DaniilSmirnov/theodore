/** A file's "Open on GitHub" only where there is a host PR: a space's
 *  branch review before it is pushed has no URL, so no link. */
import { describe, expect, it } from 'vitest';
import { hostFilesUrl } from './DiffPane';

describe('Open on GitHub', () => {
  it('links the PR’s files, and nothing without a host PR', () => {
    expect(hostFilesUrl({ url: 'https://github.com/o/r/pull/3' })).toBe(
      'https://github.com/o/r/pull/3/files',
    );
    expect(hostFilesUrl({ url: '' })).toBeNull();
    expect(hostFilesUrl(undefined)).toBeNull();
  });
});
