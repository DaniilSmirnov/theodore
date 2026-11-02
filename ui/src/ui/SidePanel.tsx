import { stripHtmlComments } from '../highlight';
import {
  useEffect,
  useRef,
  useState,
  type ReactNode,
  type RefObject,
} from 'react';
import {
  ArrowUp,
  Check,
  CircleCheck,
  CodeCommit,
  Comment as CommentIcon,
  Comments as CommentsIcon,
  FileCode,
  Layers,
  ListCheck,
  Pencil,
  Plus,
  Sparkles,
  Xmark,
} from '@gravity-ui/icons';
import {
  AvatarGroup,
  Button,
  Card,
  Chip,
  EmptyState,
  Header,
  InputGroup,
  ListBox,
  Tabs,
  TextField,
  Tooltip,
} from '@heroui/react';
import { observer } from 'mobx-react-lite';
import type {
  AgentId,
  Anchor,
  ChatMessage,
  Draft,
  Finding,
  PrId,
  SkillInfo,
  SkillRun,
  Thread,
} from '../gen/contract';
import { api } from '../core/api';
import {
  ask,
  askAboutLine,
  askPending,
  askRunId,
  cancelRun,
  commitSuggestions,
  dismissLineAsk,
  findingToDraft,
  findingToTask,
  ignoreFinding,
  jumpToLine,
  lineAskRunning,
  replyThread,
  resolveThread,
  runSkill,
} from '../actions';
import { useRootStore, useUi } from '../stores/context';
import type { LineAsk } from '../stores/EphemeralStore';
import { Md, rich } from '../highlight';
import { latinKey } from './keys';
import { useContextMenu, type ContextMenuItem } from './ContextMenu';
import { ago } from './List';
import { AgentMenu } from './AgentMenu';
import { matchSkills } from './SkillMenu';
import { useAttachments } from './attachments';
import {
  ChatEmpty,
  ChatLive,
  ChatLog,
  ChatMessage as ChatMessageView,
  UserBubble,
} from './ChatTurn';
import { Composer } from './space/Composer';
import { AgentTag, Avatar, Btn, Sev, Spinner } from './bits';
import { PatchView } from './Patch';
import { isAgentEnabled } from './Settings';
import { SkillsManagerModal } from './Skills';
import { TaskRow } from './Tasks';
import { SendToAgent } from './space/SendToAgent';
import { ToolCalls } from './ToolCalls';

export type SideTab = 'ai' | 'tasks' | 'threads';

const AGENTS: AgentId[] = ['claude', 'codex', 'gemini'];

const plural = (n: number, one: string, few: string, many: string) =>
  `${n} ${n === 1 ? one : n >= 2 && n <= 4 ? few : many}`;

/** A small accent/warning chip for a tab's count, hidden at zero. */
function CountChip({
  n,
  color = 'accent',
}: {
  n: number;
  color?: 'accent' | 'warning';
}) {
  if (n <= 0) return null;
  return (
    <Chip size="sm" variant="soft" color={color} className="ml-1">
      {n}
    </Chip>
  );
}

export const SidePanel = observer(function SidePanel({ pr }: { pr: PrId }) {
  const [tab, setTab] = useState<SideTab>('ai');
  const store = useRootStore();
  // A line ask wants the chat in view, or a referenced line its composer:
  // the AI tab takes over.
  const focusSeq =
    store.ephemeral.askFocus?.pr === pr ? store.ephemeral.askFocus.seq : null;
  const composerSeq =
    store.ephemeral.composerFocus?.pr === pr
      ? store.ephemeral.composerFocus.seq
      : null;
  useEffect(() => {
    if (focusSeq != null || composerSeq != null) setTab('ai');
  }, [focusSeq, composerSeq]);
  const open = store.models.findings
    .byPr(pr)
    .filter((f) => f.state === 'open' || f.state === 'maybe_fixed').length;
  const threads = store.models.prDetails.get(pr)?.threads ?? [];
  const unresolved = threads.filter((t) => !t.resolved).length;
  const tasksOpen = store.models.tasks
    .byPr(pr)
    .filter((t) => t.status !== 'done').length;
  return (
    <div className="side flex min-h-0 flex-col border-l border-separator bg-background">
      <Tabs
        data-plain
        selectedKey={tab}
        onSelectionChange={(k) => setTab(k as SideTab)}
        className="side-tabs flex min-h-0 flex-1 flex-col [&>[role=tabpanel]]:border-t [&>[role=tabpanel]]:border-separator"
      >
        <Tabs.ListContainer className="mx-4 my-3">
          <Tabs.List aria-label="Review panel" className="w-full">
            <Tabs.Tab id="ai" className="gap-1.5">
              <Sparkles className="size-4" />
              AI
              <CountChip n={open} />
              <Tabs.Indicator />
            </Tabs.Tab>
            <Tabs.Tab id="tasks">
              Tasks
              <CountChip n={tasksOpen} />
              <Tabs.Indicator />
            </Tabs.Tab>
            <Tabs.Tab id="threads">
              Threads
              <CountChip n={unresolved} color="warning" />
              <Tabs.Indicator />
            </Tabs.Tab>
          </Tabs.List>
        </Tabs.ListContainer>
        <Tabs.Panel id="ai" className="flex min-h-0 flex-1 flex-col p-0">
          <Chat pr={pr} />
        </Tabs.Panel>
        <Tabs.Panel id="tasks" className="flex min-h-0 flex-1 flex-col p-0">
          <PrTasks pr={pr} />
        </Tabs.Panel>
        <Tabs.Panel id="threads" className="flex min-h-0 flex-1 flex-col p-0">
          <Threads pr={pr} />
        </Tabs.Panel>
      </Tabs>
    </div>
  );
});

