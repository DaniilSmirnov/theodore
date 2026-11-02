/**
 * A space's commands on its page: "Run commands from this repo?", the
 * "Setting up…" stage, Run / Stop and the port chips in the header, the
 * logs drawer, and the check chip on each turn.
 */
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from 'react';

import {
  CircleCheck,
  CircleXmark,
  Play,
  ShieldExclamation,
  StopFill,
  Terminal,
  Xmark,
  ArrowUpRightFromSquare,
} from '@gravity-ui/icons';
import { Button, Card, Chip, Tabs, Tooltip } from '@heroui/react';
import { observer } from 'mobx-react-lite';
import type {
  Space,
  SpaceCheck,
  SpaceConfigView,
  SpaceLogLine,
} from '../../gen/contract';
import { api } from '../../core/api';
import { openExternal } from '../../core/openExternal';
import { useRootStore } from '../../stores/context';
import { K, Spinner } from '../bits';
import {
  LOG_FILTERS,
  SETUP_LABEL,
  checkChip,
  filterLogs,
  logCounts,
  portUrl,
  runControl,
  setupView,
  trustState,
  type LogFilter,
} from './commands';

const EMPTY: SpaceLogLine[] = [];

/** `space_config` for the page, refetched when what it depends on moves
 *  (trust answered, setup moved on, archived). */
export function useSpaceConfig(
  id: string,
  space: Pick<Space, 'status' | 'commands_denied' | 'setup'> | undefined,
): {
  config: SpaceConfigView | null;
  refresh: () => void;
} {
  const [got, setGot] = useState<{
    id: string;
    config: SpaceConfigView;
  } | null>(null);
  const [tick, setTick] = useState(0);
  const refresh = useCallback(() => setTick((n) => n + 1), []);
  const live = space != null && space.status !== 'archived';
  const denied = space?.commands_denied;
  const setup = space?.setup?.status;
  useEffect(() => {
    if (!live) return;
    let on = true;
    api
      .spaceConfig(id)
      .then((config) => on && setGot({ id, config }))
      .catch(() => {});
    return () => {
      on = false;
    };
  }, [id, live, denied, setup, tick]);
  const config = live && got?.id === id ? got.config : null;
  return { config, refresh };
}

/** Seeds the log buffer from the core's tail when the page opens. */
export function useSpaceLogsSeed(space: string): void {
  const store = useRootStore();
  useEffect(() => {
    let on = true;
    api
      .spaceLogs(space)
      .then((lines) => on && store.ephemeral.seedSpaceLogs(space, lines))
      .catch(() => {});
    return () => {
      on = false;
    };
  }, [store, space]);
}

/** "Run commands from this repo?" — prominent until answered; after
 *  "Not now" a small note that opens it again. */
