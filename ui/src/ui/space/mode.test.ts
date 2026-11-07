import { observable, runInAction } from 'mobx';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Space } from '../../gen/contract';
import { notifySpace, takePendingSpace } from '../../core/native';
import { setTransport } from '../../core/transport';
import type { RootStore } from '../../stores/RootStore';
import { UiStore } from '../../stores/UiStore';
import { FakeTransport } from '../../test/fakeTransport';
import { openSpaceOnClick, watchSpaceTurns } from './mode';
import { BUILD_MODE_KEY } from './spaces';

vi.mock('../../core/native', () => ({
  OPEN_SPACE_EVENT: 'open-space',
  notifySpace: vi.fn(),
  setBadge: vi.fn(() => Promise.resolve()),
  takePendingSpace: vi.fn(() => Promise.resolve(null)),
}));

const notify = vi.mocked(notifySpace);
const pendingSpace = vi.mocked(takePendingSpace);
const flush = () => new Promise((r) => setTimeout(r, 0));

let transport: FakeTransport;
let spaces: Space[];
let store: RootStore;

const space = (id: string, status: Space['status']): Space =>
  ({
    id,
    name: id,
    goal: '',
    status,
    error: null,
    turns: 1,
    updated_at: 1,
  }) as Space;

beforeEach(() => {
  localStorage.clear();
  transport = new FakeTransport();
  setTransport(transport);
  notify.mockReset();
  spaces = observable.array<Space>([], { deep: false });
  store = {
    ui: new UiStore(),
    ephemeral: { toast: vi.fn() },
    models: {
      spaces: {
        all: () => spaces,
        get: (id: string) => spaces.find((s) => s.id === id),
      },
      settings: {
        all: () => [{ key: BUILD_MODE_KEY, scope: 'user', value: true }],
      },
    },
  } as unknown as RootStore;
});

afterEach(() => vi.restoreAllMocks());

describe('a click on a space notification', () => {
  it('opens the space the shell names', async () => {
    runInAction(() => spaces.push(space('sp1', 'idle')));
    const opened = vi.fn();
    const off = openSpaceOnClick(store, opened);
    await flush();

    transport.emit('open-space', 'sp1');
    expect(store.ui.route).toEqual({ name: 'space', id: 'sp1' });
    expect(opened).toHaveBeenCalledOnce();

    // A space that is gone opens nothing.
    store.ui.go({ name: 'inbox' });
    transport.emit('open-space', 'gone');
    expect(store.ui.route).toEqual({ name: 'inbox' });
    off();
  });

  it('opens the space a link asked for before the UI listened', async () => {
    pendingSpace.mockResolvedValueOnce('sp1');
    const opened = vi.fn();
    const off = openSpaceOnClick(store, opened);
    await flush();
    // Its row isn't there yet on a cold start: the first sync brings it.
    expect(opened).not.toHaveBeenCalled();

    runInAction(() => spaces.push(space('sp1', 'idle')));
    expect(store.ui.route).toEqual({ name: 'space', id: 'sp1' });
    expect(opened).toHaveBeenCalledOnce();
    off();
  });
});

describe('a turn that ends with the window in the background', () => {
  async function finishTurn(clicks: boolean) {
    vi.spyOn(document, 'hasFocus').mockReturnValue(false);
    notify.mockResolvedValue(clicks);
    const off = watchSpaceTurns(store);
    runInAction(() => spaces.push(space('sp1', 'running')));
    runInAction(() => spaces.splice(0, 1, space('sp1', 'idle')));
    await flush();
    expect(notify).toHaveBeenCalledWith('sp1', 'sp1: turn finished', undefined);
    return off;
  }

  it('opens the space on the click, not on the next focus', async () => {
    const off = await finishTurn(true);

    window.dispatchEvent(new Event('focus'));
    expect(store.ui.route).not.toEqual({ name: 'space', id: 'sp1' });

    transport.emit('open-space', 'sp1');
    expect(store.ui.route).toEqual({ name: 'space', id: 'sp1' });
    off();
  });

  it('falls back to the next focus where clicks are not reported', async () => {
    const off = await finishTurn(false);

    window.dispatchEvent(new Event('focus'));
    expect(store.ui.route).toEqual({ name: 'space', id: 'sp1' });
    off();
  });
});
