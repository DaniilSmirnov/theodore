import { describe, expect, it } from 'vitest';
import type { InterdiffEntry } from '../../gen/contract';
import { interdiffEntry } from '../../test/factories';
import { interdiffByHunk, interdiffView } from './interdiff';

const entry = (
  over: Partial<InterdiffEntry> & { hunk: string },
): InterdiffEntry => ({
  path: 'a.ts',
  class: 'changed',
  previous: null,
  words: [],
  ...over,
});

describe('interdiffByHunk', () => {
  it('keys by each entry hunk id', () => {
    const map = interdiffByHunk([entry({ hunk: 'h1' }), entry({ hunk: 'h2' })]);
    expect([...map.keys()]).toEqual(['h1', 'h2']);
  });

  it('is empty for an empty response', () => {
    expect(interdiffByHunk([]).size).toBe(0);
  });
});

describe('interdiffView', () => {
  it('is null when there is no entry for the hunk', () => {
    expect(interdiffView(undefined)).toBeNull();
  });

  it('is null for a class other than changed', () => {
    expect(
      interdiffView(entry({ hunk: 'h1', class: 'new', words: [] })),
    ).toBeNull();
    expect(
      interdiffView(entry({ hunk: 'h1', class: 'unchanged', words: [] })),
    ).toBeNull();
    expect(
      interdiffView(entry({ hunk: 'h1', class: 'vanished', words: [] })),
    ).toBeNull();
  });

  it('is null for a changed hunk with no word-diff', () => {
    expect(
      interdiffView(entry({ hunk: 'h1', class: 'changed', words: [] })),
    ).toBeNull();
  });

  it('builds previous/now lines for a changed hunk with words', () => {
    const view = interdiffView(
      entry({
        hunk: 'h1',
        class: 'changed',
        previous: 'old1',
        words: [
          { kind: 'del', text: '1' },
          { kind: 'add', text: '2' },
        ],
      }),
    );
    expect(view).toEqual({
      previous: [{ text: '1', marks: [[0, 1]] }],
      now: [{ text: '2', marks: [[0, 1]] }],
    });
  });
});

describe('interdiff edge cases', () => {
  const words = (...w: [string, string][]) =>
    w.map(([kind, text]) => ({ kind: kind as 'ctx' | 'add' | 'del', text }));

  it('marks a multi-line deletion per line and starts marks at 0 on later lines', () => {
    const view = interdiffView(
      interdiffEntry({ words: words(['ctx', 'a '], ['del', 'x\ny']) }),
    );
    expect(view?.previous).toEqual([
      { text: 'a x', marks: [[2, 3]] },
      { text: 'y', marks: [[0, 1]] },
    ]);
    // The other side drops the deleted text entirely.
    expect(view?.now).toEqual([{ text: 'a ', marks: [] }]);
  });

  it('shows identical unmarked lines on both sides for context-only words', () => {
    const view = interdiffView(
      interdiffEntry({ words: words(['ctx', 'one\ntwo']) }),
    );
    const plain = [
      { text: 'one', marks: [] },
      { text: 'two', marks: [] },
    ];
    expect(view).toEqual({ previous: plain, now: plain });
  });

  it('keeps a trailing newline as a final empty line, and a bare newline does not mark', () => {
    const view = interdiffView(
      interdiffEntry({ words: words(['add', 'new\n']) }),
    );
    expect(view?.now).toEqual([
      { text: 'new', marks: [[0, 3]] },
      { text: '', marks: [] },
    ]);
    expect(view?.previous).toEqual([{ text: '', marks: [] }]);
  });

  it('ignores an empty-text span instead of creating a zero-width mark', () => {
    const view = interdiffView(
      interdiffEntry({ words: words(['ctx', 'a'], ['add', ''], ['ctx', 'b']) }),
    );
    expect(view?.now).toEqual([{ text: 'ab', marks: [] }]);
  });

  it('marks adjacent same-side spans separately', () => {
    const view = interdiffView(
      interdiffEntry({ words: words(['add', 'ab'], ['add', 'cd']) }),
    );
    expect(view?.now).toEqual([
      {
        text: 'abcd',
        marks: [
          [0, 2],
          [2, 4],
        ],
      },
    ]);
  });

  it('interdiffByHunk keeps the last entry when a hunk id repeats', () => {
    const a = interdiffEntry({ hunk: 'h', path: 'a.ts' });
    const b = interdiffEntry({ hunk: 'h', path: 'b.ts' });
    expect(interdiffByHunk([a, b]).get('h')?.path).toBe('b.ts');
  });

  it('a vanished entry is indexed by its own hunk id and gives no view', () => {
    const v = interdiffEntry({ hunk: 'old', class: 'vanished' });
    const map = interdiffByHunk([v]);
    expect(map.get('old')).toBe(v);
    expect(interdiffView(map.get('old'))).toBeNull();
    expect(interdiffView(map.get('current'))).toBeNull();
  });
});
