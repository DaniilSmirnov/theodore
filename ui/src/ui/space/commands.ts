/**
 * Space commands (setup / run / check / archive) and project config: pure
 * rules for what the space page and Settings → Labs show — the log buffer,
 * the trust prompt, the "Setting up…" stage, the Run button, check chips.
 */
import type {
  ConfigEntry,
  ConfigFile,
  ConfigLayer,
  Space,
  SpaceCheck,
  SpaceCommandKind,
  SpaceConfigView,
  SpaceLogLine,
  SpaceTurn,
} from '../../gen/contract';

/** Lines kept per space in memory (live events + the seed from `space_logs`). */
export const SPACE_LOG_LIMIT = 2000;

/** Appends lines, keeping the newest `limit`. */
export function appendLogs(
  buf: readonly SpaceLogLine[],
  lines: readonly SpaceLogLine[],
  limit = SPACE_LOG_LIMIT,
): SpaceLogLine[] {
  const next = [...buf, ...lines];
  return next.length > limit ? next.slice(next.length - limit) : next;
}

const lineKey = (l: SpaceLogLine) =>
  `${l.at}\u0000${l.kind}\u0000${l.stderr ? 1 : 0}\u0000${l.line}`;

/** The seed from `space_logs` (the core's tail), with the live lines that
 *  arrived while it was in flight added after it — the ones the seed
 *  doesn't already hold. */
export function seedLogs(
  seed: readonly SpaceLogLine[],
  live: readonly SpaceLogLine[],
  limit = SPACE_LOG_LIMIT,
): SpaceLogLine[] {
  const last = seed.length ? seed[seed.length - 1]!.at : -Infinity;
  const have = new Set(seed.filter((l) => l.at === last).map(lineKey));
  const extra = live.filter(
    (l) => l.at > last || (l.at === last && !have.has(lineKey(l))),
  );
  return appendLogs(seed, extra, limit);
}

export type LogFilter = 'all' | SpaceCommandKind;

export const LOG_FILTERS: { id: LogFilter; label: string }[] = [
  { id: 'all', label: 'All' },
  { id: 'setup', label: 'Setup' },
  { id: 'run', label: 'Run' },
  { id: 'check', label: 'Check' },
  { id: 'archive', label: 'Archive' },
];

export function filterLogs(
  lines: readonly SpaceLogLine[],
  filter: LogFilter,
): readonly SpaceLogLine[] {
  return filter === 'all' ? lines : lines.filter((l) => l.kind === filter);
}

/** Lines per kind, for the counts on the drawer's tabs. */
export function logCounts(
  lines: readonly SpaceLogLine[],
): Record<LogFilter, number> {
  const n: Record<LogFilter, number> = {
    all: lines.length,
    setup: 0,
    run: 0,
    check: 0,
    archive: 0,
  };
  for (const l of lines) n[l.kind]++;
  return n;
}

/** "Run commands from this repo?": `ask` shows the card, `denied` the
 *  small "skipped" note (with a way back to the card), `null` nothing. */
export type TrustState = 'ask' | 'denied' | null;

export function trustState(
  c: Pick<SpaceConfigView, 'repo_commands' | 'denied'> | null | undefined,
): TrustState {
  const repo = c?.repo_commands;
  if (!repo || repo.trusted) return null;
  return c.denied ? 'denied' : 'ask';
}

/** Where the "Setting up…" stage stands: `stage` takes the chat's place
 *  (live log, Retry / Continue on failure); `note` is a compact line for
 *  a finished setup that left notes; `null` nothing. */
export type SetupView = 'stage' | 'note' | null;

/** Setup is running or waiting for approval: the core refuses messages
 *  until it ends, so the composer is off. */
export function settingUp(s: Pick<Space, 'setup'>): boolean {
  return s.setup?.status === 'running' || s.setup?.status === 'waiting';
}

export function setupView(s: Pick<Space, 'setup'>): SetupView {
  const setup = s.setup;
  if (!setup) return null;
  switch (setup.status) {
    case 'waiting':
    case 'running':
    case 'failed':
      return 'stage';
    case 'done':
    case 'skipped':
      return setup.notes?.length ? 'note' : null;
  }
}

