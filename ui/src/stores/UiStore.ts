/**
 * Local-only UI state: navigation and tabs, selection, panel visibility.
 * Never synced. A desktop app has no address bar, so this plays the role a
 * router would. Each workspace keeps its own copy of the navigation and
 * view state (`UiSnapshot`) in `localStorage`; switching swaps it.
 */
import {
  computed,
  makeObservable,
  observable,
  observableRef,
  observableShallow,
  runInAction,
  reaction,
} from 'mobx';
import type { PrId } from '../gen/contract';
import {
  ALL,
  homeRoute,
  migrateRoute,
  type WorkspaceId,
} from '../core/workspaces';
import type { ContextMenuItem } from '../ui/ContextMenu';
import type { Display } from '../ui/List';
import { isPrFilter, type PrFilter } from '../ui/prs/filters';

export type SettingsPage =
  | 'account'
  | 'appearance'
  | 'agents'
  | 'integrations'
  | 'projects'
  /** Code intelligence per project (docs/code-intel.md §9). */
  | 'languages'
  | 'updates'
  | 'about'
  /** Listed only in sandbox mode (`AppInfo.sandbox`). */
  | 'sandbox'
  /** Settings → Spaces (the id predates the rename). */
  | 'labs'
  /** Settings → Labs: switches for features in progress (`settings/labs.ts`). */
  | 'experiments';

export interface ContextMenuState {
  x: number;
  y: number;
  items: ContextMenuItem[];
}

export type Route =
  | { name: 'inbox' }
  /** One list of the workspace's PRs; the filter (All / Mine / Drafts /
   *  Merged) is `UiStore.prsFilter`, saved per workspace. */
  | { name: 'prs' }
  /** The active project's branches; only under a single project. */
  | { name: 'branches' }
  | { name: 'tasks' }
  /** The standalone skills library: every source, filterable, with the
   *  SKILL.md editor — unlike the in-review `SkillsManagerModal`, this
   *  one isn't scoped to a PR. */
  | { name: 'skills' }
  | { name: 'settings'; page: SettingsPage }
  | { name: 'review'; pr: PrId }
  /** A blank tab: paste a PR link to turn it into a review. */
  | { name: 'new'; id: string }
  /** A space (docs/spaces.md): chat with the agent + the live diff. Only
   *  reachable with build mode on. */
  | { name: 'space'; id: string };

export type ModalName =
  | 'help'
  | 'submit'
  | 'snooze'
  | 'assign'
  | 'cmdk'
  | 'add-project'
  /** Theodore account sign-in (only when it's configured). */
  | 'sign-in'
  /** Help → Report a Problem…: a local zip of logs to review and attach. */
  | 'report-problem'
  /** The list of usage events and what is never sent. */
  | 'whats-collected'
  /** Build mode only. */
  | 'new-space';

export const sameRoute = (a: Route, b: Route): boolean =>
  JSON.stringify(a) === JSON.stringify(b);

/** `localStorage` key prefix: `"sandbox:"` in a sandbox window (set by the
 *  desktop shell before the page loads), so its tabs and prefs never mix
 *  with the real app's — both share one webview storage. */
export const STORAGE_NS: string =
  (globalThis as { __THEODORE_STORAGE_NS__?: string })
    .__THEODORE_STORAGE_NS__ ?? '';
const LS_ONBOARDED = `${STORAGE_NS}theodore-onboarded`;
const LS_DIFF_VIEW = `${STORAGE_NS}theodore-diff-view`;
const LS_THEME = `${STORAGE_NS}theodore-theme`;
/** The active workspace id. */
const LS_WORKSPACE = `${STORAGE_NS}theodore-workspace`;
/** Before workspaces: the one tab set and the aside column state. */
const LS_LEGACY_TABS = `${STORAGE_NS}theodore-tabs`;
const LS_LEGACY_ASIDE = `${STORAGE_NS}theodore-aside`;

/** `localStorage` key of one workspace's saved `UiSnapshot`. */
export const wsKey = (ws: WorkspaceId): string =>
  `${STORAGE_NS}theodore-ws:${ws}`;

