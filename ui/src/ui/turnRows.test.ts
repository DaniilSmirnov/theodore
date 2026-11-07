import { describe, expect, it } from 'vitest';
import type { TurnStep } from '../gen/contract';
import { fileType } from './FileTypeIcon';
import {
  fileChips,
  foldLine,
  groupLabel,
  splitSteps,
  stepFailed,
  toolView,
  turnItems,
  type ToolStep,
  type TurnItem,
} from './turnRows';

const text = (t: string): TurnStep => ({ kind: 'text', text: t });
const thinking: TurnStep = { kind: 'thinking', text: 'hm' };
const tool = (name: string, over: Partial<ToolStep> = {}): ToolStep => ({
  kind: 'tool',
  name,
  target: null,
  ok: true,
  ...over,
});
const run = (over: Partial<ToolStep> = {}) => tool('Bash', over);

/** A compact picture of the list: `t` text, `T` tool, `?` thinking, `[n]` a
 *  fold of n rows, `(n)` a group of n calls. */
const shape = (items: TurnItem[]) =>
  items
    .map((i) => {
      if (i.kind === 'fold') return `[${i.items.length}]`;
      if (i.kind === 'group') return `(${i.steps.length})`;
      if (i.step.kind === 'text') return 't';
      return i.step.kind === 'thinking' ? '?' : 'T';
    })
    .join(' ');

describe('the fold rule', () => {
  it('a running turn never folds or groups', () => {
    const steps = [run(), run(), run(), tool('Read'), tool('Read'), text('a')];
    expect(shape(turnItems(steps, { running: true }))).toBe('T T T T T t');
  });

  it('three or more items fold into one line, fewer stay flat', () => {
    expect(shape(turnItems([run(), run(), run()]))).toBe('[3]');
    expect(shape(turnItems([run(), run()]))).toBe('T T');
  });

  it('thinking is inside and isn’t counted', () => {
    expect(shape(turnItems([thinking, run(), thinking, run()]))).toBe(
      '? T ? T',
    );
    const [fold] = turnItems([thinking, run(), run(), run(), thinking]);
    expect(fold).toMatchObject({
      kind: 'fold',
      counts: { tools: 3, messages: 0, agents: 0 },
    });
    expect(fold!.kind === 'fold' && fold!.items).toHaveLength(5);
  });

  it('a failed tool, a question and a plan stay, and split the runs', () => {
    const steps = [
      run(),
      run(),
      run(),
      run({ ok: false }),
      run(),
      tool('ask you', { ok: null }),
      run(),
      tool('ExitPlanMode'),
    ];
    expect(shape(turnItems(steps))).toBe('[3] T T T T T');
  });

  it('the last text stays when no reply follows; otherwise it may fold', () => {
    const steps = [text('a'), run(), run(), text('last')];
    expect(shape(turnItems(steps, { pinLastText: true }))).toBe('[3] t');
    expect(shape(turnItems(steps))).toBe('[4]');
  });

  it('a stopped or failed turn keeps all it wrote', () => {
    const steps = [text('a'), run(), run(), run(), text('b'), run()];
    expect(shape(turnItems(steps, { keepText: true }))).toBe('t [3] t T');
  });

  it('rows keep the order they happened in', () => {
    const steps = [text('a'), run({ ok: false }), text('b'), run(), run()];
    const flat = turnItems(steps, { keepText: true }).flatMap((i) =>
      i.kind === 'step' ? [i.index] : [],
    );
    expect(flat).toEqual([0, 1, 2, 3, 4]);
  });

  it('a subagent’s rows don’t count, and its call is an agent', () => {
    const steps = [
      tool('Agent', { id: 'ag' }),
      run({ parent: 'ag' }),
      run({ parent: 'ag' }),
      run(),
      text('x'),
    ];
    const { own, under } = splitSteps(steps);
    expect(own).toHaveLength(3);
    expect(under.get('ag')).toHaveLength(2);
    const [fold] = turnItems(own);
    expect(fold).toMatchObject({
      counts: { tools: 1, messages: 1, agents: 1 },
      icons: ['agent', 'run'],
    });
  });

  it('a subagent’s subagent goes under the outermost call; an orphan nowhere', () => {
    const { own, under } = splitSteps([
      tool('Agent', { id: 'a' }),
      tool('Agent', { id: 'b', parent: 'a' }),
      run({ id: 'c', parent: 'b' }),
      text('  '),
    ]);
    expect(own).toHaveLength(1);
    expect(under.get('a')?.map((s) => s.kind === 'tool' && s.id)).toEqual([
      'b',
      'c',
    ]);
  });
});

describe('the summary line', () => {
  it('leaves a zero out and says the singular', () => {
    expect(foldLine({ tools: 3, messages: 5, agents: 2 })).toBe(
      '3 tool calls, 5 messages, 2 agents',
    );
    expect(foldLine({ tools: 1, messages: 0, agents: 1 })).toBe(
      '1 tool call, 1 agent',
    );
    expect(foldLine({ tools: 0, messages: 1, agents: 0 })).toBe('1 message');
  });

  it('shows each kind of tool once, first seen first', () => {
    const [fold] = turnItems([run(), tool('Read'), run(), tool('Edit')]);
    expect(fold).toMatchObject({ icons: ['run', 'read', 'edit'] });
  });
});

