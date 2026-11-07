/** jsdom has no layout: what a virtual chat needs to see one. The view is
 *  `view` tall, a row measures what `rowHeight` says, and scrolling a `.chat-log`
 *  moves its `scrollTop` and fires `scroll`, clamped like a browser's. */
import { act } from 'react';

export const chatLayout = {
  view: 400,
  rowHeight: 100,
  /** By a row's `data-index`, over `rowHeight`. */
  heights: new Map<number, number>(),
};

let observers: {
  cb: ResizeObserverCallback;
  targets: Set<Element>;
}[] = [];
const real = {
  observer: globalThis.ResizeObserver,
  scrollTo: Element.prototype.scrollTo,
};

const isLog = (el: Element) => el.classList.contains('chat-log');
const sizerOf = (el: Element) => el.firstElementChild as HTMLElement | null;
const contentHeight = (el: Element) => {
  const sizer = sizerOf(el);
  if (!sizer) return 0;
  const min = parseFloat(sizer.style.minHeight) || 0;
  return Math.max(parseFloat(sizer.style.height) || 0, min);
};
const rowHeight = (el: Element) => {
  const i = Number(el.getAttribute('data-index'));
  return chatLayout.heights.get(i) ?? chatLayout.rowHeight;
};

export function installChatLayout() {
  Object.defineProperty(HTMLElement.prototype, 'clientHeight', {
    configurable: true,
    get(this: HTMLElement) {
      return isLog(this) ? chatLayout.view : 0;
    },
  });
  Object.defineProperty(HTMLElement.prototype, 'clientWidth', {
    configurable: true,
    get(this: HTMLElement) {
      return isLog(this) ? 800 : 0;
    },
  });
  Object.defineProperty(HTMLElement.prototype, 'scrollHeight', {
    configurable: true,
    get(this: HTMLElement) {
      return isLog(this) ? contentHeight(this) : 0;
    },
  });
  Object.defineProperty(HTMLElement.prototype, 'offsetHeight', {
    configurable: true,
    get(this: HTMLElement) {
      return this.hasAttribute('data-index') ? rowHeight(this) : 0;
    },
  });
  Element.prototype.scrollTo = function scrollTo(
    this: Element,
    arg?: ScrollToOptions | number,
  ) {
    const top = typeof arg === 'object' ? (arg.top ?? 0) : 0;
    const max = Math.max(this.scrollHeight - this.clientHeight, 0);
    this.scrollTop = Math.min(Math.max(top, 0), max);
    // Like a browser's: after the write, not inside it.
    queueMicrotask(() => {
      act(() => {
        this.dispatchEvent(new Event('scroll'));
      });
    });
  } as typeof Element.prototype.scrollTo;
  observers = [];
  globalThis.ResizeObserver = class {
    private readonly entry;
    constructor(cb: ResizeObserverCallback) {
      this.entry = { cb, targets: new Set<Element>() };
      observers.push(this.entry);
    }
    observe(el: Element) {
      this.entry.targets.add(el);
    }
    unobserve(el: Element) {
      this.entry.targets.delete(el);
    }
    disconnect() {
      this.entry.targets.clear();
    }
  } as unknown as typeof ResizeObserver;
}

export function uninstallChatLayout() {
  const proto = HTMLElement.prototype as unknown as Record<string, unknown>;
  delete proto.clientHeight;
  delete proto.clientWidth;
  delete proto.scrollHeight;
  delete proto.offsetHeight;
  Element.prototype.scrollTo = real.scrollTo;
  globalThis.ResizeObserver = real.observer;
  chatLayout.heights.clear();
  chatLayout.view = 400;
  chatLayout.rowHeight = 100;
}

/** The observers hear of every observed row's size, as after a layout. */
export function layoutRows() {
  act(() => {
    for (const { cb, targets } of observers) {
      const entries = [...targets]
        .filter((t) => t.hasAttribute('data-index'))
        .map((target) => ({
          target,
          borderBoxSize: [{ blockSize: rowHeight(target), inlineSize: 800 }],
        }));
      if (entries.length) {
        cb(entries as unknown as ResizeObserverEntry[], {} as ResizeObserver);
      }
    }
  });
}

/** The view changed size. */
export function layoutView() {
  act(() => {
    for (const { cb, targets } of observers) {
      const entries = [...targets]
        .filter(isLog)
        .map((target) => ({ target }) as unknown as ResizeObserverEntry);
      if (entries.length) cb(entries, {} as ResizeObserver);
    }
  });
}

/** You scrolling: the position moves, and `scroll` fires. */
export function userScroll(el: HTMLElement, top: number) {
  act(() => {
    el.dispatchEvent(new WheelEvent('wheel', { bubbles: true }));
    el.scrollTop = top;
    el.dispatchEvent(new Event('scroll'));
  });
}

/** Lets frames (the virtualizer's reconcile, a restore, a toggle) run. */
export async function frames(n = 3) {
  for (let i = 0; i < n; i += 1) {
    await act(async () => {
      await new Promise((r) => requestAnimationFrame(r));
    });
  }
}
