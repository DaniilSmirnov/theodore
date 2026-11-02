/**
 * Hover in the diff: one host per diff, driven by a delegated
 * `pointermove` on the diff's scroll root (like `ColorPreviewHost`), so the
 * thousands of lines carry no handlers. A line is hoverable when it has a
 * head-side number (`.ln[data-line]`); the text under the pointer is hit
 * the way ⌘-click's `symbolAt` does. Debounce and cache: `hover.ts`.
 */
import { useEffect, useState, type RefObject } from 'react';
import { createPortal } from 'react-dom';
import type { PrId } from '../../gen/contract';
import { api } from '../../core/api';
import { Md } from '../../highlight';
import {
  HoverCache,
  HoverController,
  hoverTargetAt,
  sourceNote,
  type HoverShown,
  type HoverTarget,
} from './hover';

interface Box {
  left: number;
  top: number;
  bottom: number;
}

/** Shared by every diff: an answer stays valid while the PR's head does
 *  (the head is in the key). */
const cache = new HoverCache((req) => api.codeIntelHover(req));

const WIDTH = 560;

/** The identifier under the pointer of `e`, and its box. */
function targetAt(e: PointerEvent): { target: HoverTarget; box: Box } | null {
  const el = e.target instanceof Element ? e.target : null;
  if (!el || el.closest('.ln-act, .no, .mark')) return null;
  const ln = el.closest<HTMLElement>('.ln[data-line]');
  const code = ln?.querySelector('code.src');
  const path = ln?.dataset.path;
  const line = Number(ln?.dataset.line);
  if (!ln || !code || !path || !line) return null;
  const range = document.caretRangeFromPoint?.(e.clientX, e.clientY);
  const node = range?.startContainer;
  if (!range || !node || node.nodeType !== 3 || !code.contains(node))
    return null;
  // The offset in the whole line: the text before the hit node, plus the
  // offset in it.
  const before = document.createRange();
  before.setStart(code, 0);
  before.setEnd(node, range.startOffset);
  const offset = before.toString().length;
  const text = code.textContent ?? '';
  const target = hoverTargetAt(text, offset, path, line);
  if (!target) return null;
  // The word's box, within the hit node.
  const local = range.startOffset;
  const nodeText = node.textContent ?? '';
  let a = local;
  let b = local;
  while (a > 0 && /[\w$]/.test(nodeText[a - 1]!)) a--;
  while (b < nodeText.length && /[\w$]/.test(nodeText[b]!)) b++;
  const word = document.createRange();
  word.setStart(node, a);
  word.setEnd(node, b);
  const r = word.getBoundingClientRect();
  return { target, box: { left: r.left, top: r.top, bottom: r.bottom } };
}

export function CodeHoverHost({
  pr,
  rev,
  root,
}: {
  pr: PrId;
  /** The PR's head: answers are at its worktree. */
  rev: string;
  root: RefObject<HTMLElement | null>;
}) {
  const [shown, setShown] = useState<HoverShown<Box> | null>(null);
  useEffect(() => {
    const el = root.current;
    if (!el) return;
    const ctl = new HoverController<Box>(cache, setShown);
    let frame = 0;
    let last: PointerEvent | null = null;
    const move = (e: PointerEvent) => {
      last = e;
      if (frame) return;
      frame = requestAnimationFrame(() => {
        frame = 0;
        if (!last) return;
        const hit = targetAt(last);
        ctl.move(
          pr,
          rev,
          hit?.target ?? null,
          last.metaKey,
          hit?.box ?? { left: 0, top: 0, bottom: 0 },
        );
      });
    };
    const leave = () => {
      last = null;
      ctl.leave();
    };
    const key = (e: KeyboardEvent) => {
      if (e.key === 'Escape') leave();
    };
    el.addEventListener('pointermove', move);
    el.addEventListener('pointerleave', leave);
    el.addEventListener('pointerdown', leave);
    el.addEventListener('scroll', leave, true);
    document.addEventListener('keydown', key);
    return () => {
      cancelAnimationFrame(frame);
      ctl.leave();
      el.removeEventListener('pointermove', move);
      el.removeEventListener('pointerleave', leave);
      el.removeEventListener('pointerdown', leave);
      el.removeEventListener('scroll', leave, true);
      document.removeEventListener('keydown', key);
    };
  }, [pr, rev, root]);

  if (!shown) return null;
  return createPortal(<HoverCard shown={shown} />, document.body);
}

/** The hover's card, in the symbol peek's look: below the word, or above
 *  it near the window's bottom; kept on screen. */
export function HoverCard({ shown }: { shown: HoverShown<Box> }) {
  const { hover, target, anchor } = shown;
  const note = sourceNote(hover);
  const above = anchor.bottom > window.innerHeight - 280;
  const left = Math.max(
    8,
    Math.min(anchor.left - 8, window.innerWidth - WIDTH - 8),
  );
  return (
    <div
      role="tooltip"
      className="code-hover pointer-events-none fixed z-50 flex max-h-64 flex-col overflow-hidden rounded-xl border border-separator bg-overlay text-overlay-foreground shadow-overlay"
      style={{
        left,
        maxWidth: WIDTH,
        ...(above
          ? { top: anchor.top - 4, transform: 'translateY(-100%)' }
          : { top: anchor.bottom + 4 }),
      }}
    >
      <div className="flex items-center gap-2 border-b border-separator px-3 py-1.5">
        <code className="min-w-0 flex-1 truncate font-mono text-xs">
          {target.name}
        </code>
        {note && (
          <span className="code-hover-source shrink-0 text-[11px] text-muted">
            {note}
          </span>
        )}
      </div>
      <div className="min-h-0 overflow-hidden px-3 py-1 text-xs">
        <Md text={hover.text} />
      </div>
    </div>
  );
}
