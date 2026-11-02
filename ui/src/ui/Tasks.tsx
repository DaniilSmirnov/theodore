import { useEffect, useState } from 'react';
import {
  Button,
  Dropdown,
  Header,
  ListBox,
  Tabs,
  TextField,
} from '@heroui/react';
import { observer } from 'mobx-react-lite';
import type { Key } from 'react';
import {
  ArrowUturnCcwLeft,
  Check,
  ListCheck,
  Sparkles,
} from '@gravity-ui/icons';
import type { AgentId, Task, TaskStatus } from '../gen/contract';
import { decideTask, delegateTask } from '../actions';
import { useRootStore, useUi } from '../stores/context';
import { AgentTag, Btn, Empty, Hints, K, Spinner } from './bits';
import { useContextMenu, type ContextMenuItem } from './ContextMenu';
import { PatchView } from './Patch';
import { TaskLive } from './RunLive';
import { ViewHeader } from './List';
import { latinKey, useViewKeys } from './keys';
import { TaskIcon } from './StatusIcon';

const AGENTS: AgentId[] = ['claude', 'codex', 'gemini'];

const ORDER: TaskStatus[] = ['review', 'todo', 'progress', 'done'];
const TITLE: Record<TaskStatus, string> = {
  review: 'To verify',
  todo: 'Todo',
  progress: 'In progress',
  done: 'Done',
};

type TaskTab = 'mine' | 'agents' | 'others' | 'all';

/** Who a task is on: human `assignee.id` is the host login, so "mine" is
 *  whichever task is assigned to the signed-in `viewer`. There's no `by`
 *  (who delegated it) in our contract, so unlike the prototype "mine"
 *  doesn't also pull in agent work the viewer merely requested, and
 *  "others" can't require that the viewer was the one who assigned it —
 *  both fall back to the coarser assignee-only read. */
/** `me` is what a task made before the login was known is assigned to. */
export const isMe = (id: string, me: string | null) =>
  id === 'me' || (me != null && id === me);

function taskTabs(me: string | null): {
  id: TaskTab;
  title: string;
  match: (t: Task) => boolean;
}[] {
  return [
    {
      id: 'mine',
      title: 'Mine',
      match: (t) => t.assignee.kind === 'human' && isMe(t.assignee.id, me),
    },
    {
      id: 'agents',
      title: 'Agents',
      match: (t) => t.assignee.kind === 'agent',
    },
    {
      id: 'others',
      title: 'Waiting on others',
      match: (t) => t.assignee.kind === 'human' && !isMe(t.assignee.id, me),
    },
    { id: 'all', title: 'All', match: () => true },
  ];
}

