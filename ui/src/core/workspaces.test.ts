import { describe, expect, it } from 'vitest';
import type { InboxState, PrSummary, Project, RepoRef } from '../gen/contract';
import type { Route } from '../stores/UiStore';
import { sameRepo as projectsSameRepo } from '../ui/Projects';
import {
  ALL,
  homeRoute,
  inWorkspace,
  isWaiting,
  kindOf,
  migrateRoute,
  orderProjects,
  sectionsOf,
  placeRoute,
  reorderIds,
  repoOfPrId,
  sameRepo,
  waitingCounts,
  workspaceForKey,
  type PlaceContext,
} from './workspaces';

const repo = (name: string, over: Partial<RepoRef> = {}): RepoRef => ({
  host: 'github',
  owner: 'acme',
  name,
  ...over,
});

const project = (name: string, over: Partial<Project> = {}): Project => ({
  id: `github:acme/${name}`,
  repo: repo(name),
  path: null,
  status: 'ready',
  error: null,
  default_branch: 'main',
  added_at: 0,
  ...over,
});

const local = project('core.loc', {
  id: 'local:core.loc',
  repo: { host: 'local', owner: '', name: 'core.loc' },
  path: '/tmp/core.loc',
});

type Row = PrSummary & { id: string };
const pr = (id: string, r: RepoRef, over: Partial<PrSummary> = {}): Row => ({
  id,
  pr: { repo: r, number: 1, host_id: null },
  title: 't',
  author: { login: 'me', name: null, avatar_url: null },
  state: 'open',
  base_ref: 'main',
  head_ref: 'f',
  head_sha: 'abc',
  additions: 0,
  deletions: 0,
  changed_files: 0,
  reasons: ['review_requested'],
  checks: 'none',
  approvals: 0,
  required_approvals: null,
  updated_at: 0,
  url: '',
  ...over,
});

const web = project('web');
const api = project('api');

describe('repo matching', () => {
  it('agrees with the project page on what counts as the same repo', () => {
    const pairs: [RepoRef, RepoRef][] = [
      [repo('web'), repo('web')],
      [repo('web'), repo('api')],
      [repo('web'), repo('web', { host: 'gitlab' })],
      [repo('web'), repo('web', { owner: 'other' })],
    ];
    for (const [a, b] of pairs)
      expect(sameRepo(a, b)).toBe(projectsSameRepo(a, b));
  });

  it('reads the repo out of a PR id', () => {
    expect(repoOfPrId('github:acme/web#12')).toEqual(repo('web'));
    expect(repoOfPrId('gitlab:group/sub/proj#3')).toEqual({
      host: 'gitlab',
      owner: 'group/sub',
      name: 'proj',
    });
    expect(repoOfPrId('pr')).toBeNull();
    expect(repoOfPrId('github:web#1')).toBeNull();
  });
});

describe('scoping', () => {
  const projects = [web, api];

  it('shows everything under All', () => {
    expect(inWorkspace(ALL, repo('web'), projects)).toBe(true);
    expect(inWorkspace(ALL, repo('elsewhere'), projects)).toBe(true);
    expect(inWorkspace(ALL, null, projects)).toBe(true);
  });

  it('shows only its own repo in a project', () => {
    expect(inWorkspace(web.id, repo('web'), projects)).toBe(true);
    expect(inWorkspace(web.id, repo('api'), projects)).toBe(false);
    expect(inWorkspace(web.id, repo('web', { host: 'gitlab' }), projects)).toBe(
      false,
    );
  });

  it('keeps PRs of repos nobody added out of every project', () => {
    for (const p of projects)
      expect(inWorkspace(p.id, repo('stray'), projects)).toBe(false);
  });

  it('shows nothing for a workspace whose project is gone or unknown', () => {
    expect(inWorkspace('github:acme/gone', repo('gone'), projects)).toBe(false);
    expect(inWorkspace(web.id, null, projects)).toBe(false);
  });

  it('tells local folders apart', () => {
    expect(kindOf(ALL, [web, local])).toBe('all');
    expect(kindOf(web.id, [web, local])).toBe('repo');
    expect(kindOf(local.id, [web, local])).toBe('local');
  });
});

describe('switcher order', () => {
  const a = project('a', { added_at: 3 });
  const b = project('b', { added_at: 1 });
  const c = project('c', { added_at: 2 });

  it('follows the dragged order, then adds the rest by add time', () => {
    expect(orderProjects([a, b, c], []).map((p) => p.repo.name)).toEqual([
      'b',
      'c',
      'a',
    ]);
    expect(
      orderProjects([a, b, c], [a.id, 'gone']).map((p) => p.repo.name),
    ).toEqual(['a', 'b', 'c']);
  });

  it('moves dragged ids before or after the drop target', () => {
    const ids = ['a', 'b', 'c', 'd'];
    expect(reorderIds(ids, ['d'], 'a', 'before')).toEqual(['d', 'a', 'b', 'c']);
    expect(reorderIds(ids, ['a'], 'c', 'after')).toEqual(['b', 'c', 'a', 'd']);
    expect(reorderIds(ids, ['a', 'b'], 'd', 'after')).toEqual([
      'c',
      'd',
      'a',
      'b',
    ]);
  });

  it('leaves the order alone for a no-op drop', () => {
    const ids = ['a', 'b'];
    expect(reorderIds(ids, ['a'], 'a', 'after')).toBe(ids);
    expect(reorderIds(ids, ['x'], 'a', 'after')).toBe(ids);
    expect(reorderIds(ids, ['a'], 'x', 'after')).toBe(ids);
  });
});

