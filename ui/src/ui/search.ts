/**
 * ⌘F (GitHub #77): a search field over whichever list is "current" — the
 * PR list on Inbox / My PRs, or the sidebar's Spaces section everywhere
 * else build mode is on. Pure matching and scope rules, so they're
 * unit-testable without mounting a page.
 */
import type { PrSummary, Space } from '../gen/contract';

export type SearchScope = 'prs' | 'spaces';

function normalize(s: string): string {
  return s.normalize('NFD').replace(/[̀-ͯ]/g, '').toLowerCase();
}

/** Fuzzy-ish substring match: every word in `query` must appear somewhere
 *  in `text`, case- and diacritic-insensitive — good enough for "type a
 *  few letters, see it" without the weight of real fuzzy scoring. An
 *  empty query matches everything. */
export function matchesSearch(text: string, query: string): boolean {
  const words = normalize(query).split(/\s+/).filter(Boolean);
  if (words.length === 0) return true;
  const hay = normalize(text);
  return words.every((w) => hay.includes(w));
}

/** Title, number, author, repo and branch — what ⌘F searches on a PR row. */
export function prSearchText(
  p: Pick<PrSummary, 'title' | 'pr' | 'author' | 'head_ref'>,
): string {
  return `${p.pr.number} ${p.title} ${p.author.login} ${p.pr.repo.name} ${p.head_ref}`;
}

/** Name, title, goal and branch — what ⌘F searches on a space row. */
export function spaceSearchText(
  s: Pick<Space, 'name' | 'title' | 'goal' | 'branch'>,
): string {
  return `${s.name} ${s.title ?? ''} ${s.goal} ${s.branch}`;
}

/** Which list ⌘F acts on for the current route — `null` leaves the key
 *  alone: `review` and `space` are diff/text views where the webview's own
 *  find fits better (today neither binds ⌘F at all, so this only reserves
 *  the key for it); `settings` has no sidebar Spaces section to search. */
export function findScopeFor(
  routeName: string,
  spacesOn: boolean,
): SearchScope | null {
  if (routeName === 'inbox' || routeName === 'prs') return 'prs';
  if (routeName === 'review' || routeName === 'space') return null;
  if (routeName === 'settings') return null;
  return spacesOn ? 'spaces' : null;
}
