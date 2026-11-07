import { useElementWidth } from './useElementWidth';
import { readProgress } from './review/progress';
import { RowsSkeleton } from './Loading';
import { useEffect, useRef, useState } from 'react';
import type { ComponentProps, Key, ReactNode } from 'react';
import {
  ArrowRotateLeft,
  Archive,
  ChevronDown,
  ChevronRight,
  Clock,
  CodePullRequest,
  Link,
  LayoutSideContentRight,
  Persons,
  Sliders,
  Sparkles,
} from '@gravity-ui/icons';
import {
  Button,
  Card,
  Checkbox,
  Dropdown,
  Header,
  Label,
  ListBox,
  Popover,
  Select,
  Separator,
  Switch,
  Tag,
  TagGroup,
  ToggleButton,
  Toolbar,
  Tooltip,
  Typography,
} from '@heroui/react';
import { observer } from 'mobx-react-lite';
import type {
  InboxReason,
  Person,
  PrId,
  PrState,
  PrSummary,
  RepoRef,
  StackInfo,
} from '../gen/contract';
import { markDone, refreshInbox, snoozePrs, triage } from '../actions';
import { api } from '../core/api';
import { useRootStore, useUi } from '../stores/context';
import { Avatar, Btn, Empty, Hints, K } from './bits';
import { useContextMenu, type ContextMenuItem } from './ContextMenu';
import { ListSearchBar } from './ListSearchBar';
import { Peek } from './Peek';
import { matchesSearch, prSearchText } from './search';
import { PrStatusIcon, prStatusLabel } from './StatusIcon';
import { latinKey, useViewKeys } from './keys';
import {
  useCheckboxRange,
  useCheckboxRangeEvents,
  type CheckboxRangeModifiers,
} from './useCheckboxRange';

/** A PR row as the pool stores it — stamped with its own id (see `sync/types.ts`). */
export type InboxPr = PrSummary & { id: PrId };

/** A PR belongs in the inbox only while open and with a reason to be there —
 *  project refreshes also store merged/closed PRs and ones with no reason. */
export function inInbox(p: Pick<PrSummary, 'state' | 'reasons'>): boolean {
  return (p.state === 'open' || p.state === 'draft') && p.reasons.length > 0;
}

const SECTIONS: { id: string; title: string; reasons: InboxReason[] }[] = [
  {
    id: 'review',
    title: 'Needs your review',
    reasons: ['review_requested', 'team_review_requested'],
  },
  { id: 'assigned', title: 'Assigned to you', reasons: ['assigned'] },
  { id: 'mentioned', title: 'Mentions', reasons: ['mentioned'] },
  {
    id: 'updated',
    title: 'Updated since your review',
    reasons: ['updated_after_review'],
  },
  { id: 'authored', title: 'Authored by you', reasons: ['authored'] },
];

export const STATUS_ORDER: PrState[] = ['draft', 'open', 'merged', 'closed'];

/** Builds the local `PrId` for `{repo, number}` — mirrors Rust's
 *  `PrRef::id()` (plan "Stack"): `StackInfo` only carries bare PR numbers,
 *  since it's always scoped to the same repo as the row it's on. */
export function prIdFor(repo: RepoRef, number: number): PrId {
  return `${repo.host}:${repo.owner}/${repo.name}#${number}` as PrId;
}

export function ago(ms: number): string {
  const s = Math.max(0, (Date.now() - ms) / 1000);
  if (s < 60) return `${Math.round(s)}s`;
  if (s < 3600) return `${Math.round(s / 60)}m`;
  if (s < 86400) return `${Math.round(s / 3600)}h`;
  const d = s / 86400;
  if (d < 60) return `${Math.round(d)}d`;
  if (d < 365) return `${Math.round(d / 30)}mo`;
  return `${Math.round(d / 365)}y`;
}

// ── Display: grouping, ordering, which columns a row shows ────────────────
// Ported from the prototype's `Display`/`ROW_PROPS`, trimmed to the fields
// `PrSummary` actually has — no stack/linear/ai/note/team, so those columns
// and sort/group keys that depended on them (risk) are dropped, not faked.

export type RowProp = 'id' | 'author' | 'age';
export const ROW_PROPS: RowProp[] = ['id', 'author', 'age'];

export const PROP_LABEL: Record<RowProp, string> = {
  id: 'Number',
  author: 'Author',
  age: 'Age',
};

export const COL: Record<RowProp, string> = {
  id: '72px',
  author: '22px',
  age: '40px',
};

/** The number column with the repo name in front: room for a typical
 *  name before it truncates (the full slug is in its tooltip). */
const ID_WITH_REPO = '160px';

