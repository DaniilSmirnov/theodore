/**
 * One look for every chat with an agent — the review's AI panel and a
 * space's chat: the scrolling log, the empty state, a message (yours as a
 * bubble on the right, the agent's under its tag with copy / vote), and
 * the live reply still being written. Each chat adds its own bits through
 * slots (file:line chips, undo, a turn's check); Stop is the composer's.
 */
import { useState, type ReactNode } from 'react';
import {
  ArrowDown,
  Check,
  Copy,
  ThumbsDown,
  ThumbsUp,
} from '@gravity-ui/icons';
import { Alert, Button, Tooltip, Typography } from '@heroui/react';
import { observer } from 'mobx-react-lite';
import type {
  AgentId,
  Attachment,
  ChatMessage as Message,
} from '../gen/contract';
import { useRootStore } from '../stores/context';
import { Md, rich } from '../highlight';
import { MessageAttachments } from './attachments';
import { AgentTag, Btn } from './bits';
import { LiveText } from './RunLive';
import { ToolCalls } from './ToolCalls';
import {
  LiveTimeline,
  LiveTimer,
  TurnActivity,
  TurnFiles,
  TurnFooter,
} from './TurnTimeline';
import { useChatScroll } from './useChatScroll';

/** The scrolling log (`useChatScroll`). A wide page keeps a reading width; a
 *  narrow panel uses all of it. */
export function ChatLog({
  scope,
  children,
}: {
  /** Keeps the scroll position per this id. */
  scope?: string;
  children: ReactNode;
}) {
  const { scroller, content, away, jump } = useChatScroll(scope);
  return (
    <div className="relative flex min-h-0 flex-1 flex-col">
      <div
        ref={scroller}
        className="chat-log flex min-h-0 flex-1 flex-col overflow-auto"
      >
        <div
          ref={content}
          className="mx-auto flex w-full max-w-[860px] flex-1 flex-col gap-5 px-4 py-5"
        >
          {children}
        </div>
      </div>
      {away && (
        <Button
          size="sm"
          variant="secondary"
          className="chat-jump absolute bottom-2 left-1/2 z-10 h-7 -translate-x-1/2 gap-1 rounded-full px-3 text-xs shadow-sm"
          onPress={jump}
        >
          <ArrowDown className="size-3.5" />
          Jump to latest
        </Button>
      )}
    </div>
  );
}

/** Nothing said yet: what the agent can do here, and (`children`) ways to
 *  start. */
export function ChatEmpty({
  icon,
  title,
  hint,
  children,
}: {
  icon: ReactNode;
  title: string;
  hint: string;
  children?: ReactNode;
}) {
  return (
    <div className="chat-empty m-auto flex w-full flex-col gap-4">
      <div className="flex flex-col items-center gap-2 text-center">
        <span className="grid size-10 place-items-center rounded-full bg-default text-muted [&>svg]:size-5">
          {icon}
        </span>
        <Typography.Heading level={6}>{title}</Typography.Heading>
        <Typography.Paragraph size="sm" color="muted" align="center">
          {hint}
        </Typography.Paragraph>
      </div>
      {children}
    </div>
  );
}

/** Typing indicator: three dots, like a messenger. */
export function Dots() {
  return (
    <span
      className="inline-flex h-5 items-center gap-1"
      aria-label="Agent is thinking"
    >
      {[0, 150, 300].map((d) => (
        <span
          key={d}
          className="size-1.5 animate-bounce rounded-full bg-muted"
          style={{ animationDelay: `${d}ms` }}
        />
      ))}
    </span>
  );
}

/** A run with no output in a while — a note, not a control: Stop is the
 *  composer's. */
export function StalledHint() {
  return (
    <Alert status="warning" className="agent-stalled">
      <Alert.Indicator />
      <Alert.Content>
        <Alert.Description className="text-xs">
          No response in a while — the agent may be stuck.
        </Alert.Description>
      </Alert.Content>
    </Alert>
  );
}

