import { describe, expect, it } from 'vitest';
import type { Project, RepoRef } from '../../gen/contract';
import { ALL } from '../../core/workspaces';
import type { InboxPr } from '../List';
import {
  filterCount,
  isPrFilter,
  openPrCount,
  prGroups,
  scopeToWorkspace,
  type PrGroup,
} from './filters';

const web: RepoRef = { host: 'github', owner: 'acme', name: 'web' };
const api: RepoRef = { host: 'github', owner: 'acme', name: 'api' };
const stray: RepoRef = { host: 'github', owner: 'else', name: 'where' };

const project = (repo: RepoRef): Project => ({
  id: `github:${repo.owner}/${repo.name}`,
  repo,
  path: null,
  status: 'ready',
  error: null,
  default_branch: 'main',
  added_at: 0,
});
const projects = [project(web), project(api)];

const me = { login: 'me', name: null, avatar_url: null };
const them = { login: 'them', name: null, avatar_url: null };

const pr = (over: Partial<InboxPr> & { id: string }): InboxPr => ({
  pr: { repo: web, number: 1, host_id: null },
  title: 'a change',
  author: them,
  state: 'open',
  base_ref: 'main',
  head_ref: 'feature',
  head_sha: 'abc',
  additions: 1,
  deletions: 1,
  changed_files: 1,
  reasons: [],
  checks: 'pass',
  approvals: 0,
  required_approvals: null,
  updated_at: 0,
  url: 'https://github.com/acme/web/pull/1',
  review_decision: null,
  conflicts: false,
  ...over,
});
const branchReview = (id: string): InboxPr =>
  pr({ id, url: '', author: me, head_ref: id });

const ids = (groups: { items: InboxPr[] }[]) =>
  groups.flatMap((g) => g.items.map((p) => p.id));
const group = (groups: PrGroup[], id: string) =>
  groups.find((g) => g.id === id);

const rows = [
  pr({ id: 'open', updated_at: 5 }),
  pr({ id: 'draft', state: 'draft', updated_at: 4 }),
  pr({ id: 'merged-old', state: 'merged', updated_at: 1 }),
  pr({ id: 'merged-new', state: 'merged', updated_at: 9 }),
  pr({ id: 'closed', state: 'closed', updated_at: 3 }),
  pr({ id: 'review', reasons: ['review_requested'], updated_at: 2 }),
  pr({ id: 'mine-open', author: me, updated_at: 8 }),
  pr({ id: 'mine-draft', author: me, state: 'draft', updated_at: 7 }),
  pr({ id: 'mine-merged', author: me, state: 'merged', updated_at: 6 }),
  branchReview('b1'),
];

describe('prGroups: All', () => {
  it('shows open and draft PRs, never merged or closed ones', () => {
    const shown = ids(prGroups(rows, 'all', 'me'));
    expect(shown).toEqual(
      expect.arrayContaining(['open', 'draft', 'review', 'mine-open', 'b1']),
    );
    for (const gone of ['merged-old', 'merged-new', 'closed', 'mine-merged'])
      expect(shown).not.toContain(gone);
  });

  it('puts branch reviews in their own group, collapsed', () => {
    const g = group(prGroups(rows, 'all', 'me'), 'branches');
    expect(g?.title).toBe('Branch reviews');
    expect(g?.collapsed).toBe(true);
    expect(ids([g!])).toEqual(['b1']);
  });

  it('keeps the project sections, review requests first', () => {
    const groups = prGroups(rows, 'all', 'me');
    expect(groups[0]!.id).toBe('needs_review');
    expect(ids([group(groups, 'yours')!])).toEqual(['mine-open', 'mine-draft']);
  });

  it('drops empty groups', () => {
    const groups = prGroups([pr({ id: 'only' })], 'all', 'me');
    expect(groups.map((g) => g.id)).toEqual(['open']);
  });
});

