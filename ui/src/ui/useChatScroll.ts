import { useCallback, useLayoutEffect, useRef, useState } from 'react';

/** This close to the bottom still counts as being there. */
export const NEAR_BOTTOM = 80;

/** Where a chat was left scrolled up, by `scope`. A chat left at the bottom
 *  has no entry: it opens at the bottom, past whatever came since. */
const leftAt = new Map<string, number>();

export function measure(el: HTMLElement) {
  const max = el.scrollHeight - el.clientHeight;
  // Past the end while the view bounces: still the end.
  return { top: Math.min(el.scrollTop, max), max };
}

/** After a scroll: is the chat still following new content? Upward with the
 *  same room to scroll is you. The content shrinking moves the view up too,
 *  and that isn't leaving the bottom. Near the bottom counts neither way while
 *  text streams: a small scroll up must not be pulled straight back. */
export function stillFollowing(
  was: { following: boolean; top: number; max: number },
  top: number,
  max: number,
): boolean {
  const up = top < was.top - 1 && max >= was.max;
  return !up && (was.following || max - top <= NEAR_BOTTOM);
}

/** A chat's scrolling. New content is followed only while you are at the
 *  bottom; scrolled up, nothing moves you and `away` offers the way back.
 *
 *  ```text
 *  at the bottom ──scroll up──▶ away    content grows: stays put
 *        ▲                        │
 *        └──scroll down, or jump──┘
 *  ```
 *
 *  `scope` keeps the position across remounts; without one a chat opens at
 *  the bottom. */
export function useChatScroll(scope?: string) {
  const scroller = useRef<HTMLDivElement>(null);
  const content = useRef<HTMLDivElement>(null);
  const [away, setAway] = useState(false);
  const pos = useRef({
    following: true,
    /** A position to get back to once the content is that tall. */
    restore: null as number | null,
    top: 0,
    max: 0,
  });

  useLayoutEffect(() => {
    const el = scroller.current;
    const inner = content.current;
    if (!el || !inner) return;
    const p = pos.current;
    const saved = scope == null ? undefined : leftAt.get(scope);
    p.following = saved == null;
    p.restore = saved ?? null;
    setAway(saved != null);

    // Content or the viewport changed size.
    const onResize = () => {
      if (el.clientHeight === 0) return; // hidden: nothing to measure
      if (p.restore != null) {
        el.scrollTop = p.restore;
        if (el.scrollHeight - el.clientHeight >= p.restore) p.restore = null;
      } else if (p.following) {
        el.scrollTop = el.scrollHeight;
      }
      Object.assign(p, measure(el));
    };
    const onScroll = () => {
      if (el.clientHeight === 0) return;
      const { top, max } = measure(el);
      if (p.restore != null) {
        if (Math.abs(top - p.top) <= 1) return; // our own move
        p.restore = null;
      }
      p.following = stillFollowing(p, top, max);
      p.top = top;
      p.max = max;
      setAway(!p.following);
      if (scope == null) return;
      if (p.following) leftAt.delete(scope);
      else leftAt.set(scope, top);
    };

    onResize();
    el.addEventListener('scroll', onScroll, { passive: true });
    const observer = new ResizeObserver(onResize);
    observer.observe(el);
    observer.observe(inner);
    return () => {
      el.removeEventListener('scroll', onScroll);
      observer.disconnect();
    };
  }, [scope]);

  const jump = useCallback(() => {
    const el = scroller.current;
    if (!el) return;
    const p = pos.current;
    p.restore = null;
    p.following = true;
    el.scrollTop = el.scrollHeight;
    Object.assign(p, measure(el));
    if (scope != null) leftAt.delete(scope);
    setAway(false);
  }, [scope]);

  return { scroller, content, away, jump };
}
