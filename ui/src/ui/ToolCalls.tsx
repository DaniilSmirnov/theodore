import { useState } from 'react';
import { CircleCheck } from '@gravity-ui/icons';
import { Disclosure } from '@heroui/react';
import { Spinner } from './bits';
import { runningLine, summarizeTools, times, usedLine } from './toolSummary';

const plural = (n: number, one: string, few: string, many: string) =>
  `${n} ${n === 1 ? one : n >= 2 && n <= 4 ? few : many}`;

/** Whether a turn's step list is open, by run id — kept across the remount
 *  when a running turn becomes a finished one. */
const stepsOpen = new Map<string, boolean>();

/** A turn's tool calls, folded: while the agent works, only the latest step
 *  and a count; once it's done, one `Used Bash ×12, Edit ×4` line. The full
 *  list — back-to-back calls of one tool grouped — opens behind
 *  `Show N steps`. */
export function ToolCalls({
  tools,
  running,
  turn,
}: {
  tools: readonly string[];
  running: boolean;
  /** Run id; keeps the list's open state for this turn. */
  turn?: string;
}) {
  const [open, setOpen] = useState(() =>
    turn ? (stepsOpen.get(turn) ?? false) : false,
  );
  const toggle = (next: boolean) => {
    if (turn) stepsOpen.set(turn, next);
    setOpen(next);
  };
  const s = summarizeTools(tools);
  const head = running
    ? s.latest
      ? runningLine(s.latest)
      : 'Working…'
    : usedLine(s) || 'Done';
  const count = s.steps
    ? plural(s.steps, 'step', 'steps', 'steps')
    : plural(s.groups.length, 'line', 'lines', 'lines');
  return (
    <Disclosure
      isExpanded={open}
      onExpandedChange={toggle}
      className="rounded-2xl border border-separator"
    >
      <Disclosure.Heading>
        <Disclosure.Trigger className="flex w-full items-center gap-2 px-3 py-2 text-left text-xs">
          {running ? (
            <Spinner />
          ) : (
            <CircleCheck className="size-3.5 shrink-0 text-success" />
          )}
          <span className="min-w-0 flex-1 truncate font-medium" title={head}>
            {head}
          </span>
          <span className="shrink-0 text-muted">
            {open ? 'Hide' : 'Show'} {count}
          </span>
          <Disclosure.Indicator />
        </Disclosure.Trigger>
      </Disclosure.Heading>
      <Disclosure.Content>
        <Disclosure.Body className="runlog flex flex-col gap-1 px-3 pb-2.5 pt-0 font-mono text-xs text-muted">
          {s.groups.map((g, i) =>
            g.kind === 'note' ? (
              <span key={i} className="italic">
                {g.text}
              </span>
            ) : (
              <span key={i} className="flex min-w-0 gap-1.5">
                <span className="shrink-0 text-foreground">
                  {times(g.tool, g.count)}
                </span>
                {g.target && (
                  <span className="truncate" title={g.target}>
                    · {g.target}
                  </span>
                )}
                {g.failed > 0 && (
                  <span className="shrink-0 text-danger">
                    · {g.failed} failed
                  </span>
                )}
              </span>
            ),
          )}
        </Disclosure.Body>
      </Disclosure.Content>
    </Disclosure>
  );
}
