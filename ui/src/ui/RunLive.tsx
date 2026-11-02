import { useEffect, useState, type ReactNode, type RefObject } from 'react';
import { observer } from 'mobx-react-lite';
import { useRootStore } from '../stores/context';
import { Md } from '../highlight';
import { ToolCalls } from './ToolCalls';

/** A run's text as it streams in. Its own observer: a piece arriving
 *  re-renders this, not the chat around it. `follow` is scrolled into view
 *  as the text grows. */
export const LiveText = observer(function LiveText({
  run,
  fallback = null,
  follow,
}: {
  run: string;
  /** Shown until the first piece arrives. */
  fallback?: ReactNode;
  follow?: RefObject<HTMLElement | null>;
}) {
  const store = useRootStore();
  const text = store.ephemeral.agentText.get(run);
  useEffect(() => {
    if (text) follow?.current?.scrollIntoView?.({ block: 'end' });
  }, [text, follow]);
  if (!text) return <>{fallback}</>;
  return (
    <div className="live-text text-sm leading-relaxed">
      <Md text={text} />
    </div>
  );
});

/** The last `n` non-blank lines of `text`. */
export function tailLines(text: string, n: number): string {
  return text
    .split('\n')
    .filter((l) => l.trim())
    .slice(-n)
    .join('\n');
}

const TAIL = 3;

/** A task an agent is fixing: the last lines it wrote and its steps, both
 *  folded — the whole text opens on a click, the steps behind their own
 *  toggle. Nothing until the run shows something. */
export const TaskLive = observer(function TaskLive({ task }: { task: string }) {
  const store = useRootStore();
  const [open, setOpen] = useState(false);
  const run = store.ephemeral.taskRuns.get(task);
  if (!run) return null;
  const text = store.ephemeral.agentText.get(run) ?? '';
  const tools = store.ephemeral.runLog.get(run) ?? [];
  const stalled = store.ephemeral.stalled.has(run);
  if (!text.trim() && tools.length === 0 && !stalled) return null;
  return (
    <div
      className="task-live mt-1.5 flex max-w-[720px] flex-col gap-1.5"
      onClick={(e) => e.stopPropagation()}
    >
      {text.trim() && (
        <button
          type="button"
          aria-expanded={open}
          aria-label={open ? 'Show less' : 'Show all the agent wrote'}
          className={`task-live-text whitespace-pre-wrap text-left font-mono text-xs text-muted [overflow-wrap:anywhere] ${open ? 'max-h-64 overflow-auto' : 'line-clamp-3'}`}
          onClick={() => setOpen((o) => !o)}
        >
          {open ? text.trim() : tailLines(text, TAIL)}
        </button>
      )}
      {stalled && (
        <span className="text-xs text-warning">
          No output in a while — the agent may be stuck.
        </span>
      )}
      {tools.length > 0 && <ToolCalls tools={tools} running turn={run} />}
    </div>
  );
});
