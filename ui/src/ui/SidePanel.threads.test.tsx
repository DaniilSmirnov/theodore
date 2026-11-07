/** A thread card shows how the thread started, with its reply count; the
 *  count opens every comment. */
import { afterEach, describe, expect, it } from 'vitest';
import { click, mount, type Mounted } from '../test/dom';
import { RootStore } from '../stores/RootStore';
import { RootStoreProvider } from '../stores/context';
import { Threads } from './SidePanel';

const PR = 'gh:acme/app#1';
const person = (login: string) => ({ login, name: null, avatar_url: null });
const comment = (id: string, login: string, body: string, at: number) => ({
  id,
  author: person(login),
  body,
  created_at: at,
  edited: false,
  reactions: [],
});

let m: Mounted | null = null;
afterEach(() => {
  m?.unmount();
  m = null;
});

describe('a thread card', () => {
  it('shows the first comment and opens the replies on click', async () => {
    const store = new RootStore();
    store.sync.pool.put('pr_detail', PR, {
      threads: [
        {
          id: 't1',
          anchor: null,
          resolved: false,
          outdated: false,
          comments: [
            comment('c1', 'anna', 'Should this be per-token?', 1),
            comment('c2', 'bob', 'Agreed, per-token.', 2),
          ],
        },
      ],
    });
    m = await mount(
      <RootStoreProvider store={store}>
        <Threads pr={PR} />
      </RootStoreProvider>,
    );
    const card = m.host.querySelector('.titem')!;
    expect(card.textContent).toContain('Should this be per-token?');
    expect(card.textContent).not.toContain('Agreed, per-token.');
    const more = card.querySelector<HTMLButtonElement>('.thread-replies')!;
    expect(more.textContent).toContain('1 reply');
    await click(more);
    expect(card.textContent).toContain('Agreed, per-token.');
  });
});
