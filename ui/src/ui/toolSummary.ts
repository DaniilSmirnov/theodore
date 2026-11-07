/** A run's log, folded for reading. The core sends one line per event:
 *  `tool: <name>` for a call (optionally `tool: <name> · <target>` when it
 *  knows the command or file), `tool result ok=<bool>` after it, and plain
 *  notes like `stalled`. A build turn makes dozens of calls, mostly the
 *  same tool back to back — this collapses them into runs and totals. */

/** Consecutive calls of one tool, or a note line kept as-is. */
export type ToolGroup =
  | {
      kind: 'tool';
      tool: string;
      count: number;
      /** The latest call's target (command, file), truncated; null if the
       *  log didn't carry one. */
      target: string | null;
      failed: number;
    }
  | { kind: 'note'; text: string };

export type ToolSummary = {
  /** Tool calls so far. */
  steps: number;
  groups: ToolGroup[];
  /** Calls per tool, in first-seen order. */
  totals: { tool: string; count: number }[];
  /** The newest call — what the agent is doing right now. */
  latest: { tool: string; target: string | null } | null;
};

const TOOL = 'tool: ';
const RESULT = /^tool result ok=(true|false)$/;
const TARGET_SEP = ' · ';
export const TARGET_MAX = 60;

const clip = (s: string) =>
  s.length > TARGET_MAX ? `${s.slice(0, TARGET_MAX - 1)}…` : s;

/** An MCP tool without a label from the core reads by its own name:
 *  `mcp__server__do_thing` (Codex: `server__do_thing`) → `do thing`. */
export function readableTool(name: string): string {
  const own = /^(?:mcp__)?\S+?__(\S+)$/.exec(name)?.[1];
  return own ? own.replace(/_/g, ' ') : name;
}

/** A `tool: …` line's tool and whole target; `null` for any other line. */
export function parseToolLine(
  line: string,
): { tool: string; target: string | null } | null {
  if (!line.startsWith(TOOL)) return null;
  const rest = line.slice(TOOL.length).trim();
  const at = rest.indexOf(TARGET_SEP);
  if (at < 0) return { tool: readableTool(rest), target: null };
  const target = rest
    .slice(at + TARGET_SEP.length)
    .replace(/\s+/g, ' ')
    .trim();
  return {
    tool: readableTool(rest.slice(0, at).trim()),
    target: target || null,
  };
}

/** A `tool result ok=…` line's outcome; `null` for any other line. */
export function toolResultOk(line: string): boolean | null {
  const result = RESULT.exec(line);
  return result ? result[1] === 'true' : null;
}

export type ToolKind =
  'read' | 'edit' | 'run' | 'search' | 'agent' | 'mcp' | 'other';

/** The agents' own tools, as a timeline row reads them. */
const TOOL_ROWS = new Map<string, { kind: ToolKind; label: string }>(
  (
    [
      ['Read', 'read', 'Read'],
      ['NotebookRead', 'read', 'Read'],
      ['read_file', 'read', 'Read'],
      ['read_many_files', 'read', 'Read'],
      ['WebFetch', 'read', 'Fetch'],
      ['web_fetch', 'read', 'Fetch'],
      ['Edit', 'edit', 'Edit'],
      ['MultiEdit', 'edit', 'Edit'],
      ['NotebookEdit', 'edit', 'Edit'],
      ['replace', 'edit', 'Edit'],
      ['apply_patch', 'edit', 'Edit'],
      ['Write', 'edit', 'Write'],
      ['write_file', 'edit', 'Write'],
      ['Bash', 'run', 'Run'],
      ['shell', 'run', 'Run'],
      ['run_shell_command', 'run', 'Run'],
      ['Grep', 'search', 'Search'],
      ['Glob', 'search', 'Search'],
      ['glob', 'search', 'Search'],
      ['search_file_content', 'search', 'Search'],
      ['LS', 'search', 'List'],
      ['list_directory', 'search', 'List'],
      ['WebSearch', 'search', 'Search the web'],
      ['web_search', 'search', 'Search the web'],
      ['google_web_search', 'search', 'Search the web'],
      ['Task', 'agent', 'Run a subagent'],
      ['Agent', 'agent', 'Run a subagent'],
      ['TodoWrite', 'other', 'Update the to-do list'],
    ] as const
  ).map(([name, kind, label]) => [name, { kind, label }]),
);