/** A row's grid columns: pick box, status, number, title, the rest. */
export function rowColumns(props: readonly RowProp[], repo: boolean): string {
  const on = (k: RowProp) => props.includes(k);
  return [
    '16px',
    '16px',
    ...(on('id') ? [repo ? ID_WITH_REPO : COL.id] : []),
    'minmax(200px,1.5fr)',
    ...ROW_PROPS.filter((k) => k !== 'id' && on(k)).map((k) => COL[k]),
  ].join(' ');
}

export interface Display {
  groupBy: 'section' | 'status' | 'repo' | 'none';
  sort: 'activity' | 'size';
  props: RowProp[];
}

export const DEFAULT_DISPLAY: Display = {
  groupBy: 'section',
  sort: 'activity',
  props: ROW_PROPS,
};

function sortPRs(items: InboxPr[], sort: Display['sort']): InboxPr[] {
  const by =
    sort === 'size'
      ? (p: InboxPr) => -(p.additions + p.deletions)
      : (p: InboxPr) => -p.updated_at;
  return [...items].sort((a, b) => by(a) - by(b));
}

export function groupPRs(
  items: InboxPr[],
  d: Display,
): { id: string; title: string; items: InboxPr[] }[] {
  const sorted = sortPRs(items, d.sort);
  if (d.groupBy === 'none') return [{ id: 'all', title: 'All', items: sorted }];
  if (d.groupBy === 'repo') {
    const repos = [...new Set(sorted.map((p) => p.pr.repo.name))];
    return repos.map((r) => ({
      id: r,
      title: r,
      items: sorted.filter((p) => p.pr.repo.name === r),
    }));
  }
  if (d.groupBy === 'status') {
    return STATUS_ORDER.map((st) => ({
      id: st,
      title: prStatusLabel[st],
      items: sorted.filter((p) => p.state === st),
    }));
  }
  // Each PR once, in the first section its reasons reach: a PR you're
  // asked to review that also mentions you is a review request. (Listing it
  // twice also gave the list two items with one key, and the list then
  // showed the wrong rows under "Needs your review".)
  const seen = new Set<PrId>();
  return SECTIONS.map((sec) => {
    const here = sorted.filter(
      (p) => !seen.has(p.id) && p.reasons.some((r) => sec.reasons.includes(r)),
    );
    for (const p of here) seen.add(p.id);
    return { id: sec.id, title: sec.title, items: here };
  });
}

/** Wraps a `Btn` whose action has no backing command at all (not gated by
 *  `FEATURES.hostWrites` — reassigning a PR to someone else, or running a
 *  skill over a bulk selection, aren't in `.context/commands.md`): disabled,
 *  with a tooltip explaining why — never deleted, so the UI lights up with
 *  no further work the day a command lands. */
function GatedBtn({ children, ...rest }: ComponentProps<typeof Btn>) {
  const { kbd: _kbd, ...disabledRest } = rest;
  return (
    <Tooltip>
      <Btn {...disabledRest} isDisabled>
        {children}
      </Btn>
      <Tooltip.Content>Not available yet</Tooltip.Content>
    </Tooltip>
  );
}

export function ViewHeader({
  title,
  count,
  children,
}: {
  title: string;
  count?: number;
  children?: ReactNode;
}) {
  return (
    <Toolbar
      aria-label={title}
      className="view-h flex h-14 w-full min-w-0 shrink-0 items-center gap-3 overflow-x-auto border-b border-separator px-6"
    >
      <Typography.Heading
        level={6}
        className="min-w-0 shrink truncate"
        title={title}
      >
        {title}
      </Typography.Heading>
      {count !== undefined && (
        <span className="shrink-0 text-muted">{count}</span>
      )}
      <span className="flex-1" />
      <div className="flex shrink-0 items-center gap-3">{children}</div>
    </Toolbar>
  );
}

const rowCls = (sel: boolean, picked: boolean) =>
  `${sel ? 'sel' : ''} ${picked ? 'picked bg-accent-soft' : ''}`;

/** The row body; the `ListBox.Item` around it is the HeroUI row. Columns
 *  follow Display → "Row properties". */
