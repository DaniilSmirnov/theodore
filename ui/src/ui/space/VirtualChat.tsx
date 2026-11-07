/**
 * A long chat's scrolling log: only the rows near the view are mounted. The
 * one place that knows about the virtualizer, so another way of windowing
 * only has to replace this file; rows see nothing of it but `useSteadyChange`.
 *
 * ```text
 * at the bottom ──scroll up──▶ away     rows grow: nothing moves, pill shows
 *       ▲                        │
 *       └──scroll down, pill, ───┘
 *          or you send a message
 *
 * reopen ──left away──▶ restore turn + offset ──settled, or any input──▶ away
 *        └─left at the bottom, or new─────────▶ at the bottom
 *
 * a row's toggle ─▶ the sizer holds its room ─▶ the row stays where it is;
 *                   the room melts away as you scroll up, or content fills it
 *
 * hidden (no height) ─▶ rows keep their sizes, nothing is measured or moved
 *    └─shown again, left away─▶ restore turn + offset; at the bottom ─▶ the end
 * ```
 */
import {
  useCallback,
  useImperativeHandle,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
  type Ref,
} from 'react';
import { flushSync } from 'react-dom';
import { ArrowDown } from '@gravity-ui/icons';
import { Button } from '@heroui/react';
import {
  defaultRangeExtractor,
  measureElement,
  useVirtualizer,
  type Range,
  type VirtualItem,
} from '@tanstack/react-virtual';
import { SteadyContext, type Steady } from '../rowState';
import { NEAR_BOTTOM, measure, stillFollowing } from '../useChatScroll';

/** Space above the first row; each row carries the gap under itself. */
const PADDING_TOP = 20;
/** Frames a restore may take before it stops chasing a moving layout. */
const RESTORE_FRAMES = 120;

/** A place in the chat: a row, and how far into it the view starts. */
interface Place {
  key: string;
  offset: number;
}

/** Where a chat was left scrolled up, by scope. None: it opens at the bottom,
 *  past whatever came since. In memory, like what a row keeps open. */
const leftAt = new Map<string, Place>();
/** Row sizes measured so far, by scope: a reopened chat starts with them. */
const measured = new Map<string, VirtualItem[]>();

/** The row and offset the view starts at, or the end when `place` is gone. */
export function initialOffset<T extends { key: string }>(
  items: readonly T[],
  sizes: readonly VirtualItem[],
  estimate: (item: T) => number,
  place: Place | undefined,
): number {
  const known = new Map(sizes.map((s) => [s.key, s.size]));
  let top = PADDING_TOP;
  for (const item of items) {
    if (item.key === place?.key) return top + place.offset;
    top += known.get(item.key) ?? estimate(item);
  }
  return top;
}

/** What the page around a chat can ask of it. */
export interface VirtualChatControl {
  /** To the bottom, following again: you just sent something. */
  jumpToLatest: () => void;
  /** Brings the row with this key into view, mounted or not. `false`: no
   *  such row (yet). Where it lands is then yours to leave: not following. */
  scrollToKey: (key: string, align?: 'start' | 'center' | 'end') => boolean;
}

function afterFrames(n: number, run: () => void) {
  if (n <= 0) run();
  else requestAnimationFrame(() => afterFrames(n - 1, run));
}

