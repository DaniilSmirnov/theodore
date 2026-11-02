/**
 * The agent's plan for the running turn, above the composer: the steps it
 * last sent with `update_progress` (docs/spaces.md "Tools the agent
 * gets"). Gone once the turn ends; the core clears it too.
 */
import { CircleCheck } from '@gravity-ui/icons';
import type { Space, SpaceProgressStep } from '../../gen/contract';
import { Spinner } from '../bits';

/** The tracker to show: only while a turn runs, only with steps. */
export function shownProgress(space: Space) {
  const p = space.progress;
  if (space.status !== 'running' || !p || p.steps.length === 0) return null;
  return p;
}

function StepMark({ state }: { state: SpaceProgressStep['state'] }) {
  if (state === 'done') return <CircleCheck className="size-3 text-success" />;
  if (state === 'active') return <Spinner />;
  return (
    <span className="inline-block size-2 rounded-full border border-separator" />
  );
}

export function SpaceProgress({ space }: { space: Space }) {
  const p = shownProgress(space);
  if (!p) return null;
  const done = p.steps.filter((s) => s.state === 'done').length;
  return (
    <div
      className="space-progress flex flex-col gap-1 rounded-lg border border-separator px-3 py-2 text-xs"
      aria-label="Agent progress"
    >
      <span className="text-muted">
        {done} of {p.steps.length} done
        {p.note ? ` · ${p.note}` : ''}
      </span>
      <ol className="flex flex-col gap-0.5">
        {p.steps.map((s, i) => (
          <li
            key={i}
            data-state={s.state}
            className={`flex items-center gap-2 ${s.state === 'pending' ? 'text-muted' : ''} ${s.state === 'done' ? 'line-through opacity-70' : ''}`}
          >
            <span className="inline-flex size-3 items-center justify-center">
              <StepMark state={s.state} />
            </span>
            <span className="truncate">{s.label}</span>
          </li>
        ))}
      </ol>
    </div>
  );
}
