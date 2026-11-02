/**
 * Spaces (docs/spaces.md), hidden behind build mode: the space page (chat
 * with the agent on the left, the live diff vs base on the right), the
 * New space dialog, Settings → Labs, and the "turn done" notices.
 */
import { useEffect, useMemo, useRef, useState } from 'react';
import type { MutableRefObject } from 'react';

import {
  Archive,
  ArrowUpFromLine,
  ArrowUturnCcwLeft,
  BranchesRight,
  ChevronDown,
  ChevronRight,
  CodeMerge,
  CodePullRequest,
  Folder,
  LayoutSideContentRight,
  Pencil,
} from '@gravity-ui/icons';
import {
  Alert,
  Button,
  Card,
  Dropdown,
  Input,
  InputGroup,
  Label,
  Modal,
  Popover,
  Switch,
  TextField,
  ToggleButton,
  Tooltip,
  Typography,
} from '@heroui/react';
import { observer } from 'mobx-react-lite';
import type {
  AgentId,
  HunkId,
  ShipAction,
  SkillInfo,
  Space,
  SpaceConfigView,
  SpaceMode,
  SpaceStart,
} from '../../gen/contract';
import { api } from '../../core/api';
import { STORAGE_NS } from '../../stores/UiStore';
import { ALL } from '../../core/workspaces';
import { FEATURES } from '../../features';
import { setSetting } from '../../actions';

import { useRootStore, useUi } from '../../stores/context';
import { Hints, K, Spinner } from '../bits';
import { DiffPane, type ComposerState } from '../DiffPane';
import { RevisionError } from '../review/RevisionError';
import type { ReviewMode } from '../GroupsPane';
import { ToolCalls } from '../ToolCalls';
import { useAttachments } from '../attachments';
import { ChatEmpty, ChatLive, ChatLog, ChatMessage } from '../ChatTurn';
import { QueuedMessages } from './QueuedMessages';
import { canSendNow, splitQueue } from './queue';
import { latinKey, useViewKeys } from '../keys';
import { AgentChip, ChipMenu, Composer } from './Composer';
import { agentModel } from '../agentModel';
import { isBuildMode } from './mode';
import { ReviewMenu } from './ReviewMenu';
import { markSpaceSeen } from './seen';
import { useProjectIssues } from './useProjectIssues';
import {
  checksByReply,
  runControl,
  runsByReply,
  settingUp,
  setupView,
} from './commands';
import { GlobalConfigSection, SpaceSetupAssistHint } from './ConfigSection';
import { SpaceProgress } from './SpaceProgress';
import { BaseMoved, UpdatingBanner } from './BaseUpdate';
import { MergedBanner, SpaceMergeButton, SpacePrPane } from './SpacePr';
import {
  CheckChip,
  LogsDrawer,
  LogsToggle,
  PortChips,
  RunButton,
  SetupNote,
  SetupStage,
  TrustPrompt,
  toggleRun,
  useSpaceConfig,
  useSpaceLogsSeed,
} from './SpaceCommands';
import { TerminalDrawer, TerminalToggle } from './Terminal';
import {
  AUTO_ARCHIVE_KEY,
  BUILD_MODE_KEY,
  NOTIFY_KEY,
  STAGE_LABEL,
  autoArchiveDays,
  defaultTitle,
  diffStat,
  nextMode,
  notifyOn,
  spaceKeyAction,
  spaceChatKey,
  spaceStage,
  undoState,
  undoneMessages,
  unshipped,
  spaceTitle,
} from './spaces';

/** What the agent may do in a turn (docs/spaces.md §5). */
const MODES: { id: SpaceMode; label: string; hint: string }[] = [
  { id: 'ask', label: 'Ask', hint: 'Read only, no edits' },
  { id: 'edit', label: 'Edit', hint: 'Edit files in the space' },
  { id: 'run', label: 'Run', hint: 'Edit and run any command' },
];

/** The whole branch diff, ungrouped — every file of the revision. */
const WHOLE_DIFF: ReviewMode = { mode: 'groups', group: null, file: null };

type Store = ReturnType<typeof useRootStore>;

const LS_SPACE_CHANGES = `${STORAGE_NS}theodore-space-changes`;
const LS_SPACE_TERMINAL = `${STORAGE_NS}theodore-space-terminal`;
type Ui = ReturnType<typeof useUi>;

/** What the page's keys reach inside the chat and the ship controls. */
interface ChatHandle {
  focus: () => void;
  cycleMode: () => void;
}
interface ShipHandle {
  primary: () => void;
  alternate: () => void;
}

async function openGroupedReview(store: Store, ui: Ui, space: Space) {
  const res = await api.spaceReview({ space: space.id, group: true });
  await store.sync.openPr(res.pr);
  ui.go({ name: 'review', pr: res.pr });
}

function openSpacePr(store: Store, ui: Ui, space: Space) {
  const id = space.pr;
  if (!id) return;
  void store.sync.openPr(id).then(() => ui.go({ name: 'review', pr: id }));
}

function archive(ui: Ui, space: Space) {
  void api
    .archiveSpace(space.id)
    .then(() => ui.goBack())
    .catch(() => {});
}

/** Undo last turn (button on the reply, palette). Errors are toasted by
 *  api.ts. */
export function undoLastTurn(space: Pick<Space, 'id'>): Promise<void> {
  return api
    .undoSpaceTurn(space.id)
    .then(() => {})
    .catch(() => {});
}

