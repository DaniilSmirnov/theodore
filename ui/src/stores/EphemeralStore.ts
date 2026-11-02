/**
 * Everything in `Ephemeral` (agent token stream, skill run log lines, toasts,
 * host status) — never synced, kept only in memory per §3.2 of the plan.
 */
import {
  computed,
  makeObservable,
  observable,
  type ObservableMap,
  observableRef,
  observableShallow,
  runInAction,
} from 'mobx';
import type {
  AccountStatus,
  AgentId,
  Anchor,
  AppInfo,
  CommentId,
  Ephemeral,
  Person,
  PrId,
  SandboxInfo,
  Side,
  SpaceLogLine,
  ToastLevel,
  UpdateInfo,
} from '../gen/contract';
import { appendLogs, seedLogs } from '../ui/space/commands';
import { appendChunk, type TerminalChunk } from '../ui/space/terminalBuffer';
import { configPrToast } from '../ui/project/configFile';
import { suggestText } from '../ui/settings/languages';
import { api } from '../core/api';
import { openExternal } from '../core/openExternal';
import { accountGate, type AccountGate } from '../core/accountGate';

const LOG_LIMIT = 500;

export interface ToastAction {
  label: string;
  onAction: () => void;
}

export interface Toast {
  id: number;
  level: ToastLevel;
  text: string;
  /** e.g. "Undo" after a Done/Snooze/Archive-type action — never set by the
   *  `ephemeral` event from core, only by client code that pushes its own
   *  toast (a function can't cross the wire). */
  action?: ToastAction;
  /** A second choice next to `action` ("Not now"); either one closes the
   *  toast, which then stays up longer. */
  secondary?: ToastAction;
}

/** A question asked about one diff line, shown on the line itself while it
 *  runs and until its answer is opened or dismissed. */
export interface LineAsk {
  pr: PrId;
  anchor: Anchor;
  question: string;
  /** Who is (likely) answering: the `agents.chat` guess until the reply
   *  lands, then the reply's author. */
  agent: AgentId;
  status: 'asking' | 'answered' | 'failed';
  /** The agent the composer picked, sent as `ask`'s `agent` (again on
   *  Retry); `null` = core's default. */
  picked?: AgentId | null;
  /** `save_attachment` ids sent with the question (again on Retry). */
  attachments?: string[] | null;
  /** The question's chat message id, once `ask` returns it. */
  message: string | null;
  /** The reply's chat message id, once it lands. */
  reply: string | null;
  /** When it was asked (ms): the AI panel's Stop goes to the newest run. */
  startedAt?: number;
}

/** What the AI panel should bring into view: a chat message, or the
 *  bottom of the chat (`message: null`, a question not yet stored). `seq`
 *  makes asking for the same target twice still count. */
export interface AskFocus {
  pr: PrId;
  message: string | null;
  seq: number;
}

/** A question asked with no diff-line chip to pin it to — the composer's
 *  plain send, or a fixed question like a hunk's "Ask agent": the pending
 *  bubble shown in the chat (question, then the agent's streaming text)
 *  until the real reply lands. One per PR; a later ask replaces it. */
export interface ChatPending {
  run: string;
  startedAt: number;
  /** So a stalled run's Retry can resend the same question. */
  question: string;
  agent: AgentId | null;
}

/** A one-shot request to a component, per PR; `seq` makes asking twice
 *  still count. */
export interface PrSignal {
  pr: PrId;
  seq: number;
}

/** A file:line chip asked the diff to show this line. */
export interface LineJump extends PrSignal {
  anchor: Anchor;
}

export const lineAskKey = (
  pr: PrId,
  path: string,
  side: Side,
  line: number,
): string => `${pr}\u0000${path}\u0000${side}\u0000${line}`;

const groupViewKey = (pr: PrId, group: string): string => `${pr}\u0000${group}`;

export interface HostStatus {
  online: boolean;
  rateLimitedUntil: number | null;
  authOk: boolean;
}

