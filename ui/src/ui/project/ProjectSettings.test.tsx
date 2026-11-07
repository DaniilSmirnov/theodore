import { act } from 'react';
import { afterEach, beforeAll, beforeEach, describe, expect, it } from 'vitest';
import { observer } from 'mobx-react-lite';
import { runInAction } from 'mobx';
import { click, mount, settle, type Mounted } from '../../test/dom';
import type { Project } from '../../gen/contract';
import { setTransport } from '../../core/transport';
import { projectSettingsRoute } from '../../core/workspaces';
import { RootStore } from '../../stores/RootStore';
import { RootStoreProvider } from '../../stores/context';
import { FakeTransport } from '../../test/fakeTransport';
import { Modals } from '../Modals';
import { SettingsNav } from '../Settings';
import { WorkspaceSwitcher } from '../WorkspaceSwitcher';
import { ProjectSettingsNav, ProjectSettingsPage } from './ProjectSettings';

const project = (over: Partial<Project> & Pick<Project, 'id' | 'repo'>) =>
  ({
    path: null,
    status: 'ready',
    error: null,
    default_branch: 'main',
    added_at: 0,
    description: null,
    private: false,
    pushed_at: null,
    last_fetched_at: null,
    ...over,
  }) as Project;

const WEB = project({
  id: 'github:acme/web',
  repo: { host: 'github', owner: 'acme', name: 'web' },
  added_at: 1,
});
const API = project({
  id: 'github:acme/api',
  repo: { host: 'github', owner: 'acme', name: 'api' },
  added_at: 2,
});
const LOCAL = project({
  id: 'local:notes',
  repo: { host: 'local', owner: '', name: 'notes' },
  path: '/tmp/notes',
  added_at: 3,
});

/** What `App` does for the route: the nav and the page. */
const Harness = observer(function Harness({ store }: { store: RootStore }) {
  const r = store.ui.route;
  return (
    <>
      {r.name === 'project-settings' ? (
        <>
          <ProjectSettingsNav project={r.project} section={r.section} />
          <ProjectSettingsPage project={r.project} section={r.section} />
        </>
      ) : r.name === 'settings' ? (
        <SettingsNav page={r.page} />
      ) : null}
      <Modals />
    </>
  );
});

let m: Mounted | null = null;
let transport: FakeTransport;
let store: RootStore;

// jsdom has no `CSS.escape`; react-aria's Modal reads a meta tag with it.
beforeAll(() => {
  const css = (globalThis.CSS ?? {}) as { escape?: (s: string) => string };
  css.escape ??= (s) => s.replace(/[^\w-]/g, (c) => `\\${c}`);
  (globalThis as { CSS?: unknown }).CSS = css;
});

const setStatus = (status: 'idle' | 'bootstrapping' | 'online') =>
  runInAction(() => {
    store.sync.status = status;
  });

beforeEach(() => {
  localStorage.clear();
  transport = new FakeTransport();
  setTransport(transport);
  transport.onCall('remove_project', () => null);
  transport.onCall('code_intel_status', () => ({
    enabled: true,
    install: 'ask',
    lsp: {
      review: { on: false, from_config: null },
      spaces: { on: false, from_config: null },
    },
    languages: [],
  }));
  store = new RootStore();
  for (const p of [WEB, API, LOCAL]) store.sync.pool.put('project', p.id, p);
  setStatus('online');
});

afterEach(async () => {
  // Close an open dialog first, so react-aria lets go of the page.
  act(() => store.ui.closeModal());
  await settle();
  m?.unmount();
  m = null;
});

const render = async (node = <Harness store={store} />) => {
  m = await mount(<RootStoreProvider store={store}>{node}</RootStoreProvider>);
  await settle();
};

const open = (id: string, section: 'general' | 'languages' | 'config') =>
  act(() => store.ui.go(projectSettingsRoute(id, section)));

const text = () => m!.host.textContent ?? '';
const navOptions = () => [
  ...m!.host.querySelectorAll('.project-settings-nav [role="option"]'),
];
const navItems = () => navOptions().map((o) => o.textContent);

const esc = () =>
  act(() => {
    document.body.dispatchEvent(
      new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }),
    );
  });

