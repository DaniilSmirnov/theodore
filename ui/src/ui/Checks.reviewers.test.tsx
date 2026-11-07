/** The reviewer picker never offers the PR's author: the host refuses a
 *  review request from them. */
import { afterEach, beforeEach, expect, it } from 'vitest';
import { click, mount, settle, type Mounted } from '../test/dom';
import { setTransport } from '../core/transport';
import { RootStore } from '../stores/RootStore';
import { RootStoreProvider } from '../stores/context';
import { FakeTransport } from '../test/fakeTransport';
import { ReviewerPicker } from './Checks';

// jsdom has no `CSS.escape`; an open react-aria popover calls it.
(globalThis as { CSS?: { escape?: (s: string) => string } }).CSS ??= {};
globalThis.CSS.escape ??= (s: string) =>
  s.replace(/[^a-zA-Z0-9_-]/g, (c) => `\\${c}`);

const PR = 'github:acme/app#1';
const person = (login: string) => ({ login, name: null, avatar_url: null });
let m: Mounted | null = null;
let transport: FakeTransport;

beforeEach(() => {
  transport = new FakeTransport();
  setTransport(transport);
  transport.onCall('assignable_users', () => [
    person('alice'),
    person('octocat'),
    person('bob'),
  ]);
});

afterEach(() => {
  m?.unmount();
  m = null;
});

it('leaves the author out of the reviewers it offers', async () => {
  const store = new RootStore();
  store.sync.pool.put('pr_detail', PR, {
    summary: { author: person('octocat') },
    reviewers: [],
    threads: [],
    checks: [],
    commits: [],
  });
  m = await mount(
    <RootStoreProvider store={store}>
      <ReviewerPicker pr={PR} />
    </RootStoreProvider>,
  );
  await click(m.host.querySelector('button[aria-label="Reviewers"]'));
  await settle();
  await settle();
  const offered = [...document.querySelectorAll('[role^="menuitem"]')].map(
    (el) => el.textContent?.trim(),
  );
  expect(offered).toEqual(['alice', 'bob']);
});

it('says so when no one but the author could review', async () => {
  transport.onCall('assignable_users', () => [person('octocat')]);
  const store = new RootStore();
  store.sync.pool.put('pr_detail', PR, {
    summary: { author: person('octocat') },
    reviewers: [],
    threads: [],
    checks: [],
    commits: [],
  });
  m = await mount(
    <RootStoreProvider store={store}>
      <ReviewerPicker pr={PR} />
    </RootStoreProvider>,
  );
  await click(m.host.querySelector('button[aria-label="Reviewers"]'));
  await settle();
  await settle();
  expect(document.body.textContent).toContain('No one else can review this PR');
});