export const SETUP_LABEL: Record<
  NonNullable<Space['setup']>['status'],
  string
> = {
  waiting: 'Waiting for your approval to run setup',
  running: 'Setting up…',
  failed: 'Setup failed',
  done: 'Set up',
  skipped: 'Setup skipped',
};

/** The header's Run / Stop button: shown when a `run` command is set and
 *  allowed; `running` from the space's process. */
export interface RunControl {
  running: boolean;
  /** The command, for the tooltip. */
  script: string;
  /** How the last run ended, e.g. "exited 1"; `null` while running or never run. */
  last: string | null;
}

export function runControl(
  config: Pick<SpaceConfigView, 'commands'> | null | undefined,
  space: Pick<Space, 'process' | 'status'>,
): RunControl | null {
  if (space.status === 'archived') return null;
  const cmd = config?.commands.find((c) => c.kind === 'run' && c.allowed);
  if (!cmd) return null;
  const p = space.process;
  const running = p?.status === 'running';
  let last: string | null = null;
  if (p && !running) {
    last =
      p.status === 'failed'
        ? `could not start${p.error ? `: ${p.error}` : ''}`
        : p.status === 'stopped'
          ? 'stopped'
          : `exited ${p.exit_code ?? '?'}`;
  }
  return { running, script: cmd.script, last };
}

export const portUrl = (port: number) => `http://localhost:${port}`;

export type CheckTone = 'running' | 'passed' | 'failed';

/** The chip on a turn: "Check passed", "Check failed (exit 1)", "Check
 *  timed out" (a fail). */
export function checkChip(c: SpaceCheck): { tone: CheckTone; label: string } {
  if (c.status === 'running') return { tone: 'running', label: 'Checking…' };
  if (c.status === 'passed') return { tone: 'passed', label: 'Check passed' };
  if (c.timed_out) return { tone: 'failed', label: 'Check timed out' };
  return {
    tone: 'failed',
    label:
      c.exit_code != null
        ? `Check failed (exit ${c.exit_code})`
        : 'Check failed',
  };
}

/** Each agent reply's check, by the reply's message id. */
export function checksByReply(
  log: readonly SpaceTurn[] | undefined,
): Map<string, SpaceCheck> {
  const m = new Map<string, SpaceCheck>();
  for (const t of log ?? []) if (t.check) m.set(t.reply, t.check);
  return m;
}

/** Each reply's turn run id, as the core names it (`space:<id>:<message>`):
 *  tells a turn whose reply is stored from one still being written. */
export function runsByReply(
  space: string,
  log: readonly SpaceTurn[] | undefined,
): Map<string, string> {
  const m = new Map<string, string>();
  for (const t of log ?? []) m.set(t.reply, `space:${space}:${t.message}`);
  return m;
}

export const LAYER_LABEL: Record<ConfigLayer, string> = {
  global: 'global',
  repo: 'repo',
  local: 'local',
};

/** One line about a config file: "missing", "deprecated .review/config.yaml",
 *  or the error that got it ignored; `null` when it's just in use. */
export function fileStatus(f: ConfigFile): string | null {
  if (f.error) return `ignored: ${f.error}`;
  if (!f.exists) return 'not present';
  if (f.legacy) return 'deprecated .review/config.yaml';
  return null;
}

/** Entries grouped by their first key segment (`spaces`, `review`, …), in
 *  first-seen order, each group sorted by key. */
export function groupEntries(
  entries: readonly ConfigEntry[],
): { section: string; entries: ConfigEntry[] }[] {
  const groups = new Map<string, ConfigEntry[]>();
  for (const e of entries) {
    const section = e.key.split('.')[0] ?? e.key;
    const list = groups.get(section) ?? [];
    list.push(e);
    groups.set(section, list);
  }
  return [...groups].map(([section, list]) => ({
    section,
    entries: [...list].sort((a, b) => a.key.localeCompare(b.key)),
  }));
}
