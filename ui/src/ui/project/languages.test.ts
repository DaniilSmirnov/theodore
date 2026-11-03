import { observable, runInAction } from 'mobx';
import { beforeEach, describe, expect, it } from 'vitest';
import type {
  CodeInstall,
  CodeInstallProgress,
  CodeIntelStatus,
  CodeLanguage,
} from '../../gen/contract';
import { setTransport } from '../../core/transport';
import { FakeTransport } from '../../test/fakeTransport';
import {
  LanguagesModel,
  canDecline,
  canInstall,
  canRemove,
  downloadOf,
  downloadPercent,
  downloadText,
  installView,
  isDownloading,
  languageName,
  lspNote,
  needsPoll,
  removeText,
  settled,
  suggestText,
  withProgress,
  type InstallProgressMap,
  type ProgressEvent,
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

const progress = (
  state: CodeInstallProgress,
  over: Partial<ProgressEvent> = {},
): ProgressEvent => ({
  type: 'code_intel_progress',
  project: null,
  language: 'cpp',
  what: 'server',
  name: 'clangd',
  state,
  ...over,
});

const downloading = (received: number, total: number | null) =>
  progress({ kind: 'downloading', received, total });

/** An `EphemeralStore` stand-in: the map, observable. */
function source() {
  const box = observable.box<InstallProgressMap>(new Map(), { deep: false });
  return {
    get installProgress() {
      return box.get();
    },
    push(e: ProgressEvent) {
      runInAction(() => box.set(withProgress(box.get(), e)));
    },
  };
}

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
  it('offers Remove only for what Theodore downloaded', () => {
    // The grammar is ours; the server was found on the machine.
    expect(canRemove(lang())).toBe(true);
    expect(removeText(lang())).toMatch(/^Deletes its grammar from this Mac/);
    const both = lang({
      server: server({ state: 'installed', version: '18' }),
    });
    expect(removeText(both)).toMatch(/^Deletes clangd and its grammar/);
    expect(
      canRemove(
        lang({
          grammar: { state: 'missing' },
          server: server({ state: 'system', path: '/x', version: null }),
        }),
      ),
    ).toBe(false);
  });
});

describe('install progress', () => {
  it('keeps each item at its last event', () => {
    let m: InstallProgressMap = new Map();
    m = withProgress(m, downloading(1, 10));
    m = withProgress(
      m,
      progress(
        { kind: 'installed' },
        { what: 'grammar', name: 'tree-sitter-cpp' },
      ),
    );
    expect(m.get('cpp')?.server?.state).toEqual({
      kind: 'downloading',
      received: 1,
      total: 10,
    });
    expect(m.get('cpp')?.grammar?.name).toBe('tree-sitter-cpp');
  });

  it('settles when an item is installed or failed, not while downloading', () => {
    const a = withProgress(new Map(), downloading(1, 10));
    const b = withProgress(a, downloading(5, 10));
    expect(settled(b, a)).toBe(false);
    const c = withProgress(b, progress({ kind: 'installed' }));
    expect(settled(c, b)).toBe(true);
    const d = withProgress(c, progress({ kind: 'failed', error: 'x' }));
    expect(settled(d, c)).toBe(true);
    // Another language downloading leaves the settled item as it was.
    const e = withProgress(d, { ...downloading(1, 2), language: 'go' });
    expect(settled(e, d)).toBe(false);
  });

  it('reads as "12.3 of 40 MB" with a percent, or MB alone', () => {
    const d = downloadOf(
      withProgress(new Map(), downloading(12_345_678, 40_000_000)).get('cpp'),
    )!;
    expect(d.name).toBe('clangd');
    expect(downloadText(d)).toBe('12.3 of 40 MB');
    expect(downloadPercent(d)).toBe(31);
    const open = { name: 'x', received: 2_500_000, total: null };
    expect(downloadText(open)).toBe('2.5 MB');
    expect(downloadPercent(open)).toBeNull();
    expect(downloadOf(undefined)).toBeNull();
  });

  it('counts a downloading event before the status says so', () => {
    const p = withProgress(new Map(), downloading(1, 10)).get('cpp');
    expect(isDownloading(lang(), p)).toBe(true);
    const done = withProgress(new Map(), progress({ kind: 'installed' })).get(
      'cpp',
    );
    expect(isDownloading(lang(), done)).toBe(false);
  });
});

