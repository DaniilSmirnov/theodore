import { useEffect, useState, type ReactNode } from 'react';
import { ThemePicker } from './ThemePicker';
import {
  ArrowLeft,
  ArrowRotateRight,
  CurlyBrackets,
  ChevronRight,
  CircleCheckFill,
  CircleInfo,
  Cpu,
  Cubes3,
  Flask,
  Folder,
  Gear,
  Grip,
  Keyboard,
  Lock,
  LogoGithub,
  Person,
  PlugConnection,
  Sparkles,
  Palette,
} from '@gravity-ui/icons';
import {
  Button,
  Card,
  Chip,
  Input,
  Label,
  ListBox,
  Select,
  Separator,
  Switch,
  TextField,
  ToggleButton,
  ToggleButtonGroup,
  Typography,
} from '@heroui/react';
import { observer } from 'mobx-react-lite';
import { useDragAndDrop, type Key } from 'react-aria-components';
import type {
  AgentId,
  AgentStatus,
  AppInfo,
  HostAuthStatus,
  Project,
  UpdateInfo,
} from '../gen/contract';
import { api, CommandFailure } from '../core/api';
import { checkFailureText, isOffline } from '../core/updateCheck';
import { useViewKeys } from './keys';
import { setSetting } from '../actions';
import { useDeviceLogin } from '../core/deviceLogin';
import { useRootStore, useUi, useWorkspaces } from '../stores/context';
import type { SettingsPage as Page } from '../stores/UiStore';
import { AgentTag, Avatar, Btn, K, Spinner } from './bits';
import {
  CLAUDE_MODELS,
  EFFORTS,
  agentEffort,
  agentModel,
  hasModelAliases,
  setAgentEffort,
  setAgentModel,
  takesEffort,
} from './agentModel';
import { AccountCard } from './AccountCard';
import { buildLabel, buildReport, builtAtIso } from './buildStamp';
import { DeviceLoginPanel } from './DeviceLoginPanel';
import { IntegrationsPage } from './linear/IntegrationsPage';
import { projectLabel } from './Projects';
import { ConfigEditor } from './project/ConfigEditor';
import { PrivacyCard } from './Privacy';
import { SandboxBadge } from './sandbox/SandboxBadge';
import { SandboxPage } from './sandbox/SandboxPage';
import { visibleSettingsPages } from './sandbox/sandbox';
import { isLabsVisible } from './space/mode';
import { LabsPage } from './space/SpacePage';
import { isHostOn, isLinearOn } from './settings/labs';
import { LabsPage as ExperimentsPage } from './settings/LabsPage';
import { LanguagesPage } from './settings/LanguagesPage';

const AGENTS: AgentId[] = ['claude', 'codex', 'gemini'];
const AI_KEYS = ['agents.grouping', 'agents.chat', 'agents.fix'] as const;
const AI_TITLE: Record<(typeof AI_KEYS)[number], string> = {
  'agents.grouping': 'Diff grouping',
  'agents.chat': 'Chat questions',
  'agents.fix': 'Finding fixes',
};

/** Settings pages. */
const PAGES: { id: Page; title: string; icon: ReactNode }[] = [
  { id: 'account', title: 'Account', icon: <Person /> },
  { id: 'appearance', title: 'Appearance', icon: <Palette /> },
  { id: 'agents', title: 'Agents', icon: <Cpu /> },
  { id: 'integrations', title: 'Integrations', icon: <PlugConnection /> },
  { id: 'projects', title: 'Projects', icon: <Folder /> },
  { id: 'languages', title: 'Languages', icon: <CurlyBrackets /> },
  { id: 'about', title: 'About & updates', icon: <CircleInfo /> },
  { id: 'sandbox', title: 'Sandbox', icon: <Cubes3 /> },
  { id: 'labs', title: 'Spaces', icon: <Sparkles /> },
  { id: 'experiments', title: 'Labs', icon: <Flask /> },
];

const SUB: Record<Page, string> = {
  account:
    'Your Theodore account and code host sign-ins. Tokens live in the system keychain.',
  appearance: 'How Theodore looks. System follows macOS.',
  agents: 'Agent CLIs on this computer, and which one handles each task.',
  integrations: 'Other tools Theodore can link PRs to.',
  projects: 'Connected repositories and the settings effective in each.',
  languages:
    'Grammars and language servers that give reviews and spaces exact definitions and references.',
  updates: 'Version, release channel and updates.',
  about: 'Version, release channel and updates.',
  sandbox: 'The simulated host this window runs against: faults and timeline.',
  labs: 'An agent writes a change in its own worktree; you review it before it becomes a pull request.',
  experiments:
    'Features in progress. Off by default; they may change or go away.',
};