/** Under an agent's reply: copy, a vote, and (`onRetry`) ask again. */
export function MessageActions({
  text,
  onRetry,
}: {
  text: string;
  onRetry?: () => void;
}) {
  const [vote, setVote] = useState<'up' | 'down' | null>(null);
  const [copied, setCopied] = useState(false);
  const act = (
    label: string,
    icon: ReactNode,
    onPress: () => void,
    on = false,
  ) => (
    <Tooltip key={label}>
      <Button
        isIconOnly
        size="sm"
        variant="ghost"
        aria-label={label}
        className={`size-7 min-w-7 ${on ? 'text-foreground' : 'text-muted'}`}
        onPress={onPress}
      >
        {icon}
      </Button>
      <Tooltip.Content>{label}</Tooltip.Content>
    </Tooltip>
  );
  return (
    <div className="message-actions -ms-1.5 flex items-center gap-0.5">
      {act(
        copied ? 'Copied' : 'Copy',
        copied ? <Check className="size-3.5" /> : <Copy className="size-3.5" />,
        () => {
          void navigator.clipboard?.writeText(text);
          setCopied(true);
          setTimeout(() => setCopied(false), 1500);
        },
      )}
      {act(
        'Helpful',
        <ThumbsUp className="size-3.5" />,
        () => setVote(vote === 'up' ? null : 'up'),
        vote === 'up',
      )}
      {act(
        'Not helpful',
        <ThumbsDown className="size-3.5" />,
        () => setVote(vote === 'down' ? null : 'down'),
        vote === 'down',
      )}
      {onRetry && (
        <Btn tone="ghost" className="ms-1 h-7 px-2 text-xs" onPress={onRetry}>
          Ask again
        </Btn>
      )}
    </div>
  );
}

/** Whether a conversation names the agent on its replies: only when its
 *  replies come from more than one. Computed once per list and passed down,
 *  so a row re-renders only when the answer flips. */
export function mixedAgents(
  agents: Iterable<AgentId | null | undefined>,
): boolean {
  const seen = new Set<AgentId>();
  for (const a of agents) if (a) seen.add(a);
  return seen.size > 1;
}

/** The agent's tag (when `tagged`) and whatever the chat puts beside it. */
function AgentHeader({
  agent,
  tagged,
  badges,
}: {
  agent: AgentId | null | undefined;
  tagged?: boolean;
  badges?: ReactNode;
}) {
  const named = tagged && agent ? agent : null;
  if (!named && !badges) return null;
  return (
    <div className="flex min-w-0 flex-wrap items-center gap-2">
      {named && <AgentTag id={named} />}
      {badges}
    </div>
  );
}

/** What you wrote: a bubble on the right, its chips and files above —
 *  told apart from the agent's reply at a glance. `id`: the stored
 *  message's (the AI panel scrolls to it); none for a question not stored
 *  yet. */
export function UserBubble({
  id,
  text,
  refs,
  attachments,
  badges,
  className = '',
}: {
  id?: string;
  text: string;
  refs?: ReactNode;
  attachments?: Attachment[] | null;
  /** Undo on a Revert, which reads as your own message. */
  badges?: ReactNode;
  className?: string;
}) {
  return (
    <div
      data-msg={id}
      data-variant="user"
      className={`chat-message msg user flex justify-end ${className}`}
    >
      <div className="flex max-w-[85%] flex-col items-end gap-1.5">
        {badges && (
          <div className="flex items-center gap-1.5 text-xs">{badges}</div>
        )}
        {refs && (
          <div className="flex max-w-full flex-wrap justify-end gap-1.5">
            {refs}
          </div>
        )}
        <MessageAttachments attachments={attachments} align="end" />
        {(text || !attachments?.length) && (
          <div className="user-bubble rich whitespace-pre-wrap rounded-3xl bg-surface px-4 py-2 text-sm [overflow-wrap:anywhere]">
            {rich(text)}
          </div>
        )}
      </div>
    </div>
  );
}

/** `ChatMessage.failed` of a run you stopped (the core's "stopped"). */
const STOPPED = 'stopped';

/** One stored message. Yours: a `UserBubble`. The agent's: its tag, a space
 *  turn's activity, the markdown reply, its chips and files, `after` (a turn's
 *  check), then how long it took, copy / vote and the files it changed. A run
 *  that failed shows why instead, with Retry when `onRetryFailed` is given. */