describe('project settings route', () => {
  it('opens with the project and its three sections, General first', async () => {
    store.ui.go({ name: 'prs' });
    open(WEB.id, 'general');
    await render();
    expect(m!.host.querySelector('.project-settings-name')?.textContent).toBe(
      'acme/web',
    );
    expect(navItems()).toEqual(['General', 'Languages', 'Config']);
    expect(text()).toContain('Effective settings');
    expect(text()).toContain('Remove project');
    expect(text()).not.toContain('Language servers in reviews');
  });

  it('switches sections from the menu, in the same tab', async () => {
    open(WEB.id, 'general');
    await render();
    const tabs = store.ui.tabs.length;
    await click(navOptions().find((o) => o.textContent === 'Languages'));
    await settle();
    expect(store.ui.route).toEqual(projectSettingsRoute(WEB.id, 'languages'));
    expect(store.ui.tabs).toHaveLength(tabs);
    expect(text()).toContain('Language servers in reviews');
    expect(text()).not.toContain('Effective settings');
  });

  it('keeps one project settings tab, moved to the project you open', () => {
    store.ui.go({ name: 'prs' });
    store.ui.go(projectSettingsRoute(WEB.id));
    store.ui.go({ name: 'tasks' }, { newTab: true });
    store.ui.go(projectSettingsRoute(API.id, 'config'));
    expect(store.ui.tabs.filter((t) => t.name === 'project-settings')).toEqual([
      projectSettingsRoute(API.id, 'config'),
    ]);
  });

  it('Back and Esc leave in one step, however many sections were visited', async () => {
    store.ui.go({ name: 'prs' });
    open(WEB.id, 'general');
    open(WEB.id, 'languages');
    open(WEB.id, 'config');
    await render();
    await click(m!.host.querySelector('.settings-back'));
    expect(store.ui.route).toEqual({ name: 'prs' });

    open(WEB.id, 'languages');
    expect(store.ui.route.name).toBe('project-settings');
    esc();
    expect(store.ui.route).toEqual({ name: 'prs' });
  });

  it('works for a local folder', async () => {
    open(LOCAL.id, 'general');
    await render();
    expect(m!.host.querySelector('.project-settings-name')?.textContent).toBe(
      'notes',
    );
    expect(text()).toContain('The folder itself stays where it is');
    expect(navItems()).toEqual(['General', 'Languages', 'Config']);
  });

  it('removing needs a second press, then closes the tab and forgets it', async () => {
    store.ui.go({ name: 'prs' });
    open(API.id, 'general');
    await render();
    await click(m!.host.querySelector('.project-remove'));
    expect(transport.calls.some((c) => c.cmd === 'remove_project')).toBe(false);
    await click(m!.host.querySelector('.project-remove-confirm'));
    await settle();
    expect(
      transport.calls.find((c) => c.cmd === 'remove_project')?.args,
    ).toEqual({ id: API.id });
    expect(store.ui.tabs.some((t) => t.name === 'project-settings')).toBe(
      false,
    );
    // The tab goes; the workspace's home is what is left.
    expect(store.ui.route).toEqual({ name: 'inbox' });
  });

  it('a project that is gone says so, and Back still leaves', async () => {
    store.ui.go({ name: 'prs' });
    open('github:acme/removed', 'languages');
    await render();
    expect(m!.host.querySelector('.project-gone')).not.toBeNull();
    expect(navItems()).toEqual([]);
    await click(m!.host.querySelector('.settings-back'));
    expect(store.ui.route).toEqual({ name: 'prs' });
  });

  it('waits instead of reporting a project the first sync has not brought yet', async () => {
    setStatus('bootstrapping');
    open('github:acme/late', 'general');
    await render();
    expect(m!.host.querySelector('.project-gone')).toBeNull();
  });
});

describe('Settings without the Projects tab', () => {
  it('does not list Projects', async () => {
    store.ui.go({ name: 'settings', page: 'agents' });
    await render();
    const items = [
      ...m!.host.querySelectorAll('.settings-nav [role="option"]'),
    ].map((o) => o.textContent);
    expect(items).toContain('Agents');
    expect(items).not.toContain('Projects');
  });
});

describe('managing projects', () => {
  it('lists projects in switcher order and opens one’s settings', async () => {
    act(() => store.workspaces.reorder([LOCAL.id], WEB.id, 'before'));
    act(() => store.ui.openModal('manage-projects'));
    await render();
    const rows = [...document.body.querySelectorAll('.repo-row')];
    expect(rows.map((r) => r.textContent)).toEqual([
      'notes',
      'acme/web',
      'acme/api',
    ]);
    expect(document.body.textContent).toContain('Drag to reorder');
    // Enter on a row, as a keyboard does: a virtual click on a draggable
    // row starts a drag instead.
    await act(async () => {
      const row = rows[2] as HTMLElement;
      row.focus();
      row.dispatchEvent(
        new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }),
      );
      row.dispatchEvent(
        new KeyboardEvent('keyup', { key: 'Enter', bubbles: true }),
      );
    });
    await settle();
    expect(store.ui.route).toEqual(projectSettingsRoute(API.id, 'general'));
    expect(store.ui.modal).toBeNull();
  });

  it('has Add project… in the list', async () => {
    act(() => store.ui.openModal('manage-projects'));
    await render();
    await click(document.body.querySelector('.manage-add'));
    expect(store.ui.modal).toBe('add-project');
  });

  it('reorders through the switcher’s own order (ws.reorder)', () => {
    act(() => store.workspaces.reorder([API.id], WEB.id, 'before'));
    expect(store.workspaces.projects.map((p) => p.id)).toEqual([
      API.id,
      WEB.id,
      LOCAL.id,
    ]);
  });
});

describe('workspace switcher', () => {
  const menuItems = () =>
    [...document.body.querySelectorAll('[role^="menuitem"]')].map(
      (i) => i.textContent,
    );
  const item = (label: string) =>
    [...document.body.querySelectorAll('[role^="menuitem"]')].find((i) =>
      i.textContent?.includes(label),
    );

  const openMenu = async () => {
    await render(<WorkspaceSwitcher />);
    await click(m!.host.querySelector('.workspace-switcher'));
    await settle();
  };

  it('under All: Add project… and Manage projects…, no project settings', async () => {
    await openMenu();
    const items = menuItems().join('|');
    expect(items).toContain('Add project…');
    expect(items).toContain('Manage projects…');
    expect(items).not.toContain('Project settings');
    await click(item('Manage projects…'));
    expect(store.ui.modal).toBe('manage-projects');
  });

  it('under a project: Project settings opens that project’s settings', async () => {
    act(() => store.workspaces.switchTo(API.id));
    await openMenu();
    expect(menuItems().join('|')).toContain('Manage projects…');
    await click(item('Project settings'));
    expect(store.ui.route).toEqual(projectSettingsRoute(API.id, 'general'));
  });

  it('Add project… opens the add dialog', async () => {
    await openMenu();
    await click(item('Add project…'));
    expect(store.ui.modal).toBe('add-project');
  });
});
