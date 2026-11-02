/**
 * Pull requests: one list of the active workspace's PRs with a filter at
 * the top — All (open), Mine (by next step), Drafts, Merged. Under All
 * projects it spans every repo and each row names its repo. The filter is
 * saved per workspace (`UiStore.prsFilter`).
 */
import { readProgress } from './review/progress';
import { useEffect, useRef, useState } from 'react';
import { BranchesRight, CodePullRequest, Xmark } from '@gravity-ui/icons';
import { Button, Chip, Tabs, Tooltip } from '@heroui/react';
import { observer } from 'mobx-react-lite';
import type { PrId } from '../gen/contract';
import { loadViewer } from '../actions';
import { ALL } from '../core/workspaces';
import { useRootStore, useUi } from '../stores/context';
import { Avatar, Empty, Hints, K } from './bits';
import { useContextMenu } from './ContextMenu';
import { ListSearchBar } from './ListSearchBar';
import {
  PEEK_FITS_FROM,
  PeekToggle,
  PrRow,
  RowList,
  ViewHeader,
  ago,
  peekFits,
} from './List';
import { useElementWidth } from './useElementWidth';
import { RowsSkeleton } from './Loading';
import { Peek } from './Peek';
import { matchesSearch, prSearchText } from './search';
import { branchReviewMenu, closeBranchReview } from './project/closeReview';
import { isBranchReview } from './project/sections';
import { useProjectAutoRefresh } from './project/freshness';
import {
  PR_FILTERS,
  filterCount,
  prGroups,
  type PrFilter,
} from './prs/filters';
import { gatesFor } from './prs/mine';
import { PrStatusIcon } from './StatusIcon';
import { latinKey, useViewKeys } from './keys';
import { useCheckboxRange } from './useCheckboxRange';

const EMPTY: Record<PrFilter, { title: string; text: string }> = {
  all: {
    title: 'No open pull requests',
    text: 'Open pull requests of this workspace show up here.',
  },
  mine: {
    title: 'No PRs of yours',
    text: 'PRs you authored show up here with what they need to merge.',
  },
  drafts: { title: 'No drafts', text: 'Draft pull requests show up here.' },
  merged: {
    title: 'Nothing merged lately',
    text: 'Recently merged pull requests show up here.',
  },
};

/** A row under Mine: what still stands between the PR and a merge. */
const MineRow = observer(function MineRow({
  id,
  sel,
  repo,
}: {
  id: PrId;
  sel: boolean;
  repo: boolean;
}) {
  const store = useRootStore();
  const p = store.models.prSummaries.get(id);
  if (!p) return null;
  const blockers = gatesFor(p).filter((g) => !g.ok);
  return (
    <div
      id={`R-${id}`}
      className={`minerow grid h-12 w-full grid-cols-[16px_minmax(72px,auto)_minmax(140px,1fr)_minmax(0,1.4fr)_22px_40px] items-center gap-3 ${
        sel ? 'sel' : ''
      }`}
    >
      <PrStatusIcon summary={p} progress={readProgress(store, id)} />
      <span
        className="flex min-w-0 max-w-36 whitespace-nowrap text-xs tabular-nums text-muted"
        title={`${p.pr.repo.owner}/${p.pr.repo.name}#${p.pr.number}`}
      >
        {/* The repo name gives way; the number never clips. */}
        {repo && (
          <span className="min-w-0 truncate">{p.pr.repo.name}&nbsp;</span>
        )}
        <span className="shrink-0">#{p.pr.number}</span>
      </span>
      <span className="min-w-0 truncate font-medium" title={p.title}>
        {p.title}
      </span>
      <span className="flex min-w-0 gap-1 overflow-hidden">
        {p.state === 'merged' ? (
          <span className="text-xs text-muted">merged</span>
        ) : p.state === 'closed' ? (
          <span className="text-xs text-muted">closed</span>
        ) : blockers.length ? (
          blockers.map((b) => (
            <Chip
              key={b.label}
              size="sm"
              variant="soft"
              color="warning"
              className="blocker min-w-0 shrink truncate whitespace-nowrap"
            >
              {b.label}
            </Chip>
          ))
        ) : (
          <Chip size="sm" variant="soft" color="success">
            ready to merge
          </Chip>
        )}
      </span>
      <Avatar person={p.author} size={18} />
      <span className="text-right text-xs text-muted">{ago(p.updated_at)}</span>
    </div>
  );
});

