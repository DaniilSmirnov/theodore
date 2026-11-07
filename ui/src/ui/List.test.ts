import { describe, expect, it } from 'vitest';
import type { PrId, RepoRef } from '../gen/contract';
import {
  PEEK_FITS_FROM,
  followSelection,
  peekFits,
  prIdFor,
  rowColumns,
} from './List';

describe('followSelection', () => {
  const order = ['a', 'b'] as PrId[];
  const shown = ['a', 'b', 'z'] as PrId[]; // `z` is snoozed
  const base = { order, shown, lastIndex: 0 };

  it('keeps a snoozed row you clicked, so its preview shows', () => {
    expect(
      followSelection({
        ...base,
        sel: 'z' as PrId,
        prev: { sel: 'a' as PrId, inOrder: true },
      }),
    ).toBeNull();
  });

  it('moves on from a PR you just snoozed to the one that took its place', () => {
    expect(
      followSelection({
        ...base,
        order: ['b'] as PrId[],
        sel: 'a' as PrId,
        prev: { sel: 'a' as PrId, inOrder: true },
      }),
    ).toBe('b');
  });

  it('picks the first waiting PR when nothing on screen is selected', () => {
    expect(
      followSelection({
        ...base,
        sel: null,
        prev: { sel: null, inOrder: false },
      }),
    ).toBe('a');
  });
});

describe('rowColumns', () => {
  it('gives the number column room for the repo name in front of it', () => {
    // 72px cut "tamagochi" to "tamagoc…".
    expect(rowColumns(['id'], true).split(' ')[2]).toBe('160px');
    expect(rowColumns(['id'], false).split(' ')[2]).toBe('72px');
  });
});

describe('prIdFor', () => {
  it('builds a github id matching PrRef::id()', () => {
    const repo: RepoRef = { host: 'github', owner: 'acme', name: 'api' };
    expect(prIdFor(repo, 482)).toBe('github:acme/api#482');
  });

  it('builds a gitlab id with a namespaced owner', () => {
    const repo: RepoRef = { host: 'gitlab', owner: 'group/sub', name: 'proj' };
    expect(prIdFor(repo, 3)).toBe('gitlab:group/sub/proj#3');
  });
});

describe('peekFits', () => {
  it('hides Peek when the list would get squeezed', () => {
    // 900px window minus the 256px sidebar: no room for both.
    expect(peekFits(900 - 256)).toBe(false);
    expect(peekFits(PEEK_FITS_FROM - 1)).toBe(false);
    expect(peekFits(PEEK_FITS_FROM)).toBe(true);
    expect(peekFits(1400 - 256)).toBe(true);
  });
});