/** A line ask (asked from the diff) still running: its question with the
 *  file:line chip until the stored message shows above (`asked`), then the
 *  agent's live reply. Stop is the composer's; a stalled one offers Retry. */
const LineAskBubble = observer(function LineAskBubble({
  askKey,
  a,
  asked,
  follow,
}: {
  askKey: string;
  a: LineAsk;
  asked: boolean;
  follow: RefObject<HTMLDivElement | null>;
}) {
  const store = useRootStore();
  return (
    <>
      {!asked && (
        <UserBubble text={a.question} refs={<Ref a={a.anchor} pr={a.pr} />} />
      )}
      <ChatLive
        run={a.message ? askRunId(a.message) : null}
        agent={a.agent}
        badges={<Ref a={a.anchor} pr={a.pr} />}
        follow={follow}
        className="line-ask"
        data={{ 'data-line-ask': askKey }}
        onRetry={() => {
          dismissLineAsk(store, askKey);
          void askAboutLine(store, a);
        }}
      />
    </>
  );
});

/** `name.ts:12` (or `:10-14` for a range): the file name, not the path —
 *  the chip shares narrow rows; the full path is in the tooltip. */
function refLabel(a: Anchor): string {
  return `${a.path.slice(a.path.lastIndexOf('/') + 1)}:${lineRange(a)}`;
}

const lineRange = (a: Anchor): string =>
  a.start_line != null && a.start_line !== a.line
    ? `${a.start_line}-${a.line}`
    : `${a.line}`;

/** An anchor as a file:line chip. With `pr` (the chat's chips) a click
 *  shows the line in the diff; elsewhere it only orients. With `onRemove`
 *  it has a ×. Capped to its container's width — a long path truncates
 *  inside the chip instead of forcing the panel wider. */
function Ref({
  a,
  pr,
  onRemove,
}: {
  a: Anchor;
  pr?: PrId;
  onRemove?: () => void;
}) {
  const store = useRootStore();
  const label = refLabel(a);
  const body = (
    <>
      <FileCode className="size-3.5 shrink-0" />
      <span className="min-w-0 truncate">{label}</span>
      {a.side === 'old' && <span className="shrink-0 opacity-70">old</span>}
    </>
  );
  return (
    <span
      className={`ref inline-flex h-6 max-w-full items-center gap-1 rounded-full border border-separator ps-2.5 font-mono text-xs text-muted ${onRemove ? 'pe-1' : 'pe-2.5'}`}
      title={`${a.path}:${lineRange(a)}${a.side === 'old' ? ' (old side)' : ''}`}
    >
      {pr ? (
        <button
          type="button"
          className="ref-jump inline-flex min-w-0 items-center gap-1.5 hover:text-foreground"
          onClick={() => jumpToLine(store, pr, a)}
        >
          {body}
        </button>
      ) : (
        <span className="inline-flex min-w-0 items-center gap-1.5">{body}</span>
      )}
      {onRemove && (
        <button
          type="button"
          aria-label={`Remove ${label}`}
          className="ref-remove grid size-4 shrink-0 place-items-center rounded-full hover:bg-default hover:text-foreground"
          onClick={onRemove}
        >
          <Xmark className="size-3" />
        </button>
      )}
    </span>
  );
}

type FeedItem =
  | { kind: 'run'; at: number; run: SkillRun }
  | { kind: 'message'; at: number; message: ChatMessage };

/** Skill runs and chat messages in the order they happened. */
export function feedOf(
  runs: readonly SkillRun[],
  messages: readonly ChatMessage[],
): FeedItem[] {
  return [
    ...runs.map((run): FeedItem => ({ kind: 'run', at: run.started_at, run })),
    ...messages.map((message): FeedItem => ({
      kind: 'message',
      at: message.at,
      message,
    })),
  ].sort((a, b) => a.at - b.at);
}

const RANK = { high: 0, warn: 1, info: 2 } as const;

/** A skill run inside the chat: the live log, then its findings — with a
 *  severity filter and bulk actions so a long list turns into tasks or
 *  review comments in one go. A running one stops from the composer. */
