import { openExternal } from '../core/openExternal';
import { latinKey } from './latin';
import { findScopeFor } from './search';
import { isBuildMode } from './space/mode';
import {
  archivedSpacesIn,
  canMarkUnread,
  spaceTitle,
  spacesIn,
  undoState,
} from './space/spaces';
import { useEffect, useRef, useState, type Key, type ReactNode } from 'react';
import {
  EmptyState,
  Header,
  ListBox,
  Modal,
  ScrollShadow,
} from '@heroui/react';
import {
  ArrowRotateLeft,
  ArrowUpRightFromSquare,
  ArrowUturnCcwLeft,
  BranchesRight,
  CircleFill,
  Clock,
  CodePullRequest,
  Eye,
  FileText,
  Folder,
  FolderPlus,
  Gear,
  Keyboard,
  LayoutColumns,
  LayoutSideContentLeft,
  ListCheck,
  Moon,
  PaperPlane,
  Pencil,
  Pin,
  PinSlash,
  Plus,
  Tray,
} from '@gravity-ui/icons';
import { observer } from 'mobx-react-lite';
import type { AgentId, Person, SnoozeUntil } from '../gen/contract';
import {
  assignTask,
  delegateTask,
  refreshInbox,
  setDraft,
  snoozePrs,
} from '../actions';
import { api } from '../core/api';
import { FEATURES } from '../features';
import { useRootStore, useUi } from '../stores/context';
import { toggleTheme } from '../stores/UiStore';
import { ALL } from '../core/workspaces';
import { Avatar, K } from './bits';
import { projectLabel } from './Projects';
import { refreshProjectTracked } from './project/freshness';
import { markSpaceUnseen, spaceSeen } from './space/seen';
import { Sparkles } from '@gravity-ui/icons';

export interface Item {
  group?: string;
  /** Only listed once the query matches it (e.g. every PR), so an empty
   *  palette stays a short list of commands. */
  searchOnly?: boolean;
  label: string;
  icon?: ReactNode;
  sub?: string;
  keys?: string;
  run: () => void;
}

const sections = (shown: Item[]) => {
  const out: { group: string; items: { it: Item; n: number }[] }[] = [];
  shown.forEach((it, n) => {
    const g = it.group ?? '';
    if (out[out.length - 1]?.group !== g) out.push({ group: g, items: [] });
    out[out.length - 1]!.items.push({ it, n });
  });
  return out;
};

/** Linear's menu: type to filter, up/down + enter, digits pick when the query is empty. */
export function Palette({
  title,
  items,
  placeholder,
  digits,
  onClose,
}: {
  title?: string;
  items: Item[];
  placeholder: string;
  digits?: boolean;
  onClose: () => void;
}) {
  const [q, setQ] = useState('');
  const [i, setI] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  // Focus at once (keys typed right after ⌘K must land here), and again
  // once the dialog's own focus handling has run.
  useEffect(() => {
    input.current?.focus();
    const t = setTimeout(() => input.current?.focus(), 30);
    return () => clearTimeout(t);
  }, []);
  const words = q.toLowerCase().split(/\s+/).filter(Boolean);
  const shown = items.filter(
    (it) =>
      (!it.searchOnly || words.length > 0) &&
      words.every((w) =>
        `${it.group ?? ''} ${it.label} ${it.sub ?? ''}`
          .toLowerCase()
          .includes(w),
      ),
  );
  const runAt = (n: number) => {
    const it = shown[n];
    if (!it) return;
    onClose();
    it.run();
  };
  return (
    <Modal.Backdrop
      isOpen
      onOpenChange={(open) => !open && onClose()}
      variant="transparent"
      className="bg-black/20"
    >
      <Modal.Container size="lg" placement="top" className="mt-[10vh]">
        <Modal.Dialog
          aria-label={title ?? 'Commands'}
          className="palette overflow-hidden p-0"
        >
          {title && (
            <div className="pal-title px-4 pt-3 text-xs text-muted">
              {title}
            </div>
          )}
          <input
            ref={input}
            autoFocus
            className="pal-input w-full border-b border-separator bg-transparent px-4 py-3.5 text-base outline-none placeholder:text-muted"
            placeholder={placeholder}
            value={q}
            onChange={(e) => {
              setQ(e.target.value);
              setI(0);
            }}
            onKeyDown={(e) => {
              if (e.key === 'ArrowDown')
                setI((x) => Math.min(shown.length - 1, x + 1));
              else if (e.key === 'ArrowUp') setI((x) => Math.max(0, x - 1));
              else if (e.key === 'Enter') runAt(i);
              else if (digits && !q && /^[1-9]$/.test(e.key))
                runAt(Number(e.key) - 1);
              else return;
              e.preventDefault();
            }}
          />
          <ScrollShadow className="max-h-[52vh] p-1.5" hideScrollBar>
            <ListBox
              aria-label={title ?? 'Commands'}
              selectionMode="none"
              onAction={(k: Key) => runAt(Number(k))}
              renderEmptyState={() => (
                <EmptyState className="px-3 py-4">No matches</EmptyState>
              )}
              className="p-0"
            >
              {sections(shown).map((sec) => (
                <ListBox.Section key={sec.group || '_'} id={sec.group || '_'}>
                  {sec.group && (
                    <Header className="pal-group px-2 pb-1 pt-2 text-xs text-muted">
                      {sec.group}
                    </Header>
                  )}
                  {sec.items.map(({ it, n }) => (
                    <ListBox.Item
                      key={n}
                      id={n}
                      textValue={it.label}
                      className={`pal-item h-9 min-w-0 gap-2.5 rounded-2xl px-2.5 ${n === i ? 'on bg-default' : ''}`}
                      onHoverStart={() => setI(n)}
                    >
                      <span className="grid w-[18px] shrink-0 place-items-center text-muted">
                        {it.icon}
                      </span>
                      <span className="min-w-0 truncate" title={it.label}>
                        {it.label}
                      </span>
                      {it.sub && (
                        <span className="shrink-0 truncate text-xs text-muted">
                          {it.sub}
                        </span>
                      )}
                      <span className="min-w-2 flex-1" />
                      <span className="shrink-0">
                        {digits && n < 9 && !q ? (
                          <K>{n + 1}</K>
                        ) : (
                          it.keys && <K>{it.keys}</K>
                        )}
                      </span>
                    </ListBox.Item>
                  ))}
                </ListBox.Section>
              ))}
            </ListBox>
          </ScrollShadow>
        </Modal.Dialog>
      </Modal.Container>
    </Modal.Backdrop>
  );
}