export function ChatMessage({
  m,
  refs,
  badges,
  tagged,
  after,
  onAskAgain,
  onRetryFailed,
  onOpenFile,
  className = '',
}: {
  m: Message;
  /** File:line chips (the AI panel's). */
  refs?: ReactNode;
  /** Beside the agent's tag (undone, Undo). */
  badges?: ReactNode;
  /** Name the agent: this chat has replies from more than one. */
  tagged?: boolean;
  /** Under the agent's reply, before its actions. */
  after?: ReactNode;
  /** "Ask again" under the agent's reply. */
  onAskAgain?: () => void;
  onRetryFailed?: () => void;
  /** A changed file's chip: the Changes pane on it; `null`: just the pane. */
  onOpenFile?: (path: string | null) => void;
  className?: string;
}) {
  if (m.author.kind === 'me') {
    return (
      <UserBubble
        id={m.id}
        text={m.text}
        refs={refs}
        attachments={m.attachments}
        badges={badges}
        className={className}
      />
    );
  }
  return (
    <div
      data-msg={m.id}
      data-variant="agent"
      className={`chat-message msg agent flex gap-3 ${className}`}
    >
      <div className="flex min-w-0 flex-1 flex-col gap-2 pt-1">
        <AgentHeader agent={m.author.agent} tagged={tagged} badges={badges} />
        {!!m.activity?.length && (
          <TurnActivity
            reply={m.id}
            steps={m.activity}
            pinLastText={!!m.text_in_steps || !m.text.trim()}
            keepText={!!m.failed}
          />
        )}
        {m.failed && m.text && !m.text_in_steps && (
          <div className="text-sm leading-relaxed">
            <Md text={m.text} />
          </div>
        )}
        {m.failed === STOPPED ? (
          // You stopped it: no failure, just where it ended.
          <span className="agent-stopped text-xs text-muted">Stopped</span>
        ) : m.failed ? (
          <Alert status="danger" className="agent-failed">
            <Alert.Indicator />
            <Alert.Content>
              <Alert.Description className="text-xs [overflow-wrap:anywhere]">
                {m.failed}
              </Alert.Description>
              {onRetryFailed && (
                <div className="mt-2 flex gap-1.5">
                  <Button size="sm" variant="tertiary" onPress={onRetryFailed}>
                    Retry
                  </Button>
                </div>
              )}
            </Alert.Content>
          </Alert>
        ) : (
          <>
            {!m.text_in_steps && (
              <div className="text-sm leading-relaxed">
                <Md text={m.text} />
              </div>
            )}
            {refs && <div className="flex flex-wrap gap-1.5">{refs}</div>}
            <MessageAttachments attachments={m.attachments} />
          </>
        )}
        {after}
        {(!m.failed || m.duration_ms != null) && (
          <div className="flex flex-wrap items-center gap-2">
            {m.duration_ms != null && (
              <TurnFooter
                at={m.at}
                durationMs={m.duration_ms}
                model={m.model}
                agent={m.author.agent}
              />
            )}
            {!m.failed && <MessageActions text={m.text} onRetry={onAskAgain} />}
            {!!m.changed_files?.length && (
              <TurnFiles files={m.changed_files} onOpen={onOpenFile} />
            )}
          </div>
        )}
      </div>
    </div>
  );
}

/** The reply still being written: the agent's tag, then its text as it
 *  streams (dots until there is some) and its steps, or with `timeline` both
 *  as one list in the order they came, the time so far under them. Stalled: a
 *  note under them, with Retry when `onRetry` is given. No Stop here — the
 *  composer has it. */
export const ChatLive = observer(function ChatLive({
  run,
  agent,
  badges,
  tagged,
  timeline = false,
  since,
  onRetry,
  className = '',
  data,
}: {
  /** `null`: not started yet (the ask is on its way). */
  run: string | null;
  agent?: AgentId | null;
  badges?: ReactNode;
  /** Name the agent: this chat has replies from more than one. */
  tagged?: boolean;
  /** A space turn: text and steps chronological. */
  timeline?: boolean;
  /** When a space turn started, for its timer. */
  since?: number;
  onRetry?: () => void;
  className?: string;
  /** `data-*` attributes for the bubble. */
  data?: Record<`data-${string}`, string>;
}) {
  const store = useRootStore();
  const stalled = run != null && store.ephemeral.stalled.has(run);
  const tools = (!timeline && run && store.ephemeral.runLog.get(run)) || [];
  const waiting = stalled ? null : <Dots />;
  return (
    <div
      data-variant="agent"
      className={`chat-live msg agent flex gap-3 ${className}`}
      {...data}
    >
      <div className="flex min-w-0 flex-1 flex-col gap-2 pt-1">
        <AgentHeader agent={agent} tagged={tagged} badges={badges} />
        {!run ? (
          waiting
        ) : timeline ? (
          <LiveTimeline run={run} fallback={waiting} />
        ) : (
          <LiveText run={run} fallback={waiting} />
        )}
        {run && tools.length > 0 && (
          <ToolCalls tools={tools} running turn={run} />
        )}
        {timeline && (run != null || since != null) && (
          <LiveTimer run={run} since={since} />
        )}
        {stalled && <StalledHint />}
        {stalled && onRetry && (
          <Button
            size="sm"
            variant="tertiary"
            className="h-7 w-fit px-2 text-xs"
            onPress={onRetry}
          >
            Retry
          </Button>
        )}
      </div>
    </div>
  );
});
