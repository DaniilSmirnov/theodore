/**
 * Hover in the diff (docs/code-intel.md §7.1): the identifier under the
 * pointer, after a short rest (sooner with ⌘ held), asked once per
 * position with `code_intel_hover`.
 *
 * ```text
 * pointer on an identifier ─▶ same one as before? ── yes ─▶ nothing
 *                                    └─ no ─▶ hide ─▶ wait HOVER_DELAY_MS (⌘: META_HOVER_DELAY_MS)
 *                                                    ─▶ cache[pr@head:path:line:col] ── hit ─▶ show
 *                                                                                   └─ miss ─▶ code_intel_hover ─▶ show
 *                                                                                              └─ lsp answer ─▶ cache
 * pointer leaves / scroll / click / Esc ─▶ cancel, hide
 * ```
 */
import type { CodeHover, CodeHoverRequest, PrId } from '../../gen/contract';
import { identifierColAt } from './extract';

export const HOVER_DELAY_MS = 500;
export const META_HOVER_DELAY_MS = 80;
/** Positions kept, least recently used dropped first. */
export const HOVER_CACHE_MAX = 300;

/** An identifier on a head-side line of the diff. */
export interface HoverTarget {
  path: string;
  /** 1-based, the PR head's line. */
  line: number;
  /** 1-based char column of the identifier's first char. */
  col: number;
  name: string;
}

/** The hover target at `offset` of a line's text (`identifierColAt`). */
export function hoverTargetAt(
  text: string,
  offset: number,
  path: string,
  line: number,
): HoverTarget | null {
  const at = identifierColAt(text, offset);
  return at && { path, line, ...at };
}

export function hoverKey(pr: PrId, rev: string, t: HoverTarget): string {
  return `${pr}@${rev}:${t.path}:${t.line}:${t.col}`;
}

/** One answer per position: a request in flight is shared, the language
 *  server's answer is kept. No answer, a by-name one (`syntax`, `text`:
 *  the server may still be warming) or a failure is not, so the next hover
 *  asks again. The key holds the PR's head (`hoverKey`), so a new head
 *  never reuses an old answer. */
export class HoverCache {
  private readonly map = new Map<string, Promise<CodeHover | null>>();

  constructor(
    private readonly fetch: (
      req: CodeHoverRequest,
    ) => Promise<CodeHover | null>,
    private readonly max = HOVER_CACHE_MAX,
  ) {}

  get size(): number {
    return this.map.size;
  }

  get(key: string, req: CodeHoverRequest): Promise<CodeHover | null> {
    const hit = this.map.get(key);
    if (hit) {
      // Most recently used goes last.
      this.map.delete(key);
      this.map.set(key, hit);
      return hit;
    }
    const forget = () => {
      if (this.map.get(key) === p) this.map.delete(key);
    };
    const p = this.fetch(req).then(
      (hover) => {
        if (hover?.source !== 'lsp') forget();
        return hover;
      },
      () => {
        forget();
        return null;
      },
    );
    this.map.set(key, p);
    while (this.map.size > this.max) {
      this.map.delete(this.map.keys().next().value!);
    }
    return p;
  }
}

export interface HoverTimers {
  set(fn: () => void, ms: number): unknown;
  clear(handle: unknown): void;
}

const realTimers: HoverTimers = {
  set: (fn, ms) => setTimeout(fn, ms),
  clear: (h) => clearTimeout(h as ReturnType<typeof setTimeout>),
};

export interface HoverShown<A> {
  target: HoverTarget;
  hover: CodeHover;
  /** Where to draw it (the identifier's box). */
  anchor: A;
}

/** The pointer's identifier, debounced, to a shown hover or none. */
export class HoverController<A> {
  private key: string | null = null;
  private timer: unknown = null;
  /** The armed timer is the short ⌘ one. */
  private quick = false;

  constructor(
    private readonly cache: HoverCache,
    private readonly show: (s: HoverShown<A> | null) => void,
    private readonly timers: HoverTimers = realTimers,
  ) {}

  /** The pointer rests on `target` (`null`: nothing to hover). */
  move(
    pr: PrId,
    rev: string,
    target: HoverTarget | null,
    meta: boolean,
    anchor: A,
  ): void {
    const key = target ? hoverKey(pr, rev, target) : null;
    if (key === this.key) {
      // ⌘ pressed while waiting on the same word: ask now.
      if (key && target && meta && this.timer !== null && !this.quick)
        this.arm(key, pr, target, true, anchor);
      return;
    }
    this.cancel();
    this.key = key;
    this.show(null);
    if (key && target) this.arm(key, pr, target, meta, anchor);
  }

  /** The pointer left, the diff scrolled, a click or Esc. */
  leave(): void {
    this.cancel();
    this.key = null;
    this.show(null);
  }

  private arm(
    key: string,
    pr: PrId,
    target: HoverTarget,
    quick: boolean,
    anchor: A,
  ): void {
    this.cancel();
    this.quick = quick;
    this.timer = this.timers.set(
      () => {
        this.timer = null;
        void this.fire(key, pr, target, anchor);
      },
      quick ? META_HOVER_DELAY_MS : HOVER_DELAY_MS,
    );
  }

  private async fire(
    key: string,
    pr: PrId,
    target: HoverTarget,
    anchor: A,
  ): Promise<void> {
    const hover = await this.cache.get(key, {
      pr,
      path: target.path,
      line: target.line,
      col: target.col,
    });
    if (this.key !== key || !hover || !hover.text.trim()) return;
    this.show({ target, hover, anchor });
  }

  private cancel(): void {
    if (this.timer === null) return;
    this.timers.clear(this.timer);
    this.timer = null;
  }
}

/** The subtle note on an answer that isn't the language server's. */
export function sourceNote(h: CodeHover): string | null {
  switch (h.source) {
    case 'lsp':
      return null;
    case 'syntax':
      return 'from the parser';
    case 'text':
      return 'from a text search';
  }
}
