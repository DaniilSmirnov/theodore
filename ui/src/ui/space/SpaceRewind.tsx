/**
 * Going back in a space (docs/spaces.md §3 "Undo, rewind, revert"): "Back
 * to here" on an earlier agent reply, and Revert on a file of the space's
 * diff. Undo itself sits on the last reply (`SpacePage`).
 */
import { useState } from 'react';

import { ArrowUturnCcwLeft } from '@gravity-ui/icons';
import { Button, Tooltip } from '@heroui/react';
import { api } from '../../core/api';
import type { FileDiff, Space } from '../../gen/contract';
import type { ContextMenuItem } from '../ContextMenu';
import { lastAgentTurn, rewindState } from './spaces';

/** "Back to here" on the reply of a turn that isn't the last one: every
 *  later turn is undone at once. Nothing on the last reply (it has Undo)
 *  or on an undone one. */
export function BackToHere({ space, reply }: { space: Space; reply: string }) {
  const [busy, setBusy] = useState(false);
  const state = rewindState(space, reply);
  if (!state || space.status === 'archived') return null;
  return (
    <Tooltip>
      <Button
        size="sm"
        variant="ghost"
        className="space-rewind h-5 gap-1 px-1.5 text-[11px]"
        isDisabled={!state.ok}
        isPending={busy}
        onPress={() => {
          setBusy(true);
          void api
            .rewindSpace({ space: space.id, reply })
            .catch(() => {})
            .finally(() => setBusy(false));
        }}
      >
        <ArrowUturnCcwLeft className="size-3" />
        Back to here
      </Button>
      <Tooltip.Content>
        {state.reason ??
          'Undo every turn after this one; the chat keeps them, dimmed'}
      </Tooltip.Content>
    </Tooltip>
  );
}

/** A file's menu in the space's diff: Revert to base, and Revert the last
 *  turn's change. Disabled with why while the agent works. */
export function revertItems(space: Space, file: FileDiff): ContextMenuItem[] {
  const paths = file.old_path ? [file.path, file.old_path] : [file.path];
  const why =
    space.status === 'running'
      ? 'The agent is still working'
      : space.status === 'archived'
        ? 'Archived'
        : space.updating
          ? 'Finish or abort the update from base first'
          : undefined;
  const revert = (from: 'base' | 'last_turn') => () =>
    void api.revertSpaceFile({ space: space.id, paths, from }).catch(() => {});
  const items: ContextMenuItem[] = [
    {
      id: 'revert-base',
      label: 'Revert to base',
      icon: <ArrowUturnCcwLeft className="size-3.5" />,
      isDisabled: !!why,
      disabledReason: why,
      onAction: revert('base'),
    },
  ];
  if (lastAgentTurn(space)) {
    items.push({
      id: 'revert-last-turn',
      label: "Revert the last turn's change",
      isDisabled: !!why,
      disabledReason: why,
      onAction: revert('last_turn'),
    });
  }
  return items;
}
