/** The identifier at `offset` in `text` — the same charset a ⌘-click in
 *  the diff accepts (`DiffPane`'s `symbolAt`, which hit-tests the DOM and
 *  then defers here): `[A-Za-z_$][\w$]*`. Anything else under the click
 *  (punctuation, whitespace, a digit-led token) resolves to `null`, so
 *  `find_symbol` is never asked to search for something that was never a
 *  name to begin with. */
export function identifierAt(text: string, offset: number): string | null {
  return identifierSpan(text, offset)?.name ?? null;
}

/** The identifier at `offset` and its 1-based column: the column counts
 *  chars (code points), as the core does, not UTF-16 units. */
export function identifierColAt(
  text: string,
  offset: number,
): { name: string; col: number } | null {
  const span = identifierSpan(text, offset);
  if (!span) return null;
  return {
    name: span.name,
    col: Array.from(text.slice(0, span.start)).length + 1,
  };
}

/** `identifierAt` with the identifier's UTF-16 bounds in `text`. */
export function identifierSpan(
  text: string,
  offset: number,
): { name: string; start: number; end: number } | null {
  let a = offset;
  let b = offset;
  while (a > 0 && /[\w$]/.test(text[a - 1]!)) a--;
  while (b < text.length && /[\w$]/.test(text[b]!)) b++;
  const name = text.slice(a, b);
  return /^[A-Za-z_$][\w$]*$/.test(name) ? { name, start: a, end: b } : null;
}
