import { act } from 'react';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { click, mount, settle, type Mounted } from '../../test/dom';
import type { CodeIntelStatus, Project } from '../../gen/contract';
import { setTransport } from '../../core/transport';
import { RootStore } from '../../stores/RootStore';
import { RootStoreProvider } from '../../stores/context';
import { FakeTransport } from '../../test/fakeTransport';
import { LanguagesPage } from './LanguagesPage';

const PROJECT: Project = {
  id: 'github:acme/widget',
  repo: { host: 'github', owner: 'acme', name: 'widget' },
  path: null,
  status: 'ready',
  error: null,
  default_branch: 'main',
  added_at: 0,
};

const STATUS: CodeIntelStatus = {
  enabled: true,
  install: 'ask',
  lsp: {
    review: { on: false, from_config: null },
    spaces: { on: true, from_config: '~/.theodore/config.yaml' },
  },
  languages: [
    {
      language: 'rust',
      files: 214,
      grammar: { state: 'installed', version: '0.23.0' },
      server: {
        name: 'rust-analyzer',
        install: {
          state: 'system',
          path: '/usr/bin/rust-analyzer',
          version: null,
        },
        state: 'ready',
      },
      index_percent: 100,
    },
    {
      language: 'cpp',
      files: 3,
      grammar: { state: 'installed', version: '0.23.4' },
      server: {
        name: 'clangd',
        install: { state: 'missing' },
        state: 'not_running',
      },
      index_percent: 40,
    },
  ],
};

let m: Mounted | null = null;
let transport: FakeTransport;
let store: RootStore;

beforeEach(() => {
  transport = new FakeTransport();
  setTransport(transport);
  store = new RootStore();
  act(() => store.sync.pool.put('project', PROJECT.id, PROJECT));
});

afterEach(() => {
  m?.unmount();
  m = null;
});

const render = async () => {
  m = await mount(
    <RootStoreProvider store={store}>
      <LanguagesPage />
    </RootStoreProvider>,
  );
  await settle();
};

const switchFor = (label: string) =>
  m!.host.querySelector<HTMLInputElement>(`input[aria-label="${label}"]`);

describe('LanguagesPage', () => {
  it('lists languages with their states and the switches', async () => {
    transport.onCall('code_intel_status', () => STATUS);
    await render();
    const text = m!.host.textContent ?? '';
    expect(text).toContain('Rust');
    expect(text).toContain('214 files');
    expect(text).toContain('C++');
    expect(text).toContain('Missing');
    expect(text).toContain('40% indexed');
    // From a config file: disabled, with the file named.
    expect(text).toContain('Set in ~/.theodore/config.yaml');
    expect(switchFor('Language servers in spaces')?.disabled).toBe(true);
    expect(switchFor('Language servers in reviews')?.disabled).toBe(false);
    // Only the language with a missing server offers Install / Decline.
    expect(m!.host.querySelectorAll('.code-install')).toHaveLength(1);
    expect(m!.host.querySelectorAll('.code-decline')).toHaveLength(1);
  });

  it('Install calls code_intel_install for that language', async () => {
    transport.onCall('code_intel_status', () => STATUS);
    transport.onCall('code_intel_install', () => null);
    await render();
    await click(m!.host.querySelector('.code-install'));
    await settle();
    expect(
      transport.calls.find((c) => c.cmd === 'code_intel_install')?.args,
    ).toEqual({ language: 'cpp' });
  });

  it('explains when code intelligence is off', async () => {
    transport.onCall('code_intel_status', () => ({
      ...STATUS,
      enabled: false,
    }));
    await render();
    expect(m!.host.textContent).toContain(
      'Code intelligence is off for this project',
    );
    expect(m!.host.querySelector('.code-languages')).toBeNull();
  });
});
