import { describe, expect, it } from 'vitest';
import type { CodeHover, CodeHoverRequest } from '../../gen/contract';
import { mount } from '../../test/dom';
import { HoverCard } from './CodeHoverHost';
import {
  HOVER_DELAY_MS,
  HoverCache,
  HoverController,
  META_HOVER_DELAY_MS,
  hoverTargetAt,
  sourceNote,
  type HoverShown,
  type HoverTarget,
  type HoverTimers,
} from './hover';

/** Timers the test fires by hand, remembering each delay. */
class ManualTimers implements HoverTimers {
  armed = new Map<number, { fn: () => void; ms: number }>();
  private next = 1;
  set(fn: () => void, ms: number): unknown {
    const id = this.next++;
    this.armed.set(id, { fn, ms });
    return id;
  }
  clear(h: unknown): void {
    this.armed.delete(h as number);
  }
  get delays(): number[] {
    return [...this.armed.values()].map((t) => t.ms);
  }
  fire(): void {
    const all = [...this.armed.values()];
    this.armed.clear();
    for (const t of all) t.fn();
  }
}

const flush = () => new Promise((r) => setTimeout(r, 0));

const LSP: CodeHover = {
  text: '```rust\nfn helper() -> u8\n```',
  source: 'lsp',
};

function setup(answer: (req: CodeHoverRequest) => Promise<CodeHover | null>) {
  const calls: CodeHoverRequest[] = [];
  const cache = new HoverCache((req) => {
    calls.push(req);
    return answer(req);
  });
  const shown: (HoverShown<string> | null)[] = [];
  const timers = new ManualTimers();
  const ctl = new HoverController<string>(cache, (s) => shown.push(s), timers);
  return { calls, cache, shown, timers, ctl };
}

const at = (col: number, name = 'helper'): HoverTarget => ({
  path: 'src/lib.rs',
  line: 7,
  col,
  name,
});

describe('hoverTargetAt', () => {
  it('finds the identifier and its 1-based char column', () => {
    expect(hoverTargetAt('  let x = helper(1);', 12, 'a.rs', 3)).toEqual({
      path: 'a.rs',
      line: 3,
      col: 11,
      name: 'helper',
    });
    // Columns count chars: the emoji is one char, two UTF-16 units.
    expect(hoverTargetAt('"😀" + name', 9, 'a.ts', 1)?.col).toBe(7);
    expect(hoverTargetAt('a + 12', 5, 'a.ts', 1)).toBeNull();
    expect(hoverTargetAt('  ', 1, 'a.ts', 1)).toBeNull();
  });
});

