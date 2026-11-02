/**
 * A space's terminals (docs/spaces.md §3 "Terminal"): pure helpers for the
 * live output buffer — `EphemeralStore` owns the state, `Terminal.tsx` owns
 * the `xterm.js` side.
 */

/** One `Ephemeral::TerminalOutput` chunk, base64, in arrival order. */
export interface TerminalChunk {
  seq: number;
  data: string;
}

/** Chunks kept per terminal while this session runs — only a bridge across
 *  a brief gap (e.g. remounting the panel); the server's own scrollback
 *  (`SpaceTerminalInfo.scrollback`) covers everything older. */
export const TERMINAL_CHUNK_LIMIT = 300;

export function appendChunk(
  buf: readonly TerminalChunk[],
  chunk: TerminalChunk,
  limit = TERMINAL_CHUNK_LIMIT,
): TerminalChunk[] {
  const next = [...buf, chunk];
  return next.length > limit ? next.slice(next.length - limit) : next;
}

/** The chunks a terminal view still needs after writing `scrollback` seeded
 *  through `seq`: anything already folded into it is dropped, and (since a
 *  mobx map can hand back the same chunk on a later render) only chunks
 *  past `applied` are new. */
export function chunksToApply(
  buf: readonly TerminalChunk[],
  seededSeq: number,
  applied: number,
): TerminalChunk[] {
  return buf.filter((c) => c.seq > seededSeq && c.seq > applied);
}

/** Decodes a base64 chunk into raw bytes for `xterm.js`'s `write`. */
export function decodeChunk(data: string): Uint8Array {
  const bin = atob(data);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}
