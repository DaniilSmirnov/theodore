import { beforeEach, describe, expect, it } from 'vitest';
import { setTerminalOpenFor, terminalOpenFor } from './terminalOpen';

beforeEach(() => localStorage.clear());

describe('the terminal drawer, per space', () => {
  it('stays closed on the next visit once closed there', () => {
    setTerminalOpenFor('a', true);
    setTerminalOpenFor('a', false);
    expect(terminalOpenFor('a')).toBe(false);
  });

  it("is one space's choice, not every space's", () => {
    setTerminalOpenFor('a', true);
    setTerminalOpenFor('b', false);
    expect(terminalOpenFor('a')).toBe(true);
    expect(terminalOpenFor('b')).toBe(false);
  });

  it('a space never toggled takes the last choice, closed at first', () => {
    expect(terminalOpenFor('new')).toBe(false);
    setTerminalOpenFor('a', true);
    expect(terminalOpenFor('new')).toBe(true);
  });
});
