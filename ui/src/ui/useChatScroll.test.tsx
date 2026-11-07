/** A chat's scrolling, through `ChatLog`: it follows new content only from
 *  the bottom, leaves you where you scrolled, and remembers per scope. */
import { act } from 'react';
import {
  afterAll,
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
} from 'vitest';
import { mount, type Mounted } from '../test/dom';
import { ChatLog } from './ChatTurn';

const size = { content: 0, view: 0 };
let resized: ResizeObserverCallback[] = [];
const realObserver = globalThis.ResizeObserver;
let m: Mounted | null = null;
let scopes = 0;

beforeAll(() => {
  const isLog = (el: HTMLElement) => el.classList.contains('chat-log');
  Object.defineProperty(HTMLElement.prototype, 'scrollHeight', {
    configurable: true,
    get(this: HTMLElement) {
      return isLog(this) ? size.content : 0;
    },
  });
  Object.defineProperty(HTMLElement.prototype, 'clientHeight', {
    configurable: true,
    get(this: HTMLElement) {
      return isLog(this) ? size.view : 0;
    },
  });
  globalThis.ResizeObserver = class {
    constructor(cb: ResizeObserverCallback) {
      resized.push(cb);
    }
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
});

afterAll(() => {
  delete (HTMLElement.prototype as { scrollHeight?: number }).scrollHeight;
  delete (HTMLElement.prototype as { clientHeight?: number }).clientHeight;
  globalThis.ResizeObserver = realObserver;
});

beforeEach(() => {
  size.content = 1000;
  size.view = 400;
  resized = [];
});

afterEach(() => {
  m?.unmount();
  m = null;
});

const log = () => m!.host.querySelector<HTMLElement>('.chat-log')!;
const pill = () => m!.host.querySelector<HTMLButtonElement>('.chat-jump');

async function open(scope?: string) {
  m?.unmount();
  resized = [];
  m = await mount(
    <ChatLog scope={scope}>
      <p>hello</p>
    </ChatLog>,
  );
}

/** The content (or view) now measures this; the observers hear of it. */
function resizeTo(content: number, view = size.view) {
  size.content = content;
  size.view = view;
  act(() => {
    for (const cb of resized) cb([], {} as ResizeObserver);
  });
}

function scrollTo(top: number) {
  act(() => {
    log().scrollTop = top;
    log().dispatchEvent(new Event('scroll'));
  });
}

describe('a chat log', () => {
  it('opens at the bottom', async () => {
    await open();
    expect(log().scrollTop).toBe(1000);
    expect(pill()).toBeNull();
  });

  it('follows new content from the bottom', async () => {
    await open();
    scrollTo(600);
    resizeTo(1500);
    expect(log().scrollTop).toBe(1500);
    expect(pill()).toBeNull();
  });

  it('scrolled up: stays put through new content, the pill brings it back', async () => {
    await open();
    scrollTo(600);
    scrollTo(100);
    expect(pill()?.textContent).toContain('Jump to latest');

    resizeTo(1500);
    resizeTo(2000);
    expect(log().scrollTop).toBe(100);

    await act(async () => pill()!.click());
    expect(log().scrollTop).toBe(2000);
    expect(pill()).toBeNull();
    resizeTo(2500);
    expect(log().scrollTop).toBe(2500);
  });

  it('a small scroll up while text streams is not pulled back', async () => {
    await open();
    scrollTo(600);
    scrollTo(570); // within reach of the bottom, but going up
    resizeTo(1100);
    expect(log().scrollTop).toBe(570);
    expect(pill()).not.toBeNull();
  });

  it('scrolling back near the bottom follows again', async () => {
    await open();
    scrollTo(600);
    scrollTo(100);
    scrollTo(550);
    expect(pill()).toBeNull();
    resizeTo(1200);
    expect(log().scrollTop).toBe(1200);
  });

  it('content folding away is not you scrolling up', async () => {
    await open();
    scrollTo(600);
    // A turn's steps fold: the browser clamps the view to the new end.
    size.content = 700;
    scrollTo(300);
    expect(pill()).toBeNull();
    resizeTo(900);
    expect(log().scrollTop).toBe(900);
  });

  it('the view growing (a shorter composer) is not you scrolling up', async () => {
    await open();
    scrollTo(600);
    size.view = 500;
    scrollTo(500);
    expect(pill()).toBeNull();
  });

  it('stays at the bottom when the view shrinks (a taller composer)', async () => {
    await open();
    scrollTo(600);
    resizeTo(1000, 300);
    expect(log().scrollTop).toBe(1000);
    expect(pill()).toBeNull();
  });
});

describe('a chat log with a scope', () => {
  const scope = () => `scope-${(scopes += 1)}`;

  it('reopens where it was left scrolled up, each scope its own', async () => {
    const [a, b] = [scope(), scope()];
    await open(a);
    scrollTo(600);
    scrollTo(250);
    await open(b);
    expect(log().scrollTop).toBe(1000);
    expect(pill()).toBeNull();

    await open(a);
    expect(log().scrollTop).toBe(250);
    expect(pill()).not.toBeNull();
    // And doesn't lose its place to what comes next.
    resizeTo(1500);
    expect(log().scrollTop).toBe(250);
  });

  it('left at the bottom, reopens at the bottom, past what came since', async () => {
    const a = scope();
    await open(a);
    scrollTo(600);
    size.content = 1400;
    await open(a);
    expect(log().scrollTop).toBe(1400);
    expect(pill()).toBeNull();
  });

  it('the jump forgets the place', async () => {
    const a = scope();
    await open(a);
    scrollTo(600);
    scrollTo(100);
    await act(async () => pill()!.click());
    await open(a);
    expect(log().scrollTop).toBe(1000);
  });

  it('waits for content to reach the place, unless you scroll first', async () => {
    const [a, b] = [scope(), scope()];
    await open(a);
    scrollTo(600);
    scrollTo(500);
    size.content = 450;
    await open(a);
    // Our own move inside the short content says nothing about you.
    act(() => log().dispatchEvent(new Event('scroll')));
    resizeTo(2000);
    expect(log().scrollTop).toBe(500);
    expect(pill()).not.toBeNull();

    size.content = 1000;
    await open(b);
    scrollTo(600);
    scrollTo(300);
    size.content = 450;
    await open(b);
    scrollTo(0);
    resizeTo(2000);
    expect(log().scrollTop).toBe(0);
  });
});