export class EphemeralStore {
  /** Streaming text per run id, appended as `agent_delta` events arrive
   *  (a piece at a time while the agent writes). Observable per key: a
   *  view reading one run re-renders only when that run grows. */
  readonly agentText: ObservableMap<string, string> = observable.map(
    {},
    { deep: false },
  );
  /** Last lines of a run's log (its tool calls), per run id. */
  readonly runLog: ObservableMap<string, string[]> = observable.map(
    {},
    { deep: false },
  );
  /** The fix run of each task delegated this session, by task id: what
   *  its row shows live while the task is in progress. */
  readonly taskRuns: ObservableMap<string, string> = observable.map(
    {},
    { deep: false },
  );
  /** Run ids with no output for `agents.stall_after_ms` while still alive —
   *  the UI offers Retry/Cancel instead of waiting silently. Cleared as
   *  soon as the run produces output again, or ends. */
  stalled = new Set<string>();
  toasts: Toast[] = [];
  hostStatus: HostStatus = {
    online: true,
    rateLimitedUntil: null,
    authOk: true,
  };
  /** `refresh_inbox` in flight. */
  inboxLoading = false;
  /** Grouping runs in flight (and the last error) per PR id. */
  grouping = new Map<string, { running: boolean; error: string | null }>();
  /** The authenticated host user, cached once fetched via `viewer`. */
  viewer: Person | null = null;
  /** `viewer` fetch in flight — tells "loading" apart from "signed out". */
  viewerLoading = false;
  /** `app_info`, fetched once at startup; `null` until it arrives. */
  appInfo: AppInfo | null = null;
  /** The Theodore account, from `account_status` and its `account_status`
   *  event; `null` until first fetched. */
  account: AccountStatus | null = null;
  /** Optimistic overlay on `Comment.reactions`, `"<comment>:<emoji>"` ->
   *  the viewer-reacted state we just set. Wins over the server's
   *  `viewer_reacted`/`count` until the next sync brings a `Comment` whose
   *  server state already matches, at which point the caller clears it
   *  (`clearReactionOverride`) — it's then redundant, not a second source
   *  of truth. */
  reactionOverrides = new Map<string, boolean>();
  /** Suggestion comments queued for a batch commit, per PR. */
  suggestionBatch = new Map<PrId, Set<CommentId>>();
  /** A space's command output (setup / run / check / archive), per space
   *  id: live `space_log` events, seeded from `space_logs` when its page
   *  opens. Bounded (`SPACE_LOG_LIMIT`). */
  spaceLogs = new Map<string, SpaceLogLine[]>();
  /** A space terminal's live output since this session started, per
   *  terminal id (bounded) — a bridge over a brief gap (remounting the
   *  panel); `SpaceTerminalInfo.scrollback` covers everything older. */
  terminalChunks = new Map<string, TerminalChunk[]>();
  /** A terminal's exit, once it has happened since the page last read
   *  `list_space_terminals`. */
  terminalExit = new Map<
    string,
    { exitCode: number | null; error: string | null }
  >();
  /** Line asks by `lineAskKey` — an observable map, so a line reading its
   *  own key re-renders only when that key changes. */
  readonly lineAsks: ObservableMap<string, LineAsk> = observable.map(
    {},
    { deep: false },
  );
  askFocus: AskFocus | null = null;
  /** `ChatPending` by PR — the composer's and the hunk ask's shared pending
   *  bubble; an observable map so only a PR's own chat re-renders on it. */
  readonly chatPending: ObservableMap<PrId, ChatPending> = observable.map(
    {},
    { deep: false },
  );
  /** The diff line the PR chat's next question is about, per PR: a line's
   *  AI button sets it, the composer shows it as a chip and sends it as
   *  the question's `about`. One per PR — `ask` takes one anchor. */
  readonly lineRefs: ObservableMap<PrId, Anchor> = observable.map(
    {},
    { deep: false },
  );
  /** The PR chat's composer should take the caret (a line was just
   *  referenced); the composer clears it once it has. */
  composerFocus: PrSignal | null = null;
  /** The diff should show a line (a file:line chip was clicked); `Review`
   *  clears it once it has. */
  lineJump: LineJump | null = null;
  /** Groups whose diff the user opened past its default, by
   *  `groupViewKey`: a skim group's "Show all" (past "Only outliers") and a
   *  hidden group's "Show anyway". Kept here, not in `DiffPane`, so
   *  `Review`'s J/K steps through the same hunks the diff shows. */
  allEditsShown = new Set<string>();
  hiddenShown = new Set<string>();

  private nextToastId = 1;
  private nextFocusSeq = 1;