describe('prGroups: Mine', () => {
  it('lists only your PRs, by next step, merged collapsed', () => {
    const groups = prGroups(rows, 'mine', 'me');
    expect(groups.map((g) => g.id)).toEqual(['merge', 'draft', 'merged']);
    expect(ids(groups)).toEqual(['mine-open', 'mine-draft', 'mine-merged']);
    expect(group(groups, 'merged')?.collapsed).toBe(true);
    expect(group(groups, 'merge')?.collapsed).toBe(false);
  });

  it('leaves branch reviews out', () => {
    expect(ids(prGroups([branchReview('b1')], 'mine', 'me'))).toEqual([]);
  });

  it('is empty while nobody is signed in', () => {
    expect(prGroups(rows, 'mine', null)).toEqual([]);
  });
});

describe('prGroups: Drafts and Merged', () => {
  it('Drafts lists every draft, newest first', () => {
    expect(ids(prGroups(rows, 'drafts', 'me'))).toEqual([
      'mine-draft',
      'draft',
    ]);
  });

  it('Merged lists merged PRs, most recent first', () => {
    expect(ids(prGroups(rows, 'merged', 'me'))).toEqual([
      'merged-new',
      'mine-merged',
      'merged-old',
    ]);
  });
});

describe('counts', () => {
  it('counts open PRs, drafts included, branch reviews not', () => {
    expect(openPrCount(rows)).toBe(5);
    expect(filterCount(rows, 'all', 'me')).toBe(5);
  });

  it('counts your open PRs under Mine and every row otherwise', () => {
    expect(filterCount(rows, 'mine', 'me')).toBe(2);
    expect(filterCount(rows, 'mine', null)).toBe(0);
    expect(filterCount(rows, 'drafts', 'me')).toBe(2);
    expect(filterCount(rows, 'merged', 'me')).toBe(3);
  });
});

describe('scopeToWorkspace', () => {
  const spread = [
    pr({ id: 'w', pr: { repo: web, number: 1, host_id: null } }),
    pr({ id: 'a', pr: { repo: api, number: 2, host_id: null } }),
    pr({ id: 's', pr: { repo: stray, number: 3, host_id: null } }),
  ];

  it('spans every repo under All projects, ones no project owns too', () => {
    const scoped = scopeToWorkspace(spread, ALL, projects);
    expect(scoped.map((p) => p.id)).toEqual(['w', 'a', 's']);
    expect(ids(prGroups(scoped, 'all', 'me'))).toEqual(['w', 'a', 's']);
  });

  it("keeps a project to its own repo's PRs", () => {
    const scoped = scopeToWorkspace(spread, projects[1]!.id, projects);
    expect(scoped.map((p) => p.id)).toEqual(['a']);
  });
});

describe('isPrFilter', () => {
  it('knows the four filters and nothing else', () => {
    for (const f of ['all', 'mine', 'drafts', 'merged'])
      expect(isPrFilter(f)).toBe(true);
    expect(isPrFilter('open')).toBe(false);
    expect(isPrFilter(null)).toBe(false);
  });
});

describe('the Pull requests number', () => {
  it('leaves out a branch or space review stored under a host repo', () => {
    // A review of a GitHub project's branch keeps its host in the id, with
    // the branch's hash as its number and a commit's email as its author.
    const review = pr({
      id: 'github:acme/web#2681023619',
      url: '',
      author: { login: 'me@example.com', name: null, avatar_url: null },
      pr: { repo: web, number: 2681023619, host_id: 'feature' },
    });
    expect(openPrCount([review])).toBe(0);
    expect(filterCount([review], 'all', 'me')).toBe(0);
  });

  it('counts drafts and other people\'s PRs, as the list under it shows them', () => {
    const rows = [
      pr({ id: 'mine', author: me }),
      pr({ id: 'theirs', author: them }),
      pr({ id: 'draft', state: 'draft', author: them }),
      pr({ id: 'merged', state: 'merged' }),
      pr({ id: 'closed', state: 'closed' }),
      branchReview('review'),
    ];
    const listed = prGroups(rows, 'all', 'me')
      .filter((g) => g.id !== 'branches')
      .flatMap((g) => g.items);
    expect(openPrCount(rows)).toBe(3);
    expect(listed.map((p) => p.id).sort()).toEqual(['draft', 'mine', 'theirs']);
  });
});
