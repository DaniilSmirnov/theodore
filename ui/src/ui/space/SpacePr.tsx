/**
 * A space in review (docs/spaces.md §3, §7), without leaving the space
 * page: the right pane's Changes · Threads · Checks tabs, Merge in the
 * header, Continue once the PR merged, and the sidebar's `⎇ #n` glyph.
 * The review's own Threads, Checks and Merge pieces, reused.
 */
import { useEffect, useState, type ReactNode } from 'react';
import {
  ChevronDown,
  CircleCheckFill,
  CodeMerge,
  CodePullRequest,
  TriangleExclamation,
} from '@gravity-ui/icons';
import { Alert, Button, Chip, Dropdown, Tabs, Tooltip } from '@heroui/react';
import { observer } from 'mobx-react-lite';
import type {
  ConflictFile,
  MergeInfo,
  MergeMethod,
  PrId,
  PrSummary,
  Space,
} from '../../gen/contract';
import { updateBranch } from '../../actions';
import { api } from '../../core/api';
import { useRootStore } from '../../stores/context';
import { Checks, ReviewerStrip } from '../Checks';
import {
  CONFLICT_KIND_LABEL,
  METHOD,
  MergeDialog,
  STATE_LABEL,
} from '../Merge';
import { Threads } from '../SidePanel';
import {
  autoArchiveDays,
  canContinue,
  defaultMergeMethod,
  prTint,
  spaceMerge,
  type PrTint,
} from './spaces';

type Tab = 'changes' | 'threads' | 'checks';

const TINT_CLASS: Record<PrTint, string> = {
  pass: 'text-success',
  fail: 'text-danger',
  pending: 'text-warning',
  none: 'text-muted',
  merged: 'text-accent',
  closed: 'text-muted',
};

/** `⎇` tinted by the PR's checks (merged: the merge icon). */
export function PrGlyph({ pr }: { pr: PrSummary | undefined }) {
  const tint = prTint(pr);
  const Icon = tint === 'merged' ? CodeMerge : CodePullRequest;
  return (
    <Icon
      className={`space-pr-glyph ${TINT_CLASS[tint]}`}
      aria-label={`PR ${tint === 'none' ? 'open' : tint}`}
    />
  );
}

/** `merge_info` of an open PR, asked again whenever `version` changes
 *  (the PR row's head and update time); the last answer stays meanwhile.
 *  Quiet: the caller says what's unknown. */
