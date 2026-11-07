/**
 * The workspace switcher: the sidebar's top-left corner shows the active
 * workspace ("All projects" or one project) and opens a menu to switch —
 * one workspace at a time, the way a team switcher works. ⌃0 / ⌃1…9 switch
 * without opening it; the menu shows each workspace's waiting-PR count.
 * A project's settings open from "Project settings" (under a project);
 * "Manage projects…" is the list with drag to reorder, a row to open any
 * project's settings (also under All), and Add project….
 */
import { openExternal } from '../core/openExternal';
import {
  ArrowRotateLeft,
  ArrowUpRightFromSquare,
  ChevronDown,
  Folder,
  Gear,
  House,
  Plus,
} from '@gravity-ui/icons';
import { Button, Dropdown, Label } from '@heroui/react';
import { observer } from 'mobx-react-lite';
import type { ReactNode } from 'react';
import { ALL, projectSettingsRoute } from '../core/workspaces';
import { FEATURES } from '../features';
import { useRootStore, useUi, useWorkspaces } from '../stores/context';
import { Spinner } from './bits';
import { Logo } from './Logo';
import { useSyncDisplay } from './shell/SyncStatus';
import { WelcomeLogo } from './Welcome';
import { refreshProjectTracked, refreshingProjects } from './project/freshness';
import { ProjectStatusDot, hostRepoUrl, projectLabel } from './Projects';

const ADD = 'add-project';
const SETTINGS = 'project-settings';
const MANAGE = 'manage-projects';
const REFRESH = 'refresh-project';
const OPEN_HOST = 'open-on-host';

const HOST_NAME = {
  github: 'GitHub',
  gitlab: 'GitLab',
  bitbucket: 'Bitbucket',
};

function Item({
  id,
  icon,
  label,
  sub,
  n,
  keys,
  isDisabled,
  check = true,
}: {
  id: string;
  icon: ReactNode;
  label: string;
  sub?: string;
  n?: number;
  keys?: string;
  isDisabled?: boolean;
  check?: boolean;
}) {
  return (
    <Dropdown.Item
      id={id}
      textValue={label}
      isDisabled={isDisabled}
      className="gap-2.5"
    >
      <span className="grid size-4 shrink-0 place-items-center text-muted [&>svg]:size-4">
        {icon}
      </span>
      <Label className="min-w-0 flex-1 truncate" title={label}>
        {label}
        {sub && <span className="ms-1.5 text-xs text-muted">{sub}</span>}
      </Label>
      {!!n && (
        <span
          className="shrink-0 rounded-full bg-default px-1.5 text-xs tabular-nums text-muted"
          aria-label={`${n} waiting`}
        >
          {n}
        </span>
      )}
      {keys && <span className="shrink-0 text-xs text-muted">{keys}</span>}
      {check && <Dropdown.ItemIndicator />}
    </Dropdown.Item>
  );
}

export const WorkspaceSwitcher = observer(function WorkspaceSwitcher() {
  const store = useRootStore();
  const ui = useUi();
  const ws = useWorkspaces();
  const active = ws.active;
  // The trigger is narrow: just the repo (or folder) name; the menu and the
  // tooltip carry the full owner/repo.
  const full = active ? projectLabel(active) : 'All projects';
  const name = active ? (full.split('/').pop() ?? full) : full;
  const badges = ws.badges;
  const hostUrl = active ? hostRepoUrl(active.repo) : null;
  const refreshing = !!active && refreshingProjects.has(active.id);
  const sync = useSyncDisplay();
  const syncing = sync.syncing || refreshing;

  return (
    <Dropdown>
      <Button
        variant="ghost"
        aria-label={`Workspace: ${full}`}
        // No focus ring after the menu closes: focus returns here on every
        // pick, and a ring around the corner of the window reads as a bug.
        className="workspace-switcher h-8 min-w-0 flex-1 justify-start gap-2 px-1.5 data-[focus-visible=true]:bg-default data-[focus-visible=true]:ring-0 data-[focus-visible=true]:outline-none"
      >
        {/* While syncing the lenses' stripes drift, as on the welcome. */}
        {syncing ? (
          <span className="grid size-[22px] shrink-0 place-items-center">
            <WelcomeLogo size={22} glint className="logo-busy" />
          </span>
        ) : (
          <span
            className={`grid size-[22px] shrink-0 place-items-center ${sync.offline ? 'opacity-50 grayscale' : ''}`}
            title={sync.offline ? sync.detail : undefined}
          >
            <Logo size={22} />
          </span>
        )}
        <span
          className={`min-w-0 truncate font-semibold ${syncing ? 'text-muted' : ''}`}
        >
          {syncing ? 'Syncing…' : name}
        </span>
        <ChevronDown className="size-3.5 shrink-0 text-muted" />
      </Button>
      <Dropdown.Popover placement="bottom start" className="min-w-64">
        <Dropdown.Menu
          aria-label="Workspaces"
          selectionMode="single"
          selectedKeys={new Set([ui.workspace])}
          onAction={(k) => {
            const key = String(k);
            if (key === ADD) ui.openModal('add-project');
            else if (key === MANAGE) ui.openModal('manage-projects');
            else if (key === SETTINGS && active)
              ui.go(projectSettingsRoute(active.id));
            else if (key === REFRESH && active)
              void refreshProjectTracked(store, active.id);
            else if (key === OPEN_HOST && hostUrl) openExternal(hostUrl);
            else ws.switchTo(key);
          }}
        >
          <Dropdown.Section aria-label="Switch workspace">
            <Item
              id={ALL}
              icon={<House />}
              label="All projects"
              n={badges.get(ALL)}
              keys="⌃0"
            />
            {ws.projects.map((p, i) => (
              <Item
                key={p.id}
                id={p.id}
                icon={<ProjectStatusDot p={p} />}
                label={projectLabel(p)}
                sub={p.repo.host === 'local' ? 'local' : undefined}
                n={badges.get(p.id)}
                keys={i < 9 ? `⌃${i + 1}` : undefined}
              />
            ))}
          </Dropdown.Section>
          {active ? (
            <Dropdown.Section
              aria-label="This project"
              className="mt-1 border-t border-separator pt-1"
            >
              <Item
                id={REFRESH}
                icon={refreshing ? <Spinner /> : <ArrowRotateLeft />}
                label={refreshing ? 'Refreshing…' : 'Refresh project'}
                isDisabled={refreshing}
                check={false}
              />
              {hostUrl && active.repo.host !== 'local' ? (
                <Item
                  id={OPEN_HOST}
                  icon={<ArrowUpRightFromSquare />}
                  label={`Open on ${HOST_NAME[active.repo.host]}`}
                  check={false}
                />
              ) : null}
              <Item
                id={SETTINGS}
                icon={<Gear />}
                label="Project settings"
                check={false}
              />
            </Dropdown.Section>
          ) : null}
          <Dropdown.Section
            aria-label="Projects"
            className="mt-1 border-t border-separator pt-1"
          >
            <Item
              id={ADD}
              icon={<Plus />}
              label="Add project…"
              isDisabled={!FEATURES.projects}
              check={false}
            />
            <Item
              id={MANAGE}
              icon={<Folder />}
              label="Manage projects…"
              check={false}
            />
          </Dropdown.Section>
        </Dropdown.Menu>
      </Dropdown.Popover>
    </Dropdown>
  );
});
