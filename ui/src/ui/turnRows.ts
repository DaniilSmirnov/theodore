/**
 * A turn's steps as the rows it shows: what each row says, and what a finished
 * turn folds. Pure, so the rules are tested without rendering.
 *
 * ```text
 * running    every step its own row, in order, growing
 * finished   always a row: a failed tool, a question, a plan, the last text
 *                          (a stopped or failed turn: all its text)
 *            the rest between them:
 *              3 or more items ─▶ one line: 3 tool calls, 5 messages, 2 agents
 *              fewer           ─▶ flat
 *            thinking counts for nothing, and goes with what's around it
 * ```
 */
import type { TurnFile, TurnStep } from '../gen/contract';
import { mcpServer, toolRow, type ToolKind } from './toolSummary';

export type ToolStep = Extract<TurnStep, { kind: 'tool' }>;

const isTool = (s: TurnStep): s is ToolStep => s.kind === 'tool';
const ASK = 'ask you';
const PLAN = 'ExitPlanMode';
/** Claude Code's own plan and question tools: a print-mode run (`-p`) has
 *  no one to show them to, so the CLI turns them away. Expected, no failure. */
const REFUSED_IN_PRINT = new Set([PLAN, 'AskUserQuestion']);

/** A tool call that failed, as a row says it: not a refusal `-p` expects. */
export function stepFailed(step: ToolStep): boolean {
  return step.ok === false && !REFUSED_IN_PRINT.has(step.name);
}

/** How many of the same, with its plural. */
const n = (count: number, one: string, many = `${one}s`) =>
  `${count} ${count === 1 ? one : many}`;

// ── a row's words ───────────────────────────────────────────────────────────

export interface ToolView {
  kind: ToolKind;
  label: string;
  /** Muted and monospace: the command, file or question. */
  chip: string | null;
  /** The chip names a file. */
  isFile: boolean;
  /** Muted, after the chip: how much a search found. */
  note: string | null;
  added: number | null;
  removed: number | null;
}

const READS = new Set(['Read', 'NotebookRead', 'read_file', 'read_many_files']);
const SEARCHES: Record<string, string> = {
  Grep: 'grep',
  search_file_content: 'grep',
  Glob: 'glob',
  glob: 'glob',
};

/** What a tool row says. `running`: its turn is still going. */
export function toolView(step: ToolStep, running = false): ToolView {
  const { kind, label } = toolRow(step.name);
  const view: ToolView = {
    kind,
    label,
    chip: step.target,
    isFile: false,
    note: null,
    added: null,
    removed: null,
  };
  if (step.name === ASK) {
    return {
      ...view,
      label:
        running && step.ok == null ? 'Waiting for your answer' : 'Asked you',
      chip: step.detail ?? null,
    };
  }
  if (kind === 'agent') {
    return { ...view, label: step.detail ?? label, chip: null };
  }
  if (kind === 'run') return { ...view, label: step.detail ?? label };
  if (READS.has(step.name)) {
    return {
      ...view,
      isFile: true,
      label: /^\d+ lines?$/.test(step.summary ?? '')
        ? `Read ${step.summary}`
        : label,
    };
  }
  const search = SEARCHES[step.name];
  if (search && step.detail) {
    const within =
      step.target && step.target !== step.detail ? ` in ${step.target}` : '';
    return {
      ...view,
      label: `${search} for '${step.detail}'${within}`,
      chip: null,
      note: step.summary ?? null,
    };
  }
  if (kind === 'edit') {
    const written = step.removed == null && step.added != null;
    return {
      ...view,
      // Theodore's own edits read as phrases, about a symbol.
      isFile: !step.name.includes(' '),
      label: written ? `${label} ${n(step.added!, 'line')}` : label,
      added: written ? null : (step.added ?? null),
      removed: step.removed ?? null,
    };
  }
  return view;
}

// ── consecutive calls as one row ────────────────────────────────────────────

/** Read-only calls of one tool, or calls to one MCP server: `null` for the
 *  ones that stay a row each, edits and commands above all. */
function groupKey(step: ToolStep): string | null {
  if (stepFailed(step)) return null;
  const { kind } = toolRow(step.name);
  if (kind === 'mcp') return `mcp:${mcpServer(step.name)}`;
  if (kind === 'read' || kind === 'search') return `tool:${step.name}`;
  return null;
}

/** `Read 4 files`, `Search 3 times`, `linear: 2 calls`. */
export function groupLabel(step: ToolStep, count: number): string {
  const { kind, label } = toolRow(step.name);
  if (kind === 'mcp') return `${mcpServer(step.name)}: ${n(count, 'call')}`;
  if (READS.has(step.name)) return `Read ${n(count, 'file')}`;
  if (step.name === 'WebFetch' || step.name === 'web_fetch') {
    return `Fetch ${n(count, 'page')}`;
  }
  if (step.name === 'LS' || step.name === 'list_directory') {
    return `List ${n(count, 'folder')}`;
  }
  if (SEARCHES[step.name]) return `Search ${count} times`;
  return `${label} ×${count}`;
}

// ── the fold ────────────────────────────────────────────────────────────────

export interface FoldCounts {
  tools: number;
  messages: number;
  agents: number;
}