export const TrustPrompt = observer(function TrustPrompt({
  space,
  config,
  onChanged,
}: {
  space: Space;
  config: SpaceConfigView | null;
  onChanged: () => void;
}) {
  const [reviewing, setReviewing] = useState(false);
  const [busy, setBusy] = useState<'allow' | 'deny' | null>(null);
  const state = trustState(config);
  const repo = config?.repo_commands;
  if (!state || !repo || space.status === 'archived') return null;

  if (state === 'denied' && !reviewing) {
    return (
      <div className="space-trust-denied flex items-center gap-2 px-5 py-2 text-xs text-muted">
        <ShieldExclamation className="size-3.5 shrink-0" />
        <span className="flex-1">Commands from this repo were skipped.</span>
        <Button
          size="sm"
          variant="ghost"
          className="h-6 px-2 text-xs"
          onPress={() => setReviewing(true)}
        >
          Review…
        </Button>
      </div>
    );
  }

  const answer = async (allow: boolean) => {
    setBusy(allow ? 'allow' : 'deny');
    try {
      await api.spaceTrust({ space: space.id, allow, hash: repo.hash });
      setReviewing(false);
    } catch {
      // toasted by api.ts; the commands may have changed — show the new set
    } finally {
      setBusy(null);
      onChanged();
    }
  };

  return (
    <Card className="space-trust m-3 mb-0 flex flex-col gap-3 border border-warning/40 p-4">
      <div className="flex items-start gap-2">
        <ShieldExclamation className="mt-0.5 size-4 shrink-0 text-warning" />
        <div className="flex min-w-0 flex-col gap-0.5">
          <span className="text-sm font-medium">
            Run commands from this repo?
          </span>
          <span className="text-xs text-muted">
            The repo's config file asks to run these in the space. Allow is
            remembered for this project until they change; Not now skips them in
            this space only.
          </span>
        </div>
      </div>
      <ul className="flex max-h-56 flex-col gap-1.5 overflow-auto">
        {repo.items.map((it) => (
          <li key={it.key} className="flex min-w-0 flex-col gap-0.5">
            <span className="text-xs text-muted">{it.key}</span>
            <code className="whitespace-pre-wrap rounded-md bg-default px-2 py-1 font-mono text-xs [overflow-wrap:anywhere]">
              {it.value}
            </code>
          </li>
        ))}
      </ul>
      <div className="flex justify-end gap-2">
        {state === 'denied' ? (
          <Button size="sm" variant="ghost" onPress={() => setReviewing(false)}>
            Cancel
          </Button>
        ) : (
          <Button
            size="sm"
            variant="ghost"
            className="space-trust-deny"
            isDisabled={busy != null}
            isPending={busy === 'deny'}
            onPress={() => void answer(false)}
          >
            Not now
          </Button>
        )}
        <Button
          size="sm"
          className="space-trust-allow"
          isDisabled={busy != null}
          isPending={busy === 'allow'}
          onPress={() => void answer(true)}
        >
          Allow
        </Button>
      </div>
    </Card>
  );
});

/** A box of log lines, mono; stderr tinted. Follows the end while you're
 *  at the bottom, stays put once you scroll up. */
function LogLines({
  lines,
  className = '',
  empty,
}: {
  lines: readonly SpaceLogLine[];
  className?: string;
  empty: string;
}) {
  const box = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  useLayoutEffect(() => {
    const el = box.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  }, [lines]);
  return (
    <div
      ref={box}
      onScroll={(e) => {
        const el = e.currentTarget;
        stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
      }}
      className={`space-log overflow-auto rounded-md bg-default/60 px-3 py-2 font-mono text-[11px] leading-relaxed ${className}`}
    >
      {lines.length === 0 ? (
        <span className="text-muted">{empty}</span>
      ) : (
        lines.map((l, i) => (
          <div
            key={i}
            className={`whitespace-pre-wrap [overflow-wrap:anywhere] ${l.stderr ? 'text-danger' : ''}`}
          >
            {l.line}
          </div>
        ))
      )}
    </div>
  );
}

/** "Setting up…": takes the chat's place until setup ends — status, live
 *  output, notes, and Retry / Continue without setup when it failed. */
