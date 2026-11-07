/** A line's AI button puts the line in the AI composer as a chip; the
 *  question typed there goes with it as `about`. */
import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import {
  afterAll,
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
} from 'vitest';
import { setTransport } from '../core/transport';
import { RootStore } from '../stores/RootStore';
import { RootStoreProvider } from '../stores/context';
import { FakeTransport } from '../test/fakeTransport';
import { Harness, PR, revision, seed } from '../test/bigPr';
import { installChatLayout, uninstallChatLayout } from '../test/chatLayout';
import { SidePanel } from './SidePanel';

// jsdom lays nothing out: the chat's scroll-to-bottom has nowhere to go.
Element.prototype.scrollIntoView ??= function () {};

// The log is virtual: it needs a view to know which rows to mount.
beforeAll(installChatLayout);
afterAll(uninstallChatLayout);

let transport: FakeTransport;
let store: RootStore;
let host: HTMLDivElement;
let root: Root;

beforeEach(async () => {
  transport = new FakeTransport();
  transport.onCall('detect_agents', () => []);
  transport.onCall('list_skills', () => []);
  transport.onCall('ask', () => new Promise<string>(() => {}));
  setTransport(transport);
  const rev = revision();
  store = new RootStore();
  seed(store, rev);
  host = document.createElement('div');
  document.body.appendChild(host);
  root = createRoot(host);
  await act(async () =>
    root.render(
      <RootStoreProvider store={store}>
        <Harness rev={rev} onReady={() => {}} />
        <SidePanel pr={PR} />
      </RootStoreProvider>,
    ),
  );
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
});

const lineAi = (i: number) =>
  host
    .querySelector('#H-f0h0')!
    .querySelectorAll<HTMLButtonElement>(
      'button[aria-label="ask AI about this line"]',
    )[i]!;
const chip = () => host.querySelector<HTMLElement>('.chat-input .line-ref');
const box = () =>
  host.querySelector<HTMLTextAreaElement>('.chat-input textarea')!;
const sendBtn = () =>
  host.querySelector<HTMLButtonElement>(
    '.chat-input button[aria-label="Send"]',
  )!;
const asks = () => transport.calls.filter((c) => c.cmd === 'ask');

async function type(text: string) {
  await act(async () => {
    const el = box();
    Object.getOwnPropertyDescriptor(
      HTMLTextAreaElement.prototype,
      'value',
    )!.set!.call(el, text);
    el.dispatchEvent(new Event('input', { bubbles: true }));
  });
}

async function key(k: string) {
  await act(async () => {
    box().dispatchEvent(
      new KeyboardEvent('keydown', { key: k, bubbles: true }),
    );
  });
}

describe('referencing a diff line in the AI composer', () => {
  it('a click puts a chip in the composer, focuses it, sends nothing', async () => {
    expect(chip()).toBeNull();
    // A deleted line: old side.
    await act(async () => lineAi(1).click());
    expect(chip()?.textContent).toContain('file0.ts:2old');
    expect(chip()?.querySelector('.ref')?.getAttribute('title')).toBe(
      'src/mod0/file0.ts:2 (old side)',
    );
    expect(document.activeElement).toBe(box());
    expect(box().placeholder).toBe('Ask about file0.ts:2');
    expect(asks()).toHaveLength(0);
    // A question is required.
    expect(sendBtn().disabled).toBe(true);
  });

  it('sends the typed question with the line as `about`, then clears the chip', async () => {
    await act(async () => lineAi(3).click());
    const anchor = store.ephemeral.lineRefs.get(PR)!;
    await type('why compute twice?');
    await key('Enter');
    expect(asks()).toHaveLength(1);
    expect(asks()[0]).toMatchObject({
      args: { req: { pr: PR, question: 'why compute twice?', about: anchor } },
    });
    expect(chip()).toBeNull();
    expect(box().value).toBe('');
    expect(host.querySelector('#H-f0h0 .line-ask')?.textContent).toContain(
      'Asking claude…',
    );
  });

  it('× removes the chip; then a send has no `about`', async () => {
    await act(async () => lineAi(2).click());
    await act(async () =>
      chip()!.querySelector<HTMLButtonElement>('.ref-remove')!.click(),
    );
    expect(chip()).toBeNull();
    expect(store.ephemeral.lineRefs.has(PR)).toBe(false);
    await type('and in general?');
    await key('Enter');
    expect(asks()[0]).toMatchObject({ args: { req: { about: null } } });
  });

  it('Backspace in an empty box removes the chip, not with text in it', async () => {
    await act(async () => lineAi(2).click());
    await type('x');
    await key('Backspace');
    expect(chip()).not.toBeNull();
    await type('');
    await key('Backspace');
    expect(chip()).toBeNull();
  });

  it('another line replaces the chip, keeping the text', async () => {
    await act(async () => lineAi(2).click());
    await type('why?');
    await act(async () => lineAi(4).click());
    expect(host.querySelectorAll('.chat-input .line-ref .ref')).toHaveLength(1);
    expect(store.ephemeral.lineRefs.get(PR)?.line).toBe(
      Number(chip()!.textContent!.match(/:(\d+)/)![1]),
    );
    expect(chip()?.textContent).not.toContain('file0.ts:2');
    expect(box().value).toBe('why?');
  });

  it('clicking the chip asks the diff for that line', async () => {
    await act(async () => lineAi(2).click());
    const anchor = store.ephemeral.lineRefs.get(PR)!;
    await act(async () =>
      chip()!.querySelector<HTMLButtonElement>('.ref-jump')!.click(),
    );
    expect(store.ephemeral.lineJump).toMatchObject({ pr: PR, anchor });
  });
});