describe('needsPoll', () => {
  it('polls while a server warms up', () => {
    expect(needsPoll(status([lang()]))).toBe(false);
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
  it('polls a download only until its events arrive', () => {
    const s = status([
      lang({ language: 'cpp', grammar: { state: 'downloading' } }),
    ]);
    expect(needsPoll(s)).toBe(true);
    expect(needsPoll(s, withProgress(new Map(), downloading(1, 10)))).toBe(
      false,
    );
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
    const m = new LanguagesModel('github:acme/widget', {
      timers: new ManualTimers(),
    });
    m.start();
    await flush();
    expect(m.status?.languages).toHaveLength(1);
    expect(transport.calls[0]).toEqual({
      cmd: 'code_intel_status',
      args: { project: 'github:acme/widget' },
    });
  });

  it('polls while a server warms up and stops once ready', async () => {
    const timers = new ManualTimers();
    const warming = status([
      lang({ server: server({ state: 'installed', version: '1' }, 'warming') }),
    ]);
    transport.queueResult('code_intel_status', warming, warming);
    transport.queueResult('code_intel_status', status([lang()]));
    const m = new LanguagesModel('p', { timers });
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

  it('follows a download by its events, not by polling', async () => {
    const timers = new ManualTimers();
    const src = source();
    let n = 0;
    transport.onCall('code_intel_status', () => {
      n++;
      return status([
        lang({
          language: 'cpp',
          server: server(
            n === 1
              ? { state: 'downloading' }
              : { state: 'installed', version: '18' },
          ),
        }),
      ]);
    });
    const m = new LanguagesModel('p', { timers, progress: src });
    src.push(downloading(1_000_000, 4_000_000));
    m.start();
    await flush();
    expect(m.polling).toBe(false);
    expect(downloadOf(m.progress.get('cpp'))?.received).toBe(1_000_000);
    src.push(downloading(3_000_000, 4_000_000));
    await flush();
    expect(n).toBe(1);
    src.push(progress({ kind: 'installed' }));
    await flush();
    expect(n).toBe(2);
    expect(m.status?.languages[0]!.server?.install.state).toBe('installed');
    // Stopped: events no longer read anything.
    m.stop();
    src.push(progress({ kind: 'failed', error: 'x' }));
    await flush();
    expect(n).toBe(2);
  });

  it('stops polling when the section closes', async () => {
    const timers = new ManualTimers();
    transport.onCall('code_intel_status', () =>
      status([lang({ grammar: { state: 'downloading' } })]),
    );
    const m = new LanguagesModel('p', { timers });
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
    const m = new LanguagesModel('p', { timers });
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
    const m = new LanguagesModel('p', { timers: new ManualTimers() });
    const done = m.install('cpp');
    expect(m.pending.has('cpp')).toBe(true);
    resolve();
    await done;
    expect(m.pending.has('cpp')).toBe(false);
    const cmds = transport.calls.map((c) => c.cmd);
    expect(
      transport.calls.find((c) => c.cmd === 'code_intel_install')?.args,
    ).toEqual({ language: 'cpp' });
    // Once as it starts (to show Downloading), once when it ends.
    expect(cmds.filter((c) => c === 'code_intel_status')).toHaveLength(2);
  });

  it('decline calls code_intel_decline and refreshes', async () => {
    transport.onCall('code_intel_status', () => status([lang()]));
    transport.onCall('code_intel_decline', () => null);
    const m = new LanguagesModel('p', { timers: new ManualTimers() });
    await m.decline('cpp');
    expect(transport.calls.map((c) => c.cmd)).toEqual([
      'code_intel_decline',
      'code_intel_status',
    ]);
    expect(transport.calls[0]!.args).toEqual({ language: 'cpp' });
  });

  it('remove returns null once removed, the refusal otherwise', async () => {
    transport.onCall('code_intel_status', () => status([lang()]));
    transport.queueResult('code_intel_remove', null);
    const m = new LanguagesModel('p', { timers: new ManualTimers() });
    expect(await m.remove('rust')).toBeNull();
    expect(
      transport.calls.find((c) => c.cmd === 'code_intel_remove')?.args,
    ).toEqual({ language: 'rust' });
    transport.onCall('code_intel_remove', () => {
      throw { code: 'invalid', message: 'rust is open in a review' };
    });
    expect(await m.remove('rust')).toBe('rust is open in a review');
    expect(m.pending.size).toBe(0);
  });

  it('a failed install clears pending and still refreshes', async () => {
    transport.onCall('code_intel_status', () => status([lang()]));
    transport.onCall('code_intel_install', () => {
      throw { code: 'invalid', message: 'off' };
    });
    const m = new LanguagesModel('p', { timers: new ManualTimers() });
    await m.install('cpp');
    expect(m.pending.size).toBe(0);
    expect(m.status).not.toBeNull();
  });

  it('setLsp flips the switch at once, then saves it for the project', async () => {
    transport.queueResult('code_intel_status', status([lang()]));
    transport.onCall('code_intel_set_lsp', () => null);
    let written = 0;
    const m = new LanguagesModel('github:acme/widget', {
      timers: new ManualTimers(),
      onConfigWritten: () => written++,
    });
    await m.load();
    const after = status([lang()]);
    after.lsp.review = {
      on: true,
      from_config: '/w/.theodore/config.local.yaml',
    };
    transport.queueResult('code_intel_status', after);
    const done = m.setLsp('review', true);
    expect(m.status?.lsp.review.on).toBe(true);
    await done;
    expect(
      transport.calls.find((c) => c.cmd === 'code_intel_set_lsp')?.args,
    ).toEqual({
      req: { project: 'github:acme/widget', scope: 'review', on: true },
    });
    expect(m.status?.lsp.review.on).toBe(true);
    expect(written).toBe(1);
  });

  it('a refused switch rolls back and writes nothing', async () => {
    transport.onCall('code_intel_status', () => status([lang()]));
    transport.onCall('code_intel_set_lsp', () => {
      throw { code: 'invalid', message: "couldn't edit" };
    });
    let written = 0;
    const m = new LanguagesModel('p', {
      timers: new ManualTimers(),
      onConfigWritten: () => written++,
    });
    await m.load();
    await m.setLsp('review', true);
    expect(m.status?.lsp.review.on).toBe(false);
    expect(written).toBe(0);
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
    const m = new LanguagesModel('p', { timers: new ManualTimers() });
    const first = m.load();
    await m.load();
    slow.resolve!(status([lang({ files: 1 })]));
    await first;
    expect(m.status?.languages[0]!.files).toBe(99);
  });
});