const SNOOZE_LABEL: Record<SnoozeUntil['kind'], string> = {
  time: 'for 1 hour',
  next_push: 'until next push',
  checks_green: 'until checks pass',
};

const AGENTS: AgentId[] = ['claude', 'codex', 'gemini'];

/** "storefront #104" for one PR, "3 PRs" for several — never the raw id. */
function targetsLabel(
  store: ReturnType<typeof useRootStore>,
  targets: string[],
): string {
  if (targets.length !== 1) return `${targets.length} PRs`;
  const p = store.models.prSummaries.get(targets[0]!);
  if (!p) return targets[0]!;
  return p.url ? `${p.pr.repo.name} #${p.pr.number}` : p.head_ref;
}

export const SnoozeMenu = observer(function SnoozeMenu() {
  const store = useRootStore();
  const ui = useUi();
  const targets = ui.modalTargets;
  const opts: SnoozeUntil[] = [
    { kind: 'time', at: Date.now() + 3600_000 },
    { kind: 'next_push' },
    { kind: 'checks_green' },
  ];
  return (
    <Palette
      title={`Snooze · ${targetsLabel(store, targets)}`}
      placeholder="Snooze until…"
      digits
      onClose={() => ui.closeModal()}
      items={opts.map((o) => ({
        label: SNOOZE_LABEL[o.kind],
        icon: <Clock />,
        sub: o.kind !== 'time' ? 'wakes on a GitHub event' : undefined,
        run: () => snoozePrs(store, targets, o),
      }))}
    />
  );
});

/** Assign a task to a person (its PR's author, the viewer, or anyone
 *  assignable on that PR) or an agent — `A` on a task row, or its context
 *  menu's "Delegate". Mirrors the prototype's assign menu; unlike it, the
 *  people list comes from `assignable_users`, not a hardcoded team. */