const SkillRunView = observer(function SkillRunView({
  pr,
  run,
}: {
  pr: PrId;
  run: string;
}) {
  const store = useRootStore();
  const r = store.models.skillRuns.get(run);
  const log = store.ephemeral.runLog.get(run) ?? [];
  const liveText = store.ephemeral.agentText.get(run);
  const [sev, setSev] = useState<Finding['severity'] | 'all'>('all');
  const all = store.models.findings
    .byPr(pr)
    .filter((f) => f.skill === r?.skill && f.state !== 'ignored')
    .sort((a, b) => RANK[a.severity] - RANK[b.severity]);
  const openFindings = all.filter(
    (f) => f.state === 'open' || f.state === 'maybe_fixed',
  );
  const shown = all.filter((f) => sev === 'all' || f.severity === sev);
  const count = (s: Finding['severity']) =>
    all.filter((f) => f.severity === s).length;
  if (!r) return null;
  const done = r.status === 'done';
  const live = r.status === 'running';
  const other = AGENTS.find((a) => !r.agents.includes(a));
  return (
    <div className="skill flex flex-col gap-2.5">
      <div className="flex items-center gap-2 text-sm">
        <span className="min-w-0 flex-1">
          {r.status === 'cancelled' ? (
            <>
              <span className="font-mono">/{r.skill}</span> stopped
            </>
          ) : r.status === 'failed' || r.status === 'stalled' ? (
            <>
              <span className="font-mono">/{r.skill}</span>{' '}
              {r.status === 'failed' ? 'failed' : 'stalled'}
            </>
          ) : done ? (
            <>
              <span className="font-mono">/{r.skill}</span>:{' '}
              {all.length
                ? plural(all.length, 'finding', 'findings', 'findings')
                : 'nothing found'}
            </>
          ) : (
            <span className="inline-flex items-center gap-2">
              <Sparkles className="size-4 shrink-0 animate-pulse text-accent" />
              <span className="shimmer-text">
                Running /{r.skill} on {r.agents.join(' + ')}
              </span>
            </span>
          )}
        </span>
      </div>
      {live && liveText && (
        <div className="text-sm leading-relaxed">
          <Md text={liveText} />
        </div>
      )}
      {(r.status === 'failed' || r.status === 'stalled') && (
        <div className="run-error rounded-2xl border border-danger/40 bg-danger-soft px-3 py-2 text-xs text-danger">
          {r.error}
          <div className="mt-2 flex gap-1.5">
            <Button
              size="sm"
              variant="tertiary"
              onPress={() => void runSkill(store, pr, r.skill, r.agents)}
            >
              Restart
            </Button>
            <Button
              size="sm"
              variant="ghost"
              onPress={() =>
                void store.sync.queue.transact([
                  { op: 'archive', model: 'skill_run', id: r.id },
                ])
              }
            >
              Dismiss
            </Button>
            {other && (
              <Button
                size="sm"
                variant="ghost"
                onPress={() => void runSkill(store, pr, r.skill, [other])}
              >
                Only {other}
              </Button>
            )}
          </div>
        </div>
      )}
      {log.length > 0 && <ToolCalls tools={log} running={live} turn={run} />}
      {done && all.length > 0 && (
        <div
          role="toolbar"
          aria-label="Findings"
          className="flex flex-wrap items-center gap-1.5"
        >
          {(['all', 'high', 'warn', 'info'] as const)
            .filter((x) => x === 'all' || count(x))
            .map((x) => (
              <Btn
                key={x}
                tone={sev === x ? 'secondary' : 'tertiary'}
                onPress={() => setSev(x)}
              >
                {x === 'all' ? `All ${all.length}` : `${x} ${count(x)}`}
              </Btn>
            ))}
          {openFindings.length > 0 && (
            <>
              <Btn
                icon={<ListCheck />}
                onPress={() =>
                  openFindings.forEach((f) => findingToTask(store, pr, f))
                }
              >
                Add all to tasks
              </Btn>
              <Btn
                icon={<Plus />}
                onPress={() =>
                  openFindings.forEach((f) => findingToDraft(store, pr, f))
                }
              >
                Add all to review
              </Btn>
            </>
          )}
        </div>
      )}
      {shown.map((f) => (
        <FindingCard key={f.id} pr={pr} f={f} />
      ))}
      {hiddenFindingsNote(r) && (
        <span className="hidden-findings text-xs text-muted">
          {hiddenFindingsNote(r)}
        </span>
      )}
    </div>
  );
});

