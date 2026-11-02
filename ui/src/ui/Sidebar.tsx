import {
  ArrowRightFromSquare,
  BranchesRight,
  ChevronDown,
  CircleExclamation,
  CircleFill,
  CircleXmark,
  CodePullRequest,
  Gear,
  LayoutSideContentLeft,
  ListCheck,
  Magnifier,
  Pencil,
  Pin,
  PinSlash,
  Plus,
  Sparkles,
  Tray,
} from '@gravity-ui/icons';
import {
  Button,
  Dropdown,
  Header,
  Kbd,
  Label,
  ListBox,
  Separator,
  Toolbar,
  Tooltip,
} from '@heroui/react';
import { observer } from 'mobx-react-lite';
import { useEffect, useState, type ReactNode } from 'react';
import type { Space } from '../gen/contract';
import { useRootStore, useUi } from '../stores/context';
import { sameRoute, type Route } from '../stores/UiStore';
import { api } from '../core/api';
import { Avatar, K, Spinner } from './bits';
import { contextMenuHandler, type ContextMenuItem } from './ContextMenu';
import { ListSearchBar } from './ListSearchBar';
import { inInbox } from './List';
import { openPrCount } from './prs/filters';
import { matchesSearch, spaceSearchText } from './search';
import { sectionsOf, type Section } from '../core/workspaces';
import { SandboxBadge } from './sandbox/SandboxBadge';
import { WorkspaceSwitcher } from './WorkspaceSwitcher';
import { markSpaceUnseen, spaceSeen } from './space/seen';
import { isBuildMode } from './space/mode';
import { PrGlyph } from './space/SpacePr';
import {
  canMarkUnread,
  spaceGlyph,
  spaceTitle,
  spacesIn,
  type SpaceGlyph,
} from './space/spaces';

interface NavItem {
  key: string;
  icon: ReactNode;
  label: string;
  n?: number;
  route?: Route;
  /** An action instead of a route, e.g. "New space". */
  onPress?: () => void;
  /** Right-click menu for this row, built lazily (`ContextMenu.tsx`). */
  contextMenu?: () => ContextMenuItem[];
  /** A pinned space (docs/spaces.md §3): sorts first, marked with a small pin. */
  pinned?: boolean;
}

/** Pin/unpin and "Mark as unread" (docs/spaces.md §3) — the sidebar's
 *  right-click menu and ⌘K share this. */
export function spaceContextMenu(s: Space): ContextMenuItem[] {
  const pinned = s.pinned ?? false;
  return [
    {
      id: 'pin',
      label: pinned ? 'Unpin' : 'Pin',
      icon: pinned ? (
        <PinSlash className="size-4" />
      ) : (
        <Pin className="size-4" />
      ),
      onAction: () =>
        void api
          .setSpacePinned({ space: s.id, pinned: !pinned })
          .catch(() => {}),
    },
    ...(canMarkUnread(s, spaceSeen.get(s.id))
      ? [
          {
            id: 'mark-unread',
            label: 'Mark as unread',
            icon: <CircleFill className="size-3" />,
            onAction: () => markSpaceUnseen(s.id),
          },
        ]
      : []),
  ];
}

interface NavGroup {
  id: string;
  title?: string;
  items: NavItem[];
}

const routeKey = (r: Route) => JSON.stringify(r);

/** A space's run state at a glance (docs/spaces.md §3). */
function SpaceGlyphIcon({ g }: { g: SpaceGlyph }) {
  switch (g) {
    case 'running':
      return <Spinner />;
    case 'unseen':
      return (
        <span
          className="size-2 rounded-full bg-accent"
          aria-label="Turn finished"
        />
      );
    case 'failed':
      return <CircleXmark className="text-danger" aria-label="Failed" />;
    case 'idle':
      return <Pencil />;
  }
}

function IconBtn({
  label,
  onPress,
  children,
}: {
  label: string;
  onPress: () => void;
  children: ReactNode;
}) {
  return (
    <Tooltip>
      <Button
        isIconOnly
        size="sm"
        variant="ghost"
        aria-label={label}
        className="size-7 min-w-7 text-muted"
        onPress={onPress}
      >
        {children}
      </Button>
      <Tooltip.Content>{label}</Tooltip.Content>
    </Tooltip>
  );
}