describe('HoverController', () => {
  it('waits for the pointer to rest, then shows the answer', async () => {
    const t = setup(() => Promise.resolve(LSP));
    t.ctl.move('pr1', 'h1', at(5), false, 'box');
    expect(t.timers.delays).toEqual([HOVER_DELAY_MS]);
    expect(t.calls).toHaveLength(0);
    t.timers.fire();
    await flush();
    expect(t.calls).toEqual([
      { pr: 'pr1', path: 'src/lib.rs', line: 7, col: 5 },
    ]);
    expect(t.shown.at(-1)).toEqual({
      target: at(5),
      hover: LSP,
      anchor: 'box',
    });
  });

  it('debounces: moving on restarts the wait, the same word does not', () => {
    const t = setup(() => Promise.resolve(LSP));
    t.ctl.move('pr1', 'h1', at(5), false, 'a');
    t.ctl.move('pr1', 'h1', at(5), false, 'a');
    expect(t.timers.armed.size).toBe(1);
    t.ctl.move('pr1', 'h1', at(20, 'other'), false, 'b');
    expect(t.timers.armed.size).toBe(1);
    t.ctl.move('pr1', 'h1', null, false, 'c');
    expect(t.timers.armed.size).toBe(0);
    expect(t.calls).toHaveLength(0);
  });

  it('asks sooner with ⌘ held, also when pressed while resting', () => {
    const t = setup(() => Promise.resolve(LSP));
    t.ctl.move('pr1', 'h1', at(5), true, 'a');
    expect(t.timers.delays).toEqual([META_HOVER_DELAY_MS]);
    t.ctl.leave();
    t.ctl.move('pr1', 'h1', at(5), false, 'a');
    expect(t.timers.delays).toEqual([HOVER_DELAY_MS]);
    t.ctl.move('pr1', 'h1', at(5), true, 'a');
    expect(t.timers.delays).toEqual([META_HOVER_DELAY_MS]);
  });

  it('asks once per position, across moves and heads', async () => {
    const t = setup(() => Promise.resolve(LSP));
    for (let i = 0; i < 3; i++) {
      t.ctl.move('pr1', 'h1', at(5), false, 'a');
      t.timers.fire();
      await flush();
      t.ctl.leave();
    }
    expect(t.calls).toHaveLength(1);
    // A new head is a new position.
    t.ctl.move('pr1', 'h2', at(5), false, 'a');
    t.timers.fire();
    await flush();
    expect(t.calls).toHaveLength(2);
  });

  it('shares a request in flight, and drops a reply for a word left behind', async () => {
    let resolve!: (h: CodeHover | null) => void;
    const t = setup(() => new Promise((r) => (resolve = r)));
    t.ctl.move('pr1', 'h1', at(5), false, 'a');
    t.timers.fire();
    t.ctl.move('pr1', 'h1', at(20, 'other'), false, 'b');
    resolve(LSP);
    await flush();
    expect(t.shown.filter(Boolean)).toHaveLength(0);
    // Back on the first word: the cached answer, no second request.
    t.ctl.move('pr1', 'h1', at(5), false, 'a');
    t.timers.fire();
    await flush();
    expect(t.calls).toHaveLength(1);
    expect(t.shown.at(-1)?.hover).toBe(LSP);
  });

  it('shows nothing for no answer or a failure, and retries a failure', async () => {
    let fail = true;
    const t = setup(() =>
      fail ? Promise.reject(new Error('off')) : Promise.resolve(null),
    );
    t.ctl.move('pr1', 'h1', at(5), false, 'a');
    t.timers.fire();
    await flush();
    expect(t.shown.filter(Boolean)).toHaveLength(0);
    expect(t.cache.size).toBe(0);
    fail = false;
    t.ctl.leave();
    t.ctl.move('pr1', 'h1', at(5), false, 'a');
    t.timers.fire();
    await flush();
    expect(t.calls).toHaveLength(2);
    expect(t.shown.filter(Boolean)).toHaveLength(0);
  });

  it('asks again while the server warms, then keeps its answer', async () => {
    const SYNTAX: CodeHover = { text: 'fn helper()', source: 'syntax' };
    const answers: (CodeHover | null)[] = [null, SYNTAX, LSP, SYNTAX];
    const t = setup(() => Promise.resolve(answers.shift()!));
    const hover = async () => {
      t.ctl.move('pr1', 'h1', at(5), false, 'a');
      t.timers.fire();
      await flush();
      t.ctl.leave();
    };
    // Nothing yet, then a by-name answer: neither is kept.
    await hover();
    expect(t.cache.size).toBe(0);
    await hover();
    expect(t.shown.at(-2)?.hover).toBe(SYNTAX);
    expect(t.cache.size).toBe(0);
    // The server's answer is kept: no more requests.
    await hover();
    await hover();
    expect(t.calls).toHaveLength(3);
    expect(t.shown.at(-2)?.hover).toBe(LSP);
  });
});

describe('HoverCache', () => {
  it('drops the least recently used past its size', async () => {
    const calls: number[] = [];
    const cache = new HoverCache((req) => {
      calls.push(req.col);
      return Promise.resolve(LSP);
    }, 2);
    const req = (col: number) => ({ pr: 'p', path: 'a', line: 1, col });
    await cache.get('1', req(1));
    await cache.get('2', req(2));
    await cache.get('1', req(1)); // refreshed
    await cache.get('3', req(3)); // evicts 2
    await cache.get('1', req(1));
    await cache.get('2', req(2));
    expect(calls).toEqual([1, 2, 3, 2]);
  });
});

describe('HoverCard', () => {
  it('marks an answer from the parser, not one from the server', async () => {
    expect(sourceNote(LSP)).toBeNull();
    const syntax: CodeHover = {
      text: '```rust\nfn helper()\n```\nMatched by name, no language server: src/a.rs:3',
      source: 'syntax',
    };
    const m = await mount(
      <HoverCard
        shown={{
          target: at(5),
          hover: syntax,
          anchor: { left: 10, top: 10, bottom: 20 },
        }}
      />,
    );
    expect(m.host.querySelector('.code-hover-source')?.textContent).toBe(
      'from the parser',
    );
    expect(m.host.textContent).toContain('fn helper()');
    m.unmount();
    const m2 = await mount(
      <HoverCard
        shown={{
          target: at(5),
          hover: LSP,
          anchor: { left: 10, top: 10, bottom: 20 },
        }}
      />,
    );
    expect(m2.host.querySelector('.code-hover-source')).toBeNull();
    m2.unmount();
  });
});
