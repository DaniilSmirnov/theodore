import { beforeEach, describe, expect, it } from 'vitest';
import { STORAGE_NS } from '../../stores/UiStore';
import { markSpaceSeen, markSpaceUnseen, spaceSeen } from './seen';

const LS_KEY = `${STORAGE_NS}theodore-space-seen`;

beforeEach(() => {
  localStorage.clear();
  spaceSeen.clear();
});

describe('space seen marks', () => {
  it('records the latest seen time, never going backwards', () => {
    markSpaceSeen('s', 10);
    expect(spaceSeen.get('s')).toBe(10);
    markSpaceSeen('s', 5);
    expect(spaceSeen.get('s')).toBe(10);
    markSpaceSeen('s', 20);
    expect(spaceSeen.get('s')).toBe(20);
  });

  it('"Mark as unread" forgets the mark so the next check reads as unseen', () => {
    markSpaceSeen('s', 10);
    markSpaceUnseen('s');
    expect(spaceSeen.get('s')).toBeUndefined();
  });

  it('is a no-op on a space that was never seen', () => {
    expect(() => markSpaceUnseen('nope')).not.toThrow();
    expect(spaceSeen.has('nope')).toBe(false);
  });

  it('writes through to localStorage, not just the in-memory map', () => {
    markSpaceSeen('s', 10);
    expect(
      (
        JSON.parse(localStorage.getItem(LS_KEY) ?? '{}') as Record<
          string,
          number
        >
      ).s,
    ).toBe(10);
    markSpaceUnseen('s');
    expect(
      (
        JSON.parse(localStorage.getItem(LS_KEY) ?? '{}') as Record<
          string,
          number
        >
      ).s,
    ).toBeUndefined();
  });
});