/** A row of the list. `index`: the step's place in the turn, stable for it. */
export type TurnItem =
  | { kind: 'step'; index: number; step: TurnStep }
  | { kind: 'group'; index: number; steps: ToolStep[] }
  | {
      kind: 'fold';
      index: number;
      items: TurnItem[];
      counts: FoldCounts;
      icons: ToolKind[];
    };

export interface FoldOptions {
  /** The turn is going: flat, growing, nothing folds or groups. */
  running?: boolean;
  /** Nothing folds; a subagent's rows when open. */
  flat?: boolean;
  /** No reply text comes after these steps: the last text in them is the
   *  last thing the turn said. */
  pinLastText?: boolean;
  /** A stopped or failed turn keeps everything it wrote in view. */
  keepText?: boolean;
}

/** Text with nothing to show yet isn't a row. */
const shows = (s: TurnStep) => s.kind !== 'text' || !!s.text.trim();

/** A turn's own steps, and each subagent's under its outermost call. A step
 *  whose call is gone (past the bound) has nowhere to go. */
export function splitSteps(steps: readonly TurnStep[]): {
  own: TurnStep[];
  under: Map<string, TurnStep[]>;
} {
  const parentOf = new Map<string, string>();
  for (const s of steps) {
    if (isTool(s) && s.id && s.parent) parentOf.set(s.id, s.parent);
  }
  const root = (id: string) => {
    let at = id;
    for (let hops = 0; hops < 8; hops += 1) {
      const up = parentOf.get(at);
      if (!up) return at;
      at = up;
    }
    return at;
  };
  const own: TurnStep[] = [];
  const under = new Map<string, TurnStep[]>();
  for (const s of steps) {
    if (!shows(s)) continue;
    if (!s.parent) {
      own.push(s);
      continue;
    }
    const of = root(s.parent);
    const list = under.get(of);
    if (list) list.push(s);
    else under.set(of, [s]);
  }
  return { own, under };
}

/** The items a list of steps shows, in the order they happened. */
export function turnItems(
  steps: readonly TurnStep[],
  options: FoldOptions = {},
): TurnItem[] {
  const list = steps.map((step, index) => ({ step, index }));
  if (options.running) {
    return list.map(({ step, index }) => ({ kind: 'step', index, step }));
  }
  if (options.flat) return grouped(list);
  const lastText = options.pinLastText
    ? steps.map((s) => s.kind).lastIndexOf('text')
    : -1;
  const pinned = (step: TurnStep, index: number) => {
    if (step.kind === 'text') return !!options.keepText || index === lastText;
    if (!isTool(step)) return false;
    return stepFailed(step) || step.name === ASK || step.name === PLAN;
  };
  const items: TurnItem[] = [];
  let run: { step: TurnStep; index: number }[] = [];
  const flush = () => {
    items.push(...foldRun(run));
    run = [];
  };
  for (const entry of list) {
    if (pinned(entry.step, entry.index)) {
      flush();
      items.push({ kind: 'step', ...entry });
    } else {
      run.push(entry);
    }
  }
  flush();
  return items;
}

function foldRun(run: { step: TurnStep; index: number }[]): TurnItem[] {
  const counts: FoldCounts = { tools: 0, messages: 0, agents: 0 };
  const icons: ToolKind[] = [];
  for (const { step } of run) {
    if (step.kind === 'text') counts.messages += 1;
    if (!isTool(step)) continue;
    const { kind } = toolRow(step.name);
    if (kind === 'agent') counts.agents += 1;
    else counts.tools += 1;
    if (!icons.includes(kind)) icons.push(kind);
  }
  if (counts.tools + counts.messages + counts.agents < 3) return grouped(run);
  return [
    {
      kind: 'fold',
      index: run[0]!.index,
      items: grouped(run),
      counts,
      icons,
    },
  ];
}

function grouped(run: { step: TurnStep; index: number }[]): TurnItem[] {
  const items: TurnItem[] = [];
  let i = 0;
  while (i < run.length) {
    const { step, index } = run[i]!;
    const key = isTool(step) ? groupKey(step) : null;
    let end = i + 1;
    if (key) {
      while (end < run.length) {
        const next = run[end]!.step;
        if (!isTool(next) || groupKey(next) !== key) break;
        end += 1;
      }
    }
    if (end - i > 1) {
      const steps = run.slice(i, end).map((e) => e.step as ToolStep);
      items.push({ kind: 'group', index, steps });
    } else {
      items.push({ kind: 'step', index, step });
    }
    i = end;
  }
  return items;
}

/** `3 tool calls, 5 messages, 2 agents`, a zero part left out. */
export function foldLine({ tools, messages, agents }: FoldCounts): string {
  return [
    tools && n(tools, 'tool call'),
    messages && n(messages, 'message'),
    agents && n(agents, 'agent'),
  ]
    .filter(Boolean)
    .join(', ');
}

// ── the footer's files ──────────────────────────────────────────────────────

export interface FileChips {
  shown: TurnFile[];
  /** What's past them, in total. */
  rest: { files: number; additions: number; deletions: number } | null;
}

/** Up to `max` files, then the total of the others. */
export function fileChips(files: readonly TurnFile[], max = 3): FileChips {
  const shown = files.slice(0, max);
  const others = files.slice(max);
  return {
    shown,
    rest: others.length
      ? {
          files: others.length,
          additions: others.reduce((sum, f) => sum + f.additions, 0),
          deletions: others.reduce((sum, f) => sum + f.deletions, 0),
        }
      : null,
  };
}