export const AssignMenu = observer(function AssignMenu() {
  const store = useRootStore();
  const ui = useUi();
  const taskId = ui.modalTargets[0];
  const task = taskId ? store.models.tasks.get(taskId) : undefined;
  const pr = task ? store.models.prSummaries.get(task.pr) : undefined;
  const [people, setPeople] = useState<Person[]>([]);

  useEffect(() => {
    if (!task) return;
    let live = true;
    void api
      .assignableUsers(task.pr)
      .then((p) => {
        if (live) setPeople(p);
      })
      .catch(() => {
        // falls back to just the viewer + PR author below
      });
    return () => {
      live = false;
    };
  }, [task]);

  if (!task) return null;
  const viewer = store.ephemeral.viewer;
  const author = pr?.author;
  const pool = [viewer, author, ...people].filter((p): p is Person => !!p);
  const seen = new Set<string>();
  const dedup = pool.filter((p) => {
    if (seen.has(p.login)) return false;
    seen.add(p.login);
    return true;
  });

  return (
    <Palette
      title={`Assign · ${task.title}`}
      placeholder="Person or agent…"
      digits
      onClose={() => ui.closeModal()}
      items={[
        ...AGENTS.map((a) => ({
          group: 'Agents — fix it in a worktree',
          label: a,
          icon: <Sparkles className="size-4" />,
          // Same as Delegate: the agent takes the task and starts the fix.
          run: () => void delegateTask(store, task.id, a),
        })),
        ...dedup.map((p) => ({
          group: 'People',
          label: viewer && p.login === viewer.login ? 'you' : p.login,
          icon: <Avatar person={p} size={16} />,
          sub: author && p.login === author.login ? 'PR author' : undefined,
          run: () => assignTask(store, task.id, { kind: 'human', id: p.login }),
        })),
      ]}
    />
  );
});

