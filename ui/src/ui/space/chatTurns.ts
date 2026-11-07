/**
 * A space's chat as rows: one per turn, so a long chat only has to mount the
 * turns near the view.
 *
 * ```text
 * head   setup stage / note             stays mounted
 * turn   you ─ reply, timeline, cards   keyed by your message's id
 * turn   …
 * turn   you ─ the live reply
 * tail   queued messages, the question  stays mounted
 * ```
 */
import type { ChatMessage } from '../../gen/contract';

export interface ChatTurn {
  key: string;
  messages: ChatMessage[];
}

export type ChatRow =
  | { kind: 'head'; key: 'head' }
  | { kind: 'turn'; key: string; messages: ChatMessage[]; live: boolean }
  /** A running turn before its message arrived. */
  | { kind: 'live'; key: 'live' }
  | { kind: 'tail'; key: 'tail' };

/** A transcript by time, as turns: each item that `begins` one, with what
 *  follows it. Whatever comes first, with nothing before it, starts its own. */
export function groupTurns<T>(
  items: readonly T[],
  begins: (item: T) => boolean,
  keyOf: (item: T) => string,
): { key: string; items: T[] }[] {
  const turns: { key: string; items: T[] }[] = [];
  for (const item of items) {
    const last = turns.at(-1);
    if (begins(item) || !last) turns.push({ key: keyOf(item), items: [item] });
    else last.items.push(item);
  }
  return turns;
}

/** The transcript by time, as turns: each of your messages with the replies
 *  under it. A reply with no message before it starts its own. `keyOf`: a
 *  message's row key, when it differs from its id. */
export function pairTurns(
  sent: readonly ChatMessage[],
  keyOf: (id: string) => string = (id) => id,
): ChatTurn[] {
  return groupTurns(
    sent,
    (m) => m.author.kind === 'me',
    (m) => keyOf(m.id),
  ).map((t) => ({ key: t.key, messages: t.items }));
}

/** The rows, in the order the chat shows them. The live reply goes on the
 *  last turn: it is what that turn is waiting for, and it stays in the same
 *  row when its reply lands. */
export function chatRows(
  turns: readonly ChatTurn[],
  has: { head: boolean; live: boolean; tail: boolean },
): ChatRow[] {
  const rows: ChatRow[] = [];
  if (has.head) rows.push({ kind: 'head', key: 'head' });
  turns.forEach((t, i) =>
    rows.push({
      kind: 'turn',
      key: t.key,
      messages: t.messages,
      live: has.live && i === turns.length - 1,
    }),
  );
  if (has.live && turns.length === 0) rows.push({ kind: 'live', key: 'live' });
  if (has.tail) rows.push({ kind: 'tail', key: 'tail' });
  return rows;
}

/** Per line of text in the reading column, about. */
const LINE = 22;
const CHARS_PER_LINE = 90;
/** A message's bubble or header and footer, without its text. */
const MESSAGE = 72;
const GAP = 20;

/** A row's height before it was measured: near enough that scrolling up
 *  through rows nobody saw yet has little to correct. */
export function estimateRowHeight(row: ChatRow): number {
  if (row.kind !== 'turn') return row.kind === 'live' ? 120 : 80;
  let height = GAP + (row.live ? 120 : 0);
  for (const m of row.messages) {
    const lines = m.text.split('\n').reduce((n, line) => {
      return n + Math.max(1, Math.ceil(line.length / CHARS_PER_LINE));
    }, 0);
    height += MESSAGE + lines * LINE + GAP;
  }
  return height;
}
