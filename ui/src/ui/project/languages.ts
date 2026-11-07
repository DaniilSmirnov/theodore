/**
 * Project settings → Languages (docs/code-intel.md §5.1, §9): each language
 * of one project with its grammar, server and index progress, the
 * project's two "language servers in …" switches, Install / Remove /
 * Decline, and the install suggestion's toast text.
 *
 * ```text
 * start ─▶ load ─┬─ code_intel_progress ─▶ the row's bar ─▶ installed | failed ─▶ load
 *                ├─ warming (or downloading, no event yet) ─▶ wait POLL_MS ─▶ load …
 *                └─ idle: no timer until an action or an event
 * ```
 */
import { makeObservable, observableRef, reaction, runInAction } from 'mobx';
import type {
  CodeInstall,
  CodeInstallItem,
  CodeInstallPolicy,
  CodeInstallProgress,
  CodeIntelStatus,
  CodeLanguage,
  CodeLsp,
  CodeLspScope,
  CodeLspToggle,
  CodeServerState,
  Ephemeral,
} from '../../gen/contract';
import { api } from '../../core/api';
import { latestOnly } from '../sandbox/sandbox';

/** How often the section re-reads the status while a server warms up (or
 *  a download shows no progress events): a fallback, events do the rest. */
export const POLL_MS = 5000;

const NAMES: Record<string, string> = {
  rust: 'Rust',
  typescript: 'TypeScript',
  tsx: 'TSX',
  javascript: 'JavaScript',
  python: 'Python',
  go: 'Go',
  c: 'C',
  cpp: 'C++',
  swift: 'Swift',
  json: 'JSON',
  yaml: 'YAML',
  toml: 'TOML',
  markdown: 'Markdown',
  dockerfile: 'Dockerfile',
  bash: 'Bash',
  zig: 'Zig',
  lua: 'Lua',
  terraform: 'Terraform',
  php: 'PHP',
  ruby: 'Ruby',
  java: 'Java',
  kotlin: 'Kotlin',
  csharp: 'C#',
  scala: 'Scala',
  elixir: 'Elixir',
  dart: 'Dart',
  vue: 'Vue',
  svelte: 'Svelte',
  html: 'HTML',
  css: 'CSS',
  scss: 'SCSS',
  sql: 'SQL',
  'objective-c': 'Objective-C',
  perl: 'Perl',
  haskell: 'Haskell',
  ocaml: 'OCaml',
  clojure: 'Clojure',
  erlang: 'Erlang',
  r: 'R',
  julia: 'Julia',
  groovy: 'Groovy',
  powershell: 'PowerShell',
  solidity: 'Solidity',
};

/** A manifest id (`cpp`) as people write it (`C++`). */
export function languageName(id: string): string {
  return NAMES[id] ?? (id ? id[0]!.toUpperCase() + id.slice(1) : id);
}

const RUNTIME_NAMES: Record<string, string> = {
  node: 'Node.js',
  java: 'Java',
  ruby: 'Ruby',
  php: 'PHP',
  dotnet: '.NET',
  'dotnet-sdk': '.NET SDK',
};

/** A manifest runtime id (`dotnet-sdk`) as people write it (`.NET SDK`). */
export function runtimeName(id: string): string {
  return RUNTIME_NAMES[id] ?? id;
}

export type InstallTone =
  'default' | 'accent' | 'success' | 'warning' | 'danger';

export interface InstallView {
  label: string;
  tone: InstallTone;
  /** A path, why it failed, what was found, or how to get it. */
  detail: string | null;
  downloading: boolean;
}

/** `1.2.3` → `v1.2.3`; a date-like version (`2026-10-26`) stays as is. */
function versionLabel(v: string): string {
  return /^\d+\.\d/.test(v) ? `v${v}` : v;
}

/** `hint`: how to get a server Theodore can't download (`gem install
 *  ruby-lsp`); without one, "no download" is all there is to say. */