describe('⌃ keys', () => {
  const key = (code: string, over: Partial<KeyboardEvent> = {}) => ({
    ctrlKey: true,
    metaKey: false,
    altKey: false,
    shiftKey: false,
    code,
    ...over,
  });
  const ids = ['p1', 'p2', 'p3'];

  it('maps ⌃1…9 to the Nth project and ⌃0 to All', () => {
    expect(workspaceForKey(key('Digit1'), ids)).toBe('p1');
    expect(workspaceForKey(key('Digit3'), ids)).toBe('p3');
    expect(workspaceForKey(key('Digit0'), ids)).toBe(ALL);
  });

  it('ignores digits past the last project', () => {
    expect(workspaceForKey(key('Digit4'), ids)).toBeNull();
    expect(workspaceForKey(key('Digit9'), [])).toBeNull();
  });

  it('ignores other modifiers and keys', () => {
    expect(workspaceForKey(key('Digit1', { ctrlKey: false }), ids)).toBeNull();
    expect(workspaceForKey(key('Digit1', { metaKey: true }), ids)).toBeNull();
    expect(workspaceForKey(key('Digit1', { altKey: true }), ids)).toBeNull();
    expect(workspaceForKey(key('Digit1', { shiftKey: true }), ids)).toBeNull();
    expect(workspaceForKey(key('KeyA'), ids)).toBeNull();
    expect(workspaceForKey(key('Numpad1'), ids)).toBeNull();
  });
});

describe('badge counts', () => {
  const state = (p: string, over: Partial<InboxState> = {}): InboxState => ({
    pr: p,
    done: false,
    snooze: null,
    ...over,
  });

  it('counts only PRs waiting for you', () => {
    expect(isWaiting(pr('1', repo('web')), undefined)).toBe(true);
    expect(isWaiting(pr('1', repo('web'), { reasons: [] }), undefined)).toBe(
      false,
    );
    expect(
      isWaiting(pr('1', repo('web'), { state: 'merged' }), undefined),
    ).toBe(false);
    expect(isWaiting(pr('1', repo('web'), { state: 'draft' }), undefined)).toBe(
      true,
    );
    expect(isWaiting(pr('1', repo('web')), state('1', { done: true }))).toBe(
      false,
    );
    expect(
      isWaiting(
        pr('1', repo('web')),
        state('1', { snooze: { kind: 'next_push' } }),
      ),
    ).toBe(false);
  });

  it('counts per project, everything under All, and zero for idle projects', () => {
    const prs = [
      pr('w1', repo('web')),
      pr('w2', repo('web')),
      pr('w3', repo('web'), { state: 'closed' }),
      pr('a1', repo('api')),
      pr('s1', repo('stray')),
    ];
    const counts = waitingCounts(
      prs,
      [state('a1', { done: true })],
      [web, api, local],
    );
    expect(counts.get(ALL)).toBe(3);
    expect(counts.get(web.id)).toBe(2);
    expect(counts.get(api.id)).toBe(0);
    expect(counts.get(local.id)).toBe(0);
  });

  it('uses the first inbox state of a PR, as the inbox does', () => {
    const counts = waitingCounts(
      [pr('w1', repo('web'))],
      [state('w1', { done: true }), state('w1')],
      [web],
    );
    expect(counts.get(web.id)).toBe(0);
  });
});