const SOURCE_LABEL: Record<HostAuthStatus['source'], string> = {
  keychain: 'a saved token',
  gh: 'your gh CLI login',
  env: 'GITHUB_TOKEN',
  none: 'nothing',
  sandbox: 'the sandbox (simulated host)',
};

/** The sidebar while in settings: the app's sections give way to the settings' own. */
export const SettingsNav = observer(function SettingsNav({
  page,
}: {
  page: Page;
}) {
  const store = useRootStore();
  const ui = useUi();
  // Esc leaves Settings, as the hint by "Back" says (inputs and dialogs keep
  // their own Esc: `useViewKeys` skips them).
  useViewKeys(
    ui,
    (e) => {
      if (e.key !== 'Escape') return false;
      ui.goBack();
    },
    [],
  );
  const labs = isLabsVisible(store);
  // Integrations holds only Linear: it goes with the Linear lab.
  const linear = isLinearOn(store);
  const pages = visibleSettingsPages(PAGES, store.ephemeral.sandbox).filter(
    (p) => (p.id !== 'labs' || labs) && (p.id !== 'integrations' || linear),
  );
  return (
    <aside className="sidebar settings-nav flex w-64 shrink-0 flex-col overflow-hidden border-r border-separator bg-background">
      <div
        data-tauri-drag-region
        className="px-3 pb-2 pt-[calc(var(--controls-bottom)+0.75rem)]"
      >
        <Button
          variant="ghost"
          size="sm"
          className="settings-back h-8 w-full justify-start gap-2 px-2 text-muted"
          onPress={() => ui.goBack()}
        >
          <ArrowLeft className="size-4" />
          Back
          <span className="flex-1" />
          <K>Esc</K>
        </Button>
        <div className="mt-3 flex items-center gap-2 px-2">
          <Typography.Heading level={6} className="flex-1">
            Settings
          </Typography.Heading>
          <SandboxBadge />
        </div>
      </div>
      <ListBox
        aria-label="Settings pages"
        selectionMode="single"
        selectedKeys={[page === 'updates' ? 'about' : page]}
        onSelectionChange={(keys) => {
          const k = keys === 'all' ? undefined : [...keys][0];
          if (k) ui.go({ name: 'settings', page: k as Page });
        }}
        className="min-h-0 flex-1 overflow-auto px-2 pb-3"
      >
        {pages.map((p) => (
          <ListBox.Item
            key={p.id}
            id={p.id}
            textValue={p.title}
            className="nav gap-3 px-3 text-sm data-[selected=true]:font-medium"
          >
            <span className="grid size-4 place-items-center text-muted [&>svg]:size-4">
              {p.icon}
            </span>
            {p.title}
          </ListBox.Item>
        ))}
      </ListBox>
    </aside>
  );
});

export function settingValue(
  store: ReturnType<typeof useRootStore>,
  scope: string,
  key: string,
): unknown {
  return store.models.settings.get(`${scope}:${key}`)?.value;
}

/** `agents.<id>.enabled`, `user` scope, default `true` (plan
 *  "enable/disable agents") — shared by the Settings Agents page and the
 *  chat/skill agent pickers in `SidePanel`. */
export function isAgentEnabled(
  store: ReturnType<typeof useRootStore>,
  agent: AgentId,
): boolean {
  const v = settingValue(store, 'user', `agents.${agent}.enabled`);
  return v !== false;
}

/** `agents.user_mcp`, `user` scope, default `true` (issue #16): agent runs
 *  may use the user's own Claude Code MCP servers next to Theodore's tools. */
export const USER_MCP_KEY = 'agents.user_mcp';

export function isUserMcpAllowed(
  store: ReturnType<typeof useRootStore>,
): boolean {
  return settingValue(store, 'user', USER_MCP_KEY) !== false;
}

/** A host connected by a pasted token (GitLab, Bitbucket): no device-flow
 *  login, and not the primary host `host_status` reports on. */