export const PrRow = observer(function PrRow({
  id,
  sel,
  picked,
  onPick,
  props = ROW_PROPS,
  repo = true,
}: {
  id: PrId;
  sel: boolean;
  picked: boolean;
  onPick: (checked: boolean, modifiers: CheckboxRangeModifiers) => void;
  props?: RowProp[];
  /** The repo name before the number; off where every row is one repo's. */
  repo?: boolean;
}) {
  const store = useRootStore();
  const ui = useUi();
  const rangeEvents = useCheckboxRangeEvents(onPick);
  const p = store.models.prSummaries.get(id);
  const rowMenu = useContextMenu((): ContextMenuItem[] => {
    if (!p) return [];
    return [
      {
        id: 'open',
        label: 'Open',
        onAction: () => ui.go({ name: 'review', pr: id }),
      },
      {
        id: 'open-new-tab',
        label: 'Open in new tab',
        onAction: () => ui.go({ name: 'review', pr: id }, { newTab: true }),
      },
      {
        id: 'mark-done',
        label: 'Mark done',
        onAction: () => markDone(store, [id]),
      },
      {
        id: 'snooze',
        label: 'Snooze',
        items: [
          {
            id: 'snooze-1h',
            label: '1 hour',
            onAction: () =>
              snoozePrs(store, [id], {
                kind: 'time',
                at: Date.now() + 3600_000,
              }),
          },
          {
            id: 'snooze-tomorrow',
            label: 'Tomorrow',
            onAction: () =>
              snoozePrs(store, [id], {
                kind: 'time',
                at: Date.now() + 86_400_000,
              }),
          },
          {
            id: 'snooze-push',
            label: 'Until push',
            onAction: () => snoozePrs(store, [id], { kind: 'next_push' }),
          },
          {
            id: 'snooze-checks',
            label: 'Until checks green',
            onAction: () => snoozePrs(store, [id], { kind: 'checks_green' }),
          },
        ],
      },
      {
        id: 'copy-url',
        label: 'Copy URL',
        onAction: () => void navigator.clipboard.writeText(p.url),
      },
    ];
  });
  if (!p) return null;
  const on = (k: RowProp) => props.includes(k);
  const cols = rowColumns(props, repo);
  return (
    <div
      id={`R-${id}`}
      className={`prrow group grid h-11 w-full items-center gap-3 ${rowCls(sel, picked)}`}
      style={{ gridTemplateColumns: cols }}
      onContextMenu={rowMenu}
    >
      <span
        className="contents"
        onClickCapture={rangeEvents.onClickCapture}
        onBlurCapture={rangeEvents.onBlurCapture}
        onKeyDownCapture={rangeEvents.onKeyDownCapture}
      >
        <Checkbox
          aria-label="select"
          isSelected={picked}
          onChange={rangeEvents.onCheckboxChange}
          className={`pickbox ${picked ? '' : 'opacity-0 group-hover:opacity-100'}`}
        >
          <Checkbox.Content>
            <Checkbox.Control>
              <Checkbox.Indicator />
            </Checkbox.Control>
          </Checkbox.Content>
        </Checkbox>
      </span>
      <PrStatusIcon summary={p} progress={readProgress(store, id)} />
      {on('id') && (
        <span
          className="flex min-w-0 whitespace-nowrap text-xs tabular-nums text-muted"
          title={
            p.url
              ? `${p.pr.repo.owner}/${p.pr.repo.name} #${p.pr.number}`
              : `${p.pr.repo.owner}/${p.pr.repo.name} · branch ${p.head_ref}`
          }
        >
          {/* The repo name gives way first; the number always shows. A
           *  branch review has no number, only its branch. */}
          {p.url ? (
            <>
              {repo && (
                <span className="min-w-0 truncate">{p.pr.repo.name}&nbsp;</span>
              )}
              <span className="shrink-0">#{p.pr.number}</span>
            </>
          ) : (
            <span className="min-w-0 truncate">{p.head_ref}</span>
          )}
        </span>
      )}
      <span className="flex min-w-0 items-center gap-1.5">
        <span className="min-w-0 truncate font-medium" title={p.title}>
          {p.title}
        </span>
        {p.stack && <StackChips repo={p.pr.repo} stack={p.stack} />}
      </span>
      {on('author') && (
        <span className="shrink-0">
          <Avatar person={p.author} size={22} />
        </span>
      )}
      {on('age') && (
        <span className="text-right text-xs text-muted">
          {ago(p.updated_at)}
        </span>
      )}
    </div>
  );
});

/** "stacked on #n" / "n stacked" chips (plan "Stack"): a PR whose
 *  `head_ref` is this one's `base_ref` is its parent; the reverse gives
 *  children. Each chip opens the PR(s) it names. */