/** Where you were in one PR: grouped or by file, which group or file, and
 *  how far down the diff. */
export interface ReviewPos {
  mode: 'groups' | 'files';
  group: string | null;
  file: string | null;
  /** `scrollTop` of the diff, for this `mode` + `group`/`file`. */
  scroll?: number;
}

/** PRs a workspace remembers a review position for. */
const REVIEW_POS_LIMIT = 50;

/** Everything a workspace restores when you switch back to it. */
export interface UiSnapshot {
  tabs: Route[];
  route: Route;
  back: Route;
  selected: PrId | null;
  peek: boolean;
  aside: boolean;
  sidebar: boolean;
  /** Inbox Display options; `null` = the defaults. */
  inboxDisplay: Display | null;
  inboxShowDone: boolean;
  /** Tasks filter tab; `null` = the default one. */
  tasksTab: string | null;
  /** The Pull requests page's filter. */
  prsFilter: PrFilter;
  reviewPos: Record<PrId, ReviewPos>;
}

export function defaultSnapshot(ws: WorkspaceId): UiSnapshot {
  const home = homeRoute(ws);
  return {
    tabs: [home],
    route: home,
    back: home,
    selected: null,
    peek: true,
    aside: true,
    sidebar: true,
    inboxDisplay: null,
    inboxShowDone: false,
    tasksTab: null,
    prsFilter: 'all',
    reviewPos: {},
  };
}

const uniqueRoutes = (routes: Route[]): Route[] =>
  routes.filter((r, i) => routes.findIndex((x) => sameRoute(x, r)) === i);

/** A saved snapshot, from any version, over the defaults: routes that no
 *  longer exist become their replacements (`migrateRoute`), unknown ones
 *  are dropped, and a saved My PRs tab turns the filter to Mine. `null`
 *  when nothing usable is left. */
export function migrateSnapshot(
  raw: unknown,
  ws: WorkspaceId,
): UiSnapshot | null {
  if (!raw || typeof raw !== 'object') return null;
  const v = raw as Partial<Record<keyof UiSnapshot, unknown>>;
  if (!Array.isArray(v.tabs) || !v.tabs.length || !v.route) return null;
  const base = defaultSnapshot(ws);
  const tabs = uniqueRoutes(
    v.tabs.flatMap((t: unknown) => migrateRoute(t, ws) ?? []),
  );
  if (!tabs.length) return null;
  const route = migrateRoute(v.route, ws) ?? tabs[0]!;
  const back = migrateRoute(v.back, ws) ?? base.back;
  const hadMine = [...v.tabs, v.route].some(
    (t) => (t as { name?: unknown } | null)?.name === 'mine',
  );
  const prsFilter = isPrFilter(v.prsFilter)
    ? v.prsFilter
    : hadMine
      ? 'mine'
      : base.prsFilter;
  return {
    ...base,
    ...(v as Partial<UiSnapshot>),
    tabs: tabs.some((t) => sameRoute(t, route)) ? tabs : [...tabs, route],
    route,
    back,
    prsFilter,
  };
}

/** A workspace's saved snapshot over the defaults; `null` if none is saved
 *  or it can't be read. */
export function loadSnapshot(ws: WorkspaceId): UiSnapshot | null {
  try {
    return migrateSnapshot(
      JSON.parse(localStorage.getItem(wsKey(ws)) ?? 'null'),
      ws,
    );
  } catch {
    return null;
  }
}

export function saveSnapshot(ws: WorkspaceId, s: UiSnapshot): void {
  localStorage.setItem(wsKey(ws), JSON.stringify(s));
}