const TokenHostSection = observer(function TokenHostSection({
  host,
  title,
  domain,
  hint,
  placeholder,
}: {
  host: 'gitlab' | 'bitbucket';
  title: string;
  domain: string;
  hint: string;
  placeholder: string;
}) {
  const [status, setStatus] = useState<HostAuthStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [editing, setEditing] = useState(false);
  const [token, setToken] = useState('');
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    void api
      .hostStatusFor(host)
      .then((s) => live && setStatus(s))
      .catch(() => {
        // stays "Not connected"
      });
    return () => {
      live = false;
    };
  }, [host]);

  const setToken_ = (value: string | null) => {
    setError(null);
    setBusy(true);
    void api
      .setHostToken({ host, token: value })
      .then((s) => {
        setStatus(s);
        setEditing(false);
        setToken('');
      })
      .catch((e) =>
        setError(e instanceof CommandFailure ? e.message : String(e)),
      )
      .finally(() => setBusy(false));
  };

  return (
    <section className="flex flex-col gap-2">
      <Typography.Heading level={6}>{title}</Typography.Heading>
      <Card variant="secondary" className="gap-0 p-0">
        <div className="flex items-center gap-3 px-4 py-3">
          <span className="size-7 shrink-0 rounded-full bg-default" />
          <div className="flex min-w-0 flex-1 flex-col">
            <span className="text-sm font-medium">
              {status?.login ?? 'Not connected'}{' '}
              <span className="text-muted">· {domain}</span>
            </span>
            <span className="text-xs text-muted">
              {status?.connected ? 'Signed in using a saved token' : hint}
            </span>
          </div>
        </div>
        <Separator />
        {editing ? (
          <div className="flex items-center gap-2 px-4 py-3">
            <TextField
              value={token}
              onChange={setToken}
              type="password"
              className="flex-1"
            >
              <Label className="sr-only">{title} token</Label>
              <Input
                autoFocus
                placeholder={placeholder}
                className="font-mono text-sm"
              />
            </TextField>
            <Button
              size="sm"
              isDisabled={token.trim().length < 4}
              onPress={() => setToken_(token.trim())}
            >
              Save
            </Button>
            <Button
              size="sm"
              variant="ghost"
              onPress={() => {
                setEditing(false);
                setToken('');
              }}
            >
              Cancel
            </Button>
          </div>
        ) : (
          <div className="flex items-center gap-2 p-1">
            <Button
              variant="ghost"
              className="m-1 justify-start gap-2"
              onPress={() => setEditing(true)}
            >
              <Lock className="size-4" />{' '}
              {status?.connected ? 'Replace token' : `Connect ${title}`}
            </Button>
            {status?.connected && (
              <Button
                variant="ghost"
                className="m-1 justify-start gap-2 text-danger"
                isDisabled={busy}
                onPress={() => setToken_(null)}
              >
                Remove
              </Button>
            )}
          </div>
        )}
      </Card>
      {error && <span className="text-xs text-danger">{error}</span>}
    </section>
  );
});

const AccountPage = observer(function AccountPage() {
  const store = useRootStore();
  const viewer = store.ephemeral.viewer;
  const [status, setStatus] = useState<HostAuthStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [editing, setEditing] = useState(false);
  const [token, setToken] = useState('');
  const [error, setError] = useState<string | null>(null);

  const load = () => {
    setBusy(true);
    void api
      .hostStatus()
      .then(setStatus)
      .catch(() => setStatus(null))
      .finally(() => setBusy(false));
  };
  useEffect(load, []);

  const deviceLogin = useDeviceLogin(setStatus);

  const setToken_ = (value: string | null) => {
    setError(null);
    setBusy(true);
    void api
      .setHostToken({ host: 'github', token: value })
      .then((s) => {
        setStatus(s);
        setEditing(false);
        setToken('');
      })
      .catch((e) =>
        setError(e instanceof CommandFailure ? e.message : String(e)),
      )
      .finally(() => setBusy(false));
  };

  return (
    <>
      <AccountCard />
      <section className="flex flex-col gap-2">
        <Typography.Heading level={6}>GitHub</Typography.Heading>
        <Card variant="secondary" className="gap-0 p-0">
          <div className="flex items-center gap-3 px-4 py-3">
            {viewer ? (
              <Avatar person={viewer} size={28} />
            ) : (
              <span className="size-7 shrink-0 rounded-full bg-default" />
            )}
            <div className="flex min-w-0 flex-1 flex-col">
              <span className="text-sm font-medium">
                {status?.login ?? viewer?.login ?? 'Not connected'}{' '}
                <span className="text-muted">· github.com</span>
              </span>
              <span className="text-xs text-muted">
                {status?.connected
                  ? `Signed in using ${SOURCE_LABEL[status.source]}`
                  : 'Not connected'}
              </span>
            </div>
            {status?.connected && (
              <Chip size="sm" variant="soft" color="success" className="gap-1">
                <CircleCheckFill className="size-3" /> primary
              </Chip>
            )}
          </div>
          <Separator />
          {editing ? (
            <div className="flex items-center gap-2 px-4 py-3">
              <TextField
                value={token}
                onChange={setToken}
                type="password"
                className="flex-1"
              >
                <Label className="sr-only">Personal access token</Label>
                <Input
                  autoFocus
                  placeholder="ghp_…"
                  className="font-mono text-sm"
                />
              </TextField>
              <Button
                size="sm"
                isDisabled={token.trim().length < 8}
                onPress={() => setToken_(token.trim())}
              >
                Save
              </Button>
              <Button
                size="sm"
                variant="ghost"
                onPress={() => {
                  setEditing(false);
                  setToken('');
                }}
              >
                Cancel
              </Button>
            </div>
          ) : deviceLogin.phase !== 'idle' ? (
            <div className="px-4 py-3">
              <DeviceLoginPanel state={deviceLogin} />
            </div>
          ) : (
            <div className="flex items-center gap-2 p-1">
              <Button
                variant="ghost"
                className="add-token m-1 justify-start gap-2"
                onPress={() => setEditing(true)}
              >
                <Lock className="size-4" />{' '}
                {status?.connected ? 'Replace token' : 'Sign in with a token'}
              </Button>
              {deviceLogin.available && !status?.connected && (
                <Button
                  variant="ghost"
                  className="gh-device-login m-1 justify-start gap-2"
                  onPress={deviceLogin.start}
                >
                  <LogoGithub className="size-4" /> Sign in with GitHub
                </Button>
              )}
              <Button
                variant="ghost"
                className="m-1 justify-start gap-2"
                isDisabled={busy}
                onPress={load}
              >
                Check again
              </Button>
              {status?.connected && (
                <Button
                  variant="ghost"
                  className="m-1 justify-start gap-2 text-danger"
                  isDisabled={busy}
                  onPress={() => setToken_(null)}
                >
                  Remove
                </Button>
              )}
            </div>
          )}
        </Card>
        {error && <span className="text-xs text-danger">{error}</span>}
      </section>
      {isHostOn(store, 'gitlab') && (
        <TokenHostSection
          host="gitlab"
          title="GitLab"
          domain="gitlab.com"
          hint="A personal access token with the api scope"
          placeholder="glpat-…"
        />
      )}
      {isHostOn(store, 'bitbucket') && (
        <TokenHostSection
          host="bitbucket"
          title="Bitbucket"
          domain="bitbucket.org"
          hint="An app password (username:app_password) or an access token"
          placeholder="username:app_password"
        />
      )}
    </>
  );
});