export function unarchive(space: Pick<Space, 'id'>): Promise<void> {
  return api
    .unarchiveSpace(space.id)
    .then(() => {})
    .catch(() => {});
}

export const SpacePage = observer(function SpacePage({ id }: { id: string }) {
  const store = useRootStore();
  const ui = useUi();
  const space = store.models.spaces.get(id);
  const chat = useRef<ChatHandle | null>(null);
  const ship = useRef<ShipHandle | null>(null);
  const { config, refresh: refreshConfig } = useSpaceConfig(id, space);
  const [logsOpen, setLogsOpen] = useState(false);
  // The changes pane can be hidden for a chat-only page; remembered for
  // every space (it's a way of working, not a per-space thing).
  const [showChanges, setShowChanges] = useState(
    () => localStorage.getItem(LS_SPACE_CHANGES) !== '0',
  );
  const toggleChanges = () =>
    setShowChanges((v) => {
      localStorage.setItem(LS_SPACE_CHANGES, v ? '0' : '1');
      return !v;
    });
  // Same idea for the terminal drawer, default closed: its shells live in
  // the core regardless, so reopening it (or navigating back to the page)
  // just re-lists and re-attaches them.
  const [terminalOpen, setTerminalOpen] = useState(
    () => localStorage.getItem(LS_SPACE_TERMINAL) === '1',
  );
  const toggleTerminal = () =>
    setTerminalOpen((v) => {
      localStorage.setItem(LS_SPACE_TERMINAL, v ? '0' : '1');
      return !v;
    });
  // ⌃` toggles it from anywhere on the page, composer included — the same
  // reach as ⌘.'s stop, below.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!e.ctrlKey || e.key !== '`') return;
      e.preventDefault();
      setTerminalOpen((v) => {
        localStorage.setItem(LS_SPACE_TERMINAL, v ? '0' : '1');
        return !v;
      });
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, []);
  useSpaceLogsSeed(id);
  const run = space ? runControl(config, space) : null;

  // Page keys (docs/spaces.md §3) — never while typing in the composer.
  useViewKeys(
    ui,
    (e) => {
      if (!space || e.key === 'Escape') return false;
      const action = spaceKeyAction(latinKey(e), e.shiftKey);
      const archived = space.status === 'archived';
      const running = space.status === 'running';
      switch (action) {
        case 'focus':
          chat.current?.focus();
          return;
        case 'mode':
          chat.current?.cycleMode();
          return;
        case 'review':
          if (!archived)
            void openGroupedReview(store, ui, space).catch(() => {});
          return;
        case 'run':
          if (!run) return false;
          void toggleRun(space, !run.running);
          return;
        case 'ship':
          ship.current?.primary();
          return;
        case 'ship-alt':
          ship.current?.alternate();
          return;
        case 'open-pr':
          if (space.pr) openSpacePr(store, ui, space);
          return;
        case 'changes':
          toggleChanges();
          return;
        case 'archive':
          if (!running && !archived) archive(ui, space);
          return;
        default:
          return false;
      }
    },
    [space?.id, space?.status, space?.pr, run?.running, !!run],
  );

  // ⌘. stops the turn — from anywhere on the page, the composer included.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey) || latinKey(e) !== '.') return;
      if (store.models.spaces.get(id)?.status !== 'running') return;
      e.preventDefault();
      void api.spaceStop(id).catch(() => {});
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, [store, id]);

  useEffect(() => {
    void store.sync.openPr(spaceChatKey(id));
  }, [store, id]);

  // Open the live diff without grouping before anything asks for its
  // revision (a cold `get_revision` would open it grouped).
  const [opened, setOpened] = useState(false);
  const live = space && space.status !== 'archived';
  useEffect(() => {
    if (!live) return;
    let on = true;
    api
      .spaceReview({ space: id, group: false })
      .then(() => on && setOpened(true))
      .catch(() => {});
    return () => {
      on = false;
    };
  }, [id, live]);

  // Seen only when you can see it: a turn that ends with the window in the
  // background still counts on the dock badge until you come back.
  const updatedAt = space?.updated_at;
  useEffect(() => {
    if (updatedAt == null) return;
    const mark = () => markSpaceSeen(id, updatedAt);
    if (document.hasFocus()) mark();
    window.addEventListener('focus', mark);
    return () => window.removeEventListener('focus', mark);
  }, [id, updatedAt]);

  if (!space) {
    return (
      <div className="view grid flex-1 place-items-center text-muted">
        This space no longer exists.
      </div>
    );
  }
  return (
    <div className="view space-page flex min-w-0 flex-1 flex-col">
      <SpaceHeader
        space={space}
        ship={ship}
        config={config}
        logsOpen={logsOpen}
        onToggleLogs={() => setLogsOpen((v) => !v)}
        terminalOpen={terminalOpen}
        onToggleTerminal={toggleTerminal}
        showChanges={showChanges}
        onToggleChanges={toggleChanges}
      />
      <UpdatingBanner space={space} />
      <div className="flex min-h-0 flex-1">
        <section
          className={
            showChanges
              ? 'flex w-[40%] min-w-[340px] flex-col border-r border-separator bg-background'
              : 'flex w-full min-w-0 flex-col bg-background'
          }
        >
          <div className="mx-auto w-full max-w-[860px]">
            <TrustPrompt
              space={space}
              config={config}
              onChanged={refreshConfig}
            />
          </div>
          <SpaceChat space={space} config={config} handle={chat} />
        </section>
        {showChanges && (
          <section className="flex min-w-0 flex-1 flex-col">
            {opened || !live ? (
              space.pr ? (
                <SpacePrPane
                  pr={space.pr}
                  changes={<SpaceChanges space={space} />}
                />
              ) : (
                <SpaceChanges space={space} />
              )
            ) : (
              <div className="grid flex-1 place-items-center text-muted">
                <Spinner />
              </div>
            )}
          </section>
        )}
      </div>
      {logsOpen && (
        <LogsDrawer space={space} onClose={() => setLogsOpen(false)} />
      )}
      {terminalOpen && live && (
        <TerminalDrawer
          space={space.id}
          open={terminalOpen}
          onClose={() => setTerminalOpen(false)}
        />
      )}
      {space.status === 'archived' && <ArchivedBanner space={space} />}
      <MergedBanner space={space} />
      <Hints>
        <K>C</K> message the agent <K>M</K> mode <K>D</K> changes <K>⌃`</K>{' '}
        terminal <K>⌘.</K> stop <K>R</K> review <K>P</K>{' '}
        {space.pr ? 'push' : 'ship'}
        {space.pr ? (
          <>
            {' '}
            <K>O</K> open PR
          </>
        ) : null}
        {run ? (
          <>
            {' '}
            <K>⇧R</K> {run.running ? 'stop' : 'run'}
          </>
        ) : null}{' '}
        <K>⇧E</K> archive <K>Esc</K> back
      </Hints>
    </div>
  );
});

function ArchivedBanner({ space }: { space: Space }) {
  const [busy, setBusy] = useState(false);
  return (
    <Alert status="accent" className="m-3 items-center">
      <Alert.Content>
        <Alert.Description>
          Archived. The transcript and the checkpoint ref are kept.
        </Alert.Description>
      </Alert.Content>
      <Button
        size="sm"
        variant="secondary"
        className="space-unarchive shrink-0"
        isPending={busy}
        onPress={() => {
          setBusy(true);
          void unarchive(space).finally(() => setBusy(false));
        }}
      >
        <ArrowUturnCcwLeft className="size-3.5" />
        Unarchive
      </Button>
    </Alert>
  );
}

const SpaceHeader = observer(function SpaceHeader({
  space,
  ship,
  config,
  logsOpen,
  onToggleLogs,
  terminalOpen,
  onToggleTerminal,
  showChanges,
  onToggleChanges,
}: {
  space: Space;
  ship: MutableRefObject<ShipHandle | null>;
  config: SpaceConfigView | null;
  logsOpen: boolean;
  onToggleLogs: () => void;
  terminalOpen: boolean;
  onToggleTerminal: () => void;
  showChanges: boolean;
  onToggleChanges: () => void;
}) {
  const store = useRootStore();
  const ui = useUi();
  const running = space.status === 'running';
  const archived = space.status === 'archived';
  const project = store.models.projects.get(space.project);
  const pr = space.pr ? store.models.prSummaries.get(space.pr) : undefined;
  const stage = spaceStage(space, pr?.state);
  const days = autoArchiveDays(store.models.settings.all());
  return (
    // Just what you act on: the name, and the stage only once it says
    // something (in review, done). Branch, base and agent are in the
    // name's tooltip.
    <header className="flex min-h-14 flex-nowrap items-center gap-2 border-b border-separator px-6 py-2 [&>button]:shrink-0">
      <Typography.Heading
        level={6}
        className="min-w-0 shrink truncate"
        title={`${space.branch} ← ${space.base} · ${space.agent}`}
      >
        {spaceTitle(space)}
      </Typography.Heading>
      {stage !== 'drafting' && (
        <span
          className="space-stage shrink-0 rounded-full bg-default px-2 py-0.5 text-xs"
          title={
            space.done_at && days > 0
              ? `Archives itself ${days} day${days === 1 ? '' : 's'} after it was done`
              : undefined
          }
        >
          {STAGE_LABEL[stage]}
          {pr ? ` · #${pr.pr.number}` : ''}
        </span>
      )}
      <span className="flex-1" />
      {/* Once its PR merged, Continue moves it onto the new base. */}
      {!(space.pr && stage === 'done') && <BaseMoved space={space} />}
      <RunButton space={space} config={config} />
      <PortChips space={space} />
      <LogsToggle open={logsOpen} onToggle={onToggleLogs} />
      {!archived && (
        <TerminalToggle open={terminalOpen} onToggle={onToggleTerminal} />
      )}
      <ReviewMenu space={space} />
      {!archived && project && (
        <ShipControls
          space={space}
          local={project.path != null}
          handle={ship}
        />
      )}
      {!archived && space.pr && <SpaceMergeButton space={space} />}
      {space.pr && (
        <Button
          size="sm"
          variant="ghost"
          onPress={() => openSpacePr(store, ui, space)}
        >
          <CodePullRequest className="size-3.5" />
          PR
        </Button>
      )}
      <Tooltip>
        <Button
          size="sm"
          variant="ghost"
          aria-label="Archive space"
          isIconOnly
          className="space-archive shrink-0"
          isDisabled={running || archived}
          onPress={() => archive(ui, space)}
        >
          <Archive className="size-4" />
        </Button>
        <Tooltip.Content>
          Archive <K>⇧E</K> — removes the worktree; the transcript and
          checkpoints are kept
        </Tooltip.Content>
      </Tooltip>
      <Tooltip>
        <ToggleButton
          isIconOnly
          size="sm"
          aria-label="Changes"
          className="shrink-0"
          isSelected={showChanges}
          onChange={onToggleChanges}
        >
          <LayoutSideContentRight className="size-4" />
        </ToggleButton>
        <Tooltip.Content>
          {showChanges ? 'Hide' : 'Show'} the changes <K>D</K>
        </Tooltip.Content>
      </Tooltip>
    </header>
  );
});