const FindingCard = observer(function FindingCard({
  pr,
  f,
}: {
  pr: PrId;
  f: Finding;
}) {
  const store = useRootStore();
  const findingMenu = useContextMenu((): ContextMenuItem[] => [
    {
      id: 'add-review',
      label: 'Add to review',
      isDisabled: f.state === 'drafted',
      onAction: () => findingToDraft(store, pr, f),
    },
    {
      id: 'add-tasks',
      label: 'Add to tasks',
      isDisabled: f.state === 'drafted',
      onAction: () => findingToTask(store, pr, f),
    },
    {
      id: 'propose-fix',
      label: 'Propose fix',
      isDisabled: f.fix?.status === 'generating',
      onAction: () =>
        void ask(store, {
          pr,
          question: `propose a fix for: ${f.title}`,
          about: f.anchor,
          group: null,
          agent: null,
        }).catch(() => {
          // toasted by api.ts
        }),
    },
    {
      id: 'ignore',
      label: 'Ignore',
      isDisabled: f.state === 'drafted',
      onAction: () => ignoreFinding(store, f.id),
    },
  ]);
  return (
    <Card
      className={`finding mt-1 gap-2 px-4 py-3 ${f.state === 'drafted' ? 'opacity-60' : ''}`}
      onContextMenu={findingMenu}
    >
      <div className="flex items-center gap-1.5">
        <Sev s={f.severity} />
        <b>{f.title}</b>
      </div>
      <div className="flex flex-wrap items-center gap-1">
        <Ref a={f.anchor} />
        {f.agents.map((a) => (
          <AgentTag key={a} id={a} />
        ))}
        {f.agents.length > 1 && (
          <span className="text-xs text-muted">
            {f.agents.length === 2 ? 'both found it' : 'all found it'}
          </span>
        )}
      </div>
      <div className="rich text-sm text-muted">{rich(f.body)}</div>
      {f.state === 'maybe_fixed' && (
        <div className="text-xs text-success">
          May already be fixed by a later commit — recheck
        </div>
      )}
      {f.fix?.status === 'generating' && (
        <div className="flex items-center gap-2 text-xs text-muted">
          <Spinner /> {f.agents[0]} is preparing a patch…
        </div>
      )}
      {f.fix?.patch && f.fix.status !== 'generating' && (
        <>
          <PatchView
            patch={f.fix.patch}
            status={
              f.fix.status === 'applied'
                ? 'applied'
                : f.fix.status === 'suggested'
                  ? 'in review as a suggestion'
                  : f.fix.status === 'rejected'
                    ? 'rejected'
                    : undefined
            }
          />
          {f.fix.status === 'ready' && (
            <div className="fix-actions flex flex-wrap gap-1">
              <Tooltip>
                <Btn tone="primary" icon={<Check />} isDisabled>
                  Apply
                </Btn>
                <Tooltip.Content>Not available yet</Tooltip.Content>
              </Tooltip>
              <Tooltip>
                <Btn tone="ghost" isDisabled>
                  Reject
                </Btn>
                <Tooltip.Content>Not available yet</Tooltip.Content>
              </Tooltip>
            </div>
          )}
        </>
      )}
      <div className="flex flex-wrap gap-1">
        {f.state === 'drafted' ? (
          <span className="text-xs text-muted">Already in review or tasks</span>
        ) : (
          <>
            <Btn
              icon={<ListCheck />}
              onPress={() => findingToDraft(store, pr, f)}
            >
              Add to review
            </Btn>
            <Btn
              tone="secondary"
              icon={<Plus />}
              onPress={() => findingToTask(store, pr, f)}
            >
              Add to tasks
            </Btn>
            <Btn
              tone="ghost"
              icon={<Sparkles />}
              onPress={() =>
                void ask(store, {
                  pr,
                  question: `${f.title}: is this right?`,
                  about: f.anchor,
                  group: null,
                  agent: null,
                }).catch(() => {})
              }
            >
              Discuss
            </Btn>
            <Btn tone="tertiary" onPress={() => ignoreFinding(store, f.id)}>
              Ignore
            </Btn>
            <SendToAgent pr={pr} item={{ kind: 'finding', id: f.id }} />
          </>
        )}
      </div>
    </Card>
  );
});

/** "2 findings hidden by config" for a run whose findings on
 *  `review.ignore_findings` paths were dropped; `null` when none were. */
export function hiddenFindingsNote(
  run: Pick<SkillRun, 'hidden_findings'>,
): string | null {
  const n = run.hidden_findings ?? 0;
  return n > 0
    ? `${plural(n, 'finding', 'findings', 'findings')} hidden by config`
    : null;
}

/** Finds the user's question right before an agent's reply, for "Ask again". */
export function priorQuestion(
  messages: ChatMessage[],
  reply: ChatMessage,
): string {
  const i = messages.findIndex((m) => m.id === reply.id);
  for (let j = i - 1; j >= 0; j--) {
    const m = messages[j]!;
    if (m.author.kind === 'me') return m.text;
  }
  return '';
}

/** The message id behind a chat question's run (`askRunId`'s inverse). */
const askMessageOf = (run: string) => run.replace(/^ask:/, '');

/** A run going in the chat, for the composer's Stop. */
interface LiveRun {
  at: number;
  stop: () => void;
}

/** The run the composer's Stop ends: the newest one still going. */
export function newestRun<T extends { at: number }>(runs: T[]): T | null {
  return runs.reduce<T | null>((a, b) => (!a || b.at >= a.at ? b : a), null);
}