export function installView(
  i: CodeInstall,
  hint: string | null = null,
): InstallView {
  switch (i.state) {
    case 'system':
      return {
        label: i.version ? `System ${versionLabel(i.version)}` : 'System',
        tone: 'success',
        detail: i.path,
        downloading: false,
      };
    case 'installed':
      return {
        label: `Installed ${versionLabel(i.version)}`,
        tone: 'success',
        detail: null,
        downloading: false,
      };
    case 'missing':
      return {
        label: 'Missing',
        tone: 'warning',
        detail: null,
        downloading: false,
      };
    case 'downloading':
      return {
        label: 'Downloading',
        tone: 'accent',
        detail: null,
        downloading: true,
      };
    case 'unavailable':
      return {
        label: 'Unavailable',
        tone: 'default',
        detail: hint ?? 'No download for this Mac',
        downloading: false,
      };
    case 'needs_runtime':
      return {
        label: `Needs ${runtimeName(i.runtime)} ${i.min}+`,
        tone: 'warning',
        detail: i.found ? `Found ${i.found}` : null,
        downloading: false,
      };
    case 'trusted_only':
      return {
        label: 'Spaces only',
        tone: 'default',
        detail: 'Runs the project’s build, so it does not start for reviews',
        downloading: false,
      };
    case 'declined':
      return {
        label: 'Declined',
        tone: 'default',
        detail: null,
        downloading: false,
      };
    case 'failed':
      return {
        label: 'Failed',
        tone: 'danger',
        detail: i.error,
        downloading: false,
      };
  }
}

const SERVER_STATE: Record<
  CodeServerState,
  { label: string; tone: InstallTone }
> = {
  off: { label: 'Off', tone: 'default' },
  not_running: { label: 'Not running', tone: 'default' },
  warming: { label: 'Warming up', tone: 'accent' },
  ready: { label: 'Ready', tone: 'success' },
  failed: { label: 'Failed', tone: 'danger' },
};

/** The run chip. `off` names the switch that keeps the server off: with no
 *  space open, the worktree read is the project's checkout (or a PR's),
 *  served like a review, so the spaces switch alone doesn't start it. */
export function runView(
  state: CodeServerState,
  lsp: CodeLsp,
): { label: string; tone: InstallTone; title: string | null } {
  if (state === 'off' && lsp.review.on !== lsp.spaces.on) {
    return lsp.spaces.on
      ? {
          label: 'Off for reviews',
          tone: 'default',
          title: 'Language servers start in this project’s spaces',
        }
      : { label: 'Off in spaces', tone: 'default', title: null };
  }
  return { ...SERVER_STATE[state], title: null };
}

/** Something that `code_intel_install` could still fetch. */
const FETCHABLE: CodeInstall['state'][] = ['missing', 'declined', 'failed'];

export function canInstall(l: CodeLanguage): boolean {
  return (
    FETCHABLE.includes(l.grammar.state) ||
    (!!l.server && FETCHABLE.includes(l.server.install.state))
  );
}

/** Decline is the "No" to a server not on disk yet. */
export function canDecline(l: CodeLanguage): boolean {
  return l.server?.install.state === 'missing';
}

/** Remove deletes what Theodore downloaded; a server found on the machine
 *  isn't ours (docs/code-intel.md §5.1.1). */
export function canRemove(l: CodeLanguage): boolean {
  return (
    l.grammar.state === 'installed' || l.server?.install.state === 'installed'
  );
}

/** What Remove deletes, for its confirmation. */
export function removeText(l: CodeLanguage): string {
  const what = [
    l.server?.install.state === 'installed' ? l.server.name : null,
    l.grammar.state === 'installed' ? 'its grammar' : null,
  ].filter((w): w is string => w !== null);
  return `Deletes ${what.join(' and ')} from this Mac, for every project. ${languageName(l.language)} falls back to text search until you install it again.`;
}

// ── install progress (`code_intel_progress`) ─────────────────────────────

export type ProgressEvent = Extract<Ephemeral, { type: 'code_intel_progress' }>;

export interface ItemProgress {
  /** `tree-sitter-rust`, `rust-analyzer`. */
  name: string;
  state: CodeInstallProgress;
}

/** A language's grammar and server, each at its last event. */
export type LanguageProgress = Partial<Record<CodeInstallItem, ItemProgress>>;

/** Language → its last progress events. A new map on every event. */
export type InstallProgressMap = ReadonlyMap<string, LanguageProgress>;

/** Where the section reads install progress: `EphemeralStore`. */
export interface ProgressSource {
  readonly installProgress: InstallProgressMap;
}

