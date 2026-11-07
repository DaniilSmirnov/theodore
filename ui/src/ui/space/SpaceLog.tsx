/**
 * A space's chat above the composer: nothing said yet is a plain page, the
 * rest is one row per turn in a `VirtualChat`.
 */
import { memo, useCallback, type ReactNode, type Ref } from 'react';
import { Pencil } from '@gravity-ui/icons';
import { observer } from 'mobx-react-lite';
import type { ChatMessage, Space } from '../../gen/contract';
import { useRootStore } from '../../stores/context';
import type { SpaceSend } from '../../stores/EphemeralStore';
import { ChatEmpty, ChatLive, ChatLog, mixedAgents } from '../ChatTurn';
import {
  chatRows,
  estimateRowHeight,
  pairTurns,
  type ChatRow,
} from './chatTurns';
import { runsByReply, setupView } from './commands';
import { QueuedMessages } from './QueuedMessages';
import { splitQueue } from './queue';
import { sendingMessage } from './send';
import { SetupNote, SetupStage } from './SpaceCommands';
import { SpaceMessage } from './SpaceMessage';
import { QuestionCard } from './SpacePlan';
import { spaceChatKey } from './spaces';
import { VirtualChat, type VirtualChatControl } from './VirtualChat';

const EMPTY_SENDS: readonly SpaceSend[] = [];
const isPinned = (row: ChatRow) => row.kind === 'head' || row.kind === 'tail';

export const SpaceLog = observer(function SpaceLog({
  space,
  onRevise,
  control,
}: {
  space: Space;
  onRevise: () => void;
  control?: Ref<VirtualChatControl>;
}) {
  const store = useRootStore();
  // Queued messages wait under the running turn, in the order they send.
  const stored = splitQueue([
    ...store.models.chatMessages.byPr(spaceChatKey(space.id)),
  ]);
  // What you just sent shows at once, last, until its row takes over.
  const sends = store.ephemeral.spaceSends.get(space.id) ?? EMPTY_SENDS;
  const sending = sends.map((s) => sendingMessage(space.id, s));
  const sent = [...stored.sent, ...sending.filter((m) => !m.queued)];
  const queued = [...stored.queued, ...sending.filter((m) => m.queued)];
  const keyOf = (id: string) => store.ephemeral.turnKeys.get(id) ?? id;
  const turns = pairTurns(sent, keyOf);
  const run = space.status === 'running' ? space.run : null;
  // A turn whose check still runs has its reply stored already: its steps
  // and text show there, the check under them.
  const replied =
    run != null &&
    [...runsByReply(space.id, space.turn_log).values()].includes(run);
  const live = run != null ? !replied : sends.some((s) => !s.queued);
  // Replies are named only when more than one agent wrote them, the live
  // one included.
  const tagged = mixedAgents([
    ...sent.map((m) => (m.author.kind === 'agent' ? m.author.agent : null)),
    live
      ? run
        ? space.agent
        : (sends.find((s) => !s.queued)?.agent ?? space.agent)
      : null,
  ]);
  const setup = setupView(space);
  const asking = run != null && space.question?.run === run;
  const head = (
    <>
      <SetupStage space={space} />
      <SetupNote space={space} />
    </>
  );
  const tail = (
    <>
      <QueuedMessages space={space} queued={queued} keyOf={keyOf} />
      {asking && space.question && (
        <QuestionCard
          key={space.question.id}
          space={space}
          question={space.question}
        />
      )}
    </>
  );

  const renderRow = useRowRenderer(space.id, onRevise, head, tail, tagged);
  if (turns.length === 0 && !run && !live) {
    return (
      <ChatLog scope={space.id}>
        {head}
        {setup !== 'stage' && (
          <ChatEmpty
            icon={<Pencil />}
            title="Tell the agent what to change"
            hint="It works in its own worktree; your checkout is never touched."
          />
        )}
        {tail}
      </ChatLog>
    );
  }
  const rows = chatRows(turns, {
    head: setup != null,
    live,
    tail: queued.length > 0 || asking,
  });
  return (
    <VirtualChat
      scope={space.id}
      items={rows}
      estimate={estimateRowHeight}
      isPinned={isPinned}
      renderItem={renderRow}
      control={control}
    />
  );
});

function useRowRenderer(
  spaceId: string,
  onRevise: () => void,
  head: ReactNode,
  tail: ReactNode,
  tagged: boolean,
) {
  return useCallback(
    (row: ChatRow) => {
      switch (row.kind) {
        case 'head':
          return head;
        case 'tail':
          return tail;
        case 'live':
          return <SpaceLive spaceId={spaceId} tagged={tagged} />;
        case 'turn':
          return (
            <TurnRow
              spaceId={spaceId}
              messages={row.messages}
              live={row.live}
              tagged={tagged}
              onRevise={onRevise}
            />
          );
      }
    },
    [spaceId, onRevise, head, tail, tagged],
  );
}

/** Your message, the reply and its cards, the live reply while it runs. The
 *  turn's messages keep their identity until one of them changes. */
const TurnRow = memo(
  function TurnRow({
    spaceId,
    messages,
    live,
    tagged,
    onRevise,
  }: {
    spaceId: string;
    messages: ChatMessage[];
    live: boolean;
    tagged: boolean;
    onRevise: () => void;
  }) {
    return (
      <>
        {/* Yours keeps its row when the core's copy replaces what you sent. */}
        {messages.map((m, i) => (
          <SpaceMessage
            key={i === 0 ? 'first' : m.id}
            spaceId={spaceId}
            m={m}
            tagged={tagged}
            onRevise={onRevise}
          />
        ))}
        {live && (
          <SpaceLive
            spaceId={spaceId}
            tagged={tagged}
            since={messages[0]?.at}
          />
        )}
      </>
    );
  },
  (a, b) =>
    a.spaceId === b.spaceId &&
    a.live === b.live &&
    a.tagged === b.tagged &&
    a.onRevise === b.onRevise &&
    a.messages.length === b.messages.length &&
    a.messages.every((m, i) => m === b.messages[i]),
);

const SpaceLive = observer(function SpaceLive({
  spaceId,
  tagged,
  since,
}: {
  spaceId: string;
  tagged: boolean;
  /** When its turn started: your message's time. */
  since?: number;
}) {
  const { models, ephemeral } = useRootStore();
  const space = models.spaces.get(spaceId);
  if (!space) return null;
  const run = space.status === 'running' ? (space.run ?? null) : null;
  // Before its run starts: the agent you picked for it.
  const sending = ephemeral.spaceSends.get(spaceId)?.find((s) => !s.queued);
  return (
    <ChatLive
      run={run}
      agent={run ? space.agent : (sending?.agent ?? space.agent)}
      tagged={tagged}
      timeline
      since={since}
      badges={
        run && ephemeral.stoppingRuns.has(run) ? (
          <span className="space-stopping text-xs text-muted">Stopping…</span>
        ) : undefined
      }
    />
  );
});
