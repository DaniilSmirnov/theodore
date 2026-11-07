/**
 * One stored message of a space's chat, as a row that re-renders only when
 * that message, or what the space says about it, changes. A progress tick, a
 * keystroke in the composer or a new turn elsewhere leaves it alone.
 */
import { memo, useCallback, useMemo, useState } from 'react';
import { ArrowUturnCcwLeft } from '@gravity-ui/icons';
import { Button, Tooltip } from '@heroui/react';
import { compareStructural, computed } from 'mobx';
import { observer } from 'mobx-react-lite';
import { api } from '../../core/api';
import type { ChatMessage as Message } from '../../gen/contract';
import { useRootStore } from '../../stores/context';
import { ChatMessage } from '../ChatTurn';
import { CheckChip } from './SpaceCommands';
import { PlanActions } from './SpacePlan';
import { BackToHere } from './SpaceRewind';
import { messageView, type MessageView } from './messageView';

const NOTHING: MessageView = {
  undone: false,
  undoHere: false,
  undoReason: null,
  plan: false,
  planHere: false,
  back: false,
  check: null,
};

export const SpaceMessage = memo(
  observer(function SpaceMessage({
    spaceId,
    m,
    tagged,
    onRevise,
  }: {
    spaceId: string;
    m: Message;
    /** Name the agent: this chat has replies from more than one. */
    tagged: boolean;
    onRevise: () => void;
  }) {
    const store = useRootStore();
    // Compared by value: a space row that changes something else is no
    // change here.
    const view = useMemo(
      () =>
        computed(
          () => {
            const space = store.models.spaces.get(spaceId);
            return space ? messageView(space, m) : NOTHING;
          },
          { equals: compareStructural },
        ),
      [store, spaceId, m],
    );
    const v = view.get();
    const agent = m.author.kind === 'agent';
    const undoneReply = agent && v.undone;
    const onOpenFile = useCallback(
      (path: string | null) => store.ephemeral.openSpaceChanges(spaceId, path),
      [store, spaceId],
    );
    return (
      <ChatMessage
        m={m}
        tagged={tagged}
        onOpenFile={onOpenFile}
        className={v.undone ? 'space-undone opacity-50' : ''}
        badges={
          undoneReply || v.undoHere || v.back || v.plan ? (
            <>
              {v.plan && (
                <span className="space-plan-badge rounded-full bg-default px-1.5 text-[10px] text-accent">
                  plan
                </span>
              )}
              {undoneReply && (
                <span className="rounded-full bg-default px-1.5 text-[10px] text-muted">
                  undone
                </span>
              )}
              {v.undoHere && (
                <UndoTurn spaceId={spaceId} reason={v.undoReason} />
              )}
              {v.back && <SpaceBackToHere spaceId={spaceId} reply={m.id} />}
            </>
          ) : undefined
        }
        after={
          agent ? (
            <>
              {v.check && (
                <CheckChip check={v.check} keptAs={`check:${m.id}`} />
              )}
              {v.planHere && (
                <SpacePlanActions spaceId={spaceId} onRevise={onRevise} />
              )}
            </>
          ) : undefined
        }
      />
    );
  }),
);

/** These two read the whole space, so they are the ones to re-render with it. */
const SpaceBackToHere = observer(function SpaceBackToHere({
  spaceId,
  reply,
}: {
  spaceId: string;
  reply: string;
}) {
  const space = useRootStore().models.spaces.get(spaceId);
  return space ? <BackToHere space={space} reply={reply} /> : null;
});

const SpacePlanActions = observer(function SpacePlanActions({
  spaceId,
  onRevise,
}: {
  spaceId: string;
  onRevise: () => void;
}) {
  const space = useRootStore().models.spaces.get(spaceId);
  return space ? <PlanActions space={space} onRevise={onRevise} /> : null;
});

/** Undo last turn, on the agent's reply that ended it. `reason` set: shown
 *  disabled with why (still running, already shipped…). Errors are toasted by
 *  api.ts. */
function UndoTurn({
  spaceId,
  reason,
}: {
  spaceId: string;
  reason: string | null;
}) {
  const [busy, setBusy] = useState(false);
  return (
    <Tooltip>
      <Button
        size="sm"
        variant="ghost"
        className="space-undo h-5 gap-1 px-1.5 text-[11px]"
        isDisabled={reason != null}
        isPending={busy}
        onPress={() => {
          setBusy(true);
          void api
            .undoSpaceTurn(spaceId)
            .catch(() => {})
            .finally(() => setBusy(false));
        }}
      >
        <ArrowUturnCcwLeft className="size-3" />
        Undo
      </Button>
      <Tooltip.Content>
        {reason ?? 'Back to the checkpoint before this turn; the chat keeps it'}
      </Tooltip.Content>
    </Tooltip>
  );
}
