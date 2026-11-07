import { act } from 'react';
import { afterEach, beforeAll, beforeEach, describe, expect, it } from 'vitest';
import { click, mount, settle, type Mounted } from '../../test/dom';
import type {
  CodeInstall,
  CodeIntelStatus,
  CodeLanguage,
  Project,
} from '../../gen/contract';
import { setTransport } from '../../core/transport';
import { RootStore } from '../../stores/RootStore';
import { RootStoreProvider } from '../../stores/context';
import { FakeTransport } from '../../test/fakeTransport';
import { ProjectLanguages } from './ProjectLanguages';

const PROJECT: Project = {
  id: 'github:acme/widget',
  repo: { host: 'github', owner: 'acme', name: 'widget' },
  path: null,
  status: 'ready',
  error: null,
  default_branch: 'main',
  added_at: 0,
  description: null,
  private: false,
  pushed_at: null,
  last_fetched_at: null,
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
        hint: null,
      },
      index_percent: 100,
    },
    {
      language: 'cpp',
      files: 3,
      grammar: { state: 'missing' },
      server: {
        name: 'clangd',
        install: { state: 'missing' },
        state: 'not_running',
        hint: null,
      },
      index_percent: 40,
    },
  ],
};

let m: Mounted | null = null;
let transport: FakeTransport;
let store: RootStore;
let written = 0;

// jsdom has no `CSS.escape`; react-aria's Modal reads a meta tag with it.
beforeAll(() => {
  const css = (globalThis.CSS ?? {}) as { escape?: (s: string) => string };
  css.escape ??= (s) => s.replace(/[^\w-]/g, (c) => `\\${c}`);
  (globalThis as { CSS?: unknown }).CSS = css;
});

beforeEach(() => {
  transport = new FakeTransport();
  setTransport(transport);
  store = new RootStore();
  written = 0;
});

afterEach(() => {
  m?.unmount();
  m = null;
});

const render = async () => {
  m = await mount(
    <RootStoreProvider store={store}>
      <ProjectLanguages project={PROJECT} onConfigWritten={() => written++} />
    </RootStoreProvider>,
  );
  await settle();
};

const switchFor = (label: string) =>
  m!.host.querySelector<HTMLInputElement>(`input[aria-label="${label}"]`);

/** Remove's dialog renders in a portal. */
const dialog = () => document.body.querySelector('[role="dialog"]');

