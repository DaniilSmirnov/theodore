import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import type { AppInfo, UpdateInfo } from '../gen/contract';
import { RootStore } from '../stores/RootStore';
import { FakeTransport } from '../test/fakeTransport';
import { setTransport } from './transport';
import { setCommandFailureHandler } from './api';
import { checkFailureText, checkForUpdates, type Toaster } from './updateCheck';

const APP: AppInfo = {
  version: '0.1.6',
  commit: 'abc',
  build_env: 'production',
  built_at: 0,
  channel: 'stable',
  update: null,
};

/** Records every toast line, by id, latest text last. */
class FakeToaster implements Toaster {
  readonly log: { id: string; text: string; variant?: string }[] = [];
  private n = 0;
  show(text: string, opts: { variant?: string }) {
    const id = `t${++this.n}`;
    this.log.push({ id, text, variant: opts.variant });
    return id;
  }
  update(id: string, text: string, opts: { variant?: string }) {
    this.log.push({ id, text, variant: opts.variant });
  }
  readonly closed: string[] = [];
  close(id: string) {
    this.closed.push(id);
  }
  texts() {
    return this.log.map((t) => t.text);
  }
}

let transport: FakeTransport;
let store: RootStore;
let toaster: FakeToaster;

beforeEach(() => {
  transport = new FakeTransport();
  setTransport(transport);
  store = new RootStore();
  store.ephemeral.setAppInfo(APP);
  toaster = new FakeToaster();
});

afterEach(() => {
  setCommandFailureHandler(() => {});
  Object.defineProperty(navigator, 'onLine', {
    configurable: true,
    get: () => true,
  });
});

const update = (over: Partial<UpdateInfo> = {}): UpdateInfo => ({
  version: '0.1.7',
  notes: '',
  downloaded: true,
  ...over,
});

describe('checkForUpdates', () => {
  it('says checking, then that the running version is up to date', async () => {
    transport.queueResult('check_update', null);
    await checkForUpdates(store, toaster);
    expect(toaster.texts()).toEqual([
      'Checking for updates…',
      'Theodore 0.1.6 is up to date',
    ]);
    // One toast, updated in place.
    expect(new Set(toaster.log.map((t) => t.id)).size).toBe(1);
  });

  it('says downloading while the shell downloads, then leaves it to the pill', async () => {
    transport.onCall('check_update', async () => {
      // The shell's `update` event before the download starts.
      store.ephemeral.setUpdate(update({ downloaded: false }));
      await Promise.resolve();
      return update();
    });
    await checkForUpdates(store, toaster);
    expect(toaster.texts()).toEqual([
      'Checking for updates…',
      'Downloading Theodore 0.1.7…',
    ]);
    expect(toaster.closed).toEqual(['t1']);
  });

  it('gives the reason a check failed, without a second raw toast', async () => {
    const raw: string[] = [];
    setCommandFailureHandler((f) => raw.push(f.message));
    transport.onCall('check_update', () => {
      throw {
        code: 'update',
        message: "couldn't check for updates: no release for darwin-aarch64",
      };
    });
    await checkForUpdates(store, toaster);
    expect(toaster.log.at(-1)).toEqual({
      id: 't1',
      text: 'Couldn’t check for updates: no release for darwin-aarch64',
      variant: 'danger',
    });
    expect(raw).toEqual([]);
  });

  it('says offline when offline', async () => {
    Object.defineProperty(navigator, 'onLine', {
      configurable: true,
      get: () => false,
    });
    transport.onCall('check_update', () => {
      throw {
        code: 'update',
        message: "couldn't check for updates: error sending request",
      };
    });
    await checkForUpdates(store, toaster);
    expect(toaster.texts().at(-1)).toBe('You’re offline');
  });

  it('adds no toast when an update is already installed: the pill says it', async () => {
    store.ephemeral.setUpdate(update());
    await checkForUpdates(store, toaster);
    expect(toaster.texts()).toEqual([]);
    expect(transport.calls).toEqual([]);
  });
});

describe('setUpdate', () => {
  it('never hides an installed update behind a found one', () => {
    store.ephemeral.setUpdate(update());
    store.ephemeral.setUpdate(update({ downloaded: false }));
    expect(store.ephemeral.readyUpdate?.version).toBe('0.1.7');
  });
});

describe('checkFailureText', () => {
  it('drops the repeated prefix and keeps the first line', () => {
    expect(
      checkFailureText(
        "couldn't check for updates: bad signature\nmore detail",
        false,
      ),
    ).toBe('Couldn’t check for updates: bad signature');
    expect(checkFailureText('download or signature check failed', false)).toBe(
      'Couldn’t check for updates: download or signature check failed',
    );
  });

  it('names an unreachable server when online, offline when not', () => {
    const msg = "couldn't check for updates: error sending request for url";
    expect(checkFailureText(msg, false)).toBe(
      'Couldn’t check for updates: the update server is unreachable',
    );
    expect(checkFailureText(msg, true)).toBe('You’re offline');
  });
});