describe('placeRoute', () => {
  const review = (id: string): Route => ({ name: 'review', pr: id });
  const ctx = (over: Partial<PlaceContext> = {}): PlaceContext => ({
    current: ALL,
    currentTabs: [{ name: 'inbox' }],
    projects: [web, api, local],
    tabsOf: () => null,
    repoOfPr: repoOfPrId,
    ...over,
  });

  it('keeps a PR already open here', () => {
    const r = review('github:acme/api#1');
    expect(placeRoute(r, ctx({ current: web.id, currentTabs: [r] })).ws).toBe(
      web.id,
    );
  });

  it('never opens a second tab of a PR another workspace has open', () => {
    const r = review('github:acme/web#1');
    const placed = placeRoute(
      r,
      ctx({
        current: web.id,
        tabsOf: (ws) => (ws === ALL ? [r] : null),
      }),
    );
    expect(placed.ws).toBe(ALL);
  });

  it('opens in All while in All', () => {
    expect(placeRoute(review('github:acme/web#1'), ctx()).ws).toBe(ALL);
  });

  it('opens another project’s PR in the project that owns its repo', () => {
    expect(
      placeRoute(review('github:acme/api#1'), ctx({ current: web.id })).ws,
    ).toBe(api.id);
    expect(
      placeRoute(review('github:acme/web#1'), ctx({ current: web.id })).ws,
    ).toBe(web.id);
  });

  it('opens a PR no project owns in All', () => {
    expect(
      placeRoute(review('github:else/where#1'), ctx({ current: web.id })).ws,
    ).toBe(ALL);
  });

  it('stays put when the PR id has no readable repo', () => {
    expect(placeRoute(review('pr'), ctx({ current: web.id })).ws).toBe(web.id);
  });

  it('sends Inbox and Pull requests to Branches in a local folder', () => {
    for (const name of ['inbox', 'prs'] as const) {
      expect(placeRoute({ name }, ctx({ current: local.id }))).toEqual({
        ws: local.id,
        route: { name: 'branches' },
      });
      expect(placeRoute({ name }, ctx({ current: web.id })).route).toEqual({
        name,
      });
    }
  });

  it('keeps Branches to a single project', () => {
    expect(placeRoute({ name: 'branches' }, ctx()).route).toEqual({
      name: 'inbox',
    });
    expect(
      placeRoute({ name: 'branches' }, ctx({ current: web.id })).route,
    ).toEqual({ name: 'branches' });
  });

  it('starts All on the Inbox, a project on Pull requests, a folder on Branches', () => {
    expect(homeRoute(ALL)).toEqual({ name: 'inbox' });
    expect(homeRoute(web.id)).toEqual({ name: 'prs' });
    expect(homeRoute(local.id)).toEqual({ name: 'branches' });
  });
});

describe('sectionsOf', () => {
  it('gives a local folder Branches and no PR sections', () => {
    expect(sectionsOf('local')).toEqual(['branches', 'tasks', 'skills']);
  });

  it('gives Branches only to a single project', () => {
    expect(sectionsOf('all')).toEqual(['inbox', 'prs', 'tasks', 'skills']);
    expect(sectionsOf('repo')).toEqual([
      'inbox',
      'prs',
      'branches',
      'tasks',
      'skills',
    ]);
  });

  it('agrees with where a local folder opens', () => {
    expect(sectionsOf(kindOf(local.id, []))).toContain(
      homeRoute(local.id).name,
    );
  });
});

describe('migrateRoute', () => {
  it('turns My PRs into Pull requests', () => {
    expect(migrateRoute({ name: 'mine' }, ALL)).toEqual({ name: 'prs' });
    expect(migrateRoute({ name: 'mine' }, web.id)).toEqual({ name: 'prs' });
  });

  it('keeps a space tab', () => {
    const space = { name: 'space', id: 'abc' };
    expect(migrateRoute(space, web.id)).toEqual(space);
    expect(migrateRoute({ name: 'space' }, web.id)).toBeNull();
  });

  it('turns a project page into the workspace home', () => {
    const page = { name: 'project', id: web.id };
    expect(migrateRoute(page, web.id)).toEqual({ name: 'prs' });
    expect(migrateRoute(page, ALL)).toEqual({ name: 'prs' });
    expect(migrateRoute({ name: 'project', id: local.id }, local.id)).toEqual({
      name: 'branches',
    });
  });

  it('gives a local folder Branches for any PR page', () => {
    for (const name of ['mine', 'inbox', 'prs'])
      expect(migrateRoute({ name }, local.id)).toEqual({ name: 'branches' });
  });

  it('keeps routes that still exist as they are', () => {
    const kept: Route[] = [
      { name: 'inbox' },
      { name: 'tasks' },
      { name: 'skills' },
      { name: 'settings', page: 'agents' },
      { name: 'review', pr: 'github:acme/web#1' },
      { name: 'new', id: 'x' },
    ];
    for (const r of kept) expect(migrateRoute(r, ALL)).toEqual(r);
  });

  it('turns the old Settings → Projects and Languages tabs into the project’s settings', () => {
    expect(
      migrateRoute({ name: 'settings', page: 'projects' }, web.id),
    ).toEqual({
      name: 'project-settings',
      project: web.id,
      section: 'general',
    });
    expect(
      migrateRoute({ name: 'settings', page: 'languages' }, local.id),
    ).toEqual({
      name: 'project-settings',
      project: local.id,
      section: 'languages',
    });
    // Under All there is no one project: Settings' first page.
    for (const page of ['projects', 'languages'])
      expect(migrateRoute({ name: 'settings', page }, ALL)).toEqual({
        name: 'settings',
        page: 'agents',
      });
  });

  it('keeps a project settings tab, with a section it knows', () => {
    const r = { name: 'project-settings', project: web.id, section: 'config' };
    expect(migrateRoute(r, ALL)).toEqual(r);
    expect(migrateRoute({ ...r, section: 'nope' }, ALL)).toEqual({
      ...r,
      section: 'general',
    });
    expect(migrateRoute({ name: 'project-settings' }, ALL)).toBeNull();
  });

  it('drops anything it cannot read', () => {
    for (const raw of [
      null,
      'inbox',
      7,
      {},
      { name: 'gone' },
      { name: 'review' },
    ])
      expect(migrateRoute(raw, ALL)).toBeNull();
  });
});
