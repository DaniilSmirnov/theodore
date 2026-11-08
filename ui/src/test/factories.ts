/**
 * Data factories for UI tests, typed on the generated contract so a contract
 * change fails to compile here instead of passing silently. Each factory
 * returns a valid object; pass `overrides` for what the test cares about.
 */
import type {
  Anchor,
  Draft,
  Finding,
  InterdiffEntry,
  OpenPrResponse,
  Person,
  SubmitReviewRequest,
} from '../gen/contract';

export const SHA = 'a'.repeat(40);
export const PR_ID = 'gh:acme/app#1';

let seq = 0;
export const nextId = (prefix: string): string => `${prefix}-${++seq}`;

export const anchor = (o: Partial<Anchor> = {}): Anchor => ({
  rev: SHA,
  path: 'src/lib.rs',
  side: 'new',
  line: 10,
  start_line: null,
  start_side: null,
  ...o,
});

export const draft = (o: Partial<Draft> = {}): Draft => ({
  id: nextId('draft'),
  pr: PR_ID,
  anchor: anchor(),
  body: 'Please rename this.',
  source: { kind: 'me' },
  ...o,
});

export const finding = (o: Partial<Finding> = {}): Finding => ({
  id: nextId('finding'),
  pr: PR_ID,
  skill: 'review',
  agents: [],
  anchor: anchor(),
  severity: 'warn',
  title: 'Unchecked unwrap',
  body: 'This can panic.',
  state: 'open',
  fix: null,
  ...o,
});

export const person = (o: Partial<Person> = {}): Person => ({
  login: 'octocat',
  name: null,
  avatar_url: null,
  ...o,
});

export const openPrResponse = (
  o: Partial<OpenPrResponse> = {},
): OpenPrResponse => ({ pr: PR_ID, head: SHA, ...o });

export const submitReviewRequest = (
  o: Partial<SubmitReviewRequest> = {},
): SubmitReviewRequest => ({ pr: PR_ID, event: 'COMMENT', body: '', ...o });

export const interdiffEntry = (
  o: Partial<InterdiffEntry> = {},
): InterdiffEntry => ({
  hunk: nextId('hunk'),
  path: 'src/lib.rs',
  class: 'changed',
  previous: null,
  words: [],
  ...o,
});