const Chat = observer(function Chat({ pr }: { pr: PrId }) {
  const store = useRootStore();
  const [text, setText] = useState('');
  const files = useAttachments();
  const [skills, setSkills] = useState<SkillInfo[]>([]);
  const [skillAgents, setSkillAgents] = useState<AgentId[]>(AGENTS);
  const [installed, setInstalled] = useState<AgentId[] | null>(null);
  // Default to the agents actually installed on this machine, minus any the
  // user has disabled (plan "enable/disable agents": hidden from the chat
  // picker, skipped for skills).
  useEffect(() => {
    let live = true;
    void api
      .detectAgents()
      .then((all) => {
        const ids = all
          .filter((a) => a.installed && isAgentEnabled(store, a.id))
          .map((a) => a.id);
        if (!live) return;
        setInstalled(ids);
        if (ids.length > 0) setSkillAgents(ids);
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [store]);
  const [managing, setManaging] = useState(false);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  // The composer's plain send and the hunk header's "Ask agent" (DiffPane)
  // share this pending bubble via the store — `askPending` sets it,
  // watches for the reply, and clears it once the real message lands.
  const pending = store.ephemeral.chatPending.get(pr) ?? null;
  const end = useRef<HTMLDivElement>(null);
  const messages = [...store.models.chatMessages.byPr(pr)].sort(
    (a, b) => a.at - b.at,
  );
  const runs = store.models.skillRuns.byPr(pr);
  useEffect(() => {
    end.current?.scrollIntoView({ block: 'end' });
  }, [messages.length, runs.length, pending]);
  useEffect(() => {
    void api
      .listSkills(pr)
      .then(setSkills)
      .catch(() => setSkills([]));
  }, [pr]);
  // A line ask (DiffPane) wants its question or answer in view.
  const focus =
    store.ephemeral.askFocus?.pr === pr ? store.ephemeral.askFocus : null;
  useEffect(() => {
    if (!focus) return;
    const el = focus.message
      ? document.querySelector<HTMLElement>(
          `.chat [data-msg="${CSS.escape(focus.message)}"]`,
        )
      : null;
    if (el) {
      el.scrollIntoView({ block: 'center', behavior: 'smooth' });
      el.classList.remove('ask-flash');
      void el.offsetWidth; // restart the animation on a repeat focus
      el.classList.add('ask-flash');
    } else {
      end.current?.scrollIntoView({ block: 'end' });
    }
    // The question isn't synced yet: a later focus (with its id) follows.
    if (!focus.message || el) store.ephemeral.consumeAskFocus(focus.seq);
  }, [focus, store, messages.length]);
  const lineAsks = [...store.ephemeral.lineAsks.entries()].filter(
    ([, a]) => a.pr === pr && a.status === 'asking',
  );
  // The diff line the next question is about (a line's AI button), and its
  // ask for the caret.
  const lineRef = store.ephemeral.lineRefs.get(pr) ?? null;
  const composerFocus =
    store.ephemeral.composerFocus?.pr === pr
      ? store.ephemeral.composerFocus.seq
      : null;
  useEffect(() => {
    if (composerFocus == null) return;
    const el = inputRef.current;
    if (!el) return;
    el.focus();
    el.setSelectionRange(el.value.length, el.value.length);
    store.ephemeral.consumeComposerFocus(composerFocus);
  }, [composerFocus, store]);
  // Once an agent message lands after the ask, the pending bubble has
  // nothing left to show: hidden in that same render (never both at once
  // for a frame); `askPending`'s reaction then clears it from the store.
  const answered =
    pending != null &&
    messages.some((m) => m.author.kind !== 'me' && m.at >= pending.startedAt);
  const stored = (id: string | null) =>
    id != null && messages.some((m) => m.id === id);

  const stopPending = () => {
    if (!pending) return;
    void cancelRun(store, pending.run).catch(() => {});
    store.ephemeral.clearChatPending(pr);
  };
  // Stop in the composer ends the newest run still going here: the
  // pending question, a line's question, or a skill run.
  const current = newestRun<LiveRun>([
    ...(pending && !answered
      ? [{ at: pending.startedAt, stop: stopPending }]
      : []),
    ...lineAsks.map(([key, a]) => ({
      at: a.startedAt ?? 0,
      stop: () => dismissLineAsk(store, key),
    })),
    ...runs
      .filter((r) => r.status === 'running')
      .map((r) => ({
        at: r.started_at,
        stop: () => void cancelRun(store, r.id).catch(() => {}),
      })),
  ]);
  // ⌘. stops it too, as in a space — from anywhere while it runs.
  const stopRef = useRef<(() => void) | null>(null);
  useEffect(() => {
    stopRef.current = current?.stop ?? null;
  });
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey) || latinKey(e) !== '.') return;
      const stop = stopRef.current;
      if (!stop) return;
      e.preventDefault();
      stop();
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, []);

  const runMatch = (s: SkillInfo) => {
    void runSkill(store, pr, s.id, skillAgents);
    setText('');
  };

  const send = () => {
    const t = text.trim();
    if (!t || files.busy) return;
    if (t.startsWith('/')) {
      // ⏎ with the list open picks there; the arrow runs the typed name,
      // or the first skill it matches.
      const name = t.slice(1).split(' ')[0] ?? '';
      const skill =
        skills.find((s) => s.id === name) ?? matchSkills(skills, t)?.[0];
      void runSkill(store, pr, skill?.id ?? name, skillAgents);
    } else if (lineRef) {
      // About a line: its own status on the line, its own bubble here.
      if (lineAskRunning(store, pr, lineRef)) {
        // One question per line at a time: show the running one, keep this.
        void askAboutLine(store, { pr, anchor: lineRef, question: t });
        return;
      }
      void askAboutLine(store, {
        pr,
        anchor: lineRef,
        question: t,
        picked: skillAgents[0] ?? null,
        attachments: files.ids.length ? files.ids : null,
      });
      store.ephemeral.clearLineRef(pr);
      files.clear();
    } else {
      const agent = skillAgents[0] ?? null;
      void askPending(store, {
        pr,
        question: t,
        about: null,
        group: null,
        agent,
        attachments: files.ids.length ? files.ids : null,
      })
        .then(() => files.clear())
        .catch(() => {});
    }
    setText('');
  };

  const reAsk = (m: ChatMessage) => {
    const question = priorQuestion(messages, m);
    if (!question) return undefined;
    return () =>
      void ask(store, {
        pr,
        question,
        about: null,
        group: null,
        agent: m.author.kind === 'me' ? null : m.author.agent,
      }).catch(() => {});
  };

  return (
    <div className="chat flex min-h-0 flex-1 flex-col">
      {managing && (
        <SkillsManagerModal
          pr={pr}
          onClose={() => {
            setManaging(false);
            // A skill forked or added there shows up in the list right away.
            void api
              .listSkills(pr)
              .then(setSkills)
              .catch(() => {});
          }}
        />
      )}
      <ChatLog end={end}>
        {messages.length === 0 &&
        runs.length === 0 &&
        !pending &&
        lineAsks.length === 0 ? (
          <ChatEmpty
            icon={<Sparkles />}
            title="Ask about this PR"
            hint="The agent works in a worktree and sees the whole repo."
          >
            {skills.length > 0 && (
              <ListBox
                aria-label="Suggested skills"
                selectionMode="none"
                onAction={(k) => {
                  const s = skills.find((x) => x.id === String(k));
                  if (s) runMatch(s);
                }}
                className="flex w-full flex-col gap-1.5 p-0"
              >
                {skills.slice(0, 3).map((k) => (
                  <ListBox.Item
                    key={k.id}
                    id={k.id}
                    textValue={k.title}
                    className="w-full gap-2.5 rounded-2xl border border-separator bg-surface px-3 py-2.5 text-sm"
                  >
                    <span className="grid size-4 shrink-0 place-items-center font-mono text-muted">
                      /
                    </span>
                    <span className="shrink-0 font-mono">{k.id}</span>
                    <span
                      className="min-w-0 flex-1 truncate text-xs text-muted"
                      title={k.description}
                    >
                      {k.description}
                    </span>
                  </ListBox.Item>
                ))}
              </ListBox>
            )}
          </ChatEmpty>
        ) : (
          <>
            {/* Skill runs and chat as one conversation, oldest first. */}
            {feedOf(runs, messages).map((item) =>
              item.kind === 'run' ? (
                <SkillRunView key={item.run.id} pr={pr} run={item.run.id} />
              ) : (
                <ChatMessageView
                  key={item.message.id}
                  m={item.message}
                  refs={
                    item.message.refs.length > 0
                      ? item.message.refs.map((a, i) => (
                          <Ref key={i} a={a} pr={pr} />
                        ))
                      : undefined
                  }
                  onAskAgain={reAsk(item.message)}
                  onRetryFailed={reAsk(item.message)}
                />
              ),
            )}
            {pending && !answered && (
              <>
                {/* The question, until its stored message shows above. */}
                {!stored(askMessageOf(pending.run)) && (
                  <UserBubble text={pending.question} />
                )}
                <ChatLive
                  run={pending.run}
                  agent={pending.agent}
                  follow={end}
                  className="chat-pending"
                  onRetry={() => {
                    const { question, agent } = pending;
                    stopPending();
                    void askPending(store, {
                      pr,
                      question,
                      about: null,
                      group: null,
                      agent,
                    }).catch(() => {});
                  }}
                />
              </>
            )}
            {lineAsks.map(([key, a]) => (
              <LineAskBubble
                key={key}
                askKey={key}
                a={a}
                asked={stored(a.message)}
                follow={end}
              />
            ))}
          </>
        )}
      </ChatLog>

      <div className="chat-input flex flex-col gap-2 px-4 pb-3 pt-1">
        <Composer
          inputRef={inputRef}
          value={text}
          onChange={setText}
          onSubmit={send}
          label="Ask the agent"
          placeholder={
            lineRef
              ? `Ask about ${refLabel(lineRef)}`
              : 'Ask about the code, or / to run a skill'
          }
          requireText
          sendWhileRunning
          running={current != null}
          onStop={current?.stop}
          skills={skills}
          onPickSkill={runMatch}
          skillFooter={
            <div className="flex items-center gap-1 ps-3 pt-1 text-xs text-muted">
              <span className="min-w-0 truncate">
                Runs on {skillAgents.join(' + ')}
              </span>
              <Button
                size="sm"
                variant="ghost"
                className="manage-skills ms-auto shrink-0 gap-1.5 text-xs text-muted"
                onPress={() => setManaging(true)}
              >
                <Pencil className="size-3.5" />
                Manage skills…
              </Button>
            </div>
          }
          attachments={files}
          strip={
            lineRef && (
              <div className="line-ref flex w-full px-3 pb-1">
                <Ref
                  a={lineRef}
                  pr={pr}
                  onRemove={() => {
                    store.ephemeral.clearLineRef(pr);
                    inputRef.current?.focus();
                  }}
                />
              </div>
            )
          }
          // Backspace in an empty box takes the line chip back out.
          onEmptyBackspace={
            lineRef ? () => store.ephemeral.clearLineRef(pr) : undefined
          }
          chips={
            /* Only agents installed on this computer and not disabled in
               Settings → Agents are offered. */
            <AgentMenu
              multiple
              agents={(installed ?? AGENTS).filter((a) =>
                isAgentEnabled(store, a),
              )}
              selected={skillAgents}
              onSelect={setSkillAgents}
            />
          }
        />
        <div className="flex items-center gap-2 px-2 text-xs text-muted">
          <span className="min-w-0 truncate">The agent can be wrong</span>
        </div>
      </div>
    </div>
  );
});

