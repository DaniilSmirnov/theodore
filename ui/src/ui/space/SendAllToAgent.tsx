/**
 * "Send N to agent" (docs/spaces.md §3 "Review of a space"): on a space's
 * review, this footer replaces "Submit review" — every open draft and
 * finding (plus, once the space has a host PR, its unresolved threads) go
 * as one turn via `space_send_feedback`. Items already sent this way drop
 * out on their own (`Space.sent_feedback`); once the turn they rode ends,
 * the findings among them can be dismissed in bulk instead of one at a
 * time.
 */
import { useState } from 'react';
import { Check, PaperPlane } from '@gravity-ui/icons';
import { observer } from 'mobx-react-lite';
import type {
  Draft,
  Finding,
  PrId,
  SpaceFeedback,
  Thread,
} from '../../gen/contract';
import { api } from '../../core/api';
import { ignoreFinding } from '../../actions';
import { useRootStore } from '../../stores/context';
import { Btn } from '../bits';
import { isBuildMode } from './mode';
import { sentTurnDone, sentTurnOf, spaceOfReview, spaceTitle } from './spaces';

const plural = (n: number, noun: string) => `${n} ${noun}${n === 1 ? '' : 's'}`;

/** `rows` minus whatever a past "Send N" already sent. */
function unsent<T extends { id: string }>(
  kind: SpaceFeedback['kind'],
  rows: readonly T[],
  sent: (item: SpaceFeedback) => boolean,
): T[] {
  return rows.filter((r) => !sent({ kind, id: r.id } as SpaceFeedback));
}

export const SendAllToAgentFooter = observer(function SendAllToAgentFooter({
  pr,
}: {
  pr: PrId;
}) {
  const store = useRootStore();
  const [busy, setBusy] = useState(false);
  if (!isBuildMode(store)) return null;
  const space = spaceOfReview(store.models.spaces.all(), pr);
  if (!space) return null;

  const isSent = (item: SpaceFeedback) => sentTurnOf(space, item) != null;
  const openFindings = store.models.findings
    .byPr(pr)
    .filter((f) => f.state === 'open' || f.state === 'maybe_fixed');
  const drafts = unsent<Draft>('draft', store.models.drafts.byPr(pr), isSent);
  const findings = unsent<Finding>('finding', openFindings, isSent);
  // Unresolved threads count only once the space has a real PR: its own
  // branch review (drafting) has none of its own.
  const threads =
    space.pr === pr
      ? unsent<Thread>(
          'thread',
          (store.models.prDetails.get(pr)?.threads ?? []).filter(
            (t) => !t.resolved,
          ),
          isSent,
        )
      : [];
  const items: SpaceFeedback[] = [
    ...drafts.map((d): SpaceFeedback => ({ kind: 'draft', id: d.id })),
    ...findings.map((f): SpaceFeedback => ({ kind: 'finding', id: f.id })),
    ...threads.map((t): SpaceFeedback => ({ kind: 'thread', id: t.id })),
  ];

  // Findings a past batch already sent, whose turn is now done: dismiss
  // them in bulk instead of hunting each one down.
  const addressed = openFindings.filter((f) => {
    const turn = sentTurnOf(space, { kind: 'finding', id: f.id });
    return turn != null && sentTurnDone(space, turn);
  });

  if (!items.length && !addressed.length) return null;
  const running = space.status === 'running';

  const send = async () => {
    setBusy(true);
    try {
      await api.spaceSendFeedback({ space: space.id, items });
      store.ephemeral.toast(
        `Sent ${plural(items.length, 'item')} to ${spaceTitle(space)}`,
        'info',
        {
          label: 'Open',
          onAction: () => store.ui.go({ name: 'space', id: space.id }),
        },
      );
    } catch {
      // toasted by api.ts
    } finally {
      setBusy(false);
    }
  };

  const parts = [
    drafts.length ? plural(drafts.length, 'draft') : null,
    findings.length ? plural(findings.length, 'finding') : null,
    threads.length ? plural(threads.length, 'thread') : null,
  ].filter((p): p is string => p != null);

  return (
    <div className="send-all-to-agent flex min-h-10 items-center justify-between gap-3 border-t border-separator px-4 py-2 text-sm">
      <span className="min-w-0 truncate text-muted">
        {parts.length ? parts.join(' · ') : 'Nothing new to send'}
      </span>
      <div className="flex shrink-0 items-center gap-2">
        {addressed.length > 0 && (
          <Btn
            tone="tertiary"
            icon={<Check className="size-3.5" />}
            onPress={() => addressed.forEach((f) => ignoreFinding(store, f.id))}
          >
            Dismiss {plural(addressed.length, 'addressed finding')}
          </Btn>
        )}
        {items.length > 0 && (
          <Btn
            tone="primary"
            icon={<PaperPlane className="size-3.5" />}
            isDisabled={running}
            isPending={busy}
            onPress={() => void send()}
          >
            Send {items.length} to agent
          </Btn>
        )}
      </div>
    </div>
  );
});
