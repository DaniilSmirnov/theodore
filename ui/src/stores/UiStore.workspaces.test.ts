import { beforeEach, describe, expect, it, vi } from 'vitest';
import { ALL } from '../core/workspaces';
import {
  UiStore,
  loadSnapshot,
  migrateLegacyTabs,
  wsKey,
  type Route,
} from './UiStore';

const WEB = 'github:acme/web';
const API = 'github:acme/api';
const review = (pr: string): Route => ({ name: 'review', pr });

beforeEach(() => localStorage.clear());

describe('workspace snapshots', () => {
  it('restores every saved piece of state when you switch back', () => {
    const ui = new UiStore();
    ui.go(review('github:acme/web#1'));
    ui.go({ name: 'tasks' }, { newTab: true });
    ui.select('task-1');
    ui.togglePeek();
    ui.toggleAside(false);
    ui.setInboxDisplay({ groupBy: 'repo', sort: 'size', props: ['id'] });
    ui.setInboxShowDone(true);
    ui.setTasksTab('agents');
    ui.setReviewPos('github:acme/web#1', {
      mode: 'files',
      group: null,
      file: 'a.ts',
      scroll: 120,
    });
    const before = JSON.parse(JSON.stringify(ui.snapshot()));

    ui.switchWorkspace(WEB);
    expect(ui.workspace).toBe(WEB);
    expect(ui.tabs).toEqual([{ name: 'prs' }]);
    expect(ui.selected).toBeNull();
    expect(ui.peek).toBe(true);
    expect(ui.inboxDisplay).toBeNull();
    ui.toggleSidebar();

    ui.switchWorkspace(ALL);
    expect(JSON.parse(JSON.stringify(ui.snapshot()))).toEqual(before);
    ui.switchWorkspace(WEB);
    expect(ui.sidebar).toBe(false);
  });

  it('survives a relaunch, in the workspace you left', () => {
    const ui = new UiStore();
    ui.switchWorkspace(WEB);
    ui.go({ name: 'inbox' }, { newTab: true });
    const next = new UiStore();
    expect(next.workspace).toBe(WEB);
    expect(next.tabs).toEqual([{ name: 'prs' }, { name: 'inbox' }]);
    expect(next.route).toEqual({ name: 'inbox' });
  });

  it('saves the review position without waiting for another change', () => {
    vi.useFakeTimers();
    try {
      const ui = new UiStore();
      ui.setReviewPos('github:acme/web#1', {
        mode: 'groups',
        group: 'g1',
        file: null,
        scroll: 40,
      });
      vi.advanceTimersByTime(400);
      expect(loadSnapshot(ALL)?.reviewPos['github:acme/web#1']?.scroll).toBe(
        40,
      );
    } finally {
      vi.useRealTimers();
    }
  });

  it('keeps the scroll for the same group and drops it for a new one', () => {
    const ui = new UiStore();
    const pr = 'github:acme/web#1';
    ui.setReviewPos(pr, { mode: 'groups', group: 'g1', file: null, scroll: 9 });
    ui.setReviewPos(pr, { mode: 'groups', group: 'g1', file: null });
    expect(ui.reviewPos[pr]?.scroll).toBe(9);
    ui.setReviewPos(pr, { mode: 'groups', group: 'g2', file: null });
    expect(ui.reviewPos[pr]?.scroll).toBeUndefined();
  });

  it('falls back to a fresh set for an unreadable snapshot', () => {
    localStorage.setItem(wsKey(WEB), '{not json');
    expect(loadSnapshot(WEB)).toBeNull();
    localStorage.setItem(wsKey(WEB), JSON.stringify({ tabs: [] }));
    expect(loadSnapshot(WEB)).toBeNull();
  });

  it('fills fields a snapshot predates with defaults', () => {
    localStorage.setItem(
      wsKey(WEB),
      JSON.stringify({ tabs: [{ name: 'inbox' }], route: { name: 'inbox' } }),
    );
    const s = loadSnapshot(WEB)!;
    expect(s.peek).toBe(true);
    expect(s.reviewPos).toEqual({});
  });

  it('closing the last tab of a project lands on its Pull requests', () => {
    const ui = new UiStore();
    ui.switchWorkspace(WEB);
    ui.closeTab({ name: 'prs' });
    expect(ui.tabs).toEqual([{ name: 'prs' }]);
  });

  it('forgets a removed project and leaves it for All', () => {
    const ui = new UiStore();
    ui.switchWorkspace(WEB);
    ui.switchWorkspace(API);
    ui.forgetWorkspace(API);
    expect(ui.workspace).toBe(ALL);
    expect(localStorage.getItem(wsKey(API))).toBeNull();
    expect(localStorage.getItem(wsKey(WEB))).not.toBeNull();
  });
});