const PrTasks = observer(function PrTasks({ pr }: { pr: PrId }) {
  const store = useRootStore();
  const ui = useUi();
  const tasks = store.models.tasks.byPr(pr);
  return (
    <ListBox
      aria-label="PR tasks"
      selectionMode="single"
      selectedKeys={ui.selected ? [ui.selected] : []}
      onSelectionChange={(keys) => {
        const k = keys === 'all' ? null : [...keys][0];
        if (k != null) ui.select(String(k));
      }}
      renderEmptyState={() => (
        <EmptyState className="rounded-2xl border border-dashed border-separator p-4 text-sm">
          No tasks. Create one from a skill finding or a thread. Once the review
          is submitted, each comment also becomes a task for the author.
        </EmptyState>
      )}
      className="min-h-0 flex-1 overflow-auto p-4"
    >
      {tasks.map((t) => (
        <ListBox.Item
          key={t.id}
          id={t.id}
          textValue={t.title}
          className="mb-2 rounded-2xl border border-separator bg-surface px-3"
        >
          <TaskRow t={t} compact sel={ui.selected === t.id} />
        </ListBox.Item>
      ))}
    </ListBox>
  );
});

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div className="flex flex-col gap-2">
      <Header className="px-1 pb-1 text-xs font-medium text-muted">
        {title}
      </Header>
      {children}
    </div>
  );
}