/** A branch review: not a host PR, so none of `PrRow`'s inbox actions —
 *  the branch, its last commit, and Close review on hover / right-click. */
export const BranchReviewRow = observer(function BranchReviewRow({
  id,
  sel,
}: {
  id: PrId;
  sel: boolean;
}) {
  const store = useRootStore();
  const p = store.models.prSummaries.get(id);
  const menu = useContextMenu(() => branchReviewMenu(store, id));
  if (!p) return null;
  return (
    <div
      id={`R-${id}`}
      className={`prrow group flex h-11 w-full min-w-0 items-center gap-3 ${sel ? 'sel' : ''}`}
      onContextMenu={menu}
    >
      <BranchesRight className="size-4 shrink-0 text-muted" />
      <span
        className="min-w-0 max-w-[45%] truncate font-mono text-sm"
        title={p.head_ref}
      >
        {p.head_ref}
      </span>
      <span className="min-w-0 flex-1 truncate text-sm text-muted">
        {p.title}
      </span>
      <span className="shrink-0 whitespace-nowrap text-xs text-muted">
        vs {p.base_ref} · {ago(p.updated_at)}
      </span>
      {/* Closing never also opens the row it sits in. */}
      <span className="flex shrink-0" onClick={(e) => e.stopPropagation()}>
        <Tooltip>
          <Button
            isIconOnly
            size="sm"
            variant="ghost"
            aria-label="Close review"
            className="size-6 min-w-6 shrink-0 rounded-xl text-muted opacity-0 group-hover:opacity-100"
            onPress={() => void closeBranchReview(store, id)}
          >
            <Xmark className="size-3.5" />
          </Button>
          <Tooltip.Content>Close review</Tooltip.Content>
        </Tooltip>
      </span>
    </div>
  );
});