const AgentsPage = observer(function AgentsPage() {
  const store = useRootStore();
  const [detected, setDetected] = useState<AgentStatus[] | null>(null);
  useEffect(() => {
    void api
      .detectAgents()
      .then(setDetected)
      .catch(() => setDetected([]));
  }, []);
  return (
    <>
      <section className="flex flex-col gap-2">
        <Typography.Heading level={6}>
          Agents on this computer
        </Typography.Heading>
        {!detected ? (
          <Spinner />
        ) : (
          <Card variant="secondary" className="agents gap-0 p-0">
            {detected.map((a) => {
              const enabled = isAgentEnabled(store, a.id);
              return (
                <div
                  key={a.id}
                  className="flex items-center gap-3 border-b border-separator px-4 py-2.5 last:border-b-0"
                >
                  <Switch
                    size="sm"
                    aria-label={`${a.id} enabled`}
                    // A missing CLI can't run, whatever its setting says.
                    isSelected={a.installed && enabled}
                    isDisabled={!a.installed}
                    onChange={(v) =>
                      setSetting(store, 'user', `agents.${a.id}.enabled`, v)
                    }
                  >
                    <Switch.Content>
                      <Switch.Control>
                        <Switch.Thumb />
                      </Switch.Control>
                    </Switch.Content>
                  </Switch>
                  <div
                    className={`flex items-center gap-1.5 ${enabled ? '' : 'opacity-50'}`}
                  >
                    <AgentTag id={a.id} />
                  </div>
                  <span
                    className={`min-w-0 flex-1 truncate font-mono text-xs text-muted ${enabled ? '' : 'opacity-50'}`}
                  >
                    {a.path ?? '—'}
                  </span>
                  <span className="text-xs text-muted">
                    {a.installed ? (a.version ?? 'installed') : 'not found'}
                  </span>
                </div>
              );
            })}
          </Card>
        )}
        <span className="text-xs text-muted">
          Disabled agents are hidden from the chat picker and skipped for
          grouping, skills and fixes. Agents use their own sign-in (
          <code className="font-mono">claude login</code>
          ); keys never reach this app.
        </span>
      </section>
      <Separator />
      <section className="flex flex-col gap-2">
        <Typography.Heading level={6}>Model and effort</Typography.Heading>
        <Card variant="secondary" className="gap-0 p-0">
          {AGENTS.map((a) => (
            <AgentModelRow key={a} agent={a} />
          ))}
        </Card>
        <span className="text-xs text-muted">
          Used by every run of that agent: grouping, chat, skills, fixes and
          spaces. Default leaves it to the CLI. Effort is Claude Code&apos;s{' '}
          <code className="font-mono">--effort</code> and Codex&apos;s{' '}
          <code className="font-mono">model_reasoning_effort</code>; Gemini CLI
          has no such setting.
        </span>
      </section>
      <Separator />
      <section className="flex flex-col gap-2">
        <Typography.Heading level={6}>Which agent does what</Typography.Heading>
        <Card variant="secondary" className="gap-0 p-0">
          {AI_KEYS.map((key) => {
            const value =
              (settingValue(store, 'user', key) as AgentId | undefined) ??
              'claude';
            return (
              <div
                key={key}
                className="set-row flex items-center gap-4 border-b border-separator px-4 py-3 last:border-b-0"
              >
                <span className="min-w-0 flex-1 text-sm font-medium">
                  {AI_TITLE[key]}
                </span>
                <ToggleButtonGroup
                  size="sm"
                  selectionMode="single"
                  disallowEmptySelection
                  aria-label={AI_TITLE[key]}
                  selectedKeys={new Set([value])}
                  onSelectionChange={(keys) => {
                    const v = [...keys][0];
                    if (v) setSetting(store, 'user', key, String(v));
                  }}
                >
                  {AGENTS.map((a, i) => (
                    <ToggleButton
                      key={a}
                      id={a}
                      className="whitespace-nowrap"
                      // Only agents installed on this computer can be picked.
                      isDisabled={
                        detected !== null &&
                        !detected.some((d) => d.id === a && d.installed)
                      }
                    >
                      {i > 0 && <ToggleButtonGroup.Separator />}
                      {a}
                    </ToggleButton>
                  ))}
                </ToggleButtonGroup>
              </div>
            );
          })}
        </Card>
      </section>
      <Separator />
      <section className="flex flex-col gap-2">
        <Typography.Heading level={6}>MCP servers</Typography.Heading>
        <Card
          variant="secondary"
          className="set-row flex-row items-center gap-4 px-4 py-3"
        >
          <div className="flex min-w-0 flex-1 flex-col gap-0.5">
            <span className="text-sm font-medium">
              Allow my Claude MCP servers in spaces
            </span>
            <span className="text-xs text-muted">
              A space's agent can use your Claude Code MCP servers (Linear,
              GitHub…). Reviews always stay on Theodore's own tools.
            </span>
          </div>
          <Switch
            size="sm"
            aria-label="Allow my Claude MCP servers in spaces"
            isSelected={isUserMcpAllowed(store)}
            onChange={(v) => setSetting(store, 'user', USER_MCP_KEY, v)}
          >
            <Switch.Content>
              <Switch.Control>
                <Switch.Thumb />
              </Switch.Control>
            </Switch.Content>
          </Switch>
        </Card>
      </section>
    </>
  );
});