function StackChips({ repo, stack }: { repo: RepoRef; stack: StackInfo }) {
  const ui = useUi();
  const open = (number: number) =>
    ui.go({ name: 'review', pr: prIdFor(repo, number) });
  if (stack.base_pr == null && stack.children.length === 0) return null;
  return (
    <span
      className="stack-chips flex shrink-0 items-center gap-1"
      onClick={(e) => e.stopPropagation()}
    >
      {stack.base_pr != null && (
        <Tooltip>
          <button
            type="button"
            className="stack-chip shrink-0 rounded-full bg-default px-1.5 py-0.5 text-[10px] text-muted hover:text-foreground"
            onClick={() => open(stack.base_pr!)}
          >
            on #{stack.base_pr}
          </button>
          <Tooltip.Content>Stacked on #{stack.base_pr}</Tooltip.Content>
        </Tooltip>
      )}
      {stack.children.length === 1 ? (
        <Tooltip>
          <button
            type="button"
            className="stack-chip shrink-0 rounded-full bg-default px-1.5 py-0.5 text-[10px] text-muted hover:text-foreground"
            onClick={() => open(stack.children[0]!)}
          >
            #{stack.children[0]} stacked
          </button>
          <Tooltip.Content>
            #{stack.children[0]} is stacked on this
          </Tooltip.Content>
        </Tooltip>
      ) : stack.children.length > 1 ? (
        <Dropdown>
          <button
            type="button"
            className="stack-chip shrink-0 rounded-full bg-default px-1.5 py-0.5 text-[10px] text-muted hover:text-foreground"
          >
            {stack.children.length} stacked
          </button>
          <Dropdown.Popover placement="bottom start">
            <Dropdown.Menu
              aria-label="Stacked PRs"
              onAction={(k) => open(Number(k))}
            >
              {stack.children.map((n) => (
                <Dropdown.Item key={n} id={String(n)} textValue={`#${n}`}>
                  #{n}
                </Dropdown.Item>
              ))}
            </Dropdown.Menu>
          </Dropdown.Popover>
        </Dropdown>
      ) : null}
    </span>
  );
}

/** A section header inside a HeroUI ListBox; the caret collapses the section. */
export function SectionHeader({
  title,
  count,
  open,
  toggle,
}: {
  title: ReactNode;
  count: number;
  open: boolean;
  toggle: () => void;
}) {
  return (
    <Header className="ghead sticky top-0 z-[1] flex h-10 select-none items-center gap-2 border-b border-separator bg-surface/95 pl-[18px] pr-6 text-sm backdrop-blur">
      <Button
        isIconOnly
        size="sm"
        variant="ghost"
        aria-label={open ? 'collapse' : 'expand'}
        className="size-6 min-w-6 text-muted"
        onPress={toggle}
      >
        {open ? (
          <ChevronDown className="size-3.5" />
        ) : (
          <ChevronRight className="size-3.5" />
        )}
      </Button>
      <span className="font-medium">{title}</span>
      <span className="tabular-nums text-muted">{count}</span>
    </Header>
  );
}

/** A Linear-style list as one HeroUI ListBox: sections, single selection, ⏎ / double-click opens. */
export function RowList({
  groups,
  sel,
  closed,
  toggle,
  onSelect,
  onOpen,
  row,
  empty,
}: {
  groups: { id: string; title: ReactNode; items: InboxPr[] }[];
  sel: PrId | null;
  closed: Record<string, boolean>;
  toggle: (id: string) => void;
  onSelect: (id: PrId) => void;
  onOpen: (id: PrId) => void;
  row: (id: PrId) => ReactNode;
  empty?: ReactNode;
}) {
  if (empty && groups.every((g) => !g.items.length)) return <>{empty}</>;
  return (
    <ListBox
      aria-label="Pull requests"
      selectionMode="single"
      selectionBehavior="replace"
      disallowEmptySelection
      selectedKeys={sel ? [sel] : []}
      onSelectionChange={(keys) => {
        const k = keys === 'all' ? null : [...keys][0];
        if (k !== undefined && k !== null) onSelect(String(k));
      }}
      onAction={(k: Key) => onOpen(String(k))}
      className="list min-w-0 flex-1 overflow-auto p-0 pb-20"
    >
      {groups.map((g) => (
        // The collection caches a section's header by key: without the count
        // and open state in it, "Snoozed 1" stayed after an undo emptied it.
        <ListBox.Section
          key={`${g.id}:${g.items.length}:${closed[g.id] ? 0 : 1}`}
          id={g.id}
        >
          <SectionHeader
            title={g.title}
            count={g.items.length}
            open={!closed[g.id]}
            toggle={() => toggle(g.id)}
          />
          {(closed[g.id] ? [] : g.items).map((p) => (
            <ListBox.Item
              key={p.id}
              id={p.id}
              textValue={p.title}
              className="min-h-0 rounded-none border-b border-separator px-6 py-0"
            >
              {row(p.id)}
            </ListBox.Item>
          ))}
        </ListBox.Section>
      ))}
    </ListBox>
  );
}