export const CommandMenu = observer(function CommandMenu() {
  const ui = useUi();
  const store = useRootStore();
  const r = ui.route;
  // Review shortcuts live in Review.tsx's key handler; replay the key there.
  const press = (key: string) => () =>
    setTimeout(
      () => window.dispatchEvent(new KeyboardEvent('keydown', { key })),
      0,
    );
  const prSummary =
    r.name === 'review' ? store.models.prSummaries.get(r.pr) : null;
  const prLocal = prSummary?.pr.repo.host === 'local';
  const prFinished =
    prSummary?.state === 'merged' || prSummary?.state === 'closed';
  const prMine =
    !!prSummary && store.ephemeral.viewer?.login === prSummary.author.login;
  const draftItems: Item[] =
    r.name === 'review' && prSummary && !prLocal && prMine
      ? [
          {
            group: 'This PR',
            label:
              prSummary.state === 'draft'
                ? 'Ready for review'
                : 'Convert to draft',
            icon: prSummary.state === 'draft' ? <Eye /> : <FileText />,
            run: () =>
              void setDraft(store, {
                pr: r.pr,
                draft: prSummary.state !== 'draft',
              }),
          },
        ]
      : [];
  const prItems: Item[] =
    r.name === 'review'
      ? [
          ...(prFinished
            ? []
            : [
                {
                  group: 'This PR',
                  label: 'Submit review…',
                  icon: <PaperPlane />,
                  run: () => ui.openModal('submit'),
                },
                ...draftItems,
              ]),
          {
            group: 'This PR',
            label: 'Unified / split diff',
            icon: <LayoutColumns />,
            keys: 'S',
            run: press('s'),
          },
          {
            group: 'This PR',
            label: 'Ask the agent or run a skill',
            icon: <Sparkles />,
            keys: '/',
            run: press('/'),
          },
          {
            group: 'This PR',
            label: 'Open on GitHub',
            icon: <ArrowUpRightFromSquare />,
            run: () => {
              const url = store.models.prSummaries.get(r.pr)?.url;
              if (url) openExternal(url);
            },
          },
        ]
      : [];
  const ws = store.workspaces;
  const workspaceItems: Item[] = [
    ...(ui.workspace === ALL
      ? []
      : [
          {
            group: 'Workspaces',
            label: 'Switch to All projects',
            icon: <Tray />,
            keys: '⌃0',
            run: () => ws.switchTo(ALL),
          },
        ]),
    ...ws.projects.flatMap((p, i) =>
      p.id === ui.workspace
        ? []
        : [
            {
              group: 'Workspaces',
              label: `Switch to ${projectLabel(p)}`,
              icon: <Folder />,
              keys: i < 9 ? `⌃${i + 1}` : undefined,
              run: () => ws.switchTo(p.id),
            },
          ],
    ),
  ];
  const repoWorkspace = ws.kind !== 'local';
  // Every PR the active workspace shows, by repo, number, title and author.
  const pullRequests: Item[] = ws
    .scopePrs(store.models.prSummaries.all())
    .filter((p) => !!p.url)
    .sort((a, b) => b.updated_at - a.updated_at)
    .map((p) => ({
      group: 'Pull requests',
      searchOnly: true,
      label: `${p.pr.repo.name} #${p.pr.number} ${p.title}`,
      sub: `${p.pr.repo.owner}/${p.pr.repo.name} · ${p.author.login} · ${p.state}`,
      icon: <CodePullRequest />,
      run: () => ui.go({ name: 'review', pr: p.id }),
    }));
  // Spaces (build mode only): new, and every space of the workspace.
  const spaceItems: Item[] = isBuildMode(store)
    ? [
        {
          group: 'Spaces',
          label: 'New space',
          icon: <Plus />,
          keys: 'G N',
          run: () => ui.openModal('new-space'),
        },
        ...spacesIn(ui.workspace, store.models.spaces.all()).flatMap(
          (sp): Item[] => [
            {
              group: 'Spaces',
              label: spaceTitle(sp),
              sub: sp.branch,
              icon: <Pencil />,
              run: () => ui.go({ name: 'space', id: sp.id }),
            },
            // Pin/unpin and Mark as unread (docs/spaces.md §3): listed only
            // once searched for, so the default view stays short.
            {
              group: 'Spaces',
              searchOnly: true,
              label: `${sp.pinned ? 'Unpin' : 'Pin'} ${spaceTitle(sp)}`,
              icon: sp.pinned ? <PinSlash /> : <Pin />,
              run: () =>
                void api
                  .setSpacePinned({ space: sp.id, pinned: !sp.pinned })
                  .catch(() => {}),
            },
            ...(canMarkUnread(sp, spaceSeen.get(sp.id))
              ? [
                  {
                    group: 'Spaces',
                    searchOnly: true,
                    label: `Mark ${spaceTitle(sp)} as unread`,
                    icon: <CircleFill className="size-3" />,
                    run: () => markSpaceUnseen(sp.id),
                  },
                ]
              : []),
          ],
        ),
        // Archived ones only when searched for: open one to unarchive it.
        ...archivedSpacesIn(ui.workspace, store.models.spaces.all()).map(
          (sp): Item => ({
            group: 'Archived spaces',
            searchOnly: true,
            label: `${spaceTitle(sp)} (archived)`,
            sub: sp.branch,
            icon: <Pencil />,
            run: () => ui.go({ name: 'space', id: sp.id }),
          }),
        ),
      ]
    : [];
  // The open space's own commands.
  const openSpace =
    r.name === 'space' && isBuildMode(store)
      ? store.models.spaces.get(r.id)
      : undefined;
  const undo = openSpace ? undoState(openSpace) : null;
  const thisSpaceItems: Item[] = openSpace
    ? [
        ...(undo?.ok
          ? [
              {
                group: 'This space',
                label: 'Undo last turn',
                icon: <ArrowUturnCcwLeft />,
                run: () => void api.undoSpaceTurn(openSpace.id).catch(() => {}),
              },
            ]
          : []),
        ...(openSpace.status === 'archived'
          ? [
              {
                group: 'This space',
                label: 'Unarchive space',
                icon: <ArrowUturnCcwLeft />,
                run: () =>
                  void api.unarchiveSpace(openSpace.id).catch(() => {}),
              },
            ]
          : []),
      ]
    : [];
  const items: Item[] = [
    ...prItems,
    ...thisSpaceItems,
    ...spaceItems,
    {
      group: 'Go to',
      label: 'New tab — paste a PR link',
      icon: <Tray />,
      keys: '⌘T',
      run: () => ui.newTab(),
    },
    ...(repoWorkspace
      ? [
          {
            group: 'Go to',
            label: 'Inbox',
            icon: <Tray />,
            keys: 'G I',
            run: () => ui.go({ name: 'inbox' }),
          },
        ]
      : []),
    ...(repoWorkspace
      ? [
          {
            group: 'Go to',
            label: 'Pull requests',
            icon: <CodePullRequest />,
            run: () => ui.goPrs(),
          },
          ...(FEATURES.currentUser
            ? [
                {
                  group: 'Go to',
                  label: 'My pull requests',
                  icon: <CodePullRequest />,
                  keys: 'G M',
                  run: () => ui.goPrs('mine'),
                },
              ]
            : []),
        ]
      : []),
    ...(ws.active
      ? [
          {
            group: 'Go to',
            label: 'Branches',
            icon: <BranchesRight />,
            run: () => ui.go({ name: 'branches' }),
          },
        ]
      : []),
    {
      group: 'Go to',
      label: 'Tasks',
      icon: <ListCheck />,
      keys: 'G T',
      run: () => ui.go({ name: 'tasks' }),
    },
    {
      group: 'Go to',
      label: 'Skills',
      icon: <Sparkles />,
      keys: 'G K',
      run: () => ui.go({ name: 'skills' }),
    },
    {
      group: 'Go to',
      label: 'Settings',
      icon: <Gear />,
      keys: 'G S',
      run: () => ui.go({ name: 'settings', page: 'agents' }),
    },
    {
      group: 'Go to',
      label: 'Settings → Account',
      icon: <Gear />,
      run: () => ui.go({ name: 'settings', page: 'account' }),
    },
    ...workspaceItems,
    {
      group: 'General',
      label: 'Add project…',
      icon: <FolderPlus />,
      run: () => ui.openModal('add-project'),
    },
    {
      group: 'General',
      label: 'Refresh inbox',
      icon: <ArrowRotateLeft />,
      run: () => void refreshInbox(store),
    },
    ...(ws.active
      ? [
          {
            group: 'General',
            label: `Refresh ${projectLabel(ws.active)}`,
            icon: <ArrowRotateLeft />,
            run: () => void refreshProjectTracked(store, ws.active!.id),
          },
        ]
      : []),
    {
      group: 'General',
      label: 'Keyboard shortcuts',
      icon: <Keyboard />,
      keys: '?',
      run: () => ui.openModal('help'),
    },
    {
      group: 'General',
      label: 'Light / dark theme',
      icon: <Moon />,
      run: toggleTheme,
    },
    {
      group: 'General',
      label: 'Collapse / expand sidebar',
      icon: <LayoutSideContentLeft />,
      keys: '[',
      run: () => ui.toggleSidebar(),
    },
    ...pullRequests,
    {
      group: 'General',
      label: 'Undo',
      icon: <ArrowUturnCcwLeft />,
      keys: '⌘Z',
      run: () => void store.sync.undo.undo(),
    },
  ];
  return (
    <Palette
      items={items}
      placeholder="Command or action…"
      onClose={() => ui.closeModal()}
    />
  );
});