function DraftCard({ d }: { d: Draft }) {
  return (
    <Card className="titem flex flex-col gap-2 border border-dashed border-warning/50 px-4 py-3">
      <div className="flex items-center gap-2">
        <Chip size="sm" variant="soft" color="warning">
          pending
        </Chip>
        {d.source.kind === 'agent' && <AgentTag id={d.source.agent} />}
        <span className="flex-1" />
        <SendToAgent pr={d.pr} item={{ kind: 'draft', id: d.id }} />
        <Ref a={d.anchor} />
      </div>
      <p className="line-clamp-2 text-sm text-muted">
        {stripHtmlComments(d.body)}
      </p>
    </Card>
  );
}

function ThreadCardInner({ pr, thread }: { pr: PrId; thread: Thread }) {
  const store = useRootStore();
  const [reply, setReply] = useState('');
  const [busy, setBusy] = useState(false);
  const people = [
    ...new Map(thread.comments.map((c) => [c.author.login, c.author])),
  ];
  const last = thread.comments.at(-1)!;
  const send = () => {
    const body = reply.trim();
    if (!body) return;
    setBusy(true);
    void replyThread(store, pr, thread.id, body)
      .then(() => setReply(''))
      .finally(() => setBusy(false));
  };
  return (
    <Card
      className={`titem flex flex-col gap-2 border border-separator px-4 py-3 ${thread.resolved ? 'opacity-70' : ''}`}
    >
      <div className="flex items-center gap-2">
        {thread.resolved ? (
          <CircleCheck className="size-4 text-success" />
        ) : (
          <CommentIcon className="size-4 text-warning" />
        )}
        <AvatarGroup>
          {people.map(([login, person]) => (
            <Avatar key={login} person={person} size={20} />
          ))}
        </AvatarGroup>
        <span className="min-w-12 max-w-[50%] shrink-0 truncate text-sm font-medium">
          {people.map(([login]) => login).join(', ')}
        </span>
        <span className="flex-1" />
        {thread.anchor && (
          <span className="flex min-w-0 shrink justify-end">
            <Ref a={thread.anchor} />
          </span>
        )}
      </div>
      <p className="line-clamp-2 text-sm text-muted">
        {/* Name the speaker only when more than one person is in the thread;
         *  the header already shows a lone author. */}
        {people.length > 1 && (
          <>
            <span className="text-foreground">{last.author.login}:</span>{' '}
          </>
        )}
        {stripHtmlComments(last.body)}
      </p>
      <div className="flex items-center justify-between text-xs text-muted">
        <span>
          {plural(thread.comments.length, 'comment', 'comments', 'comments')} ·{' '}
          {ago(last.created_at)}
        </span>
        {thread.outdated && <span>outdated</span>}
      </div>
      {thread.resolved ? (
        <Btn
          tone="ghost"
          className="w-fit"
          onPress={() => void resolveThread(store, pr, thread.id, false)}
        >
          Reopen
        </Btn>
      ) : (
        <div className="flex flex-wrap items-center gap-1.5">
          <TextField
            aria-label="Reply"
            value={reply}
            onChange={setReply}
            className="min-w-0 flex-1 basis-40"
          >
            <InputGroup fullWidth className="rounded-2xl">
              <InputGroup.Input
                placeholder="Reply…"
                onKeyDown={(e) => {
                  if (e.key === 'Enter' && !e.shiftKey) {
                    e.preventDefault();
                    send();
                  }
                }}
              />
            </InputGroup>
          </TextField>
          <Button
            isIconOnly
            size="sm"
            aria-label="Send reply"
            isPending={busy}
            isDisabled={!reply.trim()}
            onPress={send}
          >
            <ArrowUp className="size-4" />
          </Button>
          <SendToAgent pr={pr} item={{ kind: 'thread', id: thread.id }} />
          <Btn
            tone="tertiary"
            icon={<Check />}
            onPress={() => void resolveThread(store, pr, thread.id, true)}
          >
            Resolve
          </Btn>
        </div>
      )}
    </Card>
  );
}