export const TaskRow = observer(function TaskRow({
  t,
  sel,
  compact,
}: {
  t: Task;
  sel?: boolean;
  compact?: boolean;
}) {
  const store = useRootStore();
  const ui = useUi();
  const pr = store.models.prSummaries.get(t.pr);
  const [rejecting, setRejecting] = useState(false);
  const [reason, setReason] = useState('');
  const [busy, setBusy] = useState(false);

  const accept = () => {
    setBusy(true);
    void decideTask(store, t.id, true).finally(() => setBusy(false));
  };
  const sendReject = () => {
    setBusy(true);
    void decideTask(store, t.id, false, reason.trim() || null).finally(() => {
      setBusy(false);
      setRejecting(false);
      setReason('');
    });
  };
  const delegate = (agent: AgentId | null) => {
    void delegateTask(store, t.id, agent);
  };

  const rowMenu = useContextMenu((): ContextMenuItem[] => [
    {
      id: 'assign',
      label: 'Assign…',
      onAction: () => ui.openModal('assign', [t.id]),
    },
    {
      id: 'delegate',
      label: 'Delegate',
      items: [
        {
          id: 'delegate-default',
          label: 'Default agent',
          onAction: () => delegate(null),
        },
        ...AGENTS.map((a) => ({
          id: `delegate-${a}`,
          label: a,
          onAction: () => delegate(a),
        })),
      ],
    },
    {
      id: 'accept',
      label: 'Accept',
      isDisabled: t.status !== 'review',
      onAction: accept,
    },
    {
      id: 'reject',
      label: 'Reject',
      isDisabled: t.status !== 'review',
      onAction: () => setRejecting(true),
    },
    {
      id: 'open-thread',
      label: 'Open thread',
      isDisabled: !t.thread,
      onAction: () => {
        ui.go({ name: 'review', pr: t.pr });
        ui.toggleAside(true);
      },
    },
  ]);

  // The fix running, the fix to review and the reject form: inside the title column in the
  // full list, on their own full-width row in the narrow side panel (where
  // sharing a row with the buttons squeezed the patch to a sliver).
  const details = (
    <>
      {t.status === 'progress' && <TaskLive task={t.id} />}
      {t.fix?.patch && t.status === 'review' && (
        <div
          className="task-patch mt-1.5 max-w-[720px]"
          onClick={(e) => e.stopPropagation()}
        >
          <PatchView patch={t.fix.patch} status={t.fix.status} />
        </div>
      )}
      {rejecting && (
        <div
          className="mt-1.5 flex items-center gap-1.5"
          onClick={(e) => e.stopPropagation()}
        >
          <TextField
            aria-label="Why is this rejected"
            value={reason}
            onChange={setReason}
            className="max-w-[320px] flex-1"
            autoFocus
          >
            <input
              className="w-full rounded-xl border border-separator bg-background px-2.5 py-1 text-xs outline-none"
              placeholder="Why (goes back to the agent)…"
              onKeyDown={(e) => {
                if (e.key === 'Enter') sendReject();
                if (e.key === 'Escape') setRejecting(false);
              }}
            />
          </TextField>
          <Btn tone="danger-soft" isPending={busy} onPress={sendReject}>
            Send back
          </Btn>
          <Btn tone="ghost" onPress={() => setRejecting(false)}>
            Cancel
          </Btn>
        </div>
      )}
    </>
  );

  return (
    <div
      id={`T-${t.id}`}
      className={`taskrow group flex min-h-12 w-full items-center gap-2.5 py-1.5 ${compact ? 'flex-wrap' : ''} ${sel ? 'sel' : ''}`}
      onContextMenu={rowMenu}
    >
      <TaskIcon status={t.status} />
      <div className="min-w-0 flex-1">
        <div
          className={`${compact ? 'line-clamp-2' : 'truncate'} font-medium ${t.status === 'done' ? 'text-muted' : ''}`}
          title={t.title}
        >
          {t.title}
        </div>
        {!compact && pr && (
          <div className="mt-0.5 flex min-w-0 items-center gap-2.5 text-xs text-muted">
            <span
              className="inline-flex min-w-0 cursor-pointer items-center gap-1 truncate font-mono hover:text-accent"
              title={`#${pr.pr.number} ${pr.title}`}
              onClick={(e) => {
                e.stopPropagation();
                ui.go({ name: 'review', pr: t.pr });
              }}
            >
              #{pr.pr.number} {pr.title}
            </span>
          </div>
        )}
        {!compact && details}
      </div>
      <span
        className={`task-act flex shrink-0 gap-1 ${t.status === 'review' || sel ? '' : 'opacity-0 group-hover:opacity-100'}`}
        onClick={(e) => e.stopPropagation()}
      >
        {t.status === 'review' && !rejecting && (
          <>
            <Btn
              tone="primary"
              kbd="Y"
              icon={<Check />}
              isPending={busy}
              onPress={accept}
            >
              Accept
            </Btn>
            <Btn
              tone="tertiary"
              icon={<ArrowUturnCcwLeft />}
              onPress={() => setRejecting(true)}
            >
              Reject
            </Btn>
          </>
        )}
        {t.status === 'todo' && (
          <Dropdown>
            <Button size="sm" variant="tertiary" className="gap-1.5">
              <Sparkles className="size-3.5" />
              Delegate
            </Button>
            <Dropdown.Popover placement="bottom end">
              <Dropdown.Menu
                aria-label="Delegate to"
                onAction={(k) =>
                  delegate(k === 'default' ? null : (k as AgentId))
                }
              >
                <Dropdown.Item id="default" textValue="Default agent">
                  Default agent
                </Dropdown.Item>
                {AGENTS.map((a) => (
                  <Dropdown.Item key={a} id={a} textValue={a}>
                    {a}
                  </Dropdown.Item>
                ))}
              </Dropdown.Menu>
            </Dropdown.Popover>
          </Dropdown>
        )}
      </span>
      <span className="flex min-w-[50px] shrink-0 items-center justify-end gap-1.5">
        {t.status === 'progress' && <Spinner />}
        {t.assignee.kind === 'agent' ? (
          <AgentTag id={t.assignee.id} />
        ) : (
          <span className="truncate text-xs text-muted" title={t.assignee.id}>
            {t.assignee.id}
          </span>
        )}
      </span>
      {compact && <div className="w-full min-w-0 basis-full">{details}</div>}
    </div>
  );
});