/** The kinds of Theodore's own tools, which log as phrases (`read the diff`). */
const PHRASE_KINDS: readonly [RegExp, ToolKind][] = [
  [/^read /, 'read'],
  [/^(replace|insert|rename) /, 'edit'],
  [/^(run|start|stop) /, 'run'],
  [/^(find|go to) /, 'search'],
];

/** The MCP server of a tool named by one: `mcp__server__do_thing` (Codex:
 *  `server__do_thing`) → `server`. */
export function mcpServer(name: string): string | null {
  return /^(?:mcp__)?(\S+?)__\S+$/.exec(name)?.[1] ?? null;
}

/** How a tool call reads as a row: `Bash` → Run, `ship the change` → Ship the
 *  change, `mcp__server__do_thing` → Do thing. */
export function toolRow(name: string): { kind: ToolKind; label: string } {
  const known = TOOL_ROWS.get(name);
  if (known) return known;
  const read = readableTool(name);
  return {
    kind: mcpServer(name)
      ? 'mcp'
      : (PHRASE_KINDS.find(([phrase]) => phrase.test(read))?.[1] ?? 'other'),
    label: read.charAt(0).toUpperCase() + read.slice(1),
  };
}

export function summarizeTools(lines: readonly string[]): ToolSummary {
  const groups: ToolGroup[] = [];
  const totals = new Map<string, number>();
  let latest: ToolSummary['latest'] = null;
  let steps = 0;
  for (const raw of lines) {
    const line = raw.trim();
    if (!line) continue;
    const ok = toolResultOk(line);
    if (ok != null) {
      // A result belongs to the call before it; a success is noise.
      const last = groups.at(-1);
      if (!ok && last?.kind === 'tool') last.failed += 1;
      continue;
    }
    const parsed = parseToolLine(line);
    if (!parsed) {
      groups.push({ kind: 'note', text: line });
      continue;
    }
    if (!parsed.tool) continue;
    const call = {
      tool: parsed.tool,
      target: parsed.target && clip(parsed.target),
    };
    steps += 1;
    latest = call;
    totals.set(call.tool, (totals.get(call.tool) ?? 0) + 1);
    const last = groups.at(-1);
    if (last?.kind === 'tool' && last.tool === call.tool) {
      last.count += 1;
      last.target = call.target ?? last.target;
    } else {
      groups.push({ kind: 'tool', ...call, count: 1, failed: 0 });
    }
  }
  return {
    steps,
    groups,
    totals: [...totals].map(([tool, count]) => ({ tool, count })),
    latest,
  };
}

/** `Bash ×12`, or just `Bash` for one call. */
export const times = (tool: string, count: number) =>
  count > 1 ? `${tool} ×${count}` : tool;

/** A finished turn in one line: `Used Bash ×12, Edit ×4, Read ×7`. Past
 *  `max` tools the tail folds into `+N more`. */
export function usedLine(s: ToolSummary, max = 5): string {
  if (!s.totals.length) return '';
  const shown = s.totals.slice(0, max).map((t) => times(t.tool, t.count));
  const rest = s.totals.length - shown.length;
  return `Used ${shown.join(', ')}${rest > 0 ? `, +${rest} more` : ''}`;
}

/** What the agent is doing now: `Running Bash · npm test…`. Theodore's own
 *  tools already log as phrases (`read the PR`), so those read as-is. */
export function runningLine(latest: NonNullable<ToolSummary['latest']>) {
  const phrase = /^[a-z]/.test(latest.tool) && latest.tool.includes(' ');
  const head = phrase
    ? latest.tool[0]!.toUpperCase() + latest.tool.slice(1)
    : `Running ${latest.tool}`;
  return `${head}${latest.target ? `${TARGET_SEP}${latest.target}` : ''}…`;
}
