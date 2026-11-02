/**
 * Settings → Languages (docs/code-intel.md §5.1, §9): each language of a
 * project with its grammar, server and index progress, the two "language
 * servers in …" switches, and the install suggestion's toast.
 *
 * ```text
 * start ─▶ load ─▶ downloading / warming? ──yes──▶ wait POLL_MS ─▶ load …
 *                               └──no──▶ idle: no timer until an action
 * ```
 */
import { makeObservable, observableRef, runInAction } from 'mobx';
import type {
  CodeInstall,
  CodeInstallPolicy,
  CodeIntelStatus,
  CodeLanguage,
  CodeLspScope,
  CodeLspToggle,
  CodeServerState,
} from '../../gen/contract';
import { api } from '../../core/api';
import { latestOnly } from '../sandbox/sandbox';

/** How often the page re-reads the status while something moves. */
export const POLL_MS = 2000;

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
};

/** A manifest id (`cpp`) as people write it (`C++`). */
export function languageName(id: string): string {
  return NAMES[id] ?? (id ? id[0]!.toUpperCase() + id.slice(1) : id);
}

export type InstallTone =
  'default' | 'accent' | 'success' | 'warning' | 'danger';

export interface InstallView {
  label: string;
  tone: InstallTone;
  /** A path, or why it failed. */
  detail: string | null;
  downloading: boolean;
}

/** `1.2.3` → `v1.2.3`; a date-like version (`2026-10-26`) stays as is. */
function versionLabel(v: string): string {
  return /^\d+\.\d/.test(v) ? `v${v}` : v;
}

export function installView(i: CodeInstall): InstallView {
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
        detail: 'No download for this Mac',
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

export const SERVER_STATE: Record<
  CodeServerState,
  { label: string; tone: InstallTone }
> = {
  not_running: { label: 'Not running', tone: 'default' },
  warming: { label: 'Warming up', tone: 'accent' },
  ready: { label: 'Ready', tone: 'success' },
  failed: { label: 'Failed', tone: 'danger' },
};

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

export function isDownloading(l: CodeLanguage): boolean {
  return (
    l.grammar.state === 'downloading' ||
    l.server?.install.state === 'downloading'
  );
}

/** Downloading or warming: worth reading again in `POLL_MS`. */
export function needsPoll(s: CodeIntelStatus): boolean {
  return s.languages.some(
    (l) => isDownloading(l) || l.server?.state === 'warming',
  );
}

/** "set in ~/.config/theodore/config.yaml" when a file fixes the value. */
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

/** One project's Settings → Languages: its status, polled while a
 *  download or a server start is under way, and the page's actions. */
export class LanguagesModel {
  status: CodeIntelStatus | null = null;
  /** Why the last read failed; the last good status stays shown. */
  error: string | null = null;
  /** Languages (and `lsp:<scope>`) with a command in flight. */
  pending: ReadonlySet<string> = new Set();

  private live = false;
  private timer: unknown = null;
  private readonly order = latestOnly();

  constructor(
    readonly project: string,
    private readonly timers: Timers = realTimers,
    private readonly pollMs = POLL_MS,
  ) {
    makeObservable<LanguagesModel>(this, {
      status: observableRef,
      error: observableRef,
      pending: observableRef,
    });
  }

  /** A poll timer is armed. */
  get polling(): boolean {
    return this.timer !== null;
  }

  start(): void {
    this.live = true;
    void this.load();
  }

  stop(): void {
    this.live = false;
    this.clearTimer();
  }

  async load(): Promise<void> {
    const ticket = this.order.next();
    let next: CodeIntelStatus;
    try {
      next = await api.codeIntelStatus(this.project);
    } catch (e) {
      if (!this.order.accept(ticket)) return;
      // Stop polling: the page shows why instead of retrying every 2 s.
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
   *  away, so "Downloading" (and its polling) shows while it runs. */
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
    await this.run(`lsp:${scope}`, () => api.codeIntelSetLsp({ scope, on }));
  }

  private async run(key: string, fn: () => Promise<unknown>): Promise<void> {
    this.setPending(key, true);
    try {
      await fn();
    } catch {
      // toasted by api.ts; the reload below shows what stands
    } finally {
      this.setPending(key, false);
    }
    await this.load();
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
    if (!this.live || !this.status || !needsPoll(this.status)) return;
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
