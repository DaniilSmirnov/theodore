import { describe, expect, it } from 'vitest';
import type { PrSummary, Space } from '../gen/contract';
import {
  findScopeFor,
  matchesSearch,
  prSearchText,
  spaceSearchText,
} from './search';

describe('matchesSearch', () => {
  it('matches a substring, case- and diacritic-insensitive', () => {
    expect(matchesSearch('Fix the Login Redirect', 'login')).toBe(true);
    expect(matchesSearch('Fix the Login Redirect', 'LOGIN')).toBe(true);
    expect(matchesSearch('Crème brûlée', 'creme brulee')).toBe(true);
    expect(matchesSearch('Fix the Login Redirect', 'logout')).toBe(false);
  });

  it('requires every word in the query to appear, in any order', () => {
    expect(matchesSearch('storefront #104 retry export', 'export 104')).toBe(
      true,
    );
    expect(matchesSearch('storefront #104 retry export', 'export 999')).toBe(
      false,
    );
  });

  it('matches everything for an empty or blank query', () => {
    expect(matchesSearch('anything', '')).toBe(true);
    expect(matchesSearch('anything', '   ')).toBe(true);
  });
});

describe('prSearchText', () => {
  it('includes title, number, author, repo and branch', () => {
    const p: Pick<PrSummary, 'title' | 'pr' | 'author' | 'head_ref'> = {
      title: 'Fix the login redirect',
      pr: {
        repo: { host: 'github', owner: 'acme', name: 'storefront' },
        number: 104,
        host_id: null,
      },
      author: { login: 'ada', name: null, avatar_url: null },
      head_ref: 'ada/fix-login',
    };
    const text = prSearchText(p);
    expect(matchesSearch(text, '104')).toBe(true);
    expect(matchesSearch(text, 'ada')).toBe(true);
    expect(matchesSearch(text, 'storefront')).toBe(true);
    expect(matchesSearch(text, 'fix-login')).toBe(true);
    expect(matchesSearch(text, 'redirect')).toBe(true);
  });
});

describe('spaceSearchText', () => {
  it('includes name, title, goal and branch, and tolerates no title', () => {
    const s: Pick<Space, 'name' | 'title' | 'goal' | 'branch'> = {
      name: 'fix-login',
      title: null,
      goal: 'Retry the export job on 429s',
      branch: 'ada/fix-login',
    };
    const text = spaceSearchText(s);
    expect(matchesSearch(text, 'export')).toBe(true);
    expect(matchesSearch(text, 'fix-login')).toBe(true);
    expect(() =>
      spaceSearchText({ ...s, title: 'Retry exports' }),
    ).not.toThrow();
  });
});

describe('findScopeFor', () => {
  it('searches the PR list on Inbox and My PRs', () => {
    expect(findScopeFor('inbox', true)).toBe('prs');
    expect(findScopeFor('inbox', false)).toBe('prs');
    expect(findScopeFor('prs', false)).toBe('prs');
  });

  it("leaves the key alone in a diff/text view, for the webview's own find", () => {
    expect(findScopeFor('review', true)).toBeNull();
    expect(findScopeFor('space', true)).toBeNull();
  });

  it('has no sidebar to search on Settings', () => {
    expect(findScopeFor('settings', true)).toBeNull();
  });

  it('falls back to the Spaces section elsewhere, only with build mode on', () => {
    expect(findScopeFor('branches', true)).toBe('spaces');
    expect(findScopeFor('tasks', true)).toBe('spaces');
    expect(findScopeFor('branches', false)).toBeNull();
  });
});
