import { stripHtmlComments } from '../highlight';
import {
  memo,
  useCallback,
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
  chatAgentGuess,
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
import type { ChatPending, LineAsk } from '../stores/EphemeralStore';
import type { RootStore } from '../stores/RootStore';
import { Md, rich } from '../highlight';
import { latinKey } from './keys';
import { useContextMenu, type ContextMenuItem } from './ContextMenu';
import { ago } from './List';
import { SkillAgentsMenu } from './SkillAgentsMenu';
import { matchSkills } from './SkillMenu';
import { useAttachments } from './attachments';
import {
  ChatEmpty,
  ChatLive,
  ChatLog,
  ChatMessage as ChatMessageView,
  mixedAgents,
  UserBubble,
} from './ChatTurn';
import { AgentChip, Composer } from './space/Composer';
import { estimateRowHeight, groupTurns } from './space/chatTurns';
import { VirtualChat, type VirtualChatControl } from './space/VirtualChat';
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
  tagged,
}: {
  askKey: string;
  a: LineAsk;
  asked: boolean;
  tagged: boolean;
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
        tagged={tagged}
        badges={<Ref a={a.anchor} pr={a.pr} />}
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

/** What the AI panel's log shows of one thing that happened: a skill run
 *  (it reads its own state), or a message with the question it answers. */
export type AiItem =
  | { kind: 'run'; id: string }
  | { kind: 'message'; m: ChatMessage; question: string };

/** One row of the panel: a turn (your question and what came of it), or the
 *  tail with the answer still being written. */
export type AiRow =
  | { kind: 'turn'; key: string; items: AiItem[] }
  | { kind: 'tail'; key: 'tail' };

/** The conversation as turns, oldest first: each of your messages, or a
 *  skill run, starts one, and the replies under it stay in it. An agent
 *  message carries the last question of yours before it, for "Ask again". */
export function aiTurns(
  runs: readonly SkillRun[],
  messages: readonly ChatMessage[],
): { key: string; items: AiItem[] }[] {
  let asked = '';
  const feed = feedOf(runs, messages).map((f): AiItem => {
    if (f.kind === 'run') return { kind: 'run', id: f.run.id };
    const mine = f.message.author.kind === 'me';
    const question = asked;
    if (mine) asked = f.message.text;
    return { kind: 'message', m: f.message, question: mine ? '' : question };
  });
  return groupTurns(
    feed,
    (i) => i.kind === 'run' || i.m.author.kind === 'me',
    (i) => (i.kind === 'run' ? i.id : i.m.id),
  );
}

/** A skill run's room before it was measured: its log and findings. */
const RUN_HEIGHT = 240;

function estimateAiRow(row: AiRow): number {
  if (row.kind === 'tail') return 120;
  const messages = row.items.flatMap((i) =>
    i.kind === 'message' ? [i.m] : [],
  );
  const runs = row.items.length - messages.length;
  const base = estimateRowHeight({
    kind: 'turn',
    key: row.key,
    messages,
    live: false,
  });
  return base + runs * RUN_HEIGHT;
}

const isTail = (row: AiRow) => row.kind === 'tail';

/** The ask still going: whether it still shows, and what the log has of it. */
function pendingView(store: RootStore, pr: PrId) {
  const messages = store.models.chatMessages.byPr(pr);
  // The composer's plain send and the hunk header's "Ask agent" (DiffPane)
  // share this pending bubble via the store — `askPending` sets it,
  // watches for the reply, and clears it once the real message lands.
  const pending = store.ephemeral.chatPending.get(pr) ?? null;
  // Once an agent message lands after the ask, the pending bubble has
  // nothing left to show: hidden in that same render (never both at once
  // for a frame); `askPending`'s reaction then clears it from the store.
  const answered =
    pending != null &&
    messages.some((m) => m.author.kind !== 'me' && m.at >= pending.startedAt);
  const lineAsks = [...store.ephemeral.lineAsks.entries()].filter(
    ([, a]) => a.pr === pr && a.status === 'asking',
  );
  const stored = (id: string | null) =>
    id != null && messages.some((m) => m.id === id);
  return { pending, answered, lineAsks, stored };
}

/** Ends the pending question: cancels its run, takes its bubble away. */
function stopPendingAsk(store: RootStore, pr: PrId, pending: ChatPending) {
  void cancelRun(store, pending.run).catch(() => {});
  store.ephemeral.clearChatPending(pr);
}

/** The AI panel: the log of turns above, the composer under it. The two are
 *  siblings: what you type re-renders the composer, never the turns. */
function Chat({ pr }: { pr: PrId }) {
  const store = useRootStore();
  const [skills, setSkills] = useState<SkillInfo[]>([]);
  const [skillAgents, setSkillAgents] = useState<AgentId[]>(AGENTS);
  const [installed, setInstalled] = useState<AgentId[] | null>(null);
  // Default to the agents actually installed on this machine, minus any the
  // user has disabled (plan "enable/disable agents": hidden from the chat
  // chip, skipped for skills). The first one is the agent the panel talks
  // to; the rest are the others skills also run on.
  useEffect(() => {
    let live = true;
    void api
      .detectAgents()
      .then((all) => {
        // The `agents.chat` agent leads: the chip shows it, questions go to it.
        const first = chatAgentGuess(store);
        const ids = all
          .filter((a) => a.installed && isAgentEnabled(store, a.id))
          .map((a) => a.id)
          .sort((a, b) => Number(b === first) - Number(a === first));
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
  useEffect(() => {
    void api
      .listSkills(pr)
      .then(setSkills)
      .catch(() => setSkills([]));
  }, [pr]);
  const log = useRef<VirtualChatControl | null>(null);
  /** The composer's way to empty its box. */
  const clearText = useRef<(() => void) | null>(null);
  const runMatch = useCallback(
    (s: SkillInfo) => {
      void runSkill(store, pr, s.id, skillAgents);
      clearText.current?.();
      log.current?.jumpToLatest();
    },
    [store, pr, skillAgents],
  );

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
      <AiLog pr={pr} skills={skills} onPickSkill={runMatch} control={log} />
      <AiComposer
        pr={pr}
        skills={skills}
        skillAgents={skillAgents}
        onSkillAgents={setSkillAgents}
        installed={installed}
        onPickSkill={runMatch}
        onManage={() => setManaging(true)}
        log={log}
        clearText={clearText}
      />
    </div>
  );
}

/** How long a line ask's question or answer glows once it is brought into
 *  view (`.ask-flash`). */
const FLASH_MS = 1700;

/** A message to glow: a line ask wanted it in view. */
interface AskFlash {
  id: string;
  seq: number;
}

const AiLog = observer(function AiLog({
  pr,
  skills,
  onPickSkill,
  control,
}: {
  pr: PrId;
  skills: SkillInfo[];
  onPickSkill: (s: SkillInfo) => void;
  control: RefObject<VirtualChatControl | null>;
}) {
  const store = useRootStore();
  const messages = [...store.models.chatMessages.byPr(pr)].sort(
    (a, b) => a.at - b.at,
  );
  const runs = store.models.skillRuns.byPr(pr);
  const { pending, answered, lineAsks } = pendingView(store, pr);
  const tail = (pending != null && !answered) || lineAsks.length > 0;
  const turns = aiTurns(runs, messages);
  const rows: AiRow[] = turns.map((t) => ({ kind: 'turn', ...t }));
  if (tail) rows.push({ kind: 'tail', key: 'tail' });
  // Replies are named only when more than one agent wrote them, the live
  // ones included.
  const tagged = mixedAgents([
    ...messages.map((m) => (m.author.kind === 'me' ? null : m.author.agent)),
    pending && !answered ? pending.agent : null,
    ...lineAsks.map(([, a]) => a.agent),
  ]);

  // A line ask (DiffPane) wants its question or answer in view: the row it
  // is in is scrolled to, mounted or not, and its message glows.
  const focus =
    store.ephemeral.askFocus?.pr === pr ? store.ephemeral.askFocus : null;
  const target = focus?.message
    ? (turns.find((t) =>
        t.items.some((i) => i.kind === 'message' && i.m.id === focus.message),
      )?.key ?? null)
    : null;
  const [flash, setFlash] = useState<AskFlash | null>(null);
  useEffect(() => {
    if (!focus) return;
    if (focus.message && target) {
      control.current?.scrollToKey(target, 'center');
      setFlash({ id: focus.message, seq: focus.seq });
    } else {
      control.current?.jumpToLatest();
    }
    // The question isn't synced yet: a later focus (with its id) follows.
    if (!focus.message || target) store.ephemeral.consumeAskFocus(focus.seq);
  }, [focus, target, store, control]);
  useEffect(() => {
    if (!flash) return;
    const t = setTimeout(() => setFlash(null), FLASH_MS);
    return () => clearTimeout(t);
  }, [flash]);

  const renderRow = useCallback(
    (row: AiRow) =>
      row.kind === 'tail' ? (
        <AiTail pr={pr} tagged={tagged} />
      ) : (
        <AiTurn
          pr={pr}
          tagged={tagged}
          items={row.items}
          flash={
            flash &&
            row.items.some((i) => i.kind === 'message' && i.m.id === flash.id)
              ? flash
              : null
          }
        />
      ),
    [pr, flash, tagged],
  );

  if (rows.length === 0) {
    return (
      <ChatLog>
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
                if (s) onPickSkill(s);
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
      </ChatLog>
    );
  }
  return (
    <VirtualChat
      scope={`ai:${pr}`}
      items={rows}
      estimate={estimateAiRow}
      isPinned={isTail}
      renderItem={renderRow}
      control={control}
    />
  );
});

/** Your question, skill runs and the replies: skill runs and chat as one
 *  conversation, oldest first. Keeps its identity until one of them changes,
 *  so typing and other rows' updates leave it alone. */
const AiTurn = memo(
  function AiTurn({
    pr,
    items,
    flash,
    tagged,
  }: {
    pr: PrId;
    items: AiItem[];
    tagged: boolean;
    flash: AskFlash | null;
  }) {
    const store = useRootStore();
    const reAsk = (item: AiItem & { kind: 'message' }) => {
      if (!item.question) return undefined;
      const { m, question } = item;
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
      <>
        {items.map((item) => {
          if (item.kind === 'run') {
            return <SkillRunView key={item.id} pr={pr} run={item.id} />;
          }
          const { m } = item;
          const glow = flash?.id === m.id;
          return (
            <ChatMessageView
              // A repeat focus restarts the glow.
              key={glow ? `${m.id}:${flash.seq}` : m.id}
              m={m}
              tagged={tagged}
              className={glow ? 'ask-flash' : ''}
              refs={
                m.refs.length > 0
                  ? m.refs.map((a, i) => <Ref key={i} a={a} pr={pr} />)
                  : undefined
              }
              onAskAgain={reAsk(item)}
              onRetryFailed={reAsk(item)}
            />
          );
        })}
      </>
    );
  },
  (a, b) =>
    a.pr === b.pr &&
    a.flash === b.flash &&
    a.tagged === b.tagged &&
    a.items.length === b.items.length &&
    a.items.every((x, i) => {
      const y = b.items[i]!;
      return x.kind === 'run'
        ? y.kind === 'run' && x.id === y.id
        : y.kind === 'message' && x.m === y.m && x.question === y.question;
    }),
);

/** The answer still being written, under the last turn: the question you
 *  just asked until its stored message shows, the agent's live reply, and
 *  the line asks still going. */
const AiTail = observer(function AiTail({
  pr,
  tagged,
}: {
  pr: PrId;
  tagged: boolean;
}) {
  const store = useRootStore();
  const { pending, answered, lineAsks, stored } = pendingView(store, pr);
  return (
    <>
      {pending && !answered && (
        <>
          {/* The question, until its stored message shows above. */}
          {!stored(askMessageOf(pending.run)) && (
            <UserBubble text={pending.question} />
          )}
          <ChatLive
            run={pending.run}
            agent={pending.agent}
            tagged={tagged}
            className="chat-pending"
            onRetry={() => {
              const { question, agent } = pending;
              stopPendingAsk(store, pr, pending);
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
          tagged={tagged}
        />
      ))}
    </>
  );
});

/** The box under the log, and everything that changes as you type in it. */
const AiComposer = observer(function AiComposer({
  pr,
  skills,
  skillAgents,
  onSkillAgents,
  installed,
  onPickSkill,
  onManage,
  log,
  clearText,
}: {
  pr: PrId;
  skills: SkillInfo[];
  skillAgents: AgentId[];
  onSkillAgents: (agents: AgentId[]) => void;
  installed: AgentId[] | null;
  onPickSkill: (s: SkillInfo) => void;
  onManage: () => void;
  /** The log above: sending goes to the bottom of it. */
  log: RefObject<VirtualChatControl | null>;
  clearText: RefObject<(() => void) | null>;
}) {
  const store = useRootStore();
  const [text, setText] = useState('');
  const files = useAttachments();
  // Only agents installed on this computer and not disabled in Settings →
  // Agents are offered.
  const enabled = (installed ?? []).filter((a) => isAgentEnabled(store, a));
  // The agent the panel talks to, as the chip shows it: the first of the
  // chosen ones that is offered. Its model and effort are the agent's
  // settings, which core reads when it asks.
  const chatAgent =
    (skillAgents[0] && enabled.includes(skillAgents[0])
      ? skillAgents[0]
      : enabled[0]) ??
    skillAgents[0] ??
    null;
  const pickChat = (a: AgentId) =>
    onSkillAgents([a, ...skillAgents.filter((x) => x !== a)]);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  useEffect(() => {
    clearText.current = () => setText('');
    return () => {
      clearText.current = null;
    };
  }, [clearText]);
  const runs = store.models.skillRuns.byPr(pr);
  const { pending, answered, lineAsks } = pendingView(store, pr);
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

  // Stop in the composer ends the newest run still going here: the
  // pending question, a line's question, or a skill run.
  const current = newestRun<LiveRun>([
    ...(pending && !answered
      ? [
          {
            at: pending.startedAt,
            stop: () => stopPendingAsk(store, pr, pending),
          },
        ]
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

  const send = () => {
    const t = text.trim();
    if (!t || files.busy) return;
    // What you sent is at the bottom: you are there to see it.
    log.current?.jumpToLatest();
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
        picked: chatAgent,
        attachments: files.ids.length ? files.ids : null,
      });
      store.ephemeral.clearLineRef(pr);
      files.clear();
    } else {
      const agent = chatAgent;
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

  return (
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
        onPickSkill={onPickSkill}
        skillFooter={
          <div className="flex items-center gap-1 ps-3 pt-1 text-xs text-muted">
            <span className="min-w-0 truncate">
              Runs on {skillAgents.join(' + ')}
            </span>
            <Button
              size="sm"
              variant="ghost"
              className="manage-skills ms-auto shrink-0 gap-1.5 text-xs text-muted"
              onPress={onManage}
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
          <>
            <AgentChip value={chatAgent} onChange={pickChat} />
            {chatAgent && (
              <SkillAgentsMenu
                agents={enabled}
                primary={chatAgent}
                selected={skillAgents}
                onSelect={onSkillAgents}
              />
            )}
          </>
        }
      />
      <div className="flex items-center gap-2 px-2 text-xs text-muted">
        <span className="min-w-0 truncate">The agent can be wrong</span>
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
  const [open, setOpen] = useState(false);
  const people = [
    ...new Map(thread.comments.map((c) => [c.author.login, c.author])),
  ];
  const first = thread.comments[0]!;
  const last = thread.comments.at(-1)!;
  const replies = thread.comments.length - 1;
  // How it started, then (opened) every reply in order.
  const shown = open ? thread.comments : [first];
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
      {shown.map((c) => (
        <p
          key={c.id}
          className={`text-sm text-muted ${open ? 'whitespace-pre-wrap' : 'line-clamp-2'}`}
        >
          {/* Name the speaker only when more than one person is in the
           *  thread; the header already shows a lone author. */}
          {people.length > 1 && (
            <>
              <span className="text-foreground">{c.author.login}:</span>{' '}
            </>
          )}
          {stripHtmlComments(c.body)}
        </p>
      ))}
      <div className="flex items-center justify-between text-xs text-muted">
        <span>
          {replies > 0 ? (
            <button
              type="button"
              className="thread-replies hover:text-foreground hover:underline"
              aria-expanded={open}
              onClick={() => setOpen((v) => !v)}
            >
              {open
                ? 'Hide replies'
                : `${replies} ${replies === 1 ? 'reply' : 'replies'}`}
            </button>
          ) : (
            '1 comment'
          )}{' '}
          · {ago(last.created_at)}
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
export const Threads = observer(function Threads({ pr }: { pr: PrId }) {
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
