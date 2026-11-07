/** How tall the drawer under a space page (logs, terminal) may be: what it
 *  wants, capped by the space the page has left once the chat log keeps
 *  `CHAT_LOG_MIN` and the composer its own height — not by the viewport
 *  alone, so a short window shrinks the drawer, not the chat.
 *
 *  ```text
 *  ┌ header ────────────┐
 *  │ chat log  ≥ 200 px │ ┐
 *  │ composer           │ │ shared: the panes row + the drawer
 *  ├ drawer ────────────┤ │
 *  │ min(wanted, room)  │ ┘
 *  └────────────────────┘
 *  ``` */
import { useLayoutEffect, useRef, useState, type RefObject } from 'react';

export const CHAT_LOG_MIN = 200;
/** A drawer never gets shorter than a tab strip and a few lines. */
export const DRAWER_MIN = 96;

/** `shared`: px of the panes row and the drawer together; `besideLog`: px
 *  the row needs besides its chat log (the composer, a trust prompt). */
export function drawerHeight(
  wanted: number,
  shared: number,
  besideLog: number,
): number {
  const room = shared - besideLog - CHAT_LOG_MIN;
  return Math.round(Math.max(DRAWER_MIN, Math.min(wanted, room)));
}

/** `drawerHeight`, measured off the panes `row` (and its `.chat-log`) and
 *  kept current as the window, the composer or the row resize. */
export function useDrawerHeight(
  row: RefObject<HTMLElement | null>,
  wanted: number,
  open: boolean,
): number {
  const [height, setHeight] = useState(wanted);
  // What's applied now; only `measure` changes it.
  const current = useRef(wanted);
  useLayoutEffect(() => {
    const el = row.current;
    if (!open || !el) return;
    const measure = () => {
      const log = el.querySelector<HTMLElement>('.chat-log');
      if (!log) return;
      const rowH = el.clientHeight;
      const next = drawerHeight(
        wanted,
        rowH + current.current,
        rowH - log.clientHeight,
      );
      if (next !== current.current) {
        current.current = next;
        setHeight(next);
      }
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    const log = el.querySelector('.chat-log');
    if (log) ro.observe(log);
    window.addEventListener('resize', measure);
    return () => {
      ro.disconnect();
      window.removeEventListener('resize', measure);
    };
  }, [row, wanted, open]);
  return height;
}