  constructor() {
    makeObservable(this, {
      stalled: observableShallow,
      toasts: observableShallow,
      hostStatus: observable,
      grouping: observableShallow,
      inboxLoading: observable,
      viewer: observableRef,
      viewerLoading: observable,
      appInfo: observableRef,
      forcedSyncUntil: observable,
      account: observableRef,
      accountGate: computed,
      sandbox: computed,
      reactionOverrides: observableShallow,
      suggestionBatch: observableShallow,
      spaceLogs: observableShallow,
      terminalChunks: observableShallow,
      terminalExit: observableShallow,
      askFocus: observableRef,
      composerFocus: observableRef,
      lineJump: observableRef,
      allEditsShown: observableShallow,
      hiddenShown: observableShallow,
    });
  }

  lineAskAt(
    pr: PrId,
    path: string,
    side: Side,
    line: number,
  ): LineAsk | undefined {
    return this.lineAsks.get(lineAskKey(pr, path, side, line));
  }

  setLineAsk(key: string, ask: LineAsk): void {
    runInAction(() => {
      this.lineAsks.set(key, ask);
    });
  }

  /** Merges into the ask at `key`, if it is still there. */
  patchLineAsk(key: string, patch: Partial<LineAsk>): void {
    const cur = this.lineAsks.get(key);
    if (!cur) return;
    runInAction(() => {
      this.lineAsks.set(key, { ...cur, ...patch });
    });
  }

  clearLineAsk(key: string): void {
    runInAction(() => {
      this.lineAsks.delete(key);
    });
  }

  /** Asks the AI panel to show `message` (or the chat's bottom). */
  focusAsk(pr: PrId, message: string | null): void {
    runInAction(() => {
      this.askFocus = { pr, message, seq: this.nextFocusSeq++ };
    });
  }

  /** The panel brought `seq` into view; a later mount won't again. */
  consumeAskFocus(seq: number): void {
    if (this.askFocus?.seq !== seq) return;
    runInAction(() => {
      this.askFocus = null;
    });
  }

  setChatPending(pr: PrId, p: ChatPending): void {
    runInAction(() => {
      this.chatPending.set(pr, p);
    });
  }

  clearChatPending(pr: PrId): void {
    runInAction(() => {
      this.chatPending.delete(pr);
    });
  }

  /** Makes `anchor` the PR chat's reference, replacing any other. */
  setLineRef(pr: PrId, anchor: Anchor): void {
    runInAction(() => {
      this.lineRefs.set(pr, anchor);
    });
  }

  clearLineRef(pr: PrId): void {
    runInAction(() => {
      this.lineRefs.delete(pr);
    });
  }

  /** Asks the PR chat's composer to take the caret. */
  focusComposer(pr: PrId): void {
    runInAction(() => {
      this.composerFocus = { pr, seq: this.nextFocusSeq++ };
    });
  }

  consumeComposerFocus(seq: number): void {
    if (this.composerFocus?.seq !== seq) return;
    runInAction(() => {
      this.composerFocus = null;
    });
  }

  /** Asks the diff to show `anchor`'s line. */
  jumpToLine(pr: PrId, anchor: Anchor): void {
    runInAction(() => {
      this.lineJump = { pr, anchor, seq: this.nextFocusSeq++ };
    });
  }

  consumeLineJump(seq: number): void {
    if (this.lineJump?.seq !== seq) return;
    runInAction(() => {
      this.lineJump = null;
    });
  }

  /** A group's diff toggles, in the shape `diffFiles` takes them; both
   *  off without a group. */
  groupView(
    pr: PrId,
    group: string | null | undefined,
  ): { allEdits: boolean; hiddenShown: boolean } {
    if (!group) return { allEdits: false, hiddenShown: false };
    const key = groupViewKey(pr, group);
    return {
      allEdits: this.allEditsShown.has(key),
      hiddenShown: this.hiddenShown.has(key),
    };
  }

  /** "Show all" / "Only outliers" on a skim group. */
  toggleAllEdits(pr: PrId, group: string): void {
    const key = groupViewKey(pr, group);
    runInAction(() => {
      const next = new Set(this.allEditsShown);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      this.allEditsShown = next;
    });
  }

  /** "Show anyway" on a hidden group. */
  showHidden(pr: PrId, group: string): void {
    const key = groupViewKey(pr, group);
    if (this.hiddenShown.has(key)) return;
    runInAction(() => {
      this.hiddenShown = new Set(this.hiddenShown).add(key);
    });
  }