/** Suggestions queued with "Add to batch" (`Comments.tsx`'s `CommentView`),
 *  committed together as one push — GitHub's batch-suggestion flow. */
const SuggestionBatchBar = observer(function SuggestionBatchBar({
  pr,
}: {
  pr: PrId;
}) {
  const store = useRootStore();
  const [busy, setBusy] = useState(false);
  const batch = [...store.ephemeral.batchOf(pr)];
  if (!batch.length) return null;
  return (
    <div className="batch-bar sticky bottom-0 z-[3] flex items-center gap-3 rounded-2xl border border-accent/40 bg-surface px-4 py-2 text-sm shadow-lg">
      <Layers className="size-4 text-accent" />
      <span className="min-w-0 flex-1">
        {plural(
          batch.length,
          'batched suggestion',
          'batched suggestions',
          'batched suggestions',
        )}{' '}
        — will go out as one commit
      </span>
      <Button
        size="sm"
        variant="ghost"
        onPress={() => store.ephemeral.clearBatch(pr)}
      >
        Clear
      </Button>
      <Btn
        tone="primary"
        icon={<CodeCommit />}
        className="commit-batch"
        isPending={busy}
        onPress={() => {
          setBusy(true);
          void commitSuggestions(store, pr, batch).finally(() =>
            setBusy(false),
          );
        }}
      >
        Commit batch
      </Btn>
    </div>
  );
});

/** All threads and pending drafts of this PR, grouped like an inbox. */
const Threads = observer(function Threads({ pr }: { pr: PrId }) {
  const store = useRootStore();
  const threads = store.models.prDetails.get(pr)?.threads ?? [];
  const drafts = store.models.drafts.byPr(pr);
  const open = threads.filter((t) => !t.resolved);
  const resolved = threads.filter((t) => t.resolved);

  if (!open.length && !drafts.length && !resolved.length) {
    return (
      <EmptyState className="flex flex-col items-center gap-2 p-8 text-center text-sm">
        <CommentsIcon className="size-6 text-muted" />
        No threads yet. Start one from a diff line.
      </EmptyState>
    );
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-auto p-4">
      {open.length > 0 && (
        <Section title={`Open · ${open.length}`}>
          {open.map((t) => (
            <ThreadCard key={t.id} pr={pr} thread={t} />
          ))}
        </Section>
      )}
      {drafts.length > 0 && (
        <Section title={`Drafts · ${drafts.length}`}>
          {drafts.map((d) => (
            <DraftCard key={d.id} d={d} />
          ))}
        </Section>
      )}
      {resolved.length > 0 && (
        <Section title={`Resolved · ${resolved.length}`}>
          {resolved.map((t) => (
            <ThreadCard key={t.id} pr={pr} thread={t} />
          ))}
        </Section>
      )}
      <SuggestionBatchBar pr={pr} />
    </div>
  );
});

const ThreadCard = observer(ThreadCardInner);