const DEFAULT_CHOICE = '__default';

/** One `<Select>` over `choices` plus "Default" (= unset). */
function ChoiceSelect({
  label,
  value,
  choices,
  onChange,
}: {
  label: string;
  value: string | null;
  choices: { id: string; label: string }[];
  onChange: (v: string | null) => void;
}) {
  return (
    <Select
      aria-label={label}
      value={value ?? DEFAULT_CHOICE}
      onChange={(v) => {
        if (v == null) return;
        const k = String(v);
        onChange(k === DEFAULT_CHOICE ? null : k);
      }}
      className="w-36"
    >
      <Select.Trigger>
        <Select.Value />
        <Select.Indicator />
      </Select.Trigger>
      <Select.Popover>
        <ListBox selectionMode="single">
          {[{ id: DEFAULT_CHOICE, label: 'Default' }, ...choices].map((c) => (
            <ListBox.Item key={c.id} id={c.id} textValue={c.label}>
              {c.label}
              <ListBox.ItemIndicator />
            </ListBox.Item>
          ))}
        </ListBox>
      </Select.Popover>
    </Select>
  );
}

/** Settings → Agents: an agent's default model and effort. Claude picks
 *  from its aliases; Codex and Gemini take a model name as typed. */
const AgentModelRow = observer(function AgentModelRow({
  agent,
}: {
  agent: AgentId;
}) {
  const store = useRootStore();
  const model = agentModel(store, agent);
  const [draft, setDraft] = useState(model ?? '');
  useEffect(() => setDraft(model ?? ''), [model]);
  const save = () => {
    if ((draft.trim() || null) !== model) setAgentModel(store, agent, draft);
  };
  // A full model name typed elsewhere stays editable as text.
  const alias = !model || CLAUDE_MODELS.some((m) => m.id === model);
  return (
    <div className="set-row flex items-center gap-3 border-b border-separator px-4 py-2.5 last:border-b-0">
      <span className="min-w-0 flex-1">
        <AgentTag id={agent} />
      </span>
      {hasModelAliases(agent) && alias ? (
        <ChoiceSelect
          label={`${agent} model`}
          value={model}
          choices={CLAUDE_MODELS}
          onChange={(v) => setAgentModel(store, agent, v)}
        />
      ) : (
        <TextField
          value={draft}
          onChange={setDraft}
          onBlur={save}
          onKeyDown={(e) => e.key === 'Enter' && save()}
          className="w-36"
        >
          <Label className="sr-only">{agent} model</Label>
          <Input placeholder="default model" className="font-mono text-xs" />
        </TextField>
      )}
      {takesEffort(agent) ? (
        <ChoiceSelect
          label={`${agent} effort`}
          value={agentEffort(store, agent)}
          choices={EFFORTS}
          onChange={(v) => setAgentEffort(store, agent, v)}
        />
      ) : (
        <span className="w-36 text-xs text-muted">no effort flag</span>
      )}
    </div>
  );
});

