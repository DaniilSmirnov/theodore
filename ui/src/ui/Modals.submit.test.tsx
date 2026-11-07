/** Submitting a review moves on to the next PR only once the host has it:
 *  a refusal keeps you on the PR, next to its toast. */
import { afterEach, expect, it } from 'vitest';
import { click, mount, settle, type Mounted } from '../test/dom';
import { setTransport } from '../core/transport';
import { RootStore } from '../stores/RootStore';
import { RootStoreProvider } from '../stores/context';
import { FakeTransport } from '../test/fakeTransport';
import { Modals } from './Modals';

// jsdom has no `CSS.escape`; an open react-aria modal calls it.
(globalThis as { CSS?: { escape?: (s: string) => string } }).CSS ??= {};
globalThis.CSS.escape ??= (s: string) =>
  s.replace(/[^a-zA-Z0-9_-]/g, (c) => `\\${c}`);

const PR = 'github:acme/app#1';
let m: Mounted | null = null;

afterEach(() => {
  m?.unmount();
  m = null;
});

async function submitWith(answer: () => unknown) {
  const transport = new FakeTransport();
  transport.onCall('submit_review', answer);
  setTransport(transport);
  const store = new RootStore();
  store.sync.pool.put('pr', PR, {
    pr: { repo: { host: 'github', owner: 'acme', name: 'app' }, number: 1 },
    title: 'A PR',
  });
  store.ui.go({ name: 'inbox' });
  store.ui.go({ name: 'review', pr: PR });
  store.ui.openModal('submit');
  m = await mount(
    <RootStoreProvider store={store}>
      <Modals />
    </RootStoreProvider>,
  );
  const submit = [...document.querySelectorAll('button')].find(
    (b) => b.textContent === 'Submit',
  );
  await click(submit);
  await settle();
  await settle();
  return store;
}

it('moves on once the review landed', async () => {
  const store = await submitWith(() => null);
  expect(store.ui.modal).toBeNull();
  expect(store.ui.route.name).toBe('inbox');
});

it('stays on the PR when the host refuses it', async () => {
  const store = await submitWith(() => {
    throw {
      code: 'host',
      message: 'Can not approve your own pull request',
    };
  });
  expect(store.ui.modal).toBeNull();
  expect(store.ui.route).toEqual({ name: 'review', pr: PR });
});
