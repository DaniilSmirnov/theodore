import { beforeEach, describe, expect, it } from 'vitest';
import type {
  CodeInstall,
  CodeIntelStatus,
  CodeLanguage,
} from '../../gen/contract';
import { setTransport } from '../../core/transport';
import { FakeTransport } from '../../test/fakeTransport';
import {
  LanguagesModel,
  canDecline,
  canInstall,
  installView,
  languageName,
  lspNote,
  needsPoll,
  suggestText,
  type Timers,
} from './languages';

const lang = (over: Partial<CodeLanguage> = {}): CodeLanguage => ({
  language: 'rust',
  files: 12,
  grammar: { state: 'installed', version: '0.23.0' },
  server: {
    name: 'rust-analyzer',
    install: { state: 'system', path: '/usr/bin/rust-analyzer', version: null },
    state: 'ready',
  },
  index_percent: 100,
  ...over,
});

const status = (languages: CodeLanguage[]): CodeIntelStatus => ({
  enabled: true,
  install: 'ask',
  lsp: {
    review: { on: false, from_config: null },
    spaces: { on: true, from_config: null },
  },
  languages,
});

const server = (install: CodeInstall, state: 'ready' | 'warming' = 'ready') =>
  ({ name: 'clangd', install, state }) as const;

/** Timers the test fires by hand. */
class ManualTimers implements Timers {
  armed = new Map<number, () => void>();
  private next = 1;
  set(fn: () => void): unknown {
    const id = this.next++;
    this.armed.set(id, fn);
    return id;
  }
  clear(h: unknown): void {
    this.armed.delete(h as number);
  }
  fire(): void {
    const all = [...this.armed.values()];
    this.armed.clear();
    for (const fn of all) fn();
  }
}

const flush = () => new Promise((r) => setTimeout(r, 0));

let transport: FakeTransport;

beforeEach(() => {
  transport = new FakeTransport();
  setTransport(transport);
});

describe('installView', () => {
  it('maps every install state to a label', () => {
    expect(installView({ state: 'missing' }).label).toBe('Missing');
    expect(installView({ state: 'downloading' })).toMatchObject({
      label: 'Downloading',
      downloading: true,
    });
    expect(
      installView({ state: 'system', path: '/bin/clangd', version: '18.1.3' }),
    ).toMatchObject({ label: 'System v18.1.3', detail: '/bin/clangd' });
    expect(
      installView({ state: 'system', path: '/x', version: null }).label,
    ).toBe('System');
    expect(installView({ state: 'installed', version: '0.0.95' }).label).toBe(
      'Installed v0.0.95',
    );
    // A date-like version reads better without a "v".
    expect(
      installView({ state: 'installed', version: '2026-10-26' }).label,
    ).toBe('Installed 2026-10-26');
    expect(installView({ state: 'unavailable' }).label).toBe('Unavailable');
    expect(installView({ state: 'declined' }).label).toBe('Declined');
    expect(
      installView({ state: 'failed', error: 'sha256 mismatch' }),
    ).toMatchObject({
      label: 'Failed',
      tone: 'danger',
      detail: 'sha256 mismatch',
    });
  });
});

describe('actions per language', () => {
  it('offers Install and Decline for a missing server', () => {
    const l = lang({ server: server({ state: 'missing' }) });
    expect(canInstall(l)).toBe(true);
    expect(canDecline(l)).toBe(true);
  });
  it('offers Install again after a decline or a failure, no Decline', () => {
    for (const install of [
      { state: 'declined' },
      { state: 'failed', error: 'x' },
    ] as CodeInstall[]) {
      const l = lang({ server: server(install) });
      expect(canInstall(l)).toBe(true);
      expect(canDecline(l)).toBe(false);
    }
  });
  it('offers nothing when everything is on disk or has no download', () => {
    expect(canInstall(lang())).toBe(false);
    expect(canInstall(lang({ server: server({ state: 'unavailable' }) }))).toBe(
      false,
    );
  });
  it('offers Install for a missing grammar without a server', () => {
    const l = lang({ grammar: { state: 'missing' }, server: null });
    expect(canInstall(l)).toBe(true);
    expect(canDecline(l)).toBe(false);
  });
});

describe('needsPoll', () => {
  it('polls while a grammar or server downloads, or a server warms up', () => {
    expect(needsPoll(status([lang()]))).toBe(false);
    expect(
      needsPoll(status([lang({ grammar: { state: 'downloading' } })])),
    ).toBe(true);
    expect(
      needsPoll(status([lang({ server: server({ state: 'downloading' }) })])),
    ).toBe(true);
    expect(
      needsPoll(
        status([
          lang({
            server: server({ state: 'installed', version: '18' }, 'warming'),
          }),
        ]),
      ),
    ).toBe(true);
  });
});

describe('texts', () => {
  it('names languages as people write them', () => {
    expect(languageName('cpp')).toBe('C++');
    expect(languageName('typescript')).toBe('TypeScript');
    expect(languageName('zig')).toBe('Zig');
  });
  it('words the install suggestion', () => {
    expect(suggestText('cpp', 'clangd')).toBe(
      'C++: download clangd for exact references?',
    );
  });
  it('notes the file that sets a switch', () => {
    expect(lspNote({ on: true, from_config: null })).toBeNull();
    expect(lspNote({ on: true, from_config: '~/.theodore/config.yaml' })).toBe(
      'Set in ~/.theodore/config.yaml',
    );
  });
});