describe('ProjectLanguages', () => {
  it('lists the project’s languages with their states and switches', async () => {
    transport.onCall('code_intel_status', () => STATUS);
    await render();
    const text = m!.host.textContent ?? '';
    expect(text).toContain('Rust');
    expect(text).toContain('214 files');
    expect(text).toContain('C++');
    expect(text).toContain('Missing');
    expect(text).toContain('40% indexed');
    expect(text).toContain('a PR’s code can run on this Mac');
    // A file sets it: named, and the switch still writes the local file.
    expect(text).toContain('Set in ~/.theodore/config.yaml');
    expect(switchFor('Language servers in spaces')?.disabled).toBe(false);
    expect(switchFor('Language servers in reviews')?.checked).toBe(false);
    expect(m!.host.querySelectorAll('.code-install')).toHaveLength(1);
    expect(m!.host.querySelectorAll('.code-decline')).toHaveLength(1);
    // Rust's grammar was downloaded; C++ has nothing on disk.
    expect(m!.host.querySelectorAll('.code-remove')).toHaveLength(1);
    expect(transport.calls[0]!.args).toEqual({ project: PROJECT.id });
  });

  it('a server off for reviews says so while spaces are on', async () => {
    const [rust] = STATUS.languages;
    transport.onCall('code_intel_status', () => ({
      ...STATUS,
      languages: [{ ...rust!, server: { ...rust!.server!, state: 'off' } }],
    }));
    await render();
    const chip = [...m!.host.querySelectorAll('[title]')].find(
      (e) => e.textContent === 'Off for reviews',
    );
    expect(chip?.getAttribute('title')).toBe(
      'Language servers start in this project’s spaces',
    );
  });

  it('a switch writes this project’s setting', async () => {
    transport.onCall('code_intel_status', () => STATUS);
    transport.onCall('code_intel_set_lsp', () => null);
    await render();
    await click(switchFor('Language servers in reviews'));
    await settle();
    expect(
      transport.calls.find((c) => c.cmd === 'code_intel_set_lsp')?.args,
    ).toEqual({ req: { project: PROJECT.id, scope: 'review', on: true } });
    expect(written).toBe(1);
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

  it('shows a download’s progress from its events', async () => {
    transport.onCall('code_intel_status', () => STATUS);
    await render();
    act(() =>
      store.ephemeral.handle({
        type: 'code_intel_progress',
        project: null,
        language: 'cpp',
        what: 'server',
        name: 'clangd',
        state: { kind: 'downloading', received: 12_300_000, total: 40_000_000 },
      }),
    );
    await settle();
    const bar = m!.host.querySelector('.code-download');
    expect(bar?.textContent).toContain('clangd · 12.3 of 40 MB');
    // Busy: no buttons on that row while it downloads.
    expect(m!.host.querySelectorAll('.code-install')).toHaveLength(0);
    const before = transport.calls.length;
    act(() =>
      store.ephemeral.handle({
        type: 'code_intel_progress',
        project: null,
        language: 'cpp',
        what: 'server',
        name: 'clangd',
        state: { kind: 'installed' },
      }),
    );
    await settle();
    expect(m!.host.querySelector('.code-download')).toBeNull();
    expect(
      transport.calls
        .slice(before)
        .filter((c) => c.cmd === 'code_intel_status'),
    ).toHaveLength(1);
  });

  it('says what a server needs, with nothing to install', async () => {
    const row = (
      language: string,
      name: string,
      install: CodeInstall,
      hint: string | null = null,
    ): CodeLanguage => ({
      language,
      files: 2,
      grammar: { state: 'installed', version: '1' },
      server: { name, install, state: 'not_running', hint },
      index_percent: 100,
    });
    transport.onCall('code_intel_status', () => ({
      ...STATUS,
      languages: [
        row('java', 'jdtls', {
          state: 'needs_runtime',
          runtime: 'java',
          min: '21',
          found: '17.0.2',
        }),
        row(
          'ruby',
          'ruby-lsp',
          { state: 'unavailable' },
          'gem install ruby-lsp',
        ),
        row('kotlin', 'kotlin-lsp', { state: 'trusted_only' }),
      ],
    }));
    await render();
    const text = m!.host.textContent;
    expect(text).toContain('Needs Java 21+');
    expect(text).toContain('Found 17.0.2');
    expect(text).toContain('gem install ruby-lsp');
    expect(text).not.toContain('No download for this Mac');
    expect(text).toContain('Spaces only');
    // Their grammars are in, and none of these can be fetched.
    expect(m!.host.querySelectorAll('.code-install')).toHaveLength(0);
    expect(m!.host.querySelectorAll('.code-decline')).toHaveLength(0);
  });

  it('a language with no grammar reads as text search, not as a failure', async () => {
    transport.onCall('code_intel_status', () => ({
      ...STATUS,
      languages: [
        {
          language: 'css',
          files: 8,
          grammar: { state: 'unavailable' },
          server: null,
          index_percent: 0,
        },
      ],
    }));
    await render();
    const text = m!.host.textContent;
    expect(text).toContain('Text search');
    expect(text).not.toContain('Unavailable');
    expect(text).not.toContain('% indexed');
    expect(m!.host.querySelectorAll('.code-install')).toHaveLength(0);
  });

  it('Remove asks first, then removes', async () => {
    transport.onCall('code_intel_status', () => STATUS);
    transport.onCall('code_intel_remove', () => null);
    await render();
    await click(m!.host.querySelector('.code-remove'));
    await settle();
    expect(dialog()?.textContent).toContain('Remove Rust?');
    expect(dialog()?.textContent).toContain('Deletes its grammar');
    expect(transport.calls.some((c) => c.cmd === 'code_intel_remove')).toBe(
      false,
    );
    await click(document.body.querySelector('.code-remove-confirm'));
    await settle();
    expect(
      transport.calls.find((c) => c.cmd === 'code_intel_remove')?.args,
    ).toEqual({ language: 'rust' });
    expect(dialog()).toBeNull();
  });

  it('a refused Remove keeps the dialog open with the reason', async () => {
    transport.onCall('code_intel_status', () => STATUS);
    transport.onCall('code_intel_remove', () => {
      throw {
        code: 'invalid',
        message: 'rust is in use by the review at /w/pr-12',
      };
    });
    await render();
    await click(m!.host.querySelector('.code-remove'));
    await settle();
    await click(document.body.querySelector('.code-remove-confirm'));
    await settle();
    expect(document.body.querySelector('.code-remove-error')?.textContent).toBe(
      'rust is in use by the review at /w/pr-12',
    );
    expect(dialog()).not.toBeNull();
    // Not toasted: the dialog says it.
    expect(store.ephemeral.toasts).toHaveLength(0);
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
