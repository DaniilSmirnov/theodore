/** ⌘-click on an identifier in the diff opens the symbol peek: a head-side
 *  line sends its position, so the core answers for that exact symbol; a
 *  base-side line has no head-worktree position and goes by name. The
 *  chip says who found the hits. */
import { act } from 'react';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { setTransport } from '../core/transport';
import type { FindSymbolRequest, SymbolHit } from '../gen/contract';
import { settle } from '../test/dom';
import { FakeTransport } from '../test/fakeTransport';
import { mountBigPr } from '../test/bigPr';

// The peek's tabs animate their indicator; jsdom has no animations.
Element.prototype.getAnimations ??= () => [];

describe('the symbol peek', () => {
  let view: Awaited<ReturnType<typeof mountBigPr>>;
  let transport: FakeTransport;
  let hits: SymbolHit[];

  beforeEach(async () => {
    transport = new FakeTransport();
    hits = [];
    transport.onCall('find_symbol', () => hits);
    setTransport(transport);
    view = await mountBigPr();
  });

  afterEach(() => {
    view.unmount();
    delete (document as { caretRangeFromPoint?: unknown }).caretRangeFromPoint;
  });

  /** ⌘-clicks `word` on the hunk's `i`th line. jsdom has no layout, so
   *  the caret the browser would hit is put there by hand. */
  const metaClick = async (i: number, word: string) => {
    const line = view.host
      .querySelector('#H-f0h0')!
      .querySelectorAll<HTMLElement>('.ln')[i]!;
    const code = line.querySelector('code.src')!;
    const walker = document.createTreeWalker(code, NodeFilter.SHOW_TEXT);
    let node: Text | null = null;
    while (walker.nextNode()) {
      const t = walker.currentNode as Text;
      if (t.data.includes(word)) {
        node = t;
        break;
      }
    }
    if (!node) throw new Error(`no ${word} on line ${i}`);
    const offset = node.data.indexOf(word) + 1;
    document.caretRangeFromPoint = () => {
      const r = document.createRange();
      r.setStart(node, offset);
      return r;
    };
    await act(async () => {
      node.parentElement!.dispatchEvent(
        new MouseEvent('click', { bubbles: true, metaKey: true }),
      );
    });
    await settle();
  };

  const request = () =>
    (
      transport.calls.filter((c) => c.cmd === 'find_symbol').at(-1)!.args as {
        req: FindSymbolRequest;
      }
    ).req;

  it('a head-side click sends its line, column and side', async () => {
    // Line 0 is context: `const v0 = compute(0, 0, 0); // line`, head 1.
    await metaClick(0, 'compute');
    expect(request()).toMatchObject({
      name: 'compute',
      from_path: 'src/mod0/file0.ts',
      line: 1,
      col: 12,
      side: 'head',
    });
  });

  it('a base-side click sends no head position', async () => {
    // Line 1 is a deletion: it exists only in the base.
    await metaClick(1, 'compute');
    const req = request();
    expect(req).toMatchObject({ name: 'compute', side: 'base' });
    expect(req.line ?? null).toBeNull();
    expect(req.col ?? null).toBeNull();
  });

  it("the chip names the hits' source", async () => {
    hits = [
      {
        path: 'src/mod0/file0.ts',
        line: 1,
        kind: 'definition',
        preview: 'export function compute() {',
        source: 'lsp',
      },
    ];
    await metaClick(0, 'compute');
    const peek = view.host.querySelector('.sym-peek')!;
    expect(peek.textContent).toContain('language server');
    expect(peek.textContent).not.toContain('git grep');
  });
});
