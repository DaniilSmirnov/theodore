/**
 * Spaces (docs/spaces.md), hidden behind build mode: the space page (chat
 * with the agent on the left, the live diff vs base on the right), the
 * New space dialog, Settings → Labs, and the "turn done" notices.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { MutableRefObject, RefObject } from 'react';

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
  FolderOpen,
  LayoutSideContentRight,
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
  FileDiff,
  HunkId,
  ShipAction,
  SkillInfo,
  Space,
  SpaceConfigView,
  SpaceMode,
  SpaceStart,
} from '../../gen/contract';
import { api, CommandFailure, HOST_UNSUPPORTED } from '../../core/api';
import { STORAGE_NS } from '../../stores/UiStore';
import { ALL } from '../../core/workspaces';
import { FEATURES } from '../../features';
import { setSetting } from '../../actions';

import { useRootStore, useUi } from '../../stores/context';
import { Hints, K, Spinner } from '../bits';
import { DiffPane, type ComposerState } from '../DiffPane';
import { RevisionError } from '../review/RevisionError';
import type { ReviewMode } from '../GroupsPane';
import { useAttachments } from '../attachments';
import { SpaceLog } from './SpaceLog';
import { sendToSpace, stopSpace } from './send';
import type { VirtualChatControl } from './VirtualChat';
import { canSendNow } from './queue';
import { attachTerminalMention, stripTerminalMention } from './terminalMention';
import { latinKey, useViewKeys } from '../keys';
import { AgentChip, ChipMenu, Composer } from './Composer';
import { agentModel } from '../agentModel';
import { isBuildMode } from './mode';
import { ReviewMenu } from './ReviewMenu';
import { markSpaceSeen } from './seen';
import { useProjectIssues } from './useProjectIssues';
import { parseIssueRef, startFooter, type StartKind } from './newSpaceStart';
import { setTerminalOpenFor, terminalOpenFor } from './terminalOpen';
import { runControl, settingUp } from './commands';
import { GlobalConfigSection, SpaceSetupAssistHint } from './ConfigSection';
import { SpaceProgress } from './SpaceProgress';
import { BaseMoved, UpdatingBanner } from './BaseUpdate';
import { MergedBanner, SpaceMergeButton, SpacePrPane } from './SpacePr';
import { revertItems } from './SpaceRewind';
import {
  LogsDrawer,
  LogsToggle,
  PortChips,
  RunButton,
  TrustPrompt,
  toggleRun,
  useSpaceConfig,
  useSpaceLogsSeed,
} from './SpaceCommands';
import { TerminalDrawer, TerminalToggle } from './Terminal';
import { useDrawerHeight } from './drawerHeight';
import {
  AUTO_ARCHIVE_KEY,
  BUILD_MODE_KEY,
  NOTIFY_KEY,
  STAGE_LABEL,
  autoArchiveDays,
  shipTitle,
  diffStat,
  actionablePlan,
  nextMode,
  notifyOn,
  spaceKeyAction,
  spaceChatKey,
  spaceStage,
  unshipped,
  liveTurns,
  spaceTitle,
  plainError,
  toggleDrawer,
  type Drawer,
} from './spaces';

/** What the agent may do in a turn (docs/spaces.md §5). */
const MODES: { id: SpaceMode; label: string; hint: string }[] = [
  { id: 'ask', label: 'Ask', hint: 'Read only, no edits' },
  {
    id: 'plan',
    label: 'Plan',
    hint: 'Explore, then a plan to approve before any edit',
  },
  { id: 'edit', label: 'Edit', hint: 'Edit files in the space' },
  { id: 'run', label: 'Run', hint: 'Edit and run any command' },
];

/** The whole branch diff, ungrouped — every file of the revision. */
const WHOLE_DIFF: ReviewMode = { mode: 'groups', group: null, file: null };

type Store = ReturnType<typeof useRootStore>;

const LS_SPACE_CHANGES = `${STORAGE_NS}theodore-space-changes`;
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