let gAt = 0;
/** True right after `g` — the next key belongs to a g-sequence, not a view shortcut. */
export const inSequence = () => Date.now() - gAt < 900;

/** Linear-style key sequences and global shortcuts, in the capture phase so they win. */
export function useGlobalKeys() {
  const store = useRootStore();
  const ui = useUi();
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // Behind the account gate no shortcut, and no palette (plan §6.12).
      if (store.ephemeral.accountGate !== 'open') return;
      if ((e.metaKey || e.ctrlKey) && latinKey(e) === 'k') {
        if (ui.modal === 'cmdk') ui.closeModal();
        else ui.openModal('cmdk');
      } else if (!ui.modal && (e.metaKey || e.ctrlKey) && latinKey(e) === 'f') {
        // GitHub #77: search the list that's "current" for this route.
        // `null` leaves the key alone — a diff/text view, or nowhere to
        // search (sidebar collapsed, spaces off on a non-PR page).
        const scope = findScopeFor(ui.route.name, isBuildMode(store));
        if (!scope || (scope === 'spaces' && !ui.sidebar)) return;
        ui.openListSearch(scope);
      } else if (
        (e.metaKey || e.ctrlKey) &&
        latinKey(e) === 'z' &&
        !(e.target as HTMLElement).closest('input,textarea')
      ) {
        if (e.shiftKey) void store.sync.undo.redo();
        else void store.sync.undo.undo();
      } else if (!ui.modal && store.workspaces.switchForKey(e)) {
        // ⌃1…9 / ⌃0: switch workspace, sidebar shown or not.
      } else if (e.altKey && e.code === 'KeyW' && !ui.modal) {
        ui.requestCloseTab(ui.route);
      } else if (e.altKey && /^Digit[1-9]$/.test(e.code) && !ui.modal) {
        ui.switchTab(Number(e.code.slice(5)) - 1);
      } else if (
        ui.modal ||
        e.metaKey ||
        e.ctrlKey ||
        (e.target as HTMLElement).closest('input,textarea,select')
      ) {
        return;
      } else if (inSequence() && /^[itsamkn]$/.test(latinKey(e))) {
        gAt = 0;
        if (latinKey(e) === 'n' && isBuildMode(store))
          ui.openModal('new-space');
        if (latinKey(e) === 'i') ui.go({ name: 'inbox' });
        if (latinKey(e) === 't') ui.go({ name: 'tasks' });
        if (latinKey(e) === 's') ui.go({ name: 'settings', page: 'agents' });
        if (latinKey(e) === 'm' && FEATURES.currentUser) ui.goPrs('mine');
        if (latinKey(e) === 'k') ui.go({ name: 'skills' });
      } else if (latinKey(e) === 'g') {
        gAt = Date.now();
      } else if (latinKey(e) === '[') {
        ui.toggleSidebar();
      } else if (latinKey(e) === ']' && ui.route.name === 'review') {
        ui.toggleAside();
      } else if (latinKey(e) === '?') {
        ui.openModal('help');
      } else {
        return;
      }
      e.preventDefault();
      e.stopPropagation();
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, [store, ui]);
}