/** Once: the tab set saved before workspaces existed becomes All's. */
export function migrateLegacyTabs(): void {
  const raw = localStorage.getItem(LS_LEGACY_TABS);
  if (raw === null) return;
  if (localStorage.getItem(wsKey(ALL)) === null) {
    try {
      const v = JSON.parse(raw) as {
        tabs?: { name?: string }[];
        route?: { name?: string };
      } | null;
      if (v && Array.isArray(v.tabs) && v.route) {
        // A project page belongs to that project's own workspace, not All's.
        const tabs = v.tabs.filter((t) => t?.name !== 'project');
        const route = v.route.name === 'project' ? tabs[0] : v.route;
        const s = route && migrateSnapshot({ tabs, route }, ALL);
        if (s) {
          saveSnapshot(ALL, {
            ...s,
            aside: localStorage.getItem(LS_LEGACY_ASIDE) !== '0',
          });
        }
      }
    } catch {
      // corrupt value: All starts fresh
    }
  }
  localStorage.removeItem(LS_LEGACY_TABS);
  localStorage.removeItem(LS_LEGACY_ASIDE);
}

/** Length of the tab collapse animation (`.tab-closing` in styles.css). */
export const TAB_CLOSE_MS = 180;

/** Where a navigation lands: which workspace, showing which route. Set by
 *  `WorkspaceStore`, which knows the projects. */
export type Placer = (route: Route) => { ws: WorkspaceId; route: Route };

export class UiStore {
  onboarded = localStorage.getItem(LS_ONBOARDED) === '1';
  /** The active workspace: `ALL` or a project id. */
  workspace: WorkspaceId = ALL;
  route: Route = { name: 'inbox' };
  back: Route = { name: 'inbox' };
  tabs: Route[] = [{ name: 'inbox' }];
  selected: PrId | null = null;
  picked: PrId[] = [];
  peek = true;
  sidebar = true;
  aside = true;
  inboxDisplay: Display | null = null;
  inboxShowDone = false;
  tasksTab: string | null = null;
  prsFilter: PrFilter = 'all';
  /** Per PR, read once when its review opens — not observable, so saving
   *  the scroll position doesn't re-render anything. */
  reviewPos: Record<PrId, ReviewPos> = {};
  diffView: 'unified' | 'split' =
    localStorage.getItem(LS_DIFF_VIEW) === 'split' ? 'split' : 'unified';
  modal: ModalName | null = null;
  /** ids the open modal acts on — PRs for `snooze`, a task id for `assign`. */
  modalTargets: string[] = [];
  /** The one custom context menu, open at a pointer point — see `ContextMenu.tsx`. */
  contextMenu: ContextMenuState | null = null;
  /** Tabs mid close animation (`JSON.stringify(route)`); removed after it. */
  closingTabs: string[] = [];
  place: Placer | null = null;

  private persistTimer: ReturnType<typeof setTimeout> | undefined;

  constructor() {
    migrateLegacyTabs();
    this.workspace = localStorage.getItem(LS_WORKSPACE) ?? ALL;
    this.apply(loadSnapshot(this.workspace) ?? defaultSnapshot(this.workspace));
    makeObservable(this, {
      onboarded: observable,
      workspace: observable,
      route: observableRef,
      back: observableRef,
      tabs: observableShallow,
      selected: observable,
      picked: observableShallow,
      peek: observable,
      sidebar: observable,
      aside: observable,
      inboxDisplay: observableRef,
      inboxShowDone: observable,
      tasksTab: observable,
      prsFilter: observable,
      asideIsDrawer: observable,
      asideDrawerOpen: observable,
      asideShown: computed,
      diffView: observable,
      modal: observableRef,
      modalTargets: observableShallow,
      contextMenu: observableRef,
      closingTabs: observableShallow,
    });
    reaction(
      () => this.snapshot(),
      (v) => saveSnapshot(this.workspace, v),
    );
  }

  /** This workspace's state, as saved and restored on a switch. */
  snapshot(): UiSnapshot {
    return {
      tabs: this.tabs,
      route: this.route,
      back: this.back,
      selected: this.selected,
      peek: this.peek,
      aside: this.aside,
      sidebar: this.sidebar,
      inboxDisplay: this.inboxDisplay,
      inboxShowDone: this.inboxShowDone,
      tasksTab: this.tasksTab,
      prsFilter: this.prsFilter,
      reviewPos: this.reviewPos,
    };
  }