interface ShipChoice {
  action: ShipAction;
  label: string;
  /** One line under the title field: what happens on confirm. */
  hint: string;
}

/** Open PR (▾ Open as draft) / Push on a host project; Merge locally
 *  (▾ Keep as branch) on a local folder. Confirming lets you edit the
 *  commit / PR title first. */
const ShipControls = observer(function ShipControls({
  space,
  local,
  handle,
}: {
  space: Space;
  local: boolean;
  /** `P` / `⇧P` on the page. */
  handle: MutableRefObject<ShipHandle | null>;
}) {
  const store = useRootStore();
  const [open, setOpen] = useState(false);
  const [choice, setChoice] = useState<ShipChoice | null>(null);
  const [title, setTitle] = useState('');
  const [busy, setBusy] = useState(false);
  const running = space.status === 'running';
  const changed = space.head_sha !== space.base_sha;

  const ship = async (action: ShipAction, t: string | null) => {
    setBusy(true);
    try {
      const s = await api.shipSpace({ space: space.id, action, title: t });
      setOpen(false);
      const where = s.shipped?.branch ?? s.branch;
      store.ephemeral.toast(
        action === 'merge_locally'
          ? `Merged into ${where}`
          : action === 'keep_branch'
            ? `Wrote branch ${where}`
            : action === 'push'
              ? `Pushed to ${where}`
              : `Pushed ${where}; opening the PR…`,
        'info',
      );
    } catch {
      // toasted by api.ts; the popover stays open to retry
    } finally {
      setBusy(false);
    }
  };

  if (space.pr) {
    const pending = unshipped(space);
    handle.current = {
      primary: () => {
        if (!running && !busy && pending) void ship('push', null);
      },
      alternate: () => {},
    };
    return (
      <Tooltip>
        <Button
          size="sm"
          variant="secondary"
          className="space-push"
          isDisabled={running || busy || !pending}
          isPending={busy}
          onPress={() => void ship('push', null)}
        >
          <ArrowUpFromLine className="size-3.5" />
          Push
        </Button>
        <Tooltip.Content>
          {pending
            ? 'One new commit on the PR branch with the turns since the last push'
            : 'Nothing new since the last push'}
        </Tooltip.Content>
      </Tooltip>
    );
  }

  const primary: ShipChoice = local
    ? {
        action: 'merge_locally',
        label: 'Merge locally',
        hint: "One commit by you, merged into the folder's checked-out branch. Refused if the folder has uncommitted changes or the merge conflicts.",
      }
    : {
        action: 'open_pr',
        label: 'Open PR',
        hint: `One commit by you, pushed to ${space.branch}, opened as a PR into ${space.base}.`,
      };
  const alternate: ShipChoice = local
    ? {
        action: 'keep_branch',
        label: 'Keep as branch',
        hint: `One commit by you, written as branch ${space.branch} in the folder. Nothing is checked out or merged.`,
      }
    : {
        action: 'open_draft_pr',
        label: 'Open as draft',
        hint: `One commit by you, pushed to ${space.branch}, opened as a draft PR into ${space.base}.`,
      };
  const shippedLocally =
    space.shipped != null && space.shipped.how !== 'pr' && !unshipped(space);
  const disabled = running || busy || !changed || shippedLocally;
  const start = (c: ShipChoice) => {
    setChoice(c);
    setTitle(defaultTitle(space));
    setOpen(true);
  };
  const current = choice ?? primary;
  handle.current = {
    primary: () => !disabled && start(primary),
    alternate: () => !disabled && start(alternate),
  };

  return (
    <div className="space-ship flex items-center">
      <Popover isOpen={open} onOpenChange={setOpen}>
        <Button
          size="sm"
          variant="primary"
          className="rounded-r-none"
          isDisabled={disabled}
          onPress={() => start(primary)}
        >
          {local ? (
            <CodeMerge className="size-3.5" />
          ) : (
            <CodePullRequest className="size-3.5" />
          )}
          {space.shipped?.how === 'pr' ? 'Open PR again' : primary.label}
        </Button>
        <Popover.Content
          placement="bottom end"
          offset={8}
          className="w-[380px]"
        >
          <Popover.Dialog className="flex flex-col gap-3 p-4">
            <Popover.Heading className="text-sm font-semibold">
              {current.label}
            </Popover.Heading>
            <TextField value={title} onChange={setTitle} autoFocus>
              <Label>Title</Label>
              <Input
                className="text-sm"
                onKeyDown={(e) => {
                  if (e.key === 'Enter' && title.trim()) {
                    void ship(current.action, title.trim());
                  }
                }}
              />
            </TextField>
            <span className="text-xs text-muted">{current.hint}</span>
            <div className="flex justify-end gap-2">
              <Button size="sm" variant="ghost" onPress={() => setOpen(false)}>
                Cancel
              </Button>
              <Button
                size="sm"
                isPending={busy}
                isDisabled={!title.trim()}
                onPress={() => void ship(current.action, title.trim())}
              >
                {current.label}
              </Button>
            </div>
          </Popover.Dialog>
        </Popover.Content>
      </Popover>
      <Dropdown>
        <Button
          size="sm"
          variant="primary"
          isIconOnly
          className="rounded-l-none border-l border-separator"
          aria-label="More ways to ship"
          isDisabled={disabled}
        >
          <ChevronDown className="size-3.5" />
        </Button>
        <Dropdown.Popover placement="bottom end">
          <Dropdown.Menu aria-label="Ship" onAction={() => start(alternate)}>
            <Dropdown.Item id={alternate.action} textValue={alternate.label}>
              {alternate.label}…
            </Dropdown.Item>
          </Dropdown.Menu>
        </Dropdown.Popover>
      </Dropdown>
    </div>
  );
});