function usePrKeys(order: PrId[]) {
  const ui = useUi();
  useViewKeys(
    ui,
    (e) => {
      const i = ui.selected ? order.indexOf(ui.selected) : -1;
      const move = (d: number) => {
        const next = order[Math.max(0, Math.min(order.length - 1, i + d))];
        if (next) ui.select(next);
      };
      switch (latinKey(e)) {
        case 'j':
        case 'ArrowDown':
          move(1);
          break;
        case 'k':
        case 'ArrowUp':
          move(-1);
          break;
        case 'Enter':
          if (ui.selected) ui.go({ name: 'review', pr: ui.selected });
          break;
        case ' ':
          ui.togglePeek();
          break;
        default:
          return false;
      }
    },
    [order.join()],
  );

  useEffect(() => {
    if (order.length && (!ui.selected || !order.includes(ui.selected)))
      ui.select(order[0]!);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [order.join()]);

  useEffect(() => {
    if (ui.selected)
      document
        .getElementById(`R-${ui.selected}`)
        ?.scrollIntoView({ block: 'nearest' });
  }, [ui.selected]);
}

export const PullRequests = observer(function PullRequests() {
  const store = useRootStore();
  const ui = useUi();
  const ws = store.workspaces;
  const filter = ui.prsFilter;
  const picked = ui.picked;
  // Per filter, a group the user opened or closed; else its default.
  const [toggled, setToggled] = useState<Record<string, boolean>>({});
  const pageRef = useRef<HTMLDivElement>(null);
  const fits = peekFits(useElementWidth(pageRef, PEEK_FITS_FROM));

  useEffect(() => {
    void loadViewer(store);
  }, [store]);
  useProjectAutoRefresh(ws.active);

  const viewer = store.ephemeral.viewer;
  const login = viewer?.login ?? null;
  const prs = ws.scopePrs(store.models.prSummaries.all());
  const groups = prGroups(prs, filter, login);
  // ⌘F (search.ts): matches ignore a group's collapsed state — searching
  // should surface a row even if its group was folded.
  const query = ui.listSearch?.scope === 'prs' ? ui.listSearch.query : null;
  const visibleGroups =
    query === null
      ? groups
      : groups
          .map((g) => ({
            ...g,
            items: g.items.filter((p) => matchesSearch(prSearchText(p), query)),
          }))
          .filter((g) => g.items.length);
  const closed = Object.fromEntries(
    visibleGroups.map((g) => [
      g.id,
      query === null ? (toggled[`${filter}:${g.id}`] ?? g.collapsed) : false,
    ]),
  );
  const order = visibleGroups.flatMap((g) =>
    closed[g.id] ? [] : g.items.map((p) => p.id),
  );
  usePrKeys(order);

  const showRepo = ws.id === ALL;
  const branchReviews = new Set(prs.filter(isBranchReview).map((p) => p.id));
  const pickOrder =
    filter === 'mine'
      ? []
      : visibleGroups.flatMap((g) =>
          closed[g.id]
            ? []
            : g.items.filter((p) => !branchReviews.has(p.id)).map((p) => p.id),
        );
  const range = useCheckboxRange({
    scopeKey: `prs:${ws.id}:${filter}`,
    orderedKeys: pickOrder,
    onChange: (ids, checked) => ui.setPicked(ids, checked),
  });
  const empty = EMPTY[filter];

  return (
    <div className="view relative flex min-w-0 flex-1 flex-col">
      <ViewHeader title="Pull requests" count={filterCount(prs, filter, login)}>
        <Tabs
          data-plain
          selectedKey={filter}
          onSelectionChange={(k) => ui.setPrsFilter(k as PrFilter)}
        >
          <Tabs.ListContainer>
            <Tabs.List aria-label="Pull request filter">
              {PR_FILTERS.map((f) => (
                <Tabs.Tab key={f.id} id={f.id} className="whitespace-nowrap">
                  {f.label}
                  <Tabs.Indicator />
                </Tabs.Tab>
              ))}
            </Tabs.List>
          </Tabs.ListContainer>
        </Tabs>
        <PeekToggle on={ui.peek} fits={fits} toggle={() => ui.togglePeek()} />
      </ViewHeader>
      {query !== null && (
        <ListSearchBar
          query={query}
          onQueryChange={(q) => ui.setListSearchQuery(q)}
          order={order}
          selected={ui.selected}
          onSelect={(id) => ui.select(id as PrId)}
          onOpen={(id) => ui.go({ name: 'review', pr: id as PrId })}
          onClose={() => ui.closeListSearch()}
          placeholder="Search pull requests…"
          reopenToken={ui.listSearchFocusAt}
        />
      )}
      <div ref={pageRef} className="flex min-h-0 flex-1">
        <RowList
          groups={visibleGroups}
          sel={ui.selected}
          closed={closed}
          toggle={(id) =>
            setToggled((t) => ({ ...t, [`${filter}:${id}`]: !closed[id] }))
          }
          onSelect={(id) => ui.select(id)}
          onOpen={(id) => ui.go({ name: 'review', pr: id })}
          row={(id) =>
            branchReviews.has(id) ? (
              <BranchReviewRow id={id} sel={ui.selected === id} />
            ) : filter === 'mine' ? (
              <MineRow id={id} sel={ui.selected === id} repo={showRepo} />
            ) : (
              <PrRow
                id={id}
                sel={ui.selected === id}
                picked={picked.includes(id)}
                onPick={(checked, modifiers) =>
                  range.activate(id, checked, modifiers)
                }
                repo={showRepo}
              />
            )
          }
          empty={
            filter === 'mine' && !viewer && store.ephemeral.viewerLoading ? (
              <div className="flex-1">
                <RowsSkeleton rows={6} />
              </div>
            ) : filter === 'mine' && !viewer ? (
              <Empty
                icon={<CodePullRequest />}
                title="Not signed in"
                text="Connect GitHub in Settings → Account to see your PRs."
              />
            ) : (
              <Empty
                icon={<CodePullRequest />}
                title={empty.title}
                text={empty.text}
              />
            )
          }
        />
        {ui.peek && fits && ui.selected && order.includes(ui.selected) && (
          <Peek id={ui.selected} />
        )}
      </div>
      <Hints>
        <K>J</K>
        <K>K</K> navigate <K>⏎</K> open <K>Space</K> preview <K>⌘F</K> search{' '}
        <K>⌘K</K> everything
      </Hints>
    </div>
  );
});