  private apply(s: UiSnapshot): void {
    this.tabs = s.tabs;
    this.route = s.route;
    this.back = s.back;
    this.selected = s.selected;
    this.peek = s.peek;
    this.aside = s.aside;
    this.sidebar = s.sidebar;
    this.inboxDisplay = s.inboxDisplay;
    this.inboxShowDone = s.inboxShowDone;
    this.tasksTab = s.tasksTab;
    this.prsFilter = s.prsFilter;
    this.reviewPos = s.reviewPos;
  }

  /** Saved tabs of a workspace — live ones for the active workspace. */
  tabsOf(ws: WorkspaceId): Route[] | null {
    return ws === this.workspace ? this.tabs : (loadSnapshot(ws)?.tabs ?? null);
  }

  /** Saves this workspace's state and restores `ws`'s — its own tabs,
   *  selection, filters and panels, or a fresh set on its first visit. */
  switchWorkspace(ws: WorkspaceId, opts: { save?: boolean } = {}): void {
    if (ws === this.workspace) return;
    runInAction(() => {
      if (opts.save !== false) saveSnapshot(this.workspace, this.snapshot());
      this.workspace = ws;
      this.apply(loadSnapshot(ws) ?? defaultSnapshot(ws));
      this.modal = null;
      this.picked = [];
      this.contextMenu = null;
      this.closingTabs = [];
      this.asideDrawerOpen = false;
    });
    localStorage.setItem(LS_WORKSPACE, ws);
  }

  /** A removed project: drop its saved state; leave it for All if active. */
  forgetWorkspace(ws: WorkspaceId): void {
    if (ws === ALL) return;
    if (ws === this.workspace) this.switchWorkspace(ALL, { save: false });
    localStorage.removeItem(wsKey(ws));
  }

  setReviewPos(pr: PrId, pos: ReviewPos): void {
    const prev = this.reviewPos[pr];
    const same =
      prev?.mode === pos.mode &&
      prev.group === pos.group &&
      prev.file === pos.file;
    const { [pr]: _old, ...rest } = this.reviewPos;
    // Most recent last; only the last REVIEW_POS_LIMIT PRs are kept.
    const kept = Object.entries(rest).slice(-(REVIEW_POS_LIMIT - 1));
    this.reviewPos = {
      ...Object.fromEntries(kept),
      [pr]: { ...pos, scroll: pos.scroll ?? (same ? prev.scroll : undefined) },
    };
    this.persistSoon();
  }

  /** The diff's scroll in the PR's current view — saved, never rendered. */
  setReviewScroll(pr: PrId, scroll: number): void {
    const at = this.reviewPos[pr];
    if (!at || at.scroll === scroll) return;
    this.reviewPos = { ...this.reviewPos, [pr]: { ...at, scroll } };
    this.persistSoon();
  }

  private persistSoon(): void {
    clearTimeout(this.persistTimer);
    const ws = this.workspace;
    this.persistTimer = setTimeout(() => {
      if (ws === this.workspace) saveSnapshot(ws, this.snapshot());
    }, 300);
  }

  setInboxDisplay(d: Display | null): void {
    runInAction(() => {
      this.inboxDisplay = d;
    });
  }

  setInboxShowDone(v: boolean): void {
    runInAction(() => {
      this.inboxShowDone = v;
    });
  }

  setTasksTab(t: string): void {
    runInAction(() => {
      this.tasksTab = t;
    });
  }

  setPrsFilter(f: PrFilter): void {
    runInAction(() => {
      this.prsFilter = f;
    });
  }

  /** Pull requests, optionally with a filter — set in the workspace the
   *  page lands in (a local folder has none: it lands on Branches). */
  goPrs(filter?: PrFilter): void {
    this.go({ name: 'prs' });
    if (filter && this.route.name === 'prs') this.setPrsFilter(filter);
  }

  finishOnboarding(): void {
    localStorage.setItem(LS_ONBOARDED, '1');
    runInAction(() => {
      this.onboarded = true;
      this.go({ name: 'inbox' });
    });
  }

