import { describe, expect, it } from 'vitest';
import {
  appendChunk,
  chunksToApply,
  decodeChunk,
  TERMINAL_CHUNK_LIMIT,
  type TerminalChunk,
} from './terminalBuffer';

const chunk = (seq: number, data = `d${seq}`): TerminalChunk => ({
  seq,
  data,
});

describe('appendChunk', () => {
  it('appends in order', () => {
    const buf = appendChunk(appendChunk([], chunk(1)), chunk(2));
    expect(buf).toEqual([chunk(1), chunk(2)]);
  });

  it('drops the oldest past the limit', () => {
    let buf: TerminalChunk[] = [];
    for (let i = 1; i <= 3; i++) buf = appendChunk(buf, chunk(i), 2);
    expect(buf).toEqual([chunk(2), chunk(3)]);
  });

  it('defaults to TERMINAL_CHUNK_LIMIT', () => {
    let buf: TerminalChunk[] = [];
    for (let i = 1; i <= TERMINAL_CHUNK_LIMIT + 5; i++) {
      buf = appendChunk(buf, chunk(i));
    }
    expect(buf).toHaveLength(TERMINAL_CHUNK_LIMIT);
    expect(buf[0]).toEqual(chunk(6));
  });
});

describe('chunksToApply', () => {
  const buf = [chunk(1), chunk(2), chunk(3), chunk(4)];

  it('skips chunks already folded into the seeded scrollback', () => {
    expect(chunksToApply(buf, 2, 0)).toEqual([chunk(3), chunk(4)]);
  });

  it('skips chunks already applied to the terminal', () => {
    expect(chunksToApply(buf, 0, 2)).toEqual([chunk(3), chunk(4)]);
  });

  it('is empty once everything is seeded or applied', () => {
    expect(chunksToApply(buf, 4, 0)).toEqual([]);
    expect(chunksToApply(buf, 0, 4)).toEqual([]);
  });

  it('takes the stricter of the two cutoffs', () => {
    expect(chunksToApply(buf, 1, 3)).toEqual([chunk(4)]);
  });
});

describe('decodeChunk', () => {
  it('round-trips through base64, bytes and all', () => {
    const bytes = new Uint8Array([72, 105, 0, 255, 10]); // "Hi\0<0xff>\n"
    const b64 = btoa(String.fromCharCode(...bytes));
    expect(decodeChunk(b64)).toEqual(bytes);
  });

  it('decodes plain ASCII text', () => {
    expect(decodeChunk(btoa('hello'))).toEqual(
      new Uint8Array([104, 101, 108, 108, 111]),
    );
  });
});