export const SetupStage = observer(function SetupStage({
  space,
}: {
  space: Space;
}) {
  const store = useRootStore();
  const [busy, setBusy] = useState<'retry' | 'skip' | 'fix' | null>(null);
  const setup = space.setup;
  const lines = store.ephemeral.spaceLogs.get(space.id) ?? EMPTY;
  if (!setup || setupView(space) !== 'stage') return null;
  const act = async (action: 'retry' | 'skip') => {
    setBusy(action);
    try {
      await api.spaceSetup({ space: space.id, action });
    } catch {
      // toasted by api.ts
    } finally {
      setBusy(null);
    }
  };
  const fix = async () => {
    setBusy('fix');
    try {
      await api.spaceFixSetup(space.id);
    } catch {
      // toasted by api.ts
    } finally {
      setBusy(null);
    }
  };
  const archived = space.status === 'archived';
  return (
    <Card
      variant="secondary"
      className="space-setup flex flex-col gap-2 p-4"
      data-status={setup.status}
    >
      <span
        className={`flex items-center gap-2 text-sm font-medium ${setup.status === 'failed' ? 'text-danger' : ''}`}
      >
        {setup.status === 'running' && <Spinner />}
        {setup.status === 'failed' && <CircleXmark className="size-4" />}
        {SETUP_LABEL[setup.status]}
      </span>
      {setup.goal_pending && (
        <span className="text-xs text-muted">
          Your message goes to the agent once setup ends.
        </span>
      )}
      {setup.status === 'waiting' && (
        <span className="text-xs text-muted">
          The setup command comes from the repo's config — answer “Run commands
          from this repo?” above.
        </span>
      )}
      <LogLines
        lines={filterLogs(lines, 'setup')}
        className="max-h-64"
        empty="No output yet."
      />
      {!!setup.notes?.length && (
        <ul className="flex flex-col gap-0.5 text-xs text-muted">
          {setup.notes.map((n, i) => (
            <li key={i}>· {n}</li>
          ))}
        </ul>
      )}
      {setup.status === 'failed' && setup.error && (
        <span className="text-xs text-danger [overflow-wrap:anywhere]">
          {setup.error}
        </span>
      )}
      {!archived &&
        (setup.status === 'failed' || setup.status === 'waiting') && (
          <div className="flex justify-end gap-2">
            <Button
              size="sm"
              variant="ghost"
              className="space-setup-skip"
              isDisabled={busy != null}
              isPending={busy === 'skip'}
              onPress={() => void act('skip')}
            >
              Continue without setup
            </Button>
            {setup.status === 'failed' && (
              <Button
                size="sm"
                variant="secondary"
                className="space-setup-fix"
                isDisabled={busy != null}
                isPending={busy === 'fix'}
                onPress={() => void fix()}
              >
                Fix with agent
              </Button>
            )}
            {setup.status === 'failed' && (
              <Button
                size="sm"
                className="space-setup-retry"
                isDisabled={busy != null}
                isPending={busy === 'retry'}
                onPress={() => void act('retry')}
              >
                Retry
              </Button>
            )}
          </div>
        )}
    </Card>
  );
});

/** A finished setup that left notes (files not copied, commands
 *  skipped): one compact, dismissible line. */
export const SetupNote = observer(function SetupNote({
  space,
}: {
  space: Space;
}) {
  const [hidden, setHidden] = useState(false);
  const notes = space.setup?.notes ?? [];
  if (hidden || setupView(space) !== 'note' || !space.setup) return null;
  return (
    <div className="space-setup-note flex items-start gap-2 rounded-md bg-default/60 px-3 py-2 text-xs text-muted">
      <span className="min-w-0 flex-1">
        {SETUP_LABEL[space.setup.status]} · {notes.join(' · ')}
      </span>
      <Button
        size="sm"
        variant="ghost"
        isIconOnly
        aria-label="Dismiss"
        className="size-5 min-w-0"
        onPress={() => setHidden(true)}
      >
        <Xmark className="size-3" />
      </Button>
    </div>
  );
});

/** Starts / stops the space's `run` command (⇧R). */
export function toggleRun(space: Pick<Space, 'id'>, start: boolean) {
  return api
    .spaceRun({ space: space.id, start })
    .then(() => {})
    .catch(() => {});
}

/** Header: Run / Stop for the `run` command, when one is set and allowed. */
export const RunButton = observer(function RunButton({
  space,
  config,
}: {
  space: Space;
  config: SpaceConfigView | null;
}) {
  const [busy, setBusy] = useState(false);
  const ctl = runControl(config, space);
  if (!ctl) return null;
  return (
    <Tooltip>
      <Button
        size="sm"
        variant={ctl.running ? 'secondary' : 'ghost'}
        className="space-run"
        isPending={busy}
        onPress={() => {
          setBusy(true);
          void toggleRun(space, !ctl.running).finally(() => setBusy(false));
        }}
      >
        {ctl.running ? (
          <StopFill className="size-3.5" />
        ) : (
          <Play className="size-3.5" />
        )}
        {ctl.running ? 'Stop' : 'Run'}
      </Button>
      <Tooltip.Content>
        <span className="font-mono">{ctl.script}</span>
        {ctl.last ? ` — last run ${ctl.last}` : ''} <K>⇧R</K>
      </Tooltip.Content>
    </Tooltip>
  );
});

