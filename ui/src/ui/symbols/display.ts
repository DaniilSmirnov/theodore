import type { SymbolHit, SymbolSource } from '../../gen/contract';

export interface SymbolGroup {
  path: string;
  hits: SymbolHit[];
}

/** Groups hits by file, keeping `find_symbol`'s rank order both across
 *  groups (first group is the file with the best-ranked hit) and within
 *  one (a file's own hits stay in the order the core sent them). */
export function groupByFile(hits: readonly SymbolHit[]): SymbolGroup[] {
  const order: string[] = [];
  const byPath = new Map<string, SymbolHit[]>();
  for (const h of hits) {
    let bucket = byPath.get(h.path);
    if (!bucket) {
      bucket = [];
      byPath.set(h.path, bucket);
      order.push(h.path);
    }
    bucket.push(h);
  }
  return order.map((path) => ({ path, hits: byPath.get(path)! }));
}

/** How many of each kind, for the tab labels ("Definition 2", "References 5"). */
export function countByKind(hits: readonly SymbolHit[]): {
  definitions: number;
  references: number;
} {
  let definitions = 0;
  let references = 0;
  for (const h of hits) {
    if (h.kind === 'definition') definitions++;
    else references++;
  }
  return { definitions, references };
}

/** Least trusted first: a mixed answer is only as exact as its weakest
 *  hit. No source is the grep fallback's. */
const TRUST: SymbolSource[] = ['text', 'syntax', 'lsp'];

const SOURCE_CHIP: Record<SymbolSource, { label: string; tip: string }> = {
  lsp: { label: 'language server', tip: 'Resolved by the language server' },
  syntax: { label: 'parser', tip: 'Matched by name, not a language server' },
  text: {
    label: 'text search',
    tip: 'Matched by pattern, not a language server',
  },
};

/** The peek's "who found these" chip for `hits`: their least trusted
 *  source. `null` for no hits. */
export function sourceChip(
  hits: readonly SymbolHit[],
): { label: string; tip: string } | null {
  if (hits.length === 0) return null;
  const weakest = Math.min(
    ...hits.map((h) => TRUST.indexOf(h.source ?? 'text')),
  );
  return SOURCE_CHIP[TRUST[weakest]!];
}

/** The tab the peek opens on: Definition when `find_symbol` found one,
 *  References otherwise — never a tab that would render empty. */
export function defaultTab(hits: readonly SymbolHit[]): 'def' | 'refs' {
  return hits.some((h) => h.kind === 'definition') ? 'def' : 'refs';
}