  reactionOverride(comment: CommentId, emoji: string): boolean | undefined {
    return this.reactionOverrides.get(`${comment}:${emoji}`);
  }

  setReactionOverride(comment: CommentId, emoji: string, on: boolean): void {
    runInAction(() => {
      const next = new Map(this.reactionOverrides);
      next.set(`${comment}:${emoji}`, on);
      this.reactionOverrides = next;
    });
  }

  /** Drops the override once the server's own data agrees with it — kept
   *  around any longer it would just be stale local state shadowing real
   *  sync data. */
  clearReactionOverride(comment: CommentId, emoji: string): void {
    const key = `${comment}:${emoji}`;
    if (!this.reactionOverrides.has(key)) return;
    runInAction(() => {
      const next = new Map(this.reactionOverrides);
      next.delete(key);
      this.reactionOverrides = next;
    });
  }

  batchOf(pr: PrId): Set<CommentId> {
    return this.suggestionBatch.get(pr) ?? new Set();
  }

  toggleBatch(pr: PrId, comment: CommentId): void {
    runInAction(() => {
      const next = new Map(this.suggestionBatch);
      const set = new Set(next.get(pr) ?? []);
      if (set.has(comment)) set.delete(comment);
      else set.add(comment);
      next.set(pr, set);
      this.suggestionBatch = next;
    });
  }

  clearBatch(pr: PrId, comments?: CommentId[]): void {
    runInAction(() => {
      const next = new Map(this.suggestionBatch);
      if (!comments) {
        next.delete(pr);
      } else {
        const set = new Set(next.get(pr) ?? []);
        for (const c of comments) set.delete(c);
        next.set(pr, set);
      }
      this.suggestionBatch = next;
    });
  }

  setInboxLoading(v: boolean): void {
    runInAction(() => {
      this.inboxLoading = v;
    });
  }

  setViewerLoading(v: boolean): void {
    runInAction(() => {
      this.viewerLoading = v;
    });
  }

  setAppInfo(info: AppInfo): void {
    runInAction(() => {
      this.appInfo = info;
    });
    // The accent follows the build, like the logo (`styles.css`).
    document.documentElement.dataset.build = info.build_env;
  }

  /** An update the shell found (`downloaded: false`, about to download)
   *  or downloaded since launch — its `update` event. A found one never
   *  hides one already installed: the restart stays offered. */
  setUpdate(update: UpdateInfo): void {
    runInAction(() => {
      if (!this.appInfo) return;
      if (!update.downloaded && this.appInfo.update?.downloaded) return;
      this.appInfo = { ...this.appInfo, update };
    });
  }

  /** Sandbox only: the syncing look held on until this time (ms), to see
   *  it without a slow host (Settings → Sandbox → Show syncing). */
  forcedSyncUntil = 0;

  forceSyncing(ms: number): void {
    runInAction(() => {
      this.forcedSyncUntil = Date.now() + ms;
    });
    setTimeout(
      () =>
        runInAction(() => {
          if (Date.now() >= this.forcedSyncUntil) this.forcedSyncUntil = 0;
        }),
      ms,
    );
  }

  /** Installed on disk, waiting for a restart into it. */
  get readyUpdate(): UpdateInfo | null {
    const u = this.appInfo?.update;
    return u?.downloaded ? u : null;
  }

  /** Set only when this window runs against the sandbox host. */
  get sandbox(): SandboxInfo | null {
    return this.appInfo?.sandbox ?? null;
  }

  /** `locked` while signed out where signing in is required: only the
   *  sign-in shows, shortcuts and the menu are inert. */
  get accountGate(): AccountGate {
    return accountGate(this.account);
  }

  setAccount(s: AccountStatus): void {
    runInAction(() => {
      this.account = s;
    });
  }

  setViewer(p: Person): void {
    runInAction(() => {
      this.viewer = p;
    });
  }