const UpdatesPage = observer(function UpdatesPage() {
  const store = useRootStore();
  const [update, setUpdate] = useState<UpdateInfo | null>(null);
  const [checking, setChecking] = useState(false);
  // What the last "Check now" found: nothing yet, up to date, or why it failed.
  const [result, setResult] = useState<
    { kind: 'latest'; at: number } | { kind: 'error'; message: string } | null
  >(null);
  const channel =
    (settingValue(store, 'user', 'updates.channel') as string) ?? 'stable';
  // A staging build updates from staging builds only; nothing to pick.
  const staging = store.ephemeral.appInfo?.build_env === 'staging';
  const check = () => {
    setChecking(true);
    void api
      .checkUpdate(true)
      .then((u) => {
        setUpdate(u);
        setResult(u ? null : { kind: 'latest', at: Date.now() });
      })
      .catch((e) => {
        setUpdate(null);
        setResult({
          kind: 'error',
          message: checkFailureText(
            e instanceof CommandFailure ? e.message : String(e),
            isOffline(store),
          ),
        });
      })
      .finally(() => setChecking(false));
  };
  return (
    <>
      <section className="flex flex-col gap-2">
        <Typography.Heading level={6}>Channel</Typography.Heading>
        <Card
          variant="secondary"
          className="flex-row items-center gap-4 px-4 py-3"
        >
          <span className="flex-1 text-sm text-muted">
            {staging
              ? 'A staging build updates to staging builds only'
              : 'Which build to update to'}
          </span>
          {staging ? (
            <span className="text-sm">Staging</span>
          ) : (
            <Select
              aria-label="Update channel"
              value={channel}
              onChange={(v) =>
                v && setSetting(store, 'user', 'updates.channel', v)
              }
              className="w-40"
            >
              <Select.Trigger>
                <Select.Value />
                <Select.Indicator />
              </Select.Trigger>
              <Select.Popover>
                <ListBox selectionMode="single">
                  <ListBox.Item id="stable" textValue="Stable">
                    Stable
                    <ListBox.ItemIndicator />
                  </ListBox.Item>
                  <ListBox.Item id="beta" textValue="Beta">
                    Beta
                    <ListBox.ItemIndicator />
                  </ListBox.Item>
                </ListBox>
              </Select.Popover>
            </Select>
          )}
        </Card>
      </section>
      <section className="flex flex-col gap-2">
        <Typography.Heading level={6}>Check for updates</Typography.Heading>
        <Btn
          tone="tertiary"
          className="self-start"
          isPending={checking}
          onPress={check}
        >
          Check now
        </Btn>
        {result?.kind === 'latest' && (
          <span className="text-xs text-muted">
            You're on the latest version.
          </span>
        )}
        {result?.kind === 'error' && (
          <span className="text-xs text-danger">{result.message}</span>
        )}
        {update && (
          <Card variant="secondary" className="flex flex-col gap-2 p-4 text-sm">
            <span className="font-medium">
              v{update.version}{' '}
              {update.downloaded ? '— ready to install' : '— downloading'}
            </span>
            <p className="text-muted">{update.notes}</p>
            {update.downloaded && (
              <Btn tone="primary" onPress={() => void api.installUpdate()}>
                Restart to update
              </Btn>
            )}
          </Card>
        )}
      </section>
    </>
  );
});