  /** Linear-style tabs: `review` always gets its own tab; other views replace
   *  the current tab unless `newTab` is asked for. */
  go(to: Route, opts: { newTab?: boolean } = {}): void {
    const placed = this.place?.(to);
    if (placed) this.switchWorkspace(placed.ws);
    const route = placed?.route ?? to;
    runInAction(() => {
      let tabs = this.tabs;
      // Settings is one place: going to any of its pages reuses an open
      // Settings tab (moving it to that page) instead of opening another.
      if (route.name === 'settings') {
        const at = tabs.findIndex((t) => t.name === 'settings');
        if (at >= 0) tabs = tabs.map((t, i) => (i === at ? route : t));
      }
      if (!tabs.some((t) => sameRoute(t, route))) {
        const cur = tabs.findIndex((t) => sameRoute(t, this.route));
        const push =
          route.name === 'review' ||
          opts.newTab ||
          this.route.name === 'review' ||
          cur < 0;
        // A new tab always goes last, whatever opened it.
        tabs = push
          ? [...tabs, route]
          : tabs.map((t, i) => (i === cur ? route : t));
      }
      // Back means "where you were before this view": reviews don't count,
      // and moving between Settings pages keeps pointing out of Settings.
      const withinSettings =
        this.route.name === 'settings' && route.name === 'settings';
      this.back =
        this.route.name === 'review' || withinSettings ? this.back : this.route;
      this.tabs = tabs;
      this.route = route;
      this.modal = null;
      this.picked = [];
    });
  }

  /** Open a blank tab (the `+` in the tab bar). */
  newTab(): void {
    this.go({ name: 'new', id: crypto.randomUUID() }, { newTab: true });
  }

  /** Swap `from` (e.g. a blank tab) for `to` in place. */
  replaceTab(from: Route, to: Route): void {
    // Opens in another workspace: the blank tab here goes away.
    const placed = this.place?.(to);
    if (placed && placed.ws !== this.workspace) {
      this.closeTab(from);
      this.go(to);
      return;
    }
    runInAction(() => {
      const exists = this.tabs.some((t) => sameRoute(t, to));
      this.tabs = exists
        ? this.tabs.filter((t) => !sameRoute(t, from))
        : this.tabs.map((t) => (sameRoute(t, from) ? to : t));
      if (!this.tabs.length) this.tabs = [to];
      this.route = to;
    });
  }

  goBack(): void {
    this.go(this.back);
  }

  /** Closes a tab after it collapses (see `TabBar`); instantly when the
   *  user prefers reduced motion or it is the last tab. */
  requestCloseTab(route: Route): void {
    const k = JSON.stringify(route);
    if (this.closingTabs.includes(k)) return;
    const reduced =
      typeof matchMedia !== 'undefined' &&
      matchMedia('(prefers-reduced-motion: reduce)').matches;
    if (reduced || this.tabs.length <= 1) return this.closeTab(route);
    runInAction(() => {
      this.closingTabs = [...this.closingTabs, k];
    });
    setTimeout(() => {
      runInAction(() => {
        this.closingTabs = this.closingTabs.filter((x) => x !== k);
      });
      this.closeTab(route);
    }, TAB_CLOSE_MS);
  }

  closeTab(route: Route): void {
    runInAction(() => {
      const i = this.tabs.findIndex((t) => sameRoute(t, route));
      if (i < 0) return;
      const tabs = this.tabs.filter((_, j) => j !== i);
      if (!tabs.length) tabs.push(homeRoute(this.workspace));
      const active = sameRoute(this.route, route)
        ? tabs[Math.min(i, tabs.length - 1)]!
        : this.route;
      this.tabs = tabs;
      this.route = active;
    });
  }

  switchTab(n: number): void {
    const t = this.tabs[n];
    if (t) this.go(t);
  }

  select(id: PrId | null): void {
    runInAction(() => {
      this.selected = id;
    });
  }

  togglePick(id: PrId): void {
    runInAction(() => {
      this.picked = this.picked.includes(id)
        ? this.picked.filter((x) => x !== id)
        : [...this.picked, id];
    });
  }