/** `prev` with one event applied (a new map, for MobX). */
export function withProgress(
  prev: InstallProgressMap,
  e: ProgressEvent,
): Map<string, LanguageProgress> {
  const next = new Map(prev);
  next.set(e.language, {
    ...prev.get(e.language),
    [e.what]: { name: e.name, state: e.state },
  });
  return next;
}

/** An item reached `installed` or `failed` between `prev` and `next`. */
export function settled(
  next: InstallProgressMap,
  prev: InstallProgressMap,
): boolean {
  for (const [language, items] of next) {
    for (const what of ['grammar', 'server'] as const) {
      const item = items[what];
      if (
        item &&
        item.state.kind !== 'downloading' &&
        prev.get(language)?.[what] !== item
      )
        return true;
    }
  }
  return false;
}

export interface Download {
  name: string;
  received: number;
  total: number | null;
}

/** The item of `language` downloading now, the server first. */
export function downloadOf(p: LanguageProgress | undefined): Download | null {
  for (const what of ['server', 'grammar'] as const) {
    const s = p?.[what]?.state;
    if (s?.kind === 'downloading')
      return { name: p![what]!.name, received: s.received, total: s.total };
  }
  return null;
}

/** Bytes as MB, one decimal at most: `12.3`, `40`. */
function mb(bytes: number): string {
  return String(Math.round(bytes / 100_000) / 10);
}

/** `12.3 of 40 MB`, or `12.3 MB` when the size is unknown. */
export function downloadText(d: Download): string {
  return d.total
    ? `${mb(d.received)} of ${mb(d.total)} MB`
    : `${mb(d.received)} MB`;
}

/** 0–100, `null` when the size is unknown. */
export function downloadPercent(d: Download): number | null {
  if (!d.total) return null;
  return Math.min(100, Math.round((d.received / d.total) * 100));
}

export function isDownloading(l: CodeLanguage, p?: LanguageProgress): boolean {
  return (
    downloadOf(p) !== null ||
    l.grammar.state === 'downloading' ||
    l.server?.install.state === 'downloading'
  );
}

/** Worth reading again in `POLL_MS`: a server warming up, or a download
 *  the status shows but no event has reached yet. */
export function needsPoll(
  s: CodeIntelStatus,
  progress: InstallProgressMap = new Map(),
): boolean {
  return s.languages.some(
    (l) =>
      l.server?.state === 'warming' ||
      ((l.grammar.state === 'downloading' ||
        l.server?.install.state === 'downloading') &&
        !progress.has(l.language)),
  );
}

// ── texts ─────────────────────────────────────────────────────────────────

/** "Set in ~/.theodore/config.yaml" when a config file sets the value. */
export function lspNote(t: CodeLspToggle): string | null {
  return t.from_config ? `Set in ${t.from_config}` : null;
}

export const INSTALL_POLICY_TEXT: Record<CodeInstallPolicy, string> = {
  ask: 'Grammars download on their own; a server downloads once you say yes.',
  always: 'Grammars and servers download on their own.',
  never: 'Only servers already on this Mac are used.',
};

/** The install suggestion's line (docs/code-intel.md §5.1). */
export function suggestText(language: string, server: string): string {
  return `${languageName(language)}: download ${server} for exact references?`;
}

/** One suggestion for several languages at once. */
export function suggestManyText(languages: string[]): string {
  const names = languages.map(languageName);
  const last = names.pop();
  const list = names.length > 0 ? `${names.join(', ')} and ${last}` : last;
  return `${list}: download language servers for exact references?`;
}

// ── the model ─────────────────────────────────────────────────────────────

export interface Timers {
  set(fn: () => void, ms: number): unknown;
  clear(handle: unknown): void;
}

const realTimers: Timers = {
  set: (fn, ms) => setTimeout(fn, ms),
  clear: (h) => clearTimeout(h as ReturnType<typeof setTimeout>),
};