/** What `key` resolves to for `scope`, and where that value came from. */
function effectiveOf(
  store: ReturnType<typeof useRootStore>,
  scope: string,
  key: string,
): { value: unknown; source: 'repo' | 'user' | 'default' } {
  const repoVal = settingValue(store, scope, key);
  if (repoVal !== undefined) return { value: repoVal, source: 'repo' };
  const userVal = settingValue(store, 'user', key);
  if (userVal !== undefined) return { value: userVal, source: 'user' };
  // Every agent task falls back to Claude (the core's own default).
  return { value: AGENT_DEFAULT, source: 'default' };
}

const AGENT_DEFAULT: AgentId = 'claude';

const SOURCE_NOTE: Record<'repo' | 'user' | 'default', string> = {
  repo: 'from this project’s config files',
  user: 'from your user settings',
  default: 'default',
};

/** Settings → Projects → ⟨repo⟩: the settings effective for this repo and
 *  where each came from, then the project's config file: create it from
 *  these settings, edit, check and save it (`ConfigEditor`). */
const RepoSettingsPage = observer(function RepoSettingsPage({
  project,
  onBack,
}: {
  project: Project;
  onBack: () => void;
}) {
  const store = useRootStore();
  const scope = `repo:${project.repo.owner}/${project.repo.name}`;
  const rows: { key: string; title: string }[] = [
    ...AI_KEYS.map((key) => ({ key, title: AI_TITLE[key] })),
  ];
  return (
    <>
      <Button
        variant="ghost"
        size="sm"
        className="repo-settings-back w-fit gap-2 px-2 text-muted"
        onPress={onBack}
      >
        <ArrowLeft className="size-4" />
        Projects
      </Button>
      <section className="flex flex-col gap-2">
        <Typography.Heading level={6}>Effective settings</Typography.Heading>
        <Card variant="secondary" className="gap-0 p-0">
          {rows.map((r) => {
            const eff = effectiveOf(store, scope, r.key);
            return (
              <div
                key={r.key}
                className="set-row flex items-center gap-4 border-b border-separator px-4 py-3 last:border-b-0"
              >
                <div className="flex min-w-0 flex-1 flex-col gap-0.5">
                  <span className="text-sm font-medium">{r.title}</span>
                  <span className="text-xs text-muted">
                    {SOURCE_NOTE[eff.source]}
                  </span>
                </div>
                <span className="text-sm tabular-nums text-muted">
                  {eff.value == null ? '—' : String(eff.value)}
                </span>
              </div>
            );
          })}
        </Card>
        <span className="text-xs text-muted">
          A value from the project’s config wins over your user settings for
          this repo — change it in the config below.
        </span>
      </section>
      <Separator />
      <ConfigEditor project={project} />
    </>
  );
});

const ProjectsPage = observer(function ProjectsPage() {
  const store = useRootStore();
  const ui = useUi();
  const ws = useWorkspaces();
  const projects = ws.projects; // switcher order, not add order
  const [sel, setSel] = useState<string | null>(null);
  const project = sel ? store.models.projects.get(sel) : undefined;
  const { dragAndDropHooks } = useDragAndDrop({
    getItems: (keys) => [...keys].map((key) => ({ 'text/plain': String(key) })),
    onReorder(e) {
      const { dropPosition } = e.target;
      if (dropPosition !== 'before' && dropPosition !== 'after') return;
      ws.reorder([...e.keys].map(String), String(e.target.key), dropPosition);
    },
  });
  if (project) {
    return <RepoSettingsPage project={project} onBack={() => setSel(null)} />;
  }
  return (
    <section className="flex flex-col gap-2">
      <Typography.Heading level={6}>Connected repositories</Typography.Heading>
      {projects.length === 0 ? (
        <div className="flex items-center gap-3">
          <span className="flex-1 text-sm text-muted">
            No projects yet — connect a repository or a local folder.
          </span>
          <Button
            size="sm"
            variant="secondary"
            onPress={() => ui.openModal('add-project')}
          >
            Add project…
          </Button>
        </div>
      ) : (
        <>
          <Card variant="secondary" className="gap-0 p-0">
            <ListBox
              aria-label="Projects"
              selectionMode="none"
              dragAndDropHooks={dragAndDropHooks}
              onAction={(k: Key) => setSel(String(k))}
              className="p-0 [&>*+*]:mt-0"
            >
              {projects.map((p) => (
                <ListBox.Item
                  key={p.id}
                  id={p.id}
                  textValue={projectLabel(p)}
                  className="repo-row flex w-full items-center gap-3 rounded-none border-b border-separator px-4 py-3 last:border-b-0 hover:bg-default/50 data-[dragging]:opacity-50"
                >
                  <Grip
                    aria-hidden
                    className="size-4 shrink-0 cursor-grab text-muted"
                  />
                  <Folder className="size-4 text-muted" />
                  <span className="min-w-0 flex-1 truncate text-sm">
                    {projectLabel(p)}
                  </span>
                  <ChevronRight className="size-4 text-muted" />
                </ListBox.Item>
              ))}
            </ListBox>
          </Card>
          <span className="text-xs text-muted">
            Drag to reorder — the switcher and ⌃1…9 follow this order.
          </span>
        </>
      )}
    </section>
  );
});