/** Linear's sidebar as one HeroUI ListBox: single selection = current route. */
export const Sidebar = observer(function Sidebar() {
  const store = useRootStore();
  const ui = useUi();
  // ── workspaces: sections of the selected workspace ──────────────────
  // Everything below the switcher is scoped to the active workspace; a
  // local folder has no Inbox or Pull requests, and only a single project
  // has Branches. The switcher itself (All projects + every project) is
  // `WorkspaceSwitcher`, rendered above this list.
  const ws = store.workspaces;
  const prs = ws.scopePrs(store.models.prSummaries.all());
  const inboxStates = store.models.inboxStates.all();
  // First state per PR, as `find` would pick — one pass, not one per row.
  const stateByPr = new Map<string, (typeof inboxStates)[number]>();
  for (const s of inboxStates) if (!stateByPr.has(s.pr)) stateByPr.set(s.pr, s);
  const stateOf = (pr: string) => stateByPr.get(pr);
  const inboxCount = prs.filter((p) => {
    if (!inInbox(p)) return false;
    const s = stateOf(p.id);
    return !s?.done && !s?.snooze;
  }).length;
  const tasksOpen = ws.scopeTasks(
    store.models.tasks.where((t) => t.status !== 'done'),
  ).length;
  const viewer = store.ephemeral.viewer;
  // Who is signed in to Theodore; the host login only stands in when the
  // build has no Theodore account (sandbox, fixtures, a dev build).
  const user = store.ephemeral.account?.user ?? null;
  const me = user
    ? {
        person: {
          login: user.email,
          name: user.name,
          avatar_url: user.avatar_url,
        },
        label: user.name ?? user.email,
        title: user.email,
      }
    : viewer
      ? {
          person: viewer,
          label: viewer.login,
          title: viewer.name ?? viewer.login,
        }
      : null;

  const all: Record<Section, NavItem> = {
    inbox: {
      key: routeKey({ name: 'inbox' }),
      icon: <Tray />,
      label: 'Inbox',
      n: inboxCount,
      route: { name: 'inbox' },
    },
    prs: {
      key: routeKey({ name: 'prs' }),
      icon: <CodePullRequest />,
      label: 'Pull requests',
      n: openPrCount(prs),
      route: { name: 'prs' },
    },
    branches: {
      key: routeKey({ name: 'branches' }),
      icon: <BranchesRight />,
      label: 'Branches',
      route: { name: 'branches' },
    },
    tasks: {
      key: routeKey({ name: 'tasks' }),
      icon: <ListCheck />,
      label: 'Tasks',
      n: tasksOpen,
      route: { name: 'tasks' },
    },
    skills: {
      key: routeKey({ name: 'skills' }),
      icon: <Sparkles />,
      label: 'Skills',
      route: { name: 'skills' },
    },
  };
  // Spaces (docs/spaces.md): only with build mode on.
  const spaces = isBuildMode(store)
    ? spacesIn(ui.workspace, store.models.spaces.all())
    : null;
  // ⌘F over the Spaces section (search.ts): while it's open, the section's
  // own search block takes over below and this group is left out of the
  // ListBox, so the two never show at once.
  const spacesQuery =
    ui.listSearch?.scope === 'spaces' ? ui.listSearch.query : null;
  const visibleSpaces =
    spaces && spacesQuery !== null
      ? spaces.filter((s) => matchesSearch(spaceSearchText(s), spacesQuery))
      : spaces;
  const [spaceHi, setSpaceHi] = useState<string | null>(null);
  useEffect(() => {
    const order = visibleSpaces?.map((s) => s.id) ?? [];
    if (!spaceHi || !order.includes(spaceHi)) setSpaceHi(order[0] ?? null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [spacesQuery, visibleSpaces?.map((s) => s.id).join(',')]);

  const groups: NavGroup[] = [
    { id: 'main', items: sectionsOf(ws.kind).map((k) => all[k]) },
    ...(spaces && spacesQuery === null
      ? [
          {
            id: 'spaces',
            title: 'Spaces',
            items: [
              ...spaces.map((s): NavItem => {
                const g = spaceGlyph(s, spaceSeen.get(s.id));
                // In review: `⎇ #n`, tinted by the PR's checks, unless a
                // turn runs, failed or waits for you.
                const pr = s.pr
                  ? store.models.prSummaries.get(s.pr)
                  : undefined;
                return {
                  key: routeKey({ name: 'space', id: s.id }),
                  icon:
                    s.pr && g === 'idle' ? (
                      <PrGlyph pr={pr} />
                    ) : (
                      <SpaceGlyphIcon g={g} />
                    ),
                  label: pr
                    ? `#${pr.pr.number} ${spaceTitle(s)}`
                    : spaceTitle(s),
                  route: { name: 'space', id: s.id },
                };
              }),
              ...spaces.map((s): NavItem => ({
                key: routeKey({ name: 'space', id: s.id }),
                icon: <SpaceGlyphIcon g={spaceGlyph(s, spaceSeen.get(s.id))} />,
                label: spaceTitle(s),
                route: { name: 'space', id: s.id },
                contextMenu: () => spaceContextMenu(s),
                pinned: s.pinned ?? false,
              })),
              {
                key: 'new-space',
                icon: <Plus />,
                label: 'New space',
                onPress: () => ui.openModal('new-space'),
              },
            ],
          },
        ]
      : []),
  ];
  const items = groups.flatMap((g) => g.items);
  // ── /workspaces ─────────────────────────────────────────────────────────
  const current = items.find((i) => i.route && sameRoute(i.route, ui.route));

  return (
    <aside className="sidebar flex w-64 shrink-0 flex-col overflow-hidden border-r border-separator bg-background">
      <Toolbar
        aria-label="Workspace"
        data-tauri-drag-region
        className="flex w-full items-center gap-1.5 pb-3 pe-4 ps-3.5 pt-[calc(var(--controls-bottom)+0.75rem)]"
      >
        <WorkspaceSwitcher />
        <Tooltip>
          <Button
            size="sm"
            variant="ghost"
            aria-label="Commands"
            className="h-7 gap-1.5 px-2 text-muted"
            onPress={() => ui.openModal('cmdk')}
          >
            <Magnifier className="size-4" />
            <K>⌘K</K>
          </Button>
          <Tooltip.Content>Find a command</Tooltip.Content>
        </Tooltip>
      </Toolbar>

      <ListBox
        aria-label="Navigation"
        // Navigation, not a selection: every press acts, the current route
        // is just styled. A selection list swallowed the first press after
        // a dialog closed (it only moved focus there).
        selectionMode="none"
        onAction={(k) => {
          const it = items.find((i) => i.key === String(k));
          if (it?.route) ui.go(it.route);
          else it?.onPress?.();
        }}
        className="nav-list min-h-0 flex-1 overflow-auto px-2 pb-3"
      >
        {groups.map((g) => (
          <ListBox.Section key={g.id} id={g.id}>
            {g.title && (
              <Header className="px-3 pb-1.5 pt-6 text-xs text-muted">
                {g.title}
              </Header>
            )}
            {g.items.map((i) => (
              <ListBox.Item
                key={i.key}
                id={i.key}
                textValue={i.label}
                aria-current={current?.key === i.key ? 'page' : undefined}
                className={`nav group gap-3 px-3 text-sm ${current?.key === i.key ? 'bg-default font-medium' : ''}`}
              >
                {/* `contents`: a right-click target with no layout of its
                    own, same trick as `PrRow`'s range wrapper. */}
                <span
                  className="contents"
                  onContextMenu={
                    i.contextMenu
                      ? contextMenuHandler(ui, i.contextMenu)
                      : undefined
                  }
                >
                  <span className="grid size-4 shrink-0 place-items-center text-muted [&>svg]:size-4">
                    {i.icon}
                  </span>
                  <span className="min-w-0 flex-1 truncate" title={i.label}>
                    {i.label}
                  </span>
                  {i.pinned && (
                    <Pin
                      className="size-3 shrink-0 text-muted"
                      aria-label="Pinned"
                    />
                  )}
                  {i.n !== undefined && i.n !== 0 && (
                    <span className="shrink-0 text-xs tabular-nums text-muted">
                      {i.n}
                    </span>
                  )}
                </span>
              </ListBox.Item>
            ))}
          </ListBox.Section>
        ))}
      </ListBox>

      {spaces && spacesQuery !== null && (
        <div className="spaces-search flex min-h-0 flex-col border-t border-separator">
          <Header className="px-3 pb-1.5 pt-2 text-xs text-muted">
            Spaces
          </Header>
          <ListSearchBar
            query={spacesQuery}
            onQueryChange={(q) => ui.setListSearchQuery(q)}
            order={visibleSpaces?.map((s) => s.id) ?? []}
            selected={spaceHi}
            onSelect={setSpaceHi}
            onOpen={(id) => {
              ui.go({ name: 'space', id });
              ui.closeListSearch();
            }}
            onClose={() => ui.closeListSearch()}
            placeholder="Search spaces…"
            reopenToken={ui.listSearchFocusAt}
          />
          <div className="flex min-h-0 flex-col overflow-auto px-2 pb-2">
            {(visibleSpaces ?? []).map((s) => (
              <button
                key={s.id}
                type="button"
                className={`nav flex items-center gap-3 rounded-lg px-3 py-1.5 text-start text-sm hover:bg-default ${
                  spaceHi === s.id ? 'bg-default font-medium' : ''
                }`}
                onClick={() => {
                  ui.go({ name: 'space', id: s.id });
                  ui.closeListSearch();
                }}
                onContextMenu={contextMenuHandler(ui, () =>
                  spaceContextMenu(s),
                )}
              >
                <span className="grid size-4 shrink-0 place-items-center text-muted [&>svg]:size-4">
                  <SpaceGlyphIcon g={spaceGlyph(s, spaceSeen.get(s.id))} />
                </span>
                <span className="min-w-0 flex-1 truncate" title={spaceTitle(s)}>
                  {spaceTitle(s)}
                </span>
                {s.pinned && (
                  <Pin
                    className="size-3 shrink-0 text-muted"
                    aria-label="Pinned"
                  />
                )}
              </button>
            ))}
          </div>
        </div>
      )}

      <Separator />
      <Toolbar
        aria-label="App"
        className="flex w-full items-center gap-1 px-5 pb-3 pt-2.5"
      >
        <Dropdown>
          <Button
            variant="ghost"
            size="sm"
            aria-label="Account and app"
            className="account-menu -ms-2 h-9 min-w-0 flex-1 justify-start gap-2 rounded-xl px-2"
          >
            {me ? (
              <>
                <Avatar person={me.person} size={24} />
                <span className="min-w-0 truncate text-sm" title={me.title}>
                  {me.label}
                </span>
              </>
            ) : (
              <span className="min-w-0 truncate text-sm text-muted">
                Not signed in
              </span>
            )}
            <ChevronDown className="size-3 shrink-0 text-muted" />
          </Button>
          <Dropdown.Popover placement="top start">
            <Dropdown.Menu
              aria-label="Account and app"
              onAction={(k) => {
                if (k === 'settings')
                  ui.go({ name: 'settings', page: 'account' });
                else if (k === 'report') ui.openModal('report-problem');
                else if (k === 'signout')
                  void api
                    .accountSignOut()
                    .then((st) => store.ephemeral.setAccount(st))
                    .catch(() => {});
              }}
            >
              <Dropdown.Item id="settings" textValue="Settings">
                <Gear className="size-4" />
                <Label>Settings</Label>
                <Kbd className="ms-auto">G S</Kbd>
              </Dropdown.Item>
              <Dropdown.Item id="report" textValue="Report a problem">
                <CircleExclamation className="size-4" />
                <Label>Report a problem…</Label>
              </Dropdown.Item>
              {/* Always listed; a build without a Theodore account (sandbox,
                  fixtures, a dev build) has no one to sign out. */}
              <Dropdown.Item
                id="signout"
                textValue="Sign out"
                isDisabled={!store.ephemeral.account?.user}
              >
                <ArrowRightFromSquare className="size-4" />
                <Label>Sign out</Label>
              </Dropdown.Item>
            </Dropdown.Menu>
          </Dropdown.Popover>
        </Dropdown>
        <SandboxBadge />
        <IconBtn label="Collapse sidebar  [" onPress={() => ui.toggleSidebar()}>
          <LayoutSideContentLeft className="size-4" />
        </IconBtn>
      </Toolbar>
    </aside>
  );
});