/** Bulk "Reassign" (plan "Triage"): one login, applied to every selected
 *  PR, from `firstPr`'s `assignable_users` (a stand-in for "whoever can
 *  review any of these" when the selection spans more than one repo). */
function BulkReassign({
  firstPr,
  onReassign,
}: {
  firstPr: PrId;
  onReassign: (login: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [users, setUsers] = useState<Person[] | null>(null);

  useEffect(() => {
    if (!open || users) return;
    let live = true;
    void api
      .assignableUsers(firstPr)
      .then((u) => live && setUsers(u))
      .catch(() => live && setUsers([]));
    return () => {
      live = false;
    };
  }, [open, firstPr, users]);

  return (
    <Dropdown isOpen={open} onOpenChange={setOpen}>
      <Btn tone="tertiary" icon={<Persons />}>
        Reassign
      </Btn>
      <Dropdown.Popover placement="top">
        {users === null ? (
          <div className="flex items-center gap-2 px-4 py-3 text-sm text-muted">
            Loading…
          </div>
        ) : (
          <Dropdown.Menu
            aria-label="Reassign to"
            onAction={(login) => onReassign(String(login))}
          >
            {users.map((u) => (
              <Dropdown.Item key={u.login} id={u.login} textValue={u.login}>
                {u.login}
              </Dropdown.Item>
            ))}
          </Dropdown.Menu>
        )}
      </Dropdown.Popover>
    </Dropdown>
  );
}

export function BulkBar({
  n,
  firstPr,
  onDone,
  onSnooze,
  onReassign,
  onClear,
}: {
  n: number;
  firstPr: PrId;
  onDone: () => void;
  onSnooze: () => void;
  onReassign: (login: string) => void;
  onClear: () => void;
}) {
  return (
    <Card className="bulk absolute bottom-16 left-1/2 z-10 max-w-[calc(100%-2rem)] -translate-x-1/2 px-4 py-2.5 shadow-overlay">
      <Toolbar
        aria-label="Bulk actions"
        className="flex items-center gap-1.5 overflow-x-auto"
      >
        <span className="me-2 ps-1 shrink-0 font-medium tabular-nums">
          {n} selected
        </span>
        <Btn tone="tertiary" kbd="E" icon={<Archive />} onPress={onDone}>
          Done
        </Btn>
        <Btn tone="tertiary" kbd="H" icon={<Clock />} onPress={onSnooze}>
          Snooze
        </Btn>
        <BulkReassign firstPr={firstPr} onReassign={onReassign} />
        <GatedBtn tone="tertiary" icon={<Sparkles />}>
          Pre-review
        </GatedBtn>
        <Btn
          tone="ghost"
          kbd="Esc"
          aria-label="clear selection"
          onPress={onClear}
        />
      </Toolbar>
    </Card>
  );
}

export function DisplayRow({
  label,
  children,
}: {
  label: string;
  children: ReactNode;
}) {
  return (
    <div className="flex items-center justify-between gap-4">
      <Label className="text-sm text-muted">{label}</Label>
      {children}
    </div>
  );
}

export function Picker<T extends string>({
  label,
  value,
  options,
  onChange,
}: {
  label: string;
  value: T;
  options: [T, string][];
  onChange: (v: T) => void;
}) {
  return (
    <Select
      aria-label={label}
      value={value}
      onChange={(v) => v && onChange(v as T)}
      className="w-44"
    >
      <Select.Trigger>
        <Select.Value />
        <Select.Indicator />
      </Select.Trigger>
      <Select.Popover>
        <ListBox selectionMode="single">
          {options.map(([id, t]) => (
            <ListBox.Item key={id} id={id} textValue={t}>
              {t}
              <ListBox.ItemIndicator />
            </ListBox.Item>
          ))}
        </ListBox>
      </Select.Popover>
    </Select>
  );
}

/** Linear's «Display»: grouping, ordering, which properties a row shows.
 *  The prototype also toggled list/board layout here, but our app has no
 *  board/`PRView` route to switch to (see the file's final report), so the
 *  layout toggle is dropped and this popover only ever drives the inbox. */
function DisplayButton({
  display,
  setDisplay,
  showDone,
  setShowDone,
}: {
  display: Display;
  setDisplay: (d: Partial<Display>) => void;
  showDone: boolean;
  setShowDone: (v: boolean) => void;
}) {
  return (
    <Popover>
      <Button size="sm" variant="tertiary" className="gap-1.5">
        <Sliders className="size-4" />
        Display
      </Button>
      <Popover.Content
        placement="bottom end"
        offset={8}
        className="w-[340px] p-0"
      >
        <Popover.Dialog className="display-pop flex flex-col gap-0 p-0">
          <div className="flex items-center justify-between px-4 py-3">
            <Popover.Heading className="text-sm font-semibold">
              Display
            </Popover.Heading>
            <Button
              size="sm"
              variant="ghost"
              className="h-7 gap-1 px-2 text-xs text-muted"
              onPress={() => setDisplay(DEFAULT_DISPLAY)}
            >
              <ArrowRotateLeft className="size-3.5" />
              Reset
            </Button>
          </div>

          <Separator />
          <div className="flex flex-col gap-3 px-4 py-3">
            <DisplayRow label="Grouping">
              <Picker
                label="Grouping"
                value={display.groupBy}
                onChange={(v) => setDisplay({ groupBy: v })}
                options={[
                  ['section', 'By reason'],
                  ['status', 'By status'],
                  ['repo', 'By repository'],
                  ['none', 'No grouping'],
                ]}
              />
            </DisplayRow>
            <DisplayRow label="Ordering">
              <Picker
                label="Ordering"
                value={display.sort}
                onChange={(v) => setDisplay({ sort: v })}
                options={[
                  ['activity', 'By activity'],
                  ['size', 'By size'],
                ]}
              />
            </DisplayRow>
            <DisplayRow label="Show Done">
              <Switch
                size="sm"
                aria-label="Show Done"
                isSelected={showDone}
                onChange={() => setShowDone(!showDone)}
              >
                <Switch.Content>
                  <Switch.Control>
                    <Switch.Thumb />
                  </Switch.Control>
                </Switch.Content>
              </Switch>
            </DisplayRow>
          </div>

          <Separator />
          <div className="flex flex-col gap-2.5 px-4 pb-4 pt-3">
            <Label className="text-sm text-muted">Row properties</Label>
            <TagGroup
              aria-label="Row properties"
              size="sm"
              selectionMode="multiple"
              selectionBehavior="toggle"
              escapeKeyBehavior="none"
              selectedKeys={new Set(display.props)}
              onSelectionChange={(keys) =>
                setDisplay({
                  props: ROW_PROPS.filter((p) => keys === 'all' || keys.has(p)),
                })
              }
            >
              <TagGroup.List className="flex flex-wrap gap-1.5">
                {ROW_PROPS.map((p) => (
                  <Tag key={p} id={p} textValue={PROP_LABEL[p]}>
                    {PROP_LABEL[p]}
                  </Tag>
                ))}
              </TagGroup.List>
            </TagGroup>
          </div>
        </Popover.Dialog>
      </Popover.Content>
    </Popover>
  );
}

/** The list keeps at least this much room; Peek only shows beside it when
 *  the page is at least this plus Peek's own minimum width. */
const LIST_MIN_WIDTH = 560;
const PEEK_MIN_WIDTH = 280;
export const PEEK_FITS_FROM = LIST_MIN_WIDTH + PEEK_MIN_WIDTH;

/** Whether Peek fits beside the list in a page `width` px wide. Narrower,
 *  it hides itself — `ui.peek` stays on and it comes back on widening. */
export function peekFits(width: number): boolean {
  return width >= PEEK_FITS_FROM;
}

export function PeekToggle({
  on,
  toggle,
  fits = true,
}: {
  on: boolean;
  toggle: () => void;
  /** `false` when the window is too narrow for Peek beside the list. */
  fits?: boolean;
}) {
  return (
    <Tooltip>
      <ToggleButton
        isIconOnly
        size="sm"
        aria-label="Preview"
        isSelected={on && fits}
        isDisabled={!fits}
        onChange={toggle}
      >
        <LayoutSideContentRight className="size-4" />
      </ToggleButton>
      <Tooltip.Content>
        {!fits ? (
          'Widen the window to show the preview'
        ) : (
          <>
            {on ? 'Hide' : 'Show'} preview <K>Space</K>
          </>
        )}
      </Tooltip.Content>
    </Tooltip>
  );
}

function ListBody({
  groups,
  sel,
  picked,
  onSelect,
  onOpen,
  props,
  empty,
}: {
  groups: { id: string; title: string; items: InboxPr[] }[];
  sel: PrId | null;
  picked: PrId[];
  onSelect: (id: PrId) => void;
  onOpen: (id: PrId) => void;
  props: RowProp[];
  empty?: ReactNode;
}) {
  const [closed, setClosed] = useState<Record<string, boolean>>({
    snoozed: true,
  });
  const ui = useUi();
  const store = useRootStore();
  const range = useCheckboxRange({
    scopeKey: `inbox:${store.workspaces.id}`,
    orderedKeys: groups.flatMap((g) =>
      closed[g.id] ? [] : g.items.map((p) => p.id),
    ),
    onChange: (ids, checked) => ui.setPicked(ids, checked),
  });
  return (
    <RowList
      groups={groups}
      sel={sel}
      closed={closed}
      toggle={(id) => setClosed((c) => ({ ...c, [id]: !c[id] }))}
      onSelect={onSelect}
      onOpen={onOpen}
      row={(id) => (
        <PrRow
          id={id}
          sel={sel === id}
          picked={picked.includes(id)}
          onPick={(checked, modifiers) =>
            range.activate(id, checked, modifiers)
          }
          props={props}
        />
      )}
      empty={empty}
    />
  );
}

const order = (groups: { items: InboxPr[] }[]) =>
  groups.flatMap((g) => g.items.map((p) => p.id));

/** Where the selection goes when the list changes under it, `null` to keep
 *  it. The selected PR leaving what's waiting (done, snoozed) moves it to
 *  the row that took its place; a snoozed or done row you picked yourself
 *  keeps it, so its Peek shows. */
export function followSelection({
  sel,
  prev,
  order,
  shown,
  lastIndex,
}: {
  sel: PrId | null;
  /** The selection last time, and whether it was waiting then. */
  prev: { sel: PrId | null; inOrder: boolean };
  /** What's waiting, in order: J/K's rows. */
  order: PrId[];
  /** Every row on screen, snoozed and done ones too. */
  shown: PrId[];
  lastIndex: number;
}): PrId | null {
  if (sel && order.includes(sel)) return null;
  const left = !!sel && sel === prev.sel && prev.inOrder;
  if (sel && !left && shown.includes(sel)) return null;
  return order.length ? order[Math.min(lastIndex, order.length - 1)]! : null;
}

/** Linear-style single-key verbs for the list. */
function useListKeys(order: PrId[], shown: PrId[]) {
  const store = useRootStore();
  const ui = useUi();
  useViewKeys(
    ui,
    (e) => {
      const i = ui.selected ? order.indexOf(ui.selected) : -1;
      const move = (d: number) => {
        const next = order[Math.max(0, Math.min(order.length - 1, i + d))];
        if (next) ui.select(next);
      };
      const targets = () =>
        ui.picked.length ? ui.picked : ui.selected ? [ui.selected] : [];
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
        case 'x':
          if (ui.selected) ui.togglePick(ui.selected);
          break;
        case 'e':
          markDone(store, targets());
          ui.clearPicked();
          break;
        case 'h':
          ui.openModal('snooze', targets());
          break;
        case 'Escape':
          if (!ui.picked.length) return false;
          ui.clearPicked();
          break;
        default:
          return false;
      }
    },
    [order.join()],
  );

  // When the selected PR leaves the list (done, snoozed), select the one
  // that took its place — not the top of the list.
  const lastIndex = useRef(0);
  const prev = useRef<{ sel: PrId | null; inOrder: boolean }>({
    sel: null,
    inOrder: false,
  });
  useEffect(() => {
    const next = followSelection({
      sel: ui.selected,
      prev: prev.current,
      order,
      shown,
      lastIndex: lastIndex.current,
    });
    prev.current = {
      sel: ui.selected,
      inOrder: !!ui.selected && order.includes(ui.selected),
    };
    if (ui.selected && order.includes(ui.selected))
      lastIndex.current = order.indexOf(ui.selected);
    if (next) ui.select(next);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [order.join(), ui.selected]);

  useEffect(() => {
    if (ui.selected)
      document
        .getElementById(`R-${ui.selected}`)
        ?.scrollIntoView({ block: 'nearest' });
  }, [ui.selected]);
}

// ── screens ─────────────────────────────────────────────────────────────

export const Inbox = observer(function Inbox() {
  const store = useRootStore();
  const ui = useUi();
  const [refreshing, setRefreshing] = useState(false);
  const pageRef = useRef<HTMLDivElement>(null);
  const fits = peekFits(useElementWidth(pageRef, PEEK_FITS_FROM));
  // Per workspace, in its UI snapshot.
  const showDone = ui.inboxShowDone;
  const setShowDone = (v: boolean) => ui.setInboxShowDone(v);
  const display = ui.inboxDisplay ?? DEFAULT_DISPLAY;
  const setDisplay = (patch: Partial<Display>) =>
    ui.setInboxDisplay({ ...display, ...patch });

  useEffect(() => {
    void refreshInbox(store);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const refresh = () => {
    setRefreshing(true);
    void refreshInbox(store).finally(() => setRefreshing(false));
  };

  const prs = store.workspaces
    .scopePrs(store.models.prSummaries.all())
    .filter(inInbox);
  const inboxStates = store.models.inboxStates.all();
  // First state per PR, as `find` would pick — one pass, not one per row.
  const stateByPr = new Map<string, (typeof inboxStates)[number]>();
  for (const s of inboxStates) if (!stateByPr.has(s.pr)) stateByPr.set(s.pr, s);
  const stateOf = (id: PrId) => stateByPr.get(id);
  const active = prs.filter((p) => {
    const s = stateOf(p.id);
    return !s?.done && !s?.snooze;
  });
  const snoozed = prs.filter((p) => stateOf(p.id)?.snooze);
  const done = prs.filter((p) => stateOf(p.id)?.done);

  const groups = [
    ...groupPRs(active, display).filter((g) => g.items.length),
    { id: 'snoozed', title: 'Snoozed', items: snoozed },
    ...(showDone ? [{ id: 'done', title: 'Done', items: done }] : []),
  ].filter((g) => g.items.length || g.id === 'snoozed');

  // ⌘F (search.ts): filtered groups replace the normal ones everywhere —
  // navigation, Peek and the list itself — so J/K and Enter already work
  // on just the matches.
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

  // J/K and the Peek follow what's waiting: a PR you just snoozed or marked
  // done leaves the selection, which moves on to the next one.
  useListKeys(
    order(visibleGroups.filter((g) => g.id !== 'snoozed' && g.id !== 'done')),
    order(visibleGroups),
  );

  return (
    <div className="view relative flex min-w-0 flex-1 flex-col">
      <ViewHeader title="Inbox" count={active.length}>
        <Tooltip>
          <Button
            isIconOnly
            size="sm"
            variant="ghost"
            aria-label="Refresh"
            isPending={refreshing}
            onPress={refresh}
          >
            <ArrowRotateLeft className="size-4" />
          </Button>
          <Tooltip.Content>Refresh inbox</Tooltip.Content>
        </Tooltip>
        <DisplayButton
          display={display}
          setDisplay={setDisplay}
          showDone={showDone}
          setShowDone={setShowDone}
        />
        <PeekToggle on={ui.peek} fits={fits} toggle={() => ui.togglePeek()} />
      </ViewHeader>
      {query !== null && (
        <ListSearchBar
          query={query}
          onQueryChange={(q) => ui.setListSearchQuery(q)}
          order={order(
            visibleGroups.filter((g) => g.id !== 'snoozed' && g.id !== 'done'),
          )}
          selected={ui.selected}
          onSelect={(id) => ui.select(id as PrId)}
          onOpen={(id) => ui.go({ name: 'review', pr: id as PrId })}
          onClose={() => ui.closeListSearch()}
          placeholder="Search inbox…"
          reopenToken={ui.listSearchFocusAt}
        />
      )}
      <div ref={pageRef} className="flex min-h-0 flex-1">
        <ListBody
          groups={visibleGroups}
          sel={ui.selected}
          picked={ui.picked}
          onSelect={(id) => ui.select(id)}
          onOpen={(id) => ui.go({ name: 'review', pr: id })}
          props={display.props}
          empty={
            store.ephemeral.inboxLoading ? (
              <div className="flex-1">
                <RowsSkeleton rows={6} />
              </div>
            ) : (
              <Empty
                icon={<CodePullRequest />}
                title="Inbox is empty"
                text="Wait for a review request to show up here, or open a pull request by link."
                action={
                  <Btn
                    tone="tertiary"
                    icon={<Link className="size-4" />}
                    onPress={() => ui.newTab()}
                  >
                    Open a PR link…
                  </Btn>
                }
              />
            )
          }
        />
        {ui.peek && fits && ui.selected && <Peek id={ui.selected} />}
      </div>
      <ListHints />
      {ui.picked.length > 0 && (
        <BulkBar
          n={ui.picked.length}
          firstPr={ui.picked[0]!}
          onDone={() => {
            markDone(store, ui.picked);
            ui.clearPicked();
          }}
          onSnooze={() => ui.openModal('snooze', ui.picked)}
          onReassign={(login) => {
            const ids = ui.picked;
            for (const id of ids) {
              void triage(store, id, { action: 'reassign', to: [login] });
            }
            ui.clearPicked();
          }}
          onClear={() => ui.clearPicked()}
        />
      )}
    </div>
  );
});

function ListHints() {
  return (
    <Hints>
      <K>J</K>
      <K>K</K> navigate <K>⏎</K> open <K>Space</K> preview <K>X</K> select{' '}
      <K>E</K> done <K>H</K> snooze <K>⌘F</K> search <K>⌘K</K> everything{' '}
      <K>⌘Z</K> undo
    </Hints>
  );
}