function useTaskKeys(order: string[]) {
  const store = useRootStore();
  const ui = useUi();
  useViewKeys(
    ui,
    (e) => {
      const i = ui.selected ? order.indexOf(ui.selected) : -1;
      const t = store.models.tasks.get(ui.selected ?? '');
      const move = (d: number) => {
        const next = order[Math.max(0, Math.min(order.length - 1, i + d))];
        if (next) ui.select(next);
      };
      const k = latinKey(e);
      if (k === 'j' || e.key === 'ArrowDown') move(1);
      else if (k === 'k' || e.key === 'ArrowUp') move(-1);
      else if (e.key === 'Enter' && t) ui.go({ name: 'review', pr: t.pr });
      else if (k === 'y' && t?.status === 'review')
        void decideTask(store, t.id, true);
      else if (k === 'a' && t) ui.openModal('assign', [t.id]);
      else return false;
    },
    [order.join()],
  );

  useEffect(() => {
    if (order.length && (!ui.selected || !order.includes(ui.selected)))
      ui.select(order[0]!);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [order.join()]);

  useEffect(() => {
    if (ui.selected)
      document
        .getElementById(`T-${ui.selected}`)
        ?.scrollIntoView({ block: 'nearest' });
  }, [ui.selected]);
}

export const Tasks = observer(function Tasks() {
  const store = useRootStore();
  const ui = useUi();
  const me = store.ephemeral.viewer?.login ?? null;
  const tabs = taskTabs(me);
  // Per workspace, in its UI snapshot.
  const taskTab = (ui.tasksTab ?? 'mine') as TaskTab;
  const setTaskTab = (t: TaskTab) => ui.setTasksTab(t);
  const tab = tabs.find((x) => x.id === taskTab) ?? tabs[0]!;
  const all = store.workspaces.scopeTasks(store.models.tasks.all());
  const items = all.filter(tab.match);
  const order = ORDER.flatMap((st) =>
    items.filter((t) => t.status === st).map((t) => t.id),
  );
  useTaskKeys(order);
  return (
    <div className="view flex min-w-0 flex-1 flex-col">
      {/* Open tasks, like the tab counts and the sidebar — done ones are
          listed but not counted. */}
      <ViewHeader
        title="Tasks"
        count={items.filter((t) => t.status !== 'done').length}
      >
        <Tabs
          data-plain
          selectedKey={taskTab}
          onSelectionChange={(k) => setTaskTab(k as TaskTab)}
          className="task-tabs"
        >
          <Tabs.ListContainer>
            <Tabs.List aria-label="Task filter">
              {tabs.map((x) => (
                <Tabs.Tab key={x.id} id={x.id} className="whitespace-nowrap">
                  {x.title}{' '}
                  <span className="ml-1 text-muted">
                    {
                      all.filter(x.match).filter((t) => t.status !== 'done')
                        .length
                    }
                  </span>
                  <Tabs.Indicator />
                </Tabs.Tab>
              ))}
            </Tabs.List>
          </Tabs.ListContainer>
        </Tabs>
      </ViewHeader>
      <ListBox
        aria-label="Tasks"
        selectionMode="single"
        selectedKeys={ui.selected ? [ui.selected] : []}
        onSelectionChange={(keys) => {
          const k = keys === 'all' ? null : [...keys][0];
          if (k !== undefined && k !== null) ui.select(String(k));
        }}
        onAction={(k: Key) => {
          const t = store.models.tasks.get(String(k));
          if (t) ui.go({ name: 'review', pr: t.pr });
        }}
        className="min-w-0 flex-1 overflow-auto p-0 pb-20"
        renderEmptyState={() => (
          <Empty
            icon={<ListCheck />}
            title="No tasks"
            text="Tasks come from skill findings, threads, and your review comments."
          />
        )}
      >
        {ORDER.filter((st) => items.some((t) => t.status === st)).map((st) => (
          <ListBox.Section key={st} id={st}>
            <Header className="flex h-10 items-center gap-2 border-b border-separator bg-surface/95 pl-[18px] pr-6 text-sm backdrop-blur">
              <TaskIcon status={st} />
              <span className="font-medium">{TITLE[st]}</span>
              <span className="tabular-nums text-muted">
                {items.filter((t) => t.status === st).length}
              </span>
              {st === 'review' && (
                <span className="text-xs font-normal text-muted">
                  an agent or the author brought a fix — review and accept it
                </span>
              )}
            </Header>
            {items
              .filter((t) => t.status === st)
              .map((t) => (
                <ListBox.Item
                  key={t.id}
                  id={t.id}
                  textValue={t.title}
                  className="min-h-0 rounded-none border-b border-separator px-6 py-0"
                >
                  <TaskRow t={t} sel={ui.selected === t.id} />
                </ListBox.Item>
              ))}
          </ListBox.Section>
        ))}
      </ListBox>
      <Hints>
        <K>J</K>
        <K>K</K> navigate <K>⏎</K> open PR <K>Y</K> accept fix <K>A</K> assign{' '}
        <K>⌘K</K> everything
      </Hints>
    </div>
  );
});
