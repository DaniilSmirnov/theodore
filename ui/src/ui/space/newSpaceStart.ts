/** New space's "Start from" choices (docs/spaces.md §3 "Start from an
 *  issue, a branch, or a PR"): pure helpers, kept apart from the modal. */

/** `goal`: the plain, usual case. `issue` / `branch` / `pr` adopt something
 *  that already exists on the host instead of forking a fresh branch from
 *  the goal. */
export type StartKind = 'goal' | 'issue' | 'branch' | 'pr';

/** The issue number in a pasted issue URL, `#n` or `n`; `null` for
 *  anything else. The core parses the same way (`parse_issue_number`). */
export function parseIssueRef(input: string): number | null {
  const s = input.trim();
  let raw = s;
  if (/^https?:\/\//.test(s)) {
    const path = s.split(/[?#]/)[0] ?? s;
    raw = path.replace(/\/+$/, '').split('/').pop() ?? '';
  } else {
    raw = s.replace(/^#+/, '');
  }
  if (!/^\d+$/.test(raw)) return null;
  const n = Number(raw);
  return Number.isSafeInteger(n) && n > 0 ? n : null;
}

/** The line under the composer: what the chosen start does to the
 *  worktree and to shipping. Each kind says its own. */
export function startFooter(kind: StartKind, defaultBranch?: string): string {
  const base = defaultBranch || 'the default branch';
  switch (kind) {
    case 'goal':
      return 'The agent works in its own worktree — your checkout is never touched.';
    case 'issue':
      return `A new branch off ${base}; the PR it opens closes the issue.`;
    case 'branch':
      return 'The worktree starts on its tip; shipping pushes back to the same branch.';
    case 'pr':
      return "The worktree starts on the PR's head; Push updates the same PR.";
  }
}
