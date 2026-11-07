/** "Suggestion" in a line comment's composer fills in the code it would
 *  replace: every line of a range, not just its last. */
import { act } from 'react';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { setTransport } from '../core/transport';
import { FakeTransport } from '../test/fakeTransport';
import { mountBigPr } from '../test/bigPr';

describe('the composer’s Suggestion button', () => {
  let view: Awaited<ReturnType<typeof mountBigPr>>;

  beforeEach(async () => {
    setTransport(new FakeTransport());
    view = await mountBigPr();
  });

  afterEach(() => view.unmount());

  const suggest = async () => {
    const button = [
      ...view.host.querySelectorAll<HTMLButtonElement>('.composer button'),
    ].find((b) => b.textContent === 'Suggestion')!;
    await act(async () => button.click());
    return view.host.querySelector<HTMLTextAreaElement>(
      'textarea[aria-label="comment"]',
    )!.value;
  };

  it('fills one line for a one-line comment', async () => {
    await act(async () =>
      view.controls().setComposer({
        path: 'src/mod0/file0.ts',
        side: 'new',
        line: 3,
      }),
    );
    expect(await suggest()).toBe(
      '```suggestion\nconst v3 = compute(0, 0, 3); // line\n```',
    );
  });

  it('fills every new-side line of a range, skipping deleted ones', async () => {
    // New lines 2–6 of the first hunk: v2 v3 v4 v5, then v7 (v6 is a
    // deletion, which a suggestion can't keep or replace).
    await act(async () =>
      view.controls().setComposer({
        path: 'src/mod0/file0.ts',
        side: 'new',
        line: 6,
        startLine: 2,
      }),
    );
    expect(await suggest()).toBe(
      [
        '```suggestion',
        'const v2 = compute(0, 0, 2); // line',
        'const v3 = compute(0, 0, 3); // line',
        'const v4 = compute(0, 0, 4); // line',
        'const v5 = compute(0, 0, 5); // line',
        'const v7 = compute(0, 0, 7); // line',
        '```',
      ].join('\n'),
    );
  });
});