describe('LanguagesModel', () => {
  it('loads the project status', async () => {
    transport.queueResult('code_intel_status', status([lang()]));
    const m = new LanguagesModel('github:acme/widget', new ManualTimers());
    m.start();
    await flush();
    expect(m.status?.languages).toHaveLength(1);
    expect(transport.calls[0]).toEqual({
      cmd: 'code_intel_status',
      args: { project: 'github:acme/widget' },
    });
  });

  it('polls while downloading and stops once idle', async () => {
    const timers = new ManualTimers();
    const downloading = status([lang({ grammar: { state: 'downloading' } })]);
    transport.queueResult('code_intel_status', downloading, downloading);
    transport.queueResult('code_intel_status', status([lang()]));
    const m = new LanguagesModel('p', timers);
    m.start();
    await flush();
    expect(m.polling).toBe(true);
    timers.fire();
    await flush();
    expect(m.polling).toBe(true);
    timers.fire();
    await flush();
    expect(m.polling).toBe(false);
    expect(timers.armed.size).toBe(0);
    expect(
      transport.calls.filter((c) => c.cmd === 'code_intel_status'),
    ).toHaveLength(3);
  });

  it('stops polling when the page closes', async () => {
    const timers = new ManualTimers();
    transport.onCall('code_intel_status', () =>
      status([lang({ grammar: { state: 'downloading' } })]),
    );
    const m = new LanguagesModel('p', timers);
    m.start();
    await flush();
    expect(m.polling).toBe(true);
    m.stop();
    expect(m.polling).toBe(false);
    expect(timers.armed.size).toBe(0);
  });

  it('stops polling and shows the error when a read fails', async () => {
    const timers = new ManualTimers();
    transport.queueResult(
      'code_intel_status',
      status([lang({ grammar: { state: 'downloading' } })]),
    );
    transport.onCall('code_intel_status', () => {
      throw { code: 'internal', message: 'boom' };
    });
    const m = new LanguagesModel('p', timers);
    m.start();
    await flush();
    timers.fire();
    await flush();
    expect(m.error).toBe('boom');
    expect(m.status).not.toBeNull();
    expect(m.polling).toBe(false);
  });

  it('install calls code_intel_install and refreshes', async () => {
    transport.onCall('code_intel_status', () => status([lang()]));
    let resolve!: () => void;
    transport.onCall(
      'code_intel_install',
      () => new Promise<null>((r) => (resolve = () => r(null))),
    );
    const m = new LanguagesModel('p', new ManualTimers());
    const done = m.install('cpp');
    expect(m.pending.has('cpp')).toBe(true);
    resolve();
    await done;
    expect(m.pending.has('cpp')).toBe(false);
    const cmds = transport.calls.map((c) => c.cmd);
    expect(cmds).toContain('code_intel_install');
    expect(
      transport.calls.find((c) => c.cmd === 'code_intel_install')?.args,
    ).toEqual({ language: 'cpp' });
    // Once as it starts (to show Downloading), once when it ends.
    expect(cmds.filter((c) => c === 'code_intel_status')).toHaveLength(2);
  });

  it('decline calls code_intel_decline and refreshes', async () => {
    transport.onCall('code_intel_status', () => status([lang()]));
    transport.onCall('code_intel_decline', () => null);
    const m = new LanguagesModel('p', new ManualTimers());
    await m.decline('cpp');
    expect(transport.calls.map((c) => c.cmd)).toEqual([
      'code_intel_decline',
      'code_intel_status',
    ]);
    expect(transport.calls[0]!.args).toEqual({ language: 'cpp' });
  });

  it('a failed install clears pending and still refreshes', async () => {
    transport.onCall('code_intel_status', () => status([lang()]));
    transport.onCall('code_intel_install', () => {
      throw { code: 'invalid', message: 'off' };
    });
    const m = new LanguagesModel('p', new ManualTimers());
    await m.install('cpp');
    expect(m.pending.size).toBe(0);
    expect(m.status).not.toBeNull();
  });

  it('setLsp flips the switch at once, then saves', async () => {
    transport.queueResult('code_intel_status', status([lang()]));
    transport.onCall('code_intel_set_lsp', () => null);
    const m = new LanguagesModel('p', new ManualTimers());
    await m.load();
    const after = status([lang()]);
    after.lsp.review.on = true;
    transport.queueResult('code_intel_status', after);
    const done = m.setLsp('review', true);
    expect(m.status?.lsp.review.on).toBe(true);
    await done;
    expect(
      transport.calls.find((c) => c.cmd === 'code_intel_set_lsp')?.args,
    ).toEqual({ req: { scope: 'review', on: true } });
    expect(m.status?.lsp.review.on).toBe(true);
  });

  it('a stale poll reply does not undo a newer one', async () => {
    const slow: { resolve?: (s: CodeIntelStatus) => void } = {};
    let n = 0;
    transport.onCall('code_intel_status', () => {
      n++;
      if (n === 1)
        return new Promise<CodeIntelStatus>((r) => (slow.resolve = r));
      return status([lang({ files: 99 })]);
    });
    const m = new LanguagesModel('p', new ManualTimers());
    const first = m.load();
    await m.load();
    slow.resolve!(status([lang({ files: 1 })]));
    await first;
    expect(m.status?.languages[0]!.files).toBe(99);
  });
});