describe('placement on navigation', () => {
  it('switches workspace when the placer says so, keeping both tab sets', () => {
    const ui = new UiStore();
    ui.place = (route) =>
      route.name === 'review' && route.pr.startsWith(API)
        ? { ws: API, route }
        : { ws: ui.workspace, route };
    ui.go(review(`${API}#2`));
    expect(ui.workspace).toBe(API);
    expect(ui.tabs).toEqual([{ name: 'prs' }, review(`${API}#2`)]);
    expect(loadSnapshot(ALL)?.tabs).toEqual([{ name: 'inbox' }]);
  });

  it('drops the blank tab a link was pasted into when it opens elsewhere', () => {
    const ui = new UiStore();
    ui.newTab();
    const blank = ui.route;
    ui.place = (route) =>
      route.name === 'review'
        ? { ws: API, route }
        : { ws: ui.workspace, route };
    ui.replaceTab(blank, review(`${API}#2`));
    expect(ui.workspace).toBe(API);
    expect(loadSnapshot(ALL)?.tabs).toEqual([{ name: 'inbox' }]);
  });
});

describe('migration of the pre-workspace tab set', () => {
  const legacy = {
    tabs: [{ name: 'inbox' }, { name: 'review', pr: 'github:acme/web#1' }],
    route: { name: 'review', pr: 'github:acme/web#1' },
  };

  it('moves the old tabs into All on first run', () => {
    localStorage.setItem('theodore-tabs', JSON.stringify(legacy));
    localStorage.setItem('theodore-aside', '0');
    const ui = new UiStore();
    expect(ui.workspace).toBe(ALL);
    expect(ui.tabs).toEqual(legacy.tabs);
    expect(ui.route).toEqual(legacy.route);
    expect(ui.aside).toBe(false);
    expect(localStorage.getItem('theodore-tabs')).toBeNull();
    expect(localStorage.getItem('theodore-aside')).toBeNull();
  });

  it('never overwrites an All snapshot that already exists', () => {
    localStorage.setItem(
      wsKey(ALL),
      JSON.stringify({ tabs: [{ name: 'tasks' }], route: { name: 'tasks' } }),
    );
    localStorage.setItem('theodore-tabs', JSON.stringify(legacy));
    migrateLegacyTabs();
    expect(loadSnapshot(ALL)?.tabs).toEqual([{ name: 'tasks' }]);
    expect(localStorage.getItem('theodore-tabs')).toBeNull();
  });

  it('leaves project pages to their own workspaces', () => {
    localStorage.setItem(
      'theodore-tabs',
      JSON.stringify({
        tabs: [{ name: 'tasks' }, { name: 'project', id: WEB }],
        route: { name: 'project', id: WEB },
      }),
    );
    const ui = new UiStore();
    expect(ui.tabs).toEqual([{ name: 'tasks' }]);
    expect(ui.route).toEqual({ name: 'tasks' });
  });

  it('starts fresh from a corrupt old value', () => {
    localStorage.setItem('theodore-tabs', '{oops');
    const ui = new UiStore();
    expect(ui.tabs).toEqual([{ name: 'inbox' }]);
    expect(localStorage.getItem('theodore-tabs')).toBeNull();
  });
});

describe('an old saved Projects tab', () => {
  it('loads without crashing, as the project’s settings', () => {
    localStorage.setItem(
      wsKey(WEB),
      JSON.stringify({
        tabs: [{ name: 'prs' }, { name: 'settings', page: 'projects' }],
        route: { name: 'settings', page: 'projects' },
        back: { name: 'prs' },
      }),
    );
    const s = loadSnapshot(WEB)!;
    const gone = { name: 'project-settings', project: WEB, section: 'general' };
    expect(s.route).toEqual(gone);
    expect(s.tabs).toEqual([{ name: 'prs' }, gone]);
  });

  it('under All it lands on Settings', () => {
    localStorage.setItem(
      wsKey(ALL),
      JSON.stringify({
        tabs: [{ name: 'settings', page: 'languages' }],
        route: { name: 'settings', page: 'languages' },
      }),
    );
    expect(loadSnapshot(ALL)!.route).toEqual({
      name: 'settings',
      page: 'agents',
    });
  });
});
