/**
 * Update from base (docs/spaces.md §6): "Base moved: N commits behind" in
 * the space header with its Update action, and, while a merge Theodore
 * started is resolving its conflicts, the banner with Complete and Abort.
 */
import { useEffect, useState } from 'react';

import { CodeMerge } from '@gravity-ui/icons';
import { Alert, Button, Tooltip } from '@heroui/react';
import { observer } from 'mobx-react-lite';

import type { Space, SpaceBaseStatus } from '../../gen/contract';
import { api } from '../../core/api';
import { useRootStore } from '../../stores/context';
import { useProjectAutoRefresh } from '../project/freshness';

/** "Base moved: 3 commits behind", or `null` when there is nothing to
 *  show (up to date, resolving, archived). */
export function baseMovedLabel(
  space: Pick<Space, 'status' | 'updating'>,
  status: SpaceBaseStatus | null,
): string | null {
  if (!status || status.behind <= 0) return null;
  if (space.status === 'archived' || space.updating) return null;
  const n = status.behind;
  return `Base moved: ${n} commit${n === 1 ? '' : 's'} behind`;
}

/** The resolving banner's line: who is doing what, and in which files. */
export function resolvingText(
  space: Pick<Space, 'status' | 'base' | 'updating'>,
): string | null {
  const u = space.updating;
  if (!u) return null;
  const files = u.conflicts ?? [];
  const shown = files.slice(0, 3).join(', ');
  const more = files.length > 3 ? ` and ${files.length - 3} more` : '';
  const where = files.length ? `: ${shown}${more}` : '';
  return space.status === 'running'
    ? `The agent is resolving conflicts with ${space.base}${where}`
    : `Resolving conflicts with ${space.base}${where}`;
}

/** The success toast after Update, Complete merge or Abort; `null` while
 *  it still resolves (the banner says so). */
export function updateOutcome(
  which: 'update' | 'complete' | 'abort',
  after: Pick<Space, 'base' | 'updating'>,
): string | null {
  if (which === 'abort') return 'Update aborted: back where it started';
  if (after.updating) return null;
  return which === 'update'
    ? `Updated from ${after.base}`
    : `Merged ${after.base} into the space`;
}

/** How often an open space page asks `space_base_status` again (a read of
 *  the copy on this Mac: a few ms). */
export const BASE_STATUS_POLL_MS = 60_000;

/** The base's distance, re-read when the space or its project's copy
 *  changes (a turn, an update, the poller's fetch), every minute while the
 *  page is open, and when the window comes back to the front: a local
 *  folder's base moves with a commit made there, which nothing else tells. */
export function useBaseStatus(space: Space): SpaceBaseStatus | null {
  const store = useRootStore();
  const project = store.models.projects.get(space.project) ?? null;
  const fetchedAt = project?.last_fetched_at;
  const [status, setStatus] = useState<SpaceBaseStatus | null>(null);
  const [focused, setFocused] = useState(0);
  const live = space.status !== 'archived' && space.status !== 'running';
  // Opening the page fetches a project a few minutes stale, as its own
  // page does: the base may have moved since (the stamp re-reads below).
  useProjectAutoRefresh(project);
  useEffect(() => {
    const again = () => setFocused((n) => n + 1);
    window.addEventListener('focus', again);
    // And now and then while the page stays open: a PR refresh fetches the
    // copy without stamping the project.
    const poll = setInterval(again, BASE_STATUS_POLL_MS);
    return () => {
      window.removeEventListener('focus', again);
      clearInterval(poll);
    };
  }, []);
  useEffect(() => {
    if (!live) return;
    let on = true;
    api
      .spaceBaseStatus(space.id)
      .then((s) => on && setStatus(s))
      .catch(() => on && setStatus(null));
    return () => {
      on = false;
    };
  }, [space.id, space.base_sha, space.head_sha, fetchedAt, live, focused]);
  return status;
}

/** `baseMovedLabel`'s words, in parts a narrow header can drop
 *  (`.base-moved-lead`, `.base-moved-long`: "2 behind"). */
export function BaseMovedText({ behind }: { behind: number }) {
  return (
    <>
      <span className="base-moved-lead">Base moved: </span>
      {behind}
      <span className="base-moved-long">
        {behind === 1 ? ' commit' : ' commits'}
      </span>{' '}
      behind
    </>
  );
}

/** Header: "Base moved: N commits behind" with Update. */
export const BaseMoved = observer(function BaseMoved({
  space,
}: {
  space: Space;
}) {
  const store = useRootStore();
  const status = useBaseStatus(space);
  const [busy, setBusy] = useState(false);
  const label = baseMovedLabel(space, status);
  if (!label || !status) return null;
  return (
    <Tooltip>
      <Button
        size="sm"
        variant="secondary"
        className="space-base-moved shrink-0"
        isPending={busy}
        isDisabled={space.status === 'running'}
        onPress={() => {
          setBusy(true);
          void api
            .updateSpaceFromBase(space.id)
            .then((s) => {
              const done = updateOutcome('update', s);
              if (done) store.ephemeral.toast(done, 'info');
            })
            .catch(() => {})
            .finally(() => setBusy(false));
        }}
      >
        <CodeMerge className="size-3.5" />
        <BaseMovedText behind={status.behind} /> · Update
      </Button>
      <Tooltip.Content>
        Merges {space.base} into this space. Conflicts go to the agent; pushed
        commits are never rewritten.
      </Tooltip.Content>
    </Tooltip>
  );
});

/** Under the header while an update from base resolves its conflicts. */
export const UpdatingBanner = observer(function UpdatingBanner({
  space,
}: {
  space: Space;
}) {
  const store = useRootStore();
  const [busy, setBusy] = useState<'complete' | 'abort' | null>(null);
  const text = resolvingText(space);
  if (!text) return null;
  const running = space.status === 'running';
  const act = (which: 'complete' | 'abort') => {
    setBusy(which);
    const call =
      which === 'complete'
        ? api.updateSpaceFromBase(space.id)
        : api.abortSpaceUpdate(space.id);
    void call
      .then((s) => {
        const done = updateOutcome(which, s);
        if (done) store.ephemeral.toast(done, 'info');
      })
      .catch(() => {})
      .finally(() => setBusy(null));
  };
  return (
    <Alert
      status="warning"
      className="space-updating mx-3 mt-3 w-auto shrink-0 items-center"
    >
      <Alert.Content>
        <Alert.Description>
          {text}. Theodore commits the merge once no conflict markers are left.
        </Alert.Description>
      </Alert.Content>
      <Button
        size="sm"
        variant="secondary"
        className="shrink-0"
        isPending={busy === 'complete'}
        isDisabled={running || busy !== null}
        onPress={() => act('complete')}
      >
        Complete merge
      </Button>
      <Button
        size="sm"
        variant="ghost"
        className="space-update-abort shrink-0"
        isPending={busy === 'abort'}
        isDisabled={busy !== null}
        onPress={() => act('abort')}
      >
        Abort
      </Button>
    </Alert>
  );
});