  setPicked(ids: readonly PrId[], checked: boolean): void {
    runInAction(() => {
      const changed = new Set(ids);
      this.picked = checked
        ? [...new Set([...this.picked, ...ids])]
        : this.picked.filter((id) => !changed.has(id));
    });
  }

  clearPicked(): void {
    runInAction(() => {
      this.picked = [];
    });
  }

  togglePeek(): void {
    runInAction(() => {
      this.peek = !this.peek;
    });
  }

  toggleSidebar(): void {
    runInAction(() => {
      this.sidebar = !this.sidebar;
    });
  }

  /** The review screen is too narrow for the aside column, so the aside
   *  is an overlay drawer (set by `Review` from its layout). */
  asideIsDrawer = false;
  /** Drawer open state — never persisted, so the drawer doesn't cover the
   *  diff every time a PR opens. */
  asideDrawerOpen = false;

  /** Whether the aside (AI / tasks / threads) is showing in the current layout. */
  get asideShown(): boolean {
    return this.asideIsDrawer ? this.asideDrawerOpen : this.aside;
  }

  setAsideIsDrawer(drawer: boolean): void {
    runInAction(() => {
      this.asideIsDrawer = drawer;
      if (!drawer) this.asideDrawerOpen = false;
    });
  }

  toggleAside(open = !this.asideShown): void {
    if (this.asideIsDrawer) {
      runInAction(() => {
        this.asideDrawerOpen = open;
      });
      return;
    }
    runInAction(() => {
      this.aside = open;
    });
  }

  toggleDiffView(): void {
    const v = this.diffView === 'split' ? 'unified' : 'split';
    localStorage.setItem(LS_DIFF_VIEW, v);
    runInAction(() => {
      this.diffView = v;
    });
  }

  openModal(m: ModalName, targets: string[] = []): void {
    runInAction(() => {
      this.modal = m;
      this.modalTargets = targets;
    });
  }

  closeModal(): void {
    runInAction(() => {
      this.modal = null;
    });
  }

  /** "Reset onboarding" in Settings — forgets the welcome tour was seen so
   *  it shows again on the next launch of this view (no server state). */
  resetOnboarding(): void {
    localStorage.removeItem(LS_ONBOARDED);
    runInAction(() => {
      this.onboarded = false;
    });
  }

  openContextMenu(x: number, y: number, items: ContextMenuItem[]): void {
    runInAction(() => {
      this.contextMenu = { x, y, items };
    });
  }

  closeContextMenu(): void {
    runInAction(() => {
      this.contextMenu = null;
    });
  }
}

export type ThemePref = 'system' | 'light' | 'dark';

const darkQuery = () =>
  typeof matchMedia === 'undefined'
    ? null
    : matchMedia('(prefers-color-scheme: dark)');

/** The stored choice; no choice yet means follow the system. */
export function themePref(): ThemePref {
  const v = localStorage.getItem(LS_THEME);
  return v === 'light' || v === 'dark' ? v : 'system';
}

function applyTheme(pref: ThemePref): void {
  const dark = pref === 'dark' || (pref === 'system' && !!darkQuery()?.matches);
  document.documentElement.classList.toggle('dark', dark);
  document.documentElement.dataset.theme = dark ? 'dark' : 'light';
}

export function setThemePref(pref: ThemePref): void {
  localStorage.setItem(LS_THEME, pref);
  applyTheme(pref);
  window.dispatchEvent(new Event(THEME_EVENT));
}

/** Fired when the choice changes, so a picker elsewhere follows it. */
export const THEME_EVENT = 'theodore-theme';

/** The menu's "Light / Dark Theme": flips what's on screen and pins it. */
export function toggleTheme(): void {
  const dark = document.documentElement.classList.contains('dark');
  setThemePref(dark ? 'light' : 'dark');
}

/** On launch: apply the choice, and keep following the system while the
 *  choice is "system". */
export function initialTheme(): void {
  applyTheme(themePref());
  darkQuery()?.addEventListener('change', () => {
    if (themePref() === 'system') applyTheme('system');
  });
}