export function VirtualChat<T extends { key: string }>({
  scope,
  items,
  estimate,
  isPinned,
  renderItem,
  control,
}: {
  /** Keeps the place per this id. */
  scope: string;
  /** In order. Keys stay the same for the life of a row. */
  items: readonly T[];
  /** A row's height before it was measured. */
  estimate: (item: T) => number;
  /** Stays mounted out of view: rows holding what you are typing. */
  isPinned?: (item: T) => boolean;
  renderItem: (item: T) => ReactNode;
  control?: Ref<VirtualChatControl>;
}) {
  const scroller = useRef<HTMLDivElement>(null);
  const sizer = useRef<HTMLDivElement>(null);
  const [startPlace] = useState(() => {
    const place = leftAt.get(scope);
    return place && items.some((i) => i.key === place.key) ? place : undefined;
  });
  const [startSizes] = useState(() => measured.get(scope) ?? []);
  const [away, setAway] = useState(startPlace != null);
  // A toggle's frames: nothing pins to the end while it settles.
  const [steadying, setSteadying] = useState(false);
  // A toggle at the end that grew the row you're reading leaves you off
  // the end without anything new below: no pill for that, until a row is
  // added or you scroll (the row count it happened at).
  const [quietAt, setQuietAt] = useState<number | null>(null);
  const pos = useRef({
    following: startPlace == null,
    restore: startPlace ?? (null as Place | null),
    /** The sizer holds room below the content for a row's toggle. */
    holding: false,
    /** That toggle's row isn't measured yet. */
    steady: false,
    top: 0,
    max: 0,
    /** The view had no height: nothing was measured, nothing moved. */
    hidden: false,
  });
  // A reopen restores like a first open does; this runs it again.
  const [reopened, setReopened] = useState(0);

  const getItemKey = useCallback((i: number) => items[i]!.key, [items]);
  const estimateSize = useCallback(
    (i: number) => estimate(items[i]!),
    [items, estimate],
  );
  const rangeExtractor = useCallback(
    (range: Range) => {
      const indexes = new Set(defaultRangeExtractor(range));
      if (isPinned) {
        items.forEach((item, i) => {
          if (isPinned(item)) indexes.add(i);
        });
      }
      return [...indexes].sort((a, b) => a - b);
    },
    [items, isPinned],
  );

  // Hidden, a row has no size: it would be measured as 0, and every row would
  // fit the view. It keeps what it had.
  const measureRow = useCallback(
    (
      el: HTMLDivElement,
      entry: ResizeObserverEntry | undefined,
      inst: Parameters<typeof measureElement<HTMLDivElement>>[2],
    ) => {
      if (scroller.current?.clientHeight !== 0) {
        return measureElement(el, entry, inst);
      }
      const index = inst.indexFromElement(el);
      return (
        inst.itemSizeCache.get(inst.options.getItemKey(index)) ??
        inst.options.estimateSize(index)
      );
    },
    [],
  );

  // It re-renders us itself, and we don't memoize what it returns.
  // eslint-disable-next-line react-hooks/incompatible-library
  const v = useVirtualizer<HTMLDivElement, HTMLDivElement>({
    count: items.length,
    getScrollElement: () => scroller.current,
    estimateSize,
    getItemKey,
    rangeExtractor,
    measureElement: measureRow,
    anchorTo: 'end',
    followOnAppend: true,
    // Pins to the end only while following; -1 is never at the end.
    scrollEndThreshold: away || steadying ? -1 : NEAR_BOTTOM,
    overscan: 4,
    paddingStart: PADDING_TOP,
    initialMeasurementsCache: startSizes,
    initialOffset: () => initialOffset(items, startSizes, estimate, startPlace),
  });

  const finishRestore = useCallback(() => {
    pos.current.restore = null;
  }, []);

  /** Back to the room the content needs, as far as the view allows. */
  const melt = useCallback(() => {
    const el = scroller.current;
    const room = sizer.current;
    const p = pos.current;
    if (!el || !room || !p.holding || p.steady) return;
    const needed = el.scrollTop + el.clientHeight;
    if (needed <= parseFloat(room.style.height)) {
      room.style.minHeight = '';
      p.holding = false;
    } else {
      room.style.minHeight = `${needed}px`;
    }
  }, []);

  const place = useCallback(
    (top: number): Place | null => {
      const item = v.getVirtualItemForOffset(top);
      return item ? { key: String(item.key), offset: top - item.start } : null;
    },
    [v],
  );

  const remember = useCallback(() => {
    const p = pos.current;
    if (p.following) {
      leftAt.delete(scope);
      return;
    }
    const here = place(p.top);
    if (here) leftAt.set(scope, here);
  }, [scope, place]);

  // A restore lands, then keeps landing while rows above it get measured.
  const land = useCallback(() => {
    const el = scroller.current;
    const p = pos.current;
    if (!el || !p.restore) return;
    const { key, offset } = p.restore;
    const { count, getItemKey } = v.options;
    let index = 0;
    while (index < count && getItemKey(index) !== key) index += 1;
    v.getTotalSize(); // brings the measurements up to date
    const item = index < count ? v.measurementsCache[index] : undefined;
    if (!item) return;
    const target = item.start + offset;
    if (Math.abs(el.scrollTop - target) > 1) v.scrollToOffset(target);
    Object.assign(p, measure(el));
  }, [v]);

  useLayoutEffect(() => {
    const el = scroller.current;
    if (!el) return;
    const p = pos.current;
    const onScroll = () => {
      if (el.clientHeight === 0) return; // hidden: nothing to measure
      melt();
      const { top, max } = measure(el);
      if (p.restore || p.steady) {
        p.top = top;
        p.max = max;
        return;
      }
      p.following = stillFollowing(p, top, max);
      p.top = top;
      p.max = max;
      v.options.scrollEndThreshold = p.following ? NEAR_BOTTOM : -1;
      setAway(!p.following);
      setQuietAt(null);
      remember();
    };
    el.addEventListener('scroll', onScroll, { passive: true });
    Object.assign(p, measure(el));
    return () => {
      el.removeEventListener('scroll', onScroll);
      measured.set(scope, v.takeSnapshot());
    };
  }, [v, scope, melt, remember]);

  // Any input of yours ends a restore, wherever it lands. It runs until the
  // rows in view are measured and stayed put, and no longer.
  useLayoutEffect(() => {
    if (!pos.current.restore) return;
    const stop = () => finishRestore();
    const inputs = ['wheel', 'touchstart', 'pointerdown', 'keydown'] as const;
    for (const name of inputs) {
      window.addEventListener(name, stop, { capture: true, passive: true });
    }
    let frames = 0;
    let lastTop = -1;
    let frame = 0;
    const tick = () => {
      const p = pos.current;
      if (!p.restore) return;
      land();
      const settled =
        p.top === lastTop &&
        v.getVirtualItems().every((i) => v.itemSizeCache.has(i.key));
      lastTop = p.top;
      frames += 1;
      if (settled || frames >= RESTORE_FRAMES) {
        finishRestore();
        return;
      }
      frame = requestAnimationFrame(tick);
    };
    frame = requestAnimationFrame(tick);
    return () => {
      cancelAnimationFrame(frame);
      for (const name of inputs) {
        window.removeEventListener(name, stop, { capture: true });
      }
    };
  }, [v, finishRestore, land, reopened]);

  // The content or the view changed size.
  const total = v.getTotalSize();
  const view = v.scrollRect?.height;
  useLayoutEffect(() => {
    const el = scroller.current;
    if (!el) return;
    const p = pos.current;
    if (el.clientHeight === 0) {
      p.hidden = true; // what was measured so far is still true
      return;
    }
    if (p.hidden) {
      // Shown again: a browser drops the scroll position of what it hid.
      p.hidden = false;
      const saved = p.following ? undefined : leftAt.get(scope);
      if (saved && !p.restore) {
        p.restore = saved;
        setReopened((n) => n + 1);
      }
    }
    melt();
    if (p.restore) {
      land();
      return;
    }
    if (!steadying && p.following && !p.holding) {
      const { top, max } = measure(el);
      if (max - top > 1) v.scrollToEnd();
    }
    Object.assign(p, measure(el));
  }, [v, scope, total, view, steadying, melt, land]);

  const steady = useCallback<Steady>(
    (change) => {
      const el = scroller.current;
      const room = sizer.current;
      const p = pos.current;
      if (!el || !room) {
        change();
        return;
      }
      finishRestore();
      const wasFollowing = p.following;
      room.style.minHeight = `${el.scrollTop + el.clientHeight}px`;
      p.holding = true;
      p.steady = true;
      v.options.scrollEndThreshold = -1;
      flushSync(() => {
        setSteadying(true);
        change();
      });
      // The row is measured by the next frame; whatever it pushed down is
      // below you now, and the end is only yours if you are still at it.
      afterFrames(2, () => {
        p.steady = false;
        melt();
        const { top, max } = measure(el);
        p.following = max - top <= 1;
        p.top = top;
        p.max = max;
        setSteadying(false);
        setAway(!p.following);
        setQuietAt(wasFollowing && !p.following ? v.options.count : null);
        remember();
      });
    },
    [v, finishRestore, melt, remember],
  );

  const jump = useCallback(() => {
    const p = pos.current;
    const room = sizer.current;
    finishRestore();
    if (room) room.style.minHeight = '';
    p.holding = false;
    p.following = true;
    leftAt.delete(scope);
    setAway(false);
    v.options.scrollEndThreshold = NEAR_BOTTOM;
    v.scrollToEnd();
    const el = scroller.current;
    if (el) Object.assign(p, measure(el));
  }, [v, scope, finishRestore]);
  const scrollToKey = useCallback(
    (key: string, align: 'start' | 'center' | 'end' = 'center') => {
      const { count, getItemKey } = v.options;
      let index = 0;
      while (index < count && getItemKey(index) !== key) index += 1;
      if (index >= count) return false;
      finishRestore();
      if (index < count - 1) {
        // Not the end: nothing may pull the view back down meanwhile.
        pos.current.following = false;
        v.options.scrollEndThreshold = -1;
        setAway(true);
      }
      v.scrollToIndex(index, { align });
      return true;
    },
    [v, finishRestore],
  );
  useImperativeHandle(control, () => ({ jumpToLatest: jump, scrollToKey }), [
    jump,
    scrollToKey,
  ]);

  return (
    <div className="relative flex min-h-0 flex-1 flex-col">
      <div
        ref={scroller}
        className="chat-log flex min-h-0 flex-1 flex-col overflow-auto [overflow-anchor:none]"
      >
        <div
          ref={sizer}
          className="relative w-full shrink-0"
          style={{ height: total }}
        >
          <SteadyContext.Provider value={steady}>
            {v.getVirtualItems().map((row) => (
              <div
                key={row.key}
                ref={v.measureElement}
                data-index={row.index}
                className="absolute left-0 top-0 w-full"
                style={{ transform: `translateY(${row.start}px)` }}
              >
                <div className="mx-auto flex w-full max-w-[860px] flex-col gap-5 px-4 pb-5 empty:hidden">
                  {renderItem(items[row.index]!)}
                </div>
              </div>
            ))}
          </SteadyContext.Provider>
        </div>
      </div>
      {away && quietAt !== items.length && (
        <Button
          size="sm"
          variant="secondary"
          className="chat-jump absolute bottom-2 left-1/2 z-10 h-7 -translate-x-1/2 gap-1 rounded-full px-3 text-xs shadow-sm"
          onPress={jump}
        >
          <ArrowDown className="size-3.5" />
          Jump to latest
        </Button>
      )}
    </div>
  );
}