/** Header: one chip per reserved port, opening it in the browser. */
export const PortChips = observer(function PortChips({
  space,
}: {
  space: Space;
}) {
  const ports = space.ports ?? [];
  // A port is only worth a link while the app is up to answer on it.
  const up = space.process?.status === 'running';
  if (!ports.length || !up || space.status === 'archived') return null;
  // The first port is the app the `run` command serves: "Open app".
  // Any others are extra ports it was given, named by their number.
  return (
    <>
      {ports.map((p, i) => (
        <Tooltip key={p}>
          <Button
            size="sm"
            variant="secondary"
            className="space-port shrink-0 gap-1.5"
            onPress={() => openExternal(portUrl(p))}
          >
            <ArrowUpRightFromSquare className="size-3.5" />
            {i === 0 ? 'Open app' : `Port ${p}`}
          </Button>
          <Tooltip.Content>Open {portUrl(p)} in the browser</Tooltip.Content>
        </Tooltip>
      ))}
    </>
  );
});

/** Header: opens and closes the logs drawer. */
export function LogsToggle({
  open,
  onToggle,
}: {
  open: boolean;
  onToggle: () => void;
}) {
  return (
    <Button
      size="sm"
      variant={open ? 'secondary' : 'ghost'}
      aria-label="Command logs"
      aria-pressed={open}
      className="space-logs-toggle"
      onPress={onToggle}
    >
      <Terminal className="size-3.5" />
      Logs
    </Button>
  );
}

/** The logs drawer under the page: setup / run / check / archive output,
 *  tabbed, live. */
export const LogsDrawer = observer(function LogsDrawer({
  space,
  onClose,
}: {
  space: Space;
  onClose: () => void;
}) {
  const store = useRootStore();
  const [filter, setFilter] = useState<LogFilter>('all');
  const lines = store.ephemeral.spaceLogs.get(space.id) ?? EMPTY;
  const counts = logCounts(lines);
  return (
    <section className="space-logs flex h-[260px] shrink-0 flex-col border-t border-separator">
      <div className="flex items-center gap-2 px-4 pt-2">
        <Tabs
          data-plain
          selectedKey={filter}
          onSelectionChange={(k) => setFilter(k as LogFilter)}
        >
          <Tabs.ListContainer>
            <Tabs.List aria-label="Log filter">
              {LOG_FILTERS.map((f) => (
                <Tabs.Tab key={f.id} id={f.id} className="whitespace-nowrap">
                  {f.label}
                  <span className="ml-1 text-muted">{counts[f.id]}</span>
                  <Tabs.Indicator />
                </Tabs.Tab>
              ))}
            </Tabs.List>
          </Tabs.ListContainer>
        </Tabs>
        <span className="flex-1" />
        <Button
          size="sm"
          variant="ghost"
          isIconOnly
          aria-label="Close logs"
          onPress={onClose}
        >
          <Xmark className="size-3.5" />
        </Button>
      </div>
      <LogLines
        lines={filterLogs(lines, filter)}
        className="m-3 mt-2 min-h-0 flex-1"
        empty="No output yet."
      />
    </section>
  );
});

/** On an agent reply: the turn's check — passed / failed / running —
 *  expanding to the tail of its output. */
export function CheckChip({ check }: { check: SpaceCheck }) {
  const [open, setOpen] = useState(false);
  const { tone, label } = checkChip(check);
  const output = check.output ?? [];
  return (
    <span className="space-check inline-flex flex-col gap-1" data-tone={tone}>
      <button
        type="button"
        className="inline-flex w-fit items-center"
        aria-expanded={open}
        disabled={output.length === 0}
        onClick={() => setOpen((v) => !v)}
      >
        <Chip
          size="sm"
          variant="soft"
          color={
            tone === 'passed'
              ? 'success'
              : tone === 'failed'
                ? 'danger'
                : 'default'
          }
          className="gap-1"
        >
          {tone === 'running' ? (
            <Spinner />
          ) : tone === 'passed' ? (
            <CircleCheck className="size-3" />
          ) : (
            <CircleXmark className="size-3" />
          )}
          {label}
        </Chip>
      </button>
      {open && output.length > 0 && (
        <pre className="max-h-64 overflow-auto whitespace-pre-wrap rounded-md bg-default/60 px-3 py-2 font-mono text-[11px] [overflow-wrap:anywhere]">
          {output.join('\n')}
        </pre>
      )}
    </span>
  );
}