describe('grouping', () => {
  it('groups calls of one read-only tool, not edits or commands', () => {
    const reads = [tool('Read'), tool('Read'), tool('Read'), tool('Read')];
    expect(shape(turnItems(reads, { flat: true }))).toBe('(4)');
    expect(groupLabel(tool('Read'), 4)).toBe('Read 4 files');
    const edits = [tool('Edit'), tool('Edit'), run(), run()];
    expect(shape(turnItems(edits, { flat: true }))).toBe('T T T T');
  });

  it('groups by MCP server, across its tools', () => {
    const calls = [
      tool('mcp__linear__get_issue'),
      tool('mcp__linear__list_issues'),
      tool('mcp__github__get_pr'),
    ];
    expect(shape(turnItems(calls, { flat: true }))).toBe('(2) T');
    expect(groupLabel(calls[0]!, 2)).toBe('linear: 2 calls');
  });

  it('a thought between calls, or another tool, ends a group', () => {
    const steps = [tool('Read'), thinking, tool('Read'), tool('Grep')];
    expect(shape(turnItems(steps, { flat: true }))).toBe('T ? T T');
  });

  it('groups inside a fold, and the fold counts each call', () => {
    const [fold] = turnItems([tool('Read'), tool('Read'), tool('Read')]);
    expect(fold).toMatchObject({ counts: { tools: 3 } });
    expect(shape(fold!.kind === 'fold' ? fold!.items : [])).toBe('(3)');
  });
});

describe('a tool row', () => {
  it('a command reads by what the agent said it does', () => {
    expect(
      toolView(run({ target: 'ls -la', detail: 'List it' })),
    ).toMatchObject({ kind: 'run', label: 'List it', chip: 'ls -la' });
    expect(toolView(run({ target: 'ls -la' }))).toMatchObject({
      label: 'Run',
      chip: 'ls -la',
    });
  });

  it('a read says how many lines when its result does', () => {
    expect(
      toolView(tool('Read', { target: 'a.rs', summary: '120 lines' })),
    ).toMatchObject({ label: 'Read 120 lines', chip: 'a.rs', isFile: true });
    expect(toolView(tool('Read', { target: 'a.rs' }))).toMatchObject({
      label: 'Read',
      isFile: true,
    });
  });

  it('a search names its pattern and where, and what it found', () => {
    expect(
      toolView(
        tool('Grep', { detail: 'x', target: 'src', summary: '5 matches' }),
      ),
    ).toMatchObject({ label: "grep for 'x' in src", note: '5 matches' });
    expect(toolView(tool('Grep', { detail: 'x', target: 'x' }))).toMatchObject({
      label: "grep for 'x'",
      chip: null,
    });
    expect(toolView(tool('Glob', { detail: '*.rs' })).label).toBe(
      "glob for '*.rs'",
    );
    // An older row has only its target.
    expect(toolView(tool('Grep', { target: 'x' }))).toMatchObject({
      label: 'Search',
      chip: 'x',
    });
  });

  it('an edit counts its lines; a write says how many', () => {
    expect(
      toolView(tool('Edit', { target: 'a.rs', added: 3, removed: 1 })),
    ).toMatchObject({ label: 'Edit', isFile: true, added: 3, removed: 1 });
    expect(
      toolView(tool('Write', { target: 'a.rs', added: 2, removed: null })),
    ).toMatchObject({ label: 'Write 2 lines', added: null, removed: null });
    expect(toolView(tool('Write', { target: 'a.rs', added: 1 })).label).toBe(
      'Write 1 line',
    );
  });

  it('a question says where it stands', () => {
    const ask = tool('ask you', { ok: null, detail: 'Which?' });
    expect(toolView(ask, true)).toMatchObject({
      label: 'Waiting for your answer',
      chip: 'Which?',
    });
    expect(toolView(ask).label).toBe('Asked you');
    expect(toolView({ ...ask, ok: true }, true).label).toBe('Asked you');
  });

  it('an agent reads by its description', () => {
    expect(
      toolView(tool('Agent', { detail: 'Look around', target: 'x' })),
    ).toMatchObject({ kind: 'agent', label: 'Look around', chip: null });
    expect(toolView(tool('Task')).label).toBe('Run a subagent');
  });
});

describe('the footer’s files', () => {
  const file = (n: number) => ({
    path: `f${n}.rs`,
    additions: n,
    deletions: 1,
  });

  it('shows up to three, then the total of the rest', () => {
    const three = [file(1), file(2), file(3)];
    expect(fileChips(three)).toEqual({ shown: three, rest: null });
    expect(fileChips([...three, file(4), file(5)])).toEqual({
      shown: three,
      rest: { files: 2, additions: 9, deletions: 2 },
    });
  });
});

describe('a file’s type', () => {
  it('by exact name, then extension, then none', () => {
    expect(fileType('apps/Dockerfile')).toBe('dockerfile');
    expect(fileType('Cargo.lock')).toBe('lock');
    expect(fileType('ui/pnpm-lock.yaml')).toBe('lock');
    expect(fileType('src/a.rs')).toBe('rs');
    expect(fileType('src/a.test.tsx')).toBe('tsx');
    expect(fileType('.github/ci.yml')).toBe('yaml');
    expect(fileType('notes.txt')).toBeNull();
    expect(fileType('Makefile')).toBeNull();
    // Not a property of the table's prototype.
    expect(fileType('a.constructor')).toBeNull();
    expect(fileType('constructor')).toBeNull();
  });
});

describe('refusals a print-mode run expects', () => {
  it('ExitPlanMode and AskUserQuestion turned away are not failures', () => {
    expect(stepFailed(tool('ExitPlanMode', { ok: false }))).toBe(false);
    expect(stepFailed(tool('AskUserQuestion', { ok: false }))).toBe(false);
    expect(stepFailed(run({ ok: false }))).toBe(true);
    expect(stepFailed(run())).toBe(false);
    // Not pinned as a failed row: it folds with the rest.
    const steps = [
      run(),
      tool('AskUserQuestion', { ok: false }),
      run(),
      text('done'),
    ];
    expect(shape(turnItems(steps, { pinLastText: true }))).toBe('[3] t');
  });
});