  handle(e: Ephemeral): void {
    runInAction(() => {
      switch (e.type) {
        case 'agent_delta':
          this.agentText.set(e.run, (this.agentText.get(e.run) ?? '') + e.text);
          this.clearStalled(e.run);
          return;
        case 'run_log': {
          const lines = [...(this.runLog.get(e.run) ?? []), e.line];
          this.runLog.set(e.run, lines.slice(-LOG_LIMIT));
          this.clearStalled(e.run);
          return;
        }
        case 'run_stalled': {
          const next = new Set(this.stalled);
          next.add(e.run);
          this.stalled = next;
          return;
        }
        case 'toast':
          this.toasts = [
            ...this.toasts,
            { id: this.nextToastId++, level: e.level, text: e.text },
          ];
          return;
        case 'grouping': {
          const next = new Map(this.grouping);
          next.set(e.pr, { running: e.running, error: e.error });
          this.grouping = next;
          return;
        }
        case 'host_status':
          this.hostStatus = {
            online: e.online,
            rateLimitedUntil: e.rate_limited_until,
            authOk: e.auth_ok,
          };
          return;
        case 'account_status':
          this.account = e.status;
          return;
        case 'space_log': {
          const next = new Map(this.spaceLogs);
          next.set(e.space, appendLogs(next.get(e.space) ?? [], [e.line]));
          this.spaceLogs = next;
          return;
        }
        case 'terminal_output': {
          const next = new Map(this.terminalChunks);
          next.set(
            e.terminal,
            appendChunk(next.get(e.terminal) ?? [], {
              seq: e.seq,
              data: e.data,
            }),
          );
          this.terminalChunks = next;
          return;
        }
        case 'terminal_exit': {
          const next = new Map(this.terminalExit);
          next.set(e.terminal, { exitCode: e.exit_code, error: e.error });
          this.terminalExit = next;
          return;
        }
        case 'config_pr': {
          const toast = configPrToast(e.number, e.url, openExternal);
          this.toasts = [
            ...this.toasts,
            { id: this.nextToastId++, level: 'info', ...toast },
          ];
          return;
        }
        case 'code_intel_suggest': {
          // The core asks once per language per session (§5.1).
          const { language } = e;
          this.toasts = [
            ...this.toasts,
            {
              id: this.nextToastId++,
              level: 'info',
              text: suggestText(language, e.server),
              action: {
                label: 'Download',
                onAction: () =>
                  void api.codeIntelInstall(language).catch(() => {
                    // toasted by api.ts
                  }),
              },
              secondary: {
                label: 'Not now',
                onAction: () =>
                  void api.codeIntelDecline(language).catch(() => {
                    // toasted by api.ts
                  }),
              },
            },
          ];
          return;
        }
      }
    });
  }

  /** The core's tail (`space_logs`) for a space, merged with the live
   *  lines that arrived while it was fetched. */
  seedSpaceLogs(space: string, lines: SpaceLogLine[]): void {
    runInAction(() => {
      const next = new Map(this.spaceLogs);
      next.set(space, seedLogs(lines, next.get(space) ?? []));
      this.spaceLogs = next;
    });
  }

  /** Pushes a toast from client code (as opposed to `handle()`'s server
   *  `toast` events) — the only path that can attach an `action`. */
  toast(text: string, level: ToastLevel = 'info', action?: ToastAction): void {
    runInAction(() => {
      this.toasts = [
        ...this.toasts,
        { id: this.nextToastId++, level, text, action },
      ];
    });
  }

  dismissToast(id: number): void {
    runInAction(() => {
      this.toasts = this.toasts.filter((t) => t.id !== id);
    });
  }

  /** A run is done (or cancelled/failed): its streaming text and log no longer grow. */
  clearRun(run: string): void {
    runInAction(() => {
      this.agentText.delete(run);
      this.runLog.delete(run);
      this.clearStalled(run);
    });
  }

  /** A terminal tab is closed: its buffered output and exit no longer
   *  matter (a new tab never reuses an id). */
  clearTerminal(terminal: string): void {
    runInAction(() => {
      this.terminalChunks.delete(terminal);
      this.terminalExit.delete(terminal);
    });
  }

  /** `task` is being fixed by `run`; what an earlier fix of it showed is
   *  dropped. */
  setTaskRun(task: string, run: string): void {
    runInAction(() => {
      const prev = this.taskRuns.get(task);
      if (prev && prev !== run) this.clearRun(prev);
      this.taskRuns.set(task, run);
    });
  }

  private clearStalled(run: string): void {
    if (!this.stalled.has(run)) return;
    const next = new Set(this.stalled);
    next.delete(run);
    this.stalled = next;
  }
}