const AboutPage = observer(function AboutPage() {
  const ui = useUi();
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    void api
      .appInfo()
      .then(setInfo)
      .catch(() => setInfo(null));
  }, []);
  if (!info) return <Spinner />;
  const built = builtAtIso(info);
  return (
    <section className="flex flex-col gap-2">
      <Card variant="secondary" className="flex flex-col gap-1 p-4 text-sm">
        <div className="flex items-center gap-2">
          <Gear className="size-4 text-muted" />
          <span className="build-label flex-1 font-medium">
            Theodore {buildLabel(info)}
          </span>
          <Button
            size="sm"
            variant="ghost"
            className="build-copy"
            onPress={() =>
              void navigator.clipboard
                .writeText(buildReport(info))
                .then(() => setCopied(true))
                .catch(() => {})
            }
          >
            {copied ? 'Copied' : 'Copy'}
          </Button>
        </div>
        <span
          className="build-details pl-6 font-mono text-xs break-all text-muted"
          title={built ?? undefined}
        >
          {info.commit}
          {built ? ` · built ${new Date(built).toLocaleString()}` : ''}
        </span>
      </Card>
      <UpdatesPage />
      <PrivacyCard />
      <Card
        variant="secondary"
        className="flex flex-row items-center gap-3 p-4"
      >
        <CircleInfo className="size-4 text-muted" />
        <span className="flex-1 text-sm text-muted">
          Collects logs into a zip on this Mac for you to review and send.
        </span>
        <Button
          size="sm"
          variant="ghost"
          className="report-problem-open"
          onPress={() => ui.openModal('report-problem')}
        >
          Report a problem…
        </Button>
      </Card>
      <Card
        variant="secondary"
        className="flex flex-row items-center gap-3 p-4"
      >
        <Keyboard className="size-4 text-muted" />
        <span className="flex-1 text-sm text-muted">
          Every shortcut, in one place.
        </span>
        <Button size="sm" variant="ghost" onPress={() => ui.openModal('help')}>
          Keyboard shortcuts
        </Button>
      </Card>
      <Card
        variant="secondary"
        className="flex flex-row items-center gap-3 p-4"
      >
        <ArrowRotateRight className="size-4 text-muted" />
        <span className="flex-1 text-sm text-muted">
          Show the welcome tour again the next time this view loads.
        </span>
        <Button
          size="sm"
          variant="ghost"
          className="reset-onboarding"
          onPress={() => ui.resetOnboarding()}
        >
          Reset onboarding
        </Button>
      </Card>
    </section>
  );
});

export const SettingsPage = observer(function SettingsPage({
  page,
}: {
  page: Page;
}) {
  const store = useRootStore();
  const labs = isLabsVisible(store);
  // `updates` is an old page id: it now lives on About & updates.
  const shown: Page = page === 'updates' ? 'about' : page;
  const title = PAGES.find((p) => p.id === shown)?.title ?? 'Settings';
  return (
    <div className="view settings-page flex min-w-0 flex-1 flex-col overflow-auto">
      <div className="mx-auto flex w-full max-w-[760px] flex-col gap-6 px-8 py-8">
        <div>
          <Typography.Heading level={4}>{title}</Typography.Heading>
          <span className="text-sm text-muted">{SUB[page]}</span>
        </div>
        {page === 'account' ? (
          <AccountPage />
        ) : page === 'appearance' ? (
          <section className="flex flex-col gap-2">
            <Typography.Heading level={6}>Theme</Typography.Heading>
            <ThemePicker />
          </section>
        ) : page === 'agents' ? (
          <AgentsPage />
        ) : page === 'integrations' ? (
          isLinearOn(store) ? (
            <IntegrationsPage />
          ) : null
        ) : page === 'projects' ? (
          <ProjectsPage />
        ) : page === 'languages' ? (
          <LanguagesPage />
        ) : page === 'sandbox' ? (
          <SandboxPage />
        ) : page === 'labs' ? (
          labs ? (
            <LabsPage />
          ) : null
        ) : page === 'experiments' ? (
          <ExperimentsPage />
        ) : (
          <AboutPage />
        )}
      </div>
    </div>
  );
});