function messageOf(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export interface LanguagesOptions {
  /** Install progress events; without one, only the status is read. */
  progress?: ProgressSource;
  timers?: Timers;
  pollMs?: number;
  /** A switch wrote the project's local config file. */
  onConfigWritten?: () => void;
}

const NO_PROGRESS: InstallProgressMap = new Map();

/** One project's Languages section: its status, refreshed when an install
 *  settles (and polled while a server warms up), and the section's
 *  actions. */
export class LanguagesModel {
  status: CodeIntelStatus | null = null;
  /** Why the last read failed; the last good status stays shown. */
  error: string | null = null;
  /** Languages (and `lsp:<scope>`) with a command in flight. */
  pending: ReadonlySet<string> = new Set();

  private live = false;
  private timer: unknown = null;
  private unwatch: (() => void) | null = null;
  private readonly order = latestOnly();
  private readonly timers: Timers;
  private readonly pollMs: number;

  constructor(
    readonly project: string,
    private readonly opts: LanguagesOptions = {},
  ) {
    this.timers = opts.timers ?? realTimers;
    this.pollMs = opts.pollMs ?? POLL_MS;
    makeObservable<LanguagesModel>(this, {
      status: observableRef,
      error: observableRef,
      pending: observableRef,
    });
  }

  /** Install progress per language (empty without a source). */
  get progress(): InstallProgressMap {
    return this.opts.progress?.installProgress ?? NO_PROGRESS;
  }

  /** A poll timer is armed. */
  get polling(): boolean {
    return this.timer !== null;
  }

  start(): void {
    this.live = true;
    const source = this.opts.progress;
    if (source && !this.unwatch) {
      this.unwatch = reaction(
        () => source.installProgress,
        (next, prev) => {
          if (settled(next, prev)) void this.load();
        },
      );
    }
    void this.load();
  }

  stop(): void {
    this.live = false;
    this.unwatch?.();
    this.unwatch = null;
    this.clearTimer();
  }

  async load(): Promise<void> {
    const ticket = this.order.next();
    let next: CodeIntelStatus;
    try {
      next = await api.codeIntelStatus(this.project);
    } catch (e) {
      if (!this.order.accept(ticket)) return;
      // Stop polling: the section shows why instead of retrying.
      runInAction(() => {
        this.error = messageOf(e);
      });
      this.clearTimer();
      return;
    }
    if (!this.order.accept(ticket)) return;
    runInAction(() => {
      this.status = next;
      this.error = null;
    });
    this.schedule();
  }

  /** Downloads `language`'s grammar and server. Reads the status right
   *  away, so "Downloading" shows before the first progress event. */
  async install(language: string): Promise<void> {
    await this.run(language, () => {
      const req = api.codeIntelInstall(language);
      void this.load();
      return req;
    });
  }

  async decline(language: string): Promise<void> {
    await this.run(language, () => api.codeIntelDecline(language));
  }

  /** Deletes what was downloaded for `language`. The core's refusal (a
   *  review or a space has it open), or `null` when removed. */
  async remove(language: string): Promise<string | null> {
    let refused: string | null = null;
    await this.run(language, () =>
      api.codeIntelRemove(language).catch((e: unknown) => {
        refused = messageOf(e);
      }),
    );
    return refused;
  }

  async setLsp(scope: CodeLspScope, on: boolean): Promise<void> {
    const s = this.status;
    if (s) {
      runInAction(() => {
        this.status = {
          ...s,
          lsp: { ...s.lsp, [scope]: { ...s.lsp[scope], on } },
        };
      });
    }
    const ok = await this.run(`lsp:${scope}`, () =>
      api.codeIntelSetLsp({ project: this.project, scope, on }),
    );
    if (ok) this.opts.onConfigWritten?.();
  }

  /** `fn`, then a fresh status. Whether `fn` succeeded. */
  private async run(key: string, fn: () => Promise<unknown>): Promise<boolean> {
    this.setPending(key, true);
    let ok = true;
    try {
      await fn();
    } catch {
      // toasted by api.ts; the reload below shows what stands
      ok = false;
    } finally {
      this.setPending(key, false);
    }
    await this.load();
    return ok;
  }

  private setPending(key: string, on: boolean): void {
    runInAction(() => {
      const next = new Set(this.pending);
      if (on) next.add(key);
      else next.delete(key);
      this.pending = next;
    });
  }

  private schedule(): void {
    this.clearTimer();
    if (!this.live || !this.status || !needsPoll(this.status, this.progress))
      return;
    this.timer = this.timers.set(() => {
      this.timer = null;
      void this.load();
    }, this.pollMs);
  }

  private clearTimer(): void {
    if (this.timer === null) return;
    this.timers.clear(this.timer);
    this.timer = null;
  }
}