/** The space's chat: its turns, the live one, the composer. */
export const SpaceChat = observer(function SpaceChat({
  space,
  config,
  handle,
}: {
  space: Space;
  config: SpaceConfigView | null;
  /** `C` / `M` on the page. */
  handle: MutableRefObject<ChatHandle | null>;
}) {
  const store = useRootStore();
  const [text, setText] = useState('');
  const files = useAttachments();
  const input = useRef<HTMLTextAreaElement>(null);
  const end = useRef<HTMLDivElement>(null);
  // Queued messages wait under the running turn, in the order they send.
  const { sent: messages, queued } = splitQueue([
    ...store.models.chatMessages.byPr(spaceChatKey(space.id)),
  ]);
  const run = space.status === 'running' ? space.run : null;
  const undone = undoneMessages(space);
  const undo = undoState(space);
  const checks = checksByReply(space.turn_log);
  const turnRuns = runsByReply(space.id, space.turn_log);
  // A finished turn's tool calls, kept for this session; the turn still
  // running (its check) shows them below instead.
  const turnTools = (reply: string) => {
    const r = turnRuns.get(reply);
    const tools = r && r !== run ? (store.ephemeral.runLog.get(r) ?? []) : [];
    return r && tools.length > 0 ? (
      <ToolCalls tools={tools} running={false} turn={r} />
    ) : null;
  };
  const setupStage = setupView(space) === 'stage';
  // A turn whose check still runs has its reply stored already: its text
  // shows there, not twice.
  const replied = run != null && [...turnRuns.values()].includes(run);
  useEffect(() => {
    end.current?.scrollIntoView?.({ block: 'end' });
  }, [messages.length, queued.length, run]);

  // Sticky per space: the next turn starts in the space's last mode.
  const [mode, setMode] = useState<SpaceMode>(space.mode);
  useEffect(() => setMode(space.mode), [space.mode]);
  // Open while a turn runs: a queued message keeps the picks it was sent
  // with.
  const locked = space.status === 'archived';
  // `/skill` in the box: your Claude skills and the repo's.
  const [skills, setSkills] = useState<SkillInfo[]>([]);
  useEffect(() => {
    let live = true;
    void api
      .spaceSkills(space.id)
      .then((list) => live && setSkills(list))
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [space.id]);
  // The agent for the next turn, with its model and effort (the chip).
  const [agent, setAgent] = useState<AgentId>(space.agent);
  useEffect(() => setAgent(space.agent), [space.agent]);
  handle.current = {
    focus: () => input.current?.focus(),
    cycleMode: () => !locked && setMode((m) => nextMode(m)),
  };

  const send = (now = false) => {
    const t = text.trim();
    if ((!t && !files.ids.length) || files.busy) return;
    if (space.status === 'archived') return;
    setText('');
    void (now ? api.spaceSendNow : api.spaceSend)({
      space: space.id,
      text: t,
      agent: agent === space.agent ? null : agent,
      // The chip's model, so a change there reaches the next turn; "Send
      // now" keeps the running turn's.
      model: now ? null : agentModel(store, agent),
      mode: mode === space.mode ? null : mode,
      attachments: files.ids.length ? files.ids : null,
    })
      .then(() => files.clear())
      .catch(() => setText(t));
  };
  const sendNow = canSendNow(space, {
    agent,
    mode,
    files: files.ids.length,
  })
    ? () => send(true)
    : undefined;

  return (
    <>
      <ChatLog end={end}>
        <SetupStage space={space} />
        <SetupNote space={space} />
        {messages.length === 0 && !run && !setupStage && (
          <>
            <ChatEmpty
              icon={<Pencil />}
              title="Tell the agent what to change"
              hint="It works in its own worktree; your checkout is never touched."
            />
            <SpaceSetupAssistHint space={space} config={config} />
          </>
        )}
        {messages.map((m) => {
          const agent = m.author.kind === 'agent';
          const undoneReply = agent && undone.has(m.id);
          const undoHere =
            undo.turn?.reply === m.id && space.status !== 'archived';
          return (
            <ChatMessage
              key={m.id}
              m={m}
              className={undone.has(m.id) ? 'space-undone opacity-50' : ''}
              badges={
                undoneReply || undoHere ? (
                  <>
                    {undoneReply && (
                      <span className="rounded-full bg-default px-1.5 text-[10px] text-muted">
                        undone
                      </span>
                    )}
                    {undoHere && (
                      <UndoTurn
                        space={space}
                        reason={undo.ok ? null : undo.reason}
                      />
                    )}
                  </>
                ) : undefined
              }
              after={
                agent ? (
                  <>
                    {turnTools(m.id)}
                    {checks.has(m.id) && (
                      <CheckChip check={checks.get(m.id)!} />
                    )}
                  </>
                ) : undefined
              }
            />
          );
        })}
        {run && (
          <ChatLive
            run={run}
            agent={space.agent}
            text={!replied}
            follow={end}
          />
        )}
        <QueuedMessages space={space} queued={queued} />
      </ChatLog>
      <div className="chat-input px-4 pb-3 pt-1">
        <div className="mx-auto flex w-full max-w-[860px] flex-col gap-2">
          {space.status === 'failed' && space.error && (
            <span
              className="truncate px-2 text-xs text-danger"
              title={space.error}
            >
              {space.error}
            </span>
          )}
          <SpaceProgress space={space} />
          <Composer
            inputRef={input}
            value={text}
            onChange={setText}
            onSubmit={() => send()}
            placeholder={
              space.status === 'archived'
                ? 'Archived'
                : settingUp(space)
                  ? 'Setting up…'
                  : run
                    ? 'Queue a message for after this turn…'
                    : 'Ask the agent to change something…'
            }
            disabled={space.status === 'archived' || settingUp(space)}
            running={!!run}
            onStop={() => void api.spaceStop(space.id).catch(() => {})}
            onSendNow={sendNow}
            queueWhileRunning
            skills={skills}
            attachments={files}
            chips={
              <>
                <AgentChip
                  value={agent}
                  onChange={setAgent}
                  isDisabled={locked}
                />
                <ChipMenu
                  ariaLabel="Mode"
                  label={MODES.find((m) => m.id === mode)?.label ?? mode}
                  items={MODES}
                  selected={mode}
                  onSelect={setMode}
                  isDisabled={locked}
                />
              </>
            }
          />
        </div>
      </div>
    </>
  );
});

/** Undo last turn, on the agent's reply that ended it. `reason` set:
 *  shown disabled with why (still running, already shipped…). */
function UndoTurn({ space, reason }: { space: Space; reason: string | null }) {
  const [busy, setBusy] = useState(false);
  return (
    <Tooltip>
      <Button
        size="sm"
        variant="ghost"
        className="space-undo h-5 gap-1 px-1.5 text-[11px]"
        isDisabled={reason != null}
        isPending={busy}
        onPress={() => {
          setBusy(true);
          void undoLastTurn(space).finally(() => setBusy(false));
        }}
      >
        <ArrowUturnCcwLeft className="size-3" />
        Undo
      </Button>
      <Tooltip.Content>
        {reason ?? 'Back to the checkpoint before this turn; the chat keeps it'}
      </Tooltip.Content>
    </Tooltip>
  );
}

const SpaceChanges = observer(function SpaceChanges({
  space,
}: {
  space: Space;
}) {
  const store = useRootStore();
  const review = space.review_pr;
  const summary = review ? store.models.prSummaries.get(review) : undefined;
  const revKey = review && summary ? `${review}@${summary.head_sha}` : null;
  // Bumped by Retry so the page calls `get()` again after `invalidate`.
  const [, setRetries] = useState(0);
  const revision =
    review && summary && revKey
      ? store.revisions.get(revKey, () =>
          api.getRevision({ pr: review, head: summary.head_sha }),
        )
      : null;
  const [focusHunk, setFocusHunk] = useState<HunkId | null>(null);
  const [composer, setComposer] = useState<ComposerState | null>(null);
  const setMode = useMemo(() => () => {}, []);
  const rev = revision?.status === 'ready' ? revision.data : null;
  return (
    <>
      <div className="flex items-center gap-3 border-b border-separator px-5 py-2.5 text-xs text-muted">
        <span className="tabular-nums text-foreground">
          {rev ? diffStat(rev.files) : '…'}
        </span>
        <span>
          vs {space.base} ·{' '}
          {space.turns ? `after turn ${space.turns}` : 'no turns yet'}
        </span>
        {space.status === 'running' && (
          <span>· refreshes when the turn ends</span>
        )}
      </div>
      {!review || !summary || !revision ? (
        <div className="grid flex-1 place-items-center text-muted">
          No changes yet.
        </div>
      ) : revision.status === 'error' ? (
        <RevisionError
          error={revision.error}
          onRetry={() => {
            if (revKey) store.revisions.invalidate(revKey);
            setRetries((n) => n + 1);
          }}
        />
      ) : !rev ? (
        <div className="grid flex-1 place-items-center text-muted">
          <Spinner />
        </div>
      ) : rev.files.length === 0 ? (
        <div className="grid flex-1 place-items-center text-muted">
          No changes yet.
        </div>
      ) : (
        <DiffPane
          pr={review}
          rev={rev}
          mode={WHOLE_DIFF}
          setMode={setMode}
          focusHunk={focusHunk}
          setFocusHunk={setFocusHunk}
          composer={composer}
          setComposer={setComposer}
          nextGroup={false}
          askAi={false}
        />
      )}
    </>
  );
});

/** `goal`: the plain, usual case. `issue` / `branch` / `pr` adopt something
 *  that already exists on the host (docs/spaces.md §3 "New space") instead
 *  of forking a fresh branch from the goal. */
type StartKind = 'goal' | 'issue' | 'branch' | 'pr';

const START_KIND_ITEMS: { id: StartKind; label: string; hint?: string }[] = [
  { id: 'goal', label: 'A new branch', hint: 'from the goal' },
  { id: 'issue', label: 'An existing issue' },
  { id: 'branch', label: 'An existing branch' },
  { id: 'pr', label: 'An existing PR' },
];

/** New space: a composer, not a form — say what you want; the project,
 *  agent and base sit as chips around it. ⏎ creates and sends it as the
 *  first turn; the space opens on that turn. Picking "An existing issue /
 *  branch / PR" adopts it instead: the worktree starts on its tip, and
 *  shipping pushes back to it rather than a fresh `<login>/<slug>`. */
export const NewSpaceModal = observer(function NewSpaceModal() {
  const store = useRootStore();
  const ui = useUi();
  const ready = store.models.projects.where((p) => p.status === 'ready');
  const [project, setProject] = useState<string | null>(
    ui.workspace !== ALL && ready.some((p) => p.id === ui.workspace)
      ? ui.workspace
      : (ready[0]?.id ?? null),
  );
  const [goal, setGoal] = useState('');
  const [agent, setAgent] = useState<AgentId | null>(null);
  const [busy, setBusy] = useState(false);
  const [startKind, setStartKind] = useState<StartKind>('goal');
  const [issueId, setIssueId] = useState<string | null>(null);
  const [startText, setStartText] = useState('');
  const chosen = ready.find((p) => p.id === project);
  const issues = useProjectIssues(startKind === 'issue' ? project : null);
  // Switching project, or away from "issue"/"branch"/"PR", drops whatever
  // was picked/typed for the previous one rather than silently reusing it.
  useEffect(() => {
    setIssueId(null);
    setStartText('');
  }, [project, startKind]);
  const prNumber = Number(startText.trim());
  const startFrom: SpaceStart | null =
    startKind === 'issue' && issueId
      ? { kind: 'issue', value: issueId }
      : startKind === 'branch' && startText.trim()
        ? { kind: 'branch', value: startText.trim() }
        : startKind === 'pr' && startText.trim() && Number.isInteger(prNumber) && prNumber > 0
          ? { kind: 'pr', value: prNumber }
          : null;
  // A plain goal needs the text; an adopted issue/branch/PR needs that
  // picked/typed instead — the goal becomes an optional extra note.
  const canStart = startKind === 'goal' ? goal.trim() !== '' : startFrom !== null;
  const start = async () => {
    if (!project || !canStart || busy) return;
    setBusy(true);
    try {
      const s = await api.createSpace({
        project,
        goal: goal.trim(),
        name: null,
        branch: null,
        base: null,
        agent,
        // The agent's model setting applies at each turn.
        model: null,
        start_from: startFrom,
      });
      ui.closeModal();
      ui.go({ name: 'space', id: s.id });
    } catch {
      // toasted by api.ts
    } finally {
      setBusy(false);
    }
  };
  return (
    <Modal.Backdrop isOpen onOpenChange={(open) => !open && ui.closeModal()}>
      <Modal.Container placement="top" className="mt-[14vh]">
        <Modal.Dialog
          aria-label="New space"
          className="modal new-space flex w-[680px] max-w-[92vw] flex-col gap-3 p-3"
        >
          <header className="flex items-center gap-1.5 px-1 text-sm">
            {ready.length > 1 ? (
              <ChipMenu
                ariaLabel="Project"
                label={chosen ? chosen.repo.name : 'Project'}
                icon={<Folder className="size-3.5" />}
                items={ready.map((p) => ({
                  id: p.id,
                  label: p.repo.name,
                  hint: p.path ?? `${p.repo.owner}/${p.repo.name}`,
                }))}
                selected={project ?? ''}
                onSelect={setProject}
              />
            ) : (
              <span className="flex items-center gap-1.5 px-2 text-xs">
                <Folder className="size-3.5 text-muted" />
                {chosen?.repo.name}
              </span>
            )}
            <ChevronRight className="size-3 text-muted" />
            <span className="font-medium">New space</span>
            <span className="flex-1" />
            <Modal.CloseTrigger aria-label="Close" className="static" />
          </header>
          <Composer
            value={goal}
            onChange={setGoal}
            onSubmit={() => void start()}
            placeholder={
              startKind === 'goal'
                ? 'What do you want to work on?'
                : 'Anything else the agent should know? (optional)'
            }
            rows={5}
            quiet
            autoFocus
            disabled={busy || !project}
            canSubmitEmpty={startKind !== 'goal' && startFrom !== null}
            submit={busy ? 'Creating…' : 'Create'}
            chips={
              <>
                <AgentChip value={agent} onChange={setAgent} />
                <ChipMenu
                  ariaLabel="Start from"
                  label={
                    START_KIND_ITEMS.find((i) => i.id === startKind)?.label
                  }
                  items={START_KIND_ITEMS}
                  selected={startKind}
                  onSelect={setStartKind}
                />
                {startKind === 'goal' && chosen?.default_branch && (
                  <span className="flex items-center gap-1 px-2 text-xs text-muted">
                    <BranchesRight className="size-3.5" />
                    from {chosen.default_branch}
                  </span>
                )}
              </>
            }
          />
          {startKind === 'issue' && (
            <div className="flex flex-wrap items-center gap-1.5 px-2">
              {issues.length === 0 ? (
                <span className="text-xs text-muted">
                  No open issues assigned to you on this project's host.
                </span>
              ) : (
                <ChipMenu
                  ariaLabel="Issue"
                  label={
                    issueId
                      ? `#${issueId} ${
                          issues.find((i) => String(i.number) === issueId)
                            ?.title ?? ''
                        }`
                      : 'Pick an issue'
                  }
                  items={issues.map((i) => ({
                    id: String(i.number),
                    label: `#${i.number} ${i.title}`,
                  }))}
                  selected={issueId ?? ''}
                  onSelect={setIssueId}
                />
              )}
            </div>
          )}
          {(startKind === 'branch' || startKind === 'pr') && (
            <div className="flex items-center gap-1.5 px-2">
              <TextField
                value={startText}
                onChange={setStartText}
                aria-label={startKind === 'branch' ? 'Branch name' : 'PR number'}
                autoFocus
              >
                <InputGroup className="w-56 rounded-xl">
                  {startKind === 'pr' && (
                    <InputGroup.Prefix className="text-xs text-muted">
                      #
                    </InputGroup.Prefix>
                  )}
                  <InputGroup.Input
                    placeholder={
                      startKind === 'branch' ? 'feature/retry-export' : '123'
                    }
                    inputMode={startKind === 'pr' ? 'numeric' : 'text'}
                    className="w-full text-sm"
                  />
                </InputGroup>
              </TextField>
            </div>
          )}
          <span className="px-2 text-xs text-muted">
            {startKind === 'goal' ? (
              <>
                The agent works in its own worktree — your checkout is never
                touched. <K>⇧⏎</K> new line.
              </>
            ) : (
              <>
                The worktree starts on its tip; shipping pushes back to the
                same branch.
              </>
            )}
          </span>
        </Modal.Dialog>
      </Modal.Container>
    </Modal.Backdrop>
  );
});

/** Settings → Labs: the build mode switch, and the spaces' own settings. */
export const LabsPage = observer(function LabsPage() {
  const store = useRootStore();
  const on = isBuildMode(store);
  return (
    <section className="flex flex-col gap-2">
      <Card
        variant="secondary"
        className="flex flex-row items-center gap-3 px-4 py-3"
      >
        <Switch
          size="sm"
          aria-label="Spaces"
          isSelected={on}
          isDisabled={FEATURES.buildMode}
          onChange={(v) => setSetting(store, 'user', BUILD_MODE_KEY, v)}
        >
          <Switch.Content>
            <Switch.Control>
              <Switch.Thumb />
            </Switch.Control>
          </Switch.Content>
        </Switch>
        <span className="text-sm">
          Spaces in the sidebar, ⌥N for a new one. Off hides them everywhere;
          existing spaces stay on disk.
        </span>
      </Card>
      {on && (
        <Card variant="secondary" className="gap-0 p-0">
          <NumberSettingRow
            settingKey={AUTO_ARCHIVE_KEY}
            label="Auto-archive after (days)"
            hint="Days after a space's PR merges, or it is merged locally or kept as a branch, before it archives itself. 0 = never."
            placeholder={String(autoArchiveDays([]))}
            accept={(n) => n >= 0 && Number.isInteger(n)}
          />
          <div className="set-row flex items-center gap-4 px-4 py-3">
            <div className="flex min-w-0 flex-1 flex-col gap-0.5">
              <span className="text-sm font-medium">
                Notify when a turn ends
              </span>
              <span className="text-xs text-muted">
                A system notification when a space's turn finishes or fails
                while Theodore is in the background; the dock badge counts the
                spaces waiting for you.
              </span>
            </div>
            <Switch
              size="sm"
              aria-label="Notify when a turn ends"
              isSelected={notifyOn(store.models.settings.all())}
              onChange={(v) => setSetting(store, 'user', NOTIFY_KEY, v)}
            >
              <Switch.Content>
                <Switch.Control>
                  <Switch.Thumb />
                </Switch.Control>
              </Switch.Content>
            </Switch>
          </div>
        </Card>
      )}
      {on && <GlobalConfigSection />}
    </section>
  );
});

/** One numeric user setting; empty clears it (the default applies). */
const NumberSettingRow = observer(function NumberSettingRow({
  settingKey,
  label,
  hint,
  prefix,
  placeholder,
  accept,
}: {
  settingKey: string;
  label: string;
  hint: string;
  prefix?: string;
  placeholder: string;
  accept: (n: number) => boolean;
}) {
  const store = useRootStore();
  const raw = store.models.settings.get(`user:${settingKey}`)?.value;
  const [text, setText] = useState(typeof raw === 'number' ? String(raw) : '');
  useEffect(() => {
    setText(typeof raw === 'number' ? String(raw) : '');
  }, [raw]);
  const save = () => {
    const t = text.trim();
    if (!t) {
      if (raw !== undefined) setSetting(store, 'user', settingKey, null);
      return;
    }
    const n = Number(t);
    if (Number.isFinite(n) && accept(n)) {
      setSetting(store, 'user', settingKey, n);
    }
  };
  return (
    <div className="set-row flex items-center gap-4 px-4 py-3">
      <div className="flex min-w-0 flex-1 flex-col gap-0.5">
        <span className="text-sm font-medium">{label}</span>
        <span className="text-xs text-muted">{hint}</span>
      </div>
      <TextField
        value={text}
        onChange={setText}
        aria-label={label}
        className="w-32 shrink-0"
      >
        <InputGroup className="w-full rounded-xl">
          {prefix && (
            <InputGroup.Prefix className="text-xs text-muted">
              {prefix}
            </InputGroup.Prefix>
          )}
          <InputGroup.Input
            placeholder={placeholder}
            inputMode="decimal"
            className="w-full min-w-0 text-right text-sm"
            onBlur={save}
            onKeyDown={(e) => e.key === 'Enter' && save()}
          />
        </InputGroup>
      </TextField>
    </div>
  );
});