export function archive(ui: Ui, space: Space) {
  void api
    .archiveSpace(space.id)
    .then(() => ui.goBack())
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
  // A changed file's chip in the chat opens the pane; `SpaceChanges` aims it.
  const changesFocus = store.ephemeral.spaceChangesFocus.get(id);
  useEffect(() => {
    if (!changesFocus) return;
    localStorage.setItem(LS_SPACE_CHANGES, '1');
    setShowChanges(true);
  }, [changesFocus]);
  // One drawer at a time under the page, so the chat keeps its height. The
  // terminal one is remembered per space (`terminalOpen.ts`), default
  // closed: its shells live in the core regardless, so reopening it (or
  // navigating back to the page) just re-lists and re-attaches them. The
  // page is keyed by space, so this reads once.
  const [drawer, setDrawer] = useState<Drawer | null>(() =>
    terminalOpenFor(id) ? 'terminal' : null,
  );
  const toggle = useCallback(
    (which: Drawer) =>
      setDrawer((d) => {
        const next = toggleDrawer(d, which);
        setTerminalOpenFor(id, next === 'terminal');
        return next;
      }),
    [id],
  );
  const logsOpen = drawer === 'logs';
  const terminalOpen = drawer === 'terminal';
  // The drawer gives way to the chat: capped by the room left once the chat
  // log keeps ~200 px and the composer its height, re-measured as the
  // window resizes (`drawerHeight.ts`).
  const panes = useRef<HTMLDivElement>(null);
  const drawerH = useDrawerHeight(
    panes,
    terminalOpen ? 300 : 260,
    drawer !== null,
  );
  const toggleTerminal = () => toggle('terminal');
  // ⌃` toggles it from anywhere on the page, composer included — the same
  // reach as ⌘.'s stop, below.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!e.ctrlKey || e.key !== '`') return;
      e.preventDefault();
      toggle('terminal');
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, [toggle]);
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
      const current = store.models.spaces.get(id);
      if (current?.status !== 'running') return;
      e.preventDefault();
      stopSpace(store, current);
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
        onToggleLogs={() => toggle('logs')}
        terminalOpen={terminalOpen}
        onToggleTerminal={toggleTerminal}
        showChanges={showChanges}
        onToggleChanges={toggleChanges}
      />
      <UpdatingBanner space={space} />
      {/* The panes clip to this row, so a drawer below never sits under them;
          the drawer is capped by the room left, so the chat keeps a usable
          height. */}
      <div ref={panes} className="flex min-h-0 flex-1 overflow-hidden">
        <section
          className={
            showChanges
              ? 'flex min-h-0 w-[40%] min-w-[340px] flex-col border-r border-separator bg-background'
              : 'flex min-h-0 w-full min-w-0 flex-col bg-background'
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
          <section className="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden">
            {opened || !live ? (
              space.pr ? (
                <SpacePrPane
                  space={space.id}
                  pr={space.pr}
                  changes={<SpaceChanges space={space} />}
                  showChanges={changesFocus}
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
        <LogsDrawer
          space={space}
          onClose={() => toggle('logs')}
          height={drawerH}
        />
      )}
      {terminalOpen && live && (
        <TerminalDrawer
          space={space.id}
          open={terminalOpen}
          onClose={() => toggle('terminal')}
          height={drawerH}
        />
      )}
      {space.mirror && (
        <MirrorBanner
          space={space}
          folder={
            store.models.projects.get(space.project)?.path ?? 'the folder'
          }
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
    <Alert status="accent" className="m-3 w-auto shrink-0 items-center">
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
    // The title keeps 9rem (plus the stage chip's room) on the first line;
    // the actions drop to a line of their own when they don't fit beside
    // it, and give up their labels first (`.space-header`, styles.css).
    <header className="space-header flex min-h-14 shrink-0 flex-wrap items-center gap-x-2 gap-y-1 border-b border-separator px-6 py-2 [&>button]:shrink-0">
      <div
        className={`flex flex-1 basis-0 items-center gap-2 ${stage === 'drafting' ? 'min-w-[9rem]' : 'min-w-[18rem]'}`}
      >
        <Typography.Heading
          level={6}
          className="min-w-[9rem] shrink truncate"
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
      </div>
      <div className="ml-auto flex flex-wrap items-center justify-end gap-1.5 [&>button]:shrink-0">
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
            className="btn-collapsible"
            aria-label="Open the PR"
            onPress={() => openSpacePr(store, ui, space)}
          >
            <CodePullRequest className="size-3.5" />
            <span className="btn-label">PR</span>
          </Button>
        )}
        {!archived && project?.path != null && (
          <MirrorToggle space={space} folder={project.path} />
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
      </div>
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
          className="space-push btn-collapsible-narrow"
          aria-label="Push"
          isDisabled={running || busy || !pending}
          isPending={busy}
          onPress={() => void ship('push', null)}
        >
          <ArrowUpFromLine className="size-3.5" />
          <span className="btn-label-narrow">Push</span>
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
    setTitle(shipTitle(space));
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
                placeholder="Left empty, written from the change"
                onKeyDown={(e) => {
                  if (e.key === 'Enter') {
                    void ship(current.action, title.trim() || null);
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
                onPress={() => void ship(current.action, title.trim() || null)}
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

/** "Mirror into `<folder>`" (docs/spaces.md "Mirror into a local folder"):
 *  turns it on, fresh or resuming a paused one. Once it's on, the banner
 *  below the header is where you sync or turn it off — this button just
 *  shows that it's on. */
const MirrorToggle = observer(function MirrorToggle({
  space,
  folder,
}: {
  space: Space;
  folder: string;
}) {
  const [busy, setBusy] = useState(false);
  const mirror = space.mirror;
  const on = mirror != null;
  const name = folder.split('/').filter(Boolean).pop() ?? folder;
  return (
    <Tooltip>
      <ToggleButton
        isIconOnly
        size="sm"
        aria-label="Mirror into the folder"
        className="space-mirror shrink-0"
        isSelected={on}
        isDisabled={busy || on}
        onChange={() => {
          if (on) return;
          setBusy(true);
          void api
            .mirrorSpace(space.id)
            .catch(() => {
              // toasted by api.ts
            })
            .finally(() => setBusy(false));
        }}
      >
        {on ? <FolderOpen className="size-4" /> : <Folder className="size-4" />}
      </ToggleButton>
      <Tooltip.Content>
        {on
          ? mirror?.status === 'paused'
            ? `Mirroring into ${name} is paused`
            : `Mirroring into ${name} — see the banner below`
          : `Mirror into ${name}: write this space's changes into your own checkout so you can run it there`}
      </Tooltip.Content>
    </Tooltip>
  );
});

/** While a space mirrors into a local folder: its state, "Sync now" /
 *  "Resume", and "Mirror off" — refused, with the edited paths named, if
 *  the folder disagrees with what mirroring wrote there, until you
 *  Discard. Edits made in the folder are never synced back into the
 *  space. */
const MirrorBanner = observer(function MirrorBanner({
  space,
  folder,
}: {
  space: Space;
  folder: string;
}) {
  const [busy, setBusy] = useState(false);
  const [refusal, setRefusal] = useState<string | null>(null);
  const mirror = space.mirror;
  if (!mirror) return null;
  const paused = mirror.status === 'paused';

  const off = async (discard: boolean) => {
    setBusy(true);
    try {
      await api.unmirrorSpace({ space: space.id, discard });
      setRefusal(null);
    } catch (e) {
      setRefusal(
        e instanceof CommandFailure ? e.message : 'Mirror off failed.',
      );
    } finally {
      setBusy(false);
    }
  };
  const resumeOrSync = async () => {
    setBusy(true);
    try {
      await (paused ? api.mirrorSpace(space.id) : api.syncMirror(space.id));
    } catch {
      // toasted by api.ts
    } finally {
      setBusy(false);
    }
  };

  return (
    <Alert
      status={refusal ? 'danger' : 'accent'}
      className="m-3 w-auto shrink-0 items-center"
    >
      <Alert.Content>
        <Alert.Description>
          {refusal ??
            (paused
              ? `Mirroring into ${folder} is paused — nothing is written until you resume it.`
              : `Mirroring into ${folder}. Edits made there aren't synced back.`)}
        </Alert.Description>
      </Alert.Content>
      {refusal ? (
        <Button
          size="sm"
          variant="danger"
          isPending={busy}
          onPress={() => void off(true)}
        >
          Discard and turn off
        </Button>
      ) : (
        <>
          <Button
            size="sm"
            variant="secondary"
            isPending={busy}
            onPress={() => void resumeOrSync()}
          >
            {paused ? 'Resume' : 'Sync now'}
          </Button>
          <Button
            size="sm"
            variant="ghost"
            isPending={busy}
            onPress={() => void off(false)}
          >
            Mirror off
          </Button>
        </>
      )}
    </Alert>
  );
});

/** The space's chat: its turns, the live one, the composer. The two are
 *  siblings: what you type re-renders the composer, never the turns. */
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
  const revise = useRef<(() => void) | null>(null);
  const onRevise = useCallback(() => revise.current?.(), []);
  const log = useRef<VirtualChatControl | null>(null);
  return (
    <>
      <SpaceLog space={space} onRevise={onRevise} control={log} />
      <div className="px-4">
        <div className="mx-auto w-full max-w-[860px]">
          <SpaceSetupAssistHint space={space} config={config} />
        </div>
      </div>
      <SpaceComposer space={space} handle={handle} revise={revise} log={log} />
    </>
  );
});

/** The box under the chat, and everything that changes as you type in it. */
const SpaceComposer = observer(function SpaceComposer({
  space,
  handle,
  revise,
  log,
}: {
  space: Space;
  handle: MutableRefObject<ChatHandle | null>;
  /** Revise on a plan: asks here what should change. */
  revise: MutableRefObject<(() => void) | null>;
  /** The chat above: sending goes to the bottom of it. */
  log: RefObject<VirtualChatControl | null>;
}) {
  const store = useRootStore();
  const [text, setText] = useState('');
  const files = useAttachments();
  const input = useRef<HTMLTextAreaElement>(null);
  const run = space.status === 'running' ? space.run : null;
  const planHere = actionablePlan(space);
  const [revising, setRevising] = useState(false);
  useEffect(() => {
    if (planHere == null) setRevising(false);
  }, [planHere]);

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
  revise.current = () => {
    setMode('plan');
    setRevising(true);
    input.current?.focus();
  };

  const send = (now = false) => {
    const t = text.trim();
    if ((!t && !files.ids.length) || files.busy) return;
    if (space.status === 'archived') return;
    const attached = files.saved;
    setText('');
    setRevising(false);
    files.clear();
    log.current?.jumpToLatest();
    sendToSpace(
      store,
      space,
      {
        space: space.id,
        text: t,
        agent: agent === space.agent ? null : agent,
        // The chip's model, so a change there reaches the next turn; "Send
        // now" keeps the running turn's.
        model: now ? null : agentModel(store, agent),
        mode: mode === space.mode ? null : mode,
        attachments: attached.length ? attached.map((a) => a.id) : null,
      },
      { now, agent, attachments: attached },
    ).catch(() => {
      setText(t);
      attached.forEach(files.addSaved);
    });
  };
  const sendNow = canSendNow(space, {
    agent,
    mode,
    files: files.ids.length,
  })
    ? () => send(true)
    : undefined;
  // `@terminal ` attaches the space's most recently active terminal's
  // tail (docs/spaces.md §3, §11) the way a dropped file would, instead
  // of becoming literal message text.
  const onComposerChange = (next: string) => {
    const stripped = stripTerminalMention(next);
    if (stripped.found) {
      setText(stripped.text);
      void attachTerminalMention(space.id, files, (text) =>
        store.ephemeral.toast(text, 'info'),
      );
      return;
    }
    setText(next);
  };

  return (
    <div className="chat-input px-4 pb-3 pt-1">
      <div className="mx-auto flex w-full max-w-[860px] flex-col gap-2">
        {space.status === 'failed' && space.error && (
          <span
            className="truncate px-2 text-xs text-danger"
            title={plainError(space.error)}
          >
            {plainError(space.error)}
          </span>
        )}
        <SpaceProgress space={space} />
        <Composer
          inputRef={input}
          value={text}
          onChange={onComposerChange}
          onSubmit={() => send()}
          placeholder={
            space.status === 'archived'
              ? 'Archived'
              : settingUp(space)
                ? 'Setting up…'
                : run
                  ? 'Queue a message for after this turn…'
                  : revising
                    ? 'What should change in the plan?'
                    : 'Ask the agent to change something…'
          }
          disabled={space.status === 'archived' || settingUp(space)}
          running={!!run}
          onStop={() => stopSpace(store, space)}
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
                keep
              />
            </>
          }
        />
      </div>
    </div>
  );
});

/** Scrolls to `id` at once, then again while what's above it mounts and moves
 *  it. Returns how to stop. */
function aimAt(id: string, frames = 30): () => void {
  let frame = 0;
  let left = frames;
  const tick = () => {
    document.getElementById(id)?.scrollIntoView({ block: 'center' });
    left -= 1;
    if (left > 0) frame = requestAnimationFrame(tick);
  };
  frame = requestAnimationFrame(tick);
  return () => cancelAnimationFrame(frame);
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
  // Read when a file's menu opens, so the space's state is current then.
  const fileItems = useMemo(
    () => (file: FileDiff) => revertItems(space, file),
    [space],
  );
  const rev = revision?.status === 'ready' ? revision.data : null;
  // A changed file's chip in the chat, once the diff it aims at is here. A
  // file with no hunks (binary) has nothing to aim at: the pane is open.
  const focus = store.ephemeral.spaceChangesFocus.get(space.id);
  const stopAiming = useRef<(() => void) | null>(null);
  useEffect(() => () => stopAiming.current?.(), []);
  useEffect(() => {
    if (!focus || !rev) return;
    store.ephemeral.spaceChangesOpened(space.id);
    const file = rev.files.find((f) => f.path === focus.path);
    const hunk = file?.hunks[0];
    if (!hunk) return;
    setFocusHunk(hunk.id);
    stopAiming.current?.();
    stopAiming.current = aimAt(`H-${hunk.id}`);
  }, [store, space.id, focus, rev]);
  return (
    <>
      <div className="flex items-center gap-3 border-b border-separator px-5 py-2.5 text-xs text-muted">
        <span className="tabular-nums text-foreground">
          {rev ? diffStat(rev.files) : '…'}
        </span>
        <span>
          vs {space.base} ·{' '}
          {liveTurns(space) ? `after turn ${liveTurns(space)}` : 'no turns yet'}
        </span>
        {space.status === 'running' && (
          <span>· live, follows the agent's edits</span>
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
          fileItems={fileItems}
        />
      )}
    </>
  );
});

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
  const { issues, unsupported: pickerNoIssues } = useProjectIssues(
    startKind === 'issue' ? project : null,
  );
  // Create with a typed `#12` can beat the picker's answer: the host's
  // "no issues here" then comes back from create_space instead.
  const [noIssuesFor, setNoIssuesFor] = useState<string | null>(null);
  const noIssues =
    pickerNoIssues || (project !== null && noIssuesFor === project);
  // A local folder has no host issues to start from.
  const kindItems = START_KIND_ITEMS.filter(
    (i) => i.id !== 'issue' || chosen?.repo.host !== 'local',
  );
  const kindOffered = kindItems.some((i) => i.id === startKind);
  useEffect(() => {
    if (!kindOffered) setStartKind('goal');
  }, [kindOffered]);
  // Switching project, or away from "issue"/"branch"/"PR", drops whatever
  // was picked/typed for the previous one rather than silently reusing it.
  useEffect(() => {
    setIssueId(null);
    setStartText('');
  }, [project, startKind]);
  const prNumber = Number(startText.trim());
  // A picked issue, else a typed `#n` / URL (docs: it resolves directly).
  const issueNumber = issueId ? Number(issueId) : parseIssueRef(startText);
  const startFrom: SpaceStart | null =
    startKind === 'issue' && issueNumber && !noIssues
      ? { kind: 'issue', value: String(issueNumber) }
      : startKind === 'branch' && startText.trim()
        ? { kind: 'branch', value: startText.trim() }
        : startKind === 'pr' &&
            startText.trim() &&
            Number.isInteger(prNumber) &&
            prNumber > 0
          ? { kind: 'pr', value: prNumber }
          : null;
  // A plain goal needs the text; an adopted issue/branch/PR needs that
  // picked/typed instead — the goal becomes an optional extra note.
  const canStart =
    startKind === 'goal' ? goal.trim() !== '' : startFrom !== null;
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
    } catch (e) {
      // Anything else is toasted by api.ts.
      if (e instanceof CommandFailure && e.code === HOST_UNSUPPORTED)
        setNoIssuesFor(project);
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
                  items={kindItems}
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
          {startKind === 'issue' &&
            (noIssues ? (
              <span className="px-2 text-xs text-muted">
                This project's host has no issues to start from. Start from a
                branch or a PR instead.
              </span>
            ) : (
              <div className="flex flex-wrap items-center gap-1.5 px-2">
                <TextField
                  value={issueId ? `#${issueId}` : startText}
                  onChange={(v) => {
                    setIssueId(null);
                    setStartText(v);
                  }}
                  aria-label="Issue number or URL"
                  autoFocus
                >
                  <InputGroup className="w-72 rounded-xl">
                    <InputGroup.Input
                      placeholder="#123 or an issue URL"
                      className="w-full text-sm"
                    />
                  </InputGroup>
                </TextField>
                {issues.length > 0 && (
                  <ChipMenu
                    ariaLabel="Open issues"
                    label={
                      issueId
                        ? (issues.find((i) => String(i.number) === issueId)
                            ?.title ?? 'Open issues')
                        : 'Open issues'
                    }
                    items={issues.map((i) => ({
                      id: String(i.number),
                      label: `#${i.number} ${i.title}`,
                    }))}
                    selected={issueId ?? ''}
                    onSelect={(id) => {
                      setIssueId(id);
                      setStartText('');
                    }}
                  />
                )}
              </div>
            ))}
          {(startKind === 'branch' || startKind === 'pr') && (
            <div className="flex items-center gap-1.5 px-2">
              <TextField
                value={startText}
                onChange={setStartText}
                aria-label={
                  startKind === 'branch' ? 'Branch name' : 'PR number'
                }
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
            {startFooter(startKind, chosen?.default_branch ?? undefined)}
            {startKind === 'goal' && (
              <>
                {' '}
                <K>⇧⏎</K> new line.
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