function useMergeInfo(pr: PrId | null, version: string): MergeInfo | null {
  const [got, setGot] = useState<{ pr: PrId; info: MergeInfo } | null>(null);
  useEffect(() => {
    if (!pr) return;
    let live = true;
    void api
      .mergeInfo(pr, true)
      .then((info) => live && setGot({ pr, info }))
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [pr, version]);
  return got && got.pr === pr ? got.info : null;
}

/** The conflicting files of `pr` at `head` (`conflicts`, computed
 *  locally), when the host says it conflicts; `null` until known. */
function useConflicts(
  pr: PrId,
  head: string | undefined,
  on: boolean,
): ConflictFile[] | null {
  const key = `${pr}@${head}`;
  const [got, setGot] = useState<{ key: string; files: ConflictFile[] }>();
  useEffect(() => {
    if (!on) return;
    let live = true;
    void api
      .conflicts(pr)
      .then((files) => live && files && setGot({ key, files }))
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [pr, key, on]);
  return on && got?.key === key ? got.files : null;
}

const isOpen = (s: PrSummary | undefined) =>
  s?.state === 'open' || s?.state === 'draft';

/** The right pane once the space has a PR: its diff, the PR's threads
 *  (Reply · Resolve · Send to agent) and its checks. */
export const SpacePrPane = observer(function SpacePrPane({
  pr,
  changes,
}: {
  pr: PrId;
  /** The Changes tab: the space's live diff. */
  changes: ReactNode;
}) {
  const store = useRootStore();
  const [tab, setTab] = useState<Tab>('changes');
  // Threads and checks live on the PR's detail row.
  useEffect(() => {
    void store.sync.openPr(pr).catch(() => {});
  }, [store, pr]);
  const summary = store.models.prSummaries.get(pr);
  const threads = store.models.prDetails.get(pr)?.threads ?? [];
  const unresolved = threads.filter((t) => !t.resolved).length;
  return (
    <Tabs
      data-plain
      selectedKey={tab}
      onSelectionChange={(k) => setTab(k as Tab)}
      className="space-pr-tabs flex min-h-0 flex-1 flex-col [&>[role=tabpanel]]:border-t [&>[role=tabpanel]]:border-separator"
    >
      <Tabs.ListContainer className="mx-5 my-2">
        <Tabs.List aria-label="Space pane" className="w-fit">
          <Tabs.Tab id="changes">
            Changes
            <Tabs.Indicator />
          </Tabs.Tab>
          <Tabs.Tab id="threads">
            Threads
            {unresolved > 0 && (
              <Chip size="sm" variant="soft" color="warning" className="ml-1">
                {unresolved}
              </Chip>
            )}
            <Tabs.Indicator />
          </Tabs.Tab>
          <Tabs.Tab id="checks" className="gap-1.5">
            <span className="grid size-3.5 place-items-center [&>svg]:size-3.5">
              <PrGlyph pr={summary} />
            </span>
            Checks
            <Tabs.Indicator />
          </Tabs.Tab>
        </Tabs.List>
      </Tabs.ListContainer>
      <Tabs.Panel id="changes" className="flex min-h-0 flex-1 flex-col p-0">
        {changes}
      </Tabs.Panel>
      <Tabs.Panel id="threads" className="flex min-h-0 flex-1 flex-col p-0">
        <Threads pr={pr} />
      </Tabs.Panel>
      <Tabs.Panel id="checks" className="flex min-h-0 flex-1 flex-col p-0">
        <SpaceChecks pr={pr} />
      </Tabs.Panel>
    </Tabs>
  );
});

function reviewLabel(s: PrSummary): string {
  const need =
    s.required_approvals != null
      ? `${s.approvals}/${s.required_approvals} approvals`
      : `${s.approvals} approval${s.approvals === 1 ? '' : 's'}`;
  switch (s.review_decision) {
    case 'approved':
      return `Approved · ${need}`;
    case 'changes_requested':
      return `Changes requested · ${need}`;
    case 'review_required':
      return `Review required · ${need}`;
    default:
      return need;
  }
}

function StatusRow({
  ok,
  label,
  children,
}: {
  ok: boolean | null;
  label: string;
  children: ReactNode;
}) {
  return (
    <div className="flex min-w-0 items-center gap-2">
      {ok === true ? (
        <CircleCheckFill className="size-4 shrink-0 text-success" />
      ) : ok === false ? (
        <TriangleExclamation className="size-4 shrink-0 text-warning" />
      ) : (
        <span className="size-4 shrink-0" />
      )}
      <span className="w-16 shrink-0 text-muted">{label}</span>
      <div className="flex min-w-0 flex-1 items-center gap-2">{children}</div>
    </div>
  );
}

/** Review decision, mergeable state and conflicts, then every check
 *  (Re-run on a failed one). */
const SpaceChecks = observer(function SpaceChecks({ pr }: { pr: PrId }) {
  const store = useRootStore();
  const summary = store.models.prSummaries.get(pr);
  const detail = store.models.prDetails.get(pr);
  const open = isOpen(summary);
  const info = useMergeInfo(
    open ? pr : null,
    `${summary?.head_sha}:${summary?.updated_at}`,
  );
  const conflicts = useConflicts(
    pr,
    summary?.head_sha,
    open && !!summary?.conflicts,
  );
  const [updating, setUpdating] = useState(false);

  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-auto">
      {summary && (
        <section className="space-pr-status flex flex-col gap-2 border-b border-separator px-6 py-4 text-sm">
          <StatusRow
            ok={
              summary.review_decision === 'approved'
                ? true
                : summary.review_decision === 'changes_requested'
                  ? false
                  : null
            }
            label="Review"
          >
            <span className="shrink-0">{reviewLabel(summary)}</span>
            {detail && <ReviewerStrip reviewers={detail.reviewers} />}
          </StatusRow>
          <StatusRow
            ok={
              !open
                ? summary.state === 'merged'
                : info
                  ? info.state === 'clean'
                  : null
            }
            label="Merge"
          >
            {!open
              ? summary.state === 'merged'
                ? 'Merged'
                : 'Closed without merging'
              : info
                ? `${STATE_LABEL[info.state]}${info.auto_merge_enabled ? ' · auto-merge on' : ''}${info.in_queue ? ' · in the merge queue' : ''}`
                : 'Checking…'}
          </StatusRow>
          {open && summary.conflicts && (
            <StatusRow ok={false} label="Conflicts">
              <div className="flex min-w-0 flex-col gap-1">
                {conflicts && conflicts.length > 0 ? (
                  <ul className="conflict-files flex flex-col gap-0.5">
                    {conflicts.map((f) => (
                      <li key={f.path} className="truncate text-xs">
                        <span className="font-mono">{f.path}</span>{' '}
                        <span className="text-muted">
                          — {CONFLICT_KIND_LABEL[f.kind]}
                        </span>
                      </li>
                    ))}
                  </ul>
                ) : (
                  <span>With the base branch</span>
                )}
                <Button
                  size="sm"
                  variant="ghost"
                  className="w-fit"
                  isPending={updating}
                  onPress={() => {
                    setUpdating(true);
                    void updateBranch(store, pr).finally(() =>
                      setUpdating(false),
                    );
                  }}
                >
                  Update branch
                </Button>
              </div>
            </StatusRow>
          )}
        </section>
      )}
      <Checks pr={pr} checks={detail?.checks ?? []} />
    </div>
  );
});

/** Merge in the space header (docs/spaces.md §3): the host's default
 *  method; Auto-merge while checks or reviews are pending, when the repo
 *  has it; the queue when the base uses one. ▾ picks another method or
 *  opens the full merge dialog. */
export const SpaceMergeButton = observer(function SpaceMergeButton({
  space,
}: {
  space: Space;
}) {
  const store = useRootStore();
  const pr = space.pr ?? null;
  const summary = pr ? store.models.prSummaries.get(pr) : undefined;
  const open = isOpen(summary);
  const info = useMergeInfo(
    open ? pr : null,
    `${summary?.head_sha}:${summary?.updated_at}`,
  );
  const [busy, setBusy] = useState(false);
  const [dialog, setDialog] = useState(false);
  if (!pr || !summary || !open) return null;

  const action =
    summary.state === 'draft'
      ? { kind: 'none' as const, reason: 'A draft: mark it ready first' }
      : spaceMerge(info, space);
  const method = info ? defaultMergeMethod(info) : 'squash';
  const methods: MergeMethod[] = info?.allowed_methods.length
    ? info.allowed_methods
    : ['squash', 'merge', 'rebase'];
  const merge = async (m: MergeMethod | null) => {
    setBusy(true);
    try {
      const r = await api.mergeSpace({ space: space.id, method: m });
      store.ephemeral.toast(
        r.queue
          ? `#${summary.pr.number} added to the merge queue`
          : r.auto
            ? `Auto-merge on: #${summary.pr.number} merges once checks and reviews pass`
            : `Merging #${summary.pr.number}…`,
        'info',
      );
    } catch {
      // toasted by api.ts
    } finally {
      setBusy(false);
    }
  };
  const label =
    action.kind === 'auto'
      ? 'Auto-merge'
      : action.kind === 'queue'
        ? 'Add to queue'
        : 'Merge';
  return (
    <div className="space-merge flex shrink-0 items-center">
      <Tooltip>
        <Button
          size="sm"
          variant="primary"
          className="rounded-r-none"
          isDisabled={action.kind === 'none' || busy}
          isPending={busy}
          onPress={() => void merge(null)}
        >
          <CodeMerge className="size-3.5" />
          {label}
        </Button>
        <Tooltip.Content>
          {action.kind === 'none'
            ? action.reason
            : `${METHOD[method]}${action.kind === 'auto' ? ', once checks and reviews pass' : action.kind === 'queue' ? ', through the merge queue' : ''}`}
        </Tooltip.Content>
      </Tooltip>
      <Dropdown>
        <Button
          size="sm"
          variant="primary"
          isIconOnly
          className="rounded-l-none border-l border-separator"
          aria-label="Merge options"
          isDisabled={busy}
        >
          <ChevronDown className="size-3.5" />
        </Button>
        <Dropdown.Popover placement="bottom end">
          <Dropdown.Menu
            aria-label="Merge"
            disabledKeys={action.kind === 'none' ? methods : []}
            onAction={(k) => {
              if (k === 'dialog') setDialog(true);
              else void merge(k as MergeMethod);
            }}
          >
            {[
              ...methods.map((m) => (
                <Dropdown.Item key={m} id={m} textValue={METHOD[m]}>
                  {METHOD[m]}
                </Dropdown.Item>
              )),
              <Dropdown.Item key="dialog" id="dialog" textValue="Merge options">
                Merge options…
              </Dropdown.Item>,
            ]}
          </Dropdown.Menu>
        </Dropdown.Popover>
      </Dropdown>
      {dialog && <MergeDialog pr={pr} onClose={() => setDialog(false)} />}
    </div>
  );
});

/** Once the PR merged: Continue on a new branch from the updated base, or
 *  leave it to archive itself. */
export const MergedBanner = observer(function MergedBanner({
  space,
}: {
  space: Space;
}) {
  const store = useRootStore();
  const summary = space.pr ? store.models.prSummaries.get(space.pr) : undefined;
  const [busy, setBusy] = useState(false);
  if (!canContinue(space, summary?.state)) return null;
  const days = autoArchiveDays(store.models.settings.all());
  const number = summary ? `#${summary.pr.number}` : 'The PR';
  return (
    <Alert status="success" className="space-merged m-3 items-center">
      <Alert.Content>
        <Alert.Description>
          {number} merged into {space.base}. Continue here on a new branch from
          the updated {space.base}; the chat and turns stay.
          {days > 0
            ? ` Left alone, the space archives itself ${days} day${days === 1 ? '' : 's'} after the merge.`
            : ''}
        </Alert.Description>
      </Alert.Content>
      <Button
        size="sm"
        variant="secondary"
        className="space-continue shrink-0"
        isPending={busy}
        onPress={() => {
          setBusy(true);
          void api
            .continueSpace(space.id)
            .then((s) =>
              store.ephemeral.toast(`Continuing on ${s.branch}`, 'info'),
            )
            .catch(() => {})
            .finally(() => setBusy(false));
        }}
      >
        Continue
      </Button>
    </Alert>
  );
});
