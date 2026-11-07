/** "Send to agent" (docs/spaces.md §3) next to a thread's, a draft's or a
 *  finding's own actions — only on a space's PR or its own review, and
 *  only with build mode on. The comment becomes the space's next turn.
 *  Once sent (`Space.sent_feedback`, by this button or "Send N to agent")
 *  the item says so — "sent to agent · turn N" — and sending it again,
 *  which runs another turn, asks first. */
import { useState } from 'react';
import { Check, PaperPlane } from '@gravity-ui/icons';
import { Button, Tooltip } from '@heroui/react';
import { observer } from 'mobx-react-lite';
import type { PrId, SpaceFeedback } from '../../gen/contract';
import { api } from '../../core/api';
import { useRootStore } from '../../stores/context';
import { isBuildMode } from './mode';
import { sentTurnOf, spaceOfReview, spaceTitle } from './spaces';

export const SendToAgent = observer(function SendToAgent({
  pr,
  item,
}: {
  pr: PrId;
  item: SpaceFeedback;
}) {
  const store = useRootStore();
  const [busy, setBusy] = useState(false);
  const [confirming, setConfirming] = useState(false);
  if (!isBuildMode(store)) return null;
  const space = spaceOfReview(store.models.spaces.all(), pr);
  if (!space) return null;
  const running = space.status === 'running';
  const sentIn = sentTurnOf(space, item);
  const send = async () => {
    setBusy(true);
    try {
      await api.spaceSendFeedback({ space: space.id, items: [item] });
      store.ephemeral.toast(`Sent to ${spaceTitle(space)}`, 'info', {
        label: 'Open',
        onAction: () => store.ui.go({ name: 'space', id: space.id }),
      });
    } catch {
      // toasted by api.ts
    } finally {
      setBusy(false);
      setConfirming(false);
    }
  };
  if (sentIn != null && confirming) {
    return (
      <span className="send-to-agent-confirm inline-flex items-center gap-1 whitespace-nowrap">
        <span>Send again? Runs another turn.</span>
        <Button
          size="sm"
          variant="ghost"
          className="send-to-agent-again"
          isDisabled={running}
          isPending={busy}
          onPress={() => void send()}
        >
          Send again
        </Button>
        <Button size="sm" variant="ghost" onPress={() => setConfirming(false)}>
          Cancel
        </Button>
      </span>
    );
  }
  return (
    <>
      {sentIn != null && (
        <span className="sent-to-agent inline-flex items-center gap-1 whitespace-nowrap text-success">
          <Check className="size-3.5" />
          sent to agent · turn {sentIn}
        </span>
      )}
      <Tooltip>
        <Button
          size="sm"
          variant="ghost"
          className="send-to-agent gap-1.5"
          aria-label={sentIn != null ? 'Send to agent again' : 'Send to agent'}
          isDisabled={running}
          isPending={busy}
          onPress={() => (sentIn != null ? setConfirming(true) : void send())}
        >
          <PaperPlane className="size-3.5" />
          {sentIn != null ? 'Again' : 'Send to agent'}
        </Button>
        <Tooltip.Content>
          {running
            ? `${space.agent} is still working in ${spaceTitle(space)}`
            : sentIn != null
              ? `Already sent in turn ${sentIn}; sending again runs another turn`
              : `The next turn of ${spaceTitle(space)}, quoting this with its file and line`}
        </Tooltip.Content>
      </Tooltip>
    </>
  );
});
