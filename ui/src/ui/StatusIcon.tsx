import type {
  PrState,
  PrSummary,
  ReviewDecision,
  TaskStatus,
} from '../gen/contract';
import { gatesFor } from './prs/mine';

const R = 5.5;
const C = 2 * Math.PI * R;

/** Linear-style status ring: outline + pie that fills with progress. */
function Ring({
  color,
  fill = 0,
  dashed,
  check,
  cross,
  title,
}: {
  color: string;
  fill?: number;
  dashed?: boolean;
  check?: boolean;
  cross?: boolean;
  title: string;
}) {
  return (
    <svg
      className="sicon"
      width="14"
      height="14"
      viewBox="0 0 14 14"
      aria-label={title}
    >
      <title>{title}</title>
      {check ? (
        <>
          <circle cx="7" cy="7" r="6.5" fill={color} />
          <path
            d="M4.2 7.2l1.9 1.9 3.8-4"
            stroke="var(--surface)"
            strokeWidth="1.6"
            fill="none"
            strokeLinecap="round"
          />
        </>
      ) : cross ? (
        <>
          <circle cx="7" cy="7" r="6.5" fill={color} />
          <path
            d="M4.7 4.7l4.6 4.6M9.3 4.7l-4.6 4.6"
            stroke="var(--surface)"
            strokeWidth="1.5"
            strokeLinecap="round"
          />
        </>
      ) : (
        <>
          <circle
            cx="7"
            cy="7"
            r="6"
            fill="none"
            stroke={color}
            strokeWidth="1.5"
            strokeDasharray={dashed ? '1.6 1.9' : undefined}
          />
          {fill > 0 && (
            <circle
              cx="7"
              cy="7"
              r={R / 2}
              fill="none"
              stroke={color}
              strokeWidth={R}
              strokeDasharray={`${(C / 2) * fill} ${C}`}
              transform="rotate(-90 7 7)"
            />
          )}
        </>
      )}
    </svg>
  );
}

export const prStatusLabel: Record<PrState, string> = {
  draft: 'Draft',
  open: 'Open',
  merged: 'Merged',
  closed: 'Closed',
};

export function PRIcon({
  status,
  progress = 0,
}: {
  status: PrState;
  progress?: number;
}) {
  const t = prStatusLabel[status];
  switch (status) {
    case 'draft':
      return <Ring color="var(--muted)" dashed title={t} />;
    case 'open':
      return (
        <Ring
          color="var(--accent)"
          fill={progress}
          title={`${t}${progress ? ` · read ${Math.round(progress * 100)}%` : ''}`}
        />
      );
    case 'merged':
      return <Ring color="var(--accent)" check title={t} />;
    case 'closed':
      return <Ring color="var(--muted)" cross title={t} />;
  }
}

export const reviewDecisionLabel: Record<ReviewDecision, string> = {
  approved: 'Approved',
  changes_requested: 'Changes requested',
  review_required: 'Review required',
};

/** `PrSummary.review_decision` — approved / changes requested / review
 *  required, the way the prototype's `PRIcon` folds review state into its
 *  status ring. */
export function ReviewDecisionIcon({ decision }: { decision: ReviewDecision }) {
  const t = reviewDecisionLabel[decision];
  switch (decision) {
    case 'approved':
      return <Ring color="var(--success)" check title={t} />;
    case 'changes_requested':
      return <Ring color="var(--danger)" cross title={t} />;
    case 'review_required':
      return <Ring color="var(--muted)" title={t} />;
  }
}

/** `PrSummary.conflicts` — merge conflicts with the base branch. */
export function ConflictsIcon() {
  return (
    <Ring color="var(--danger)" cross title="Conflicts with base branch" />
  );
}

export const taskLabel: Record<TaskStatus, string> = {
  todo: 'Todo',
  progress: 'In progress',
  review: 'To verify',
  done: 'Done',
};

export function TaskIcon({ status }: { status: TaskStatus }) {
  const t = taskLabel[status];
  if (status === 'todo') return <Ring color="var(--muted)" title={t} />;
  if (status === 'progress')
    return <Ring color="var(--warning)" fill={0.5} title={t} />;
  if (status === 'review')
    return <Ring color="var(--accent)" fill={0.75} title={t} />;
  return <Ring color="var(--accent)" check title={t} />;
}

/** What is wrong with an open or draft PR, most urgent first: failing
 *  checks, conflicts with its base, changes requested. Empty when it's fine. */
export function prProblems(
  p: Pick<
    PrSummary,
    'state' | 'checks' | 'conflicts' | 'review_decision' | 'base_ref'
  >,
): string[] {
  if (p.state !== 'open' && p.state !== 'draft') return [];
  const out: string[] = [];
  if (p.checks === 'fail') out.push('Checks failing');
  if (p.conflicts) out.push(`Conflicts with ${p.base_ref}`);
  if (p.review_decision === 'changes_requested') out.push('Changes requested');
  return out;
}

export type PrTone =
  'problem' | 'pending' | 'ready' | 'draft' | 'open' | 'merged' | 'closed';

/** Where a PR stands, in one word and one tone — what the status mark and
 *  the status chip both show. Draft comes first (it isn't up for merging
 *  yet, whatever else is wrong), then a problem over everything else. */
export function prStatus(p: PrSummary): { tone: PrTone; label: string } {
  if (p.state === 'merged') return { tone: 'merged', label: 'Merged' };
  if (p.state === 'closed') return { tone: 'closed', label: 'Closed' };
  const problems = prProblems(p);
  if (p.state === 'draft')
    return { tone: 'draft', label: ['Draft', ...problems].join(' · ') };
  if (problems.length) return { tone: 'problem', label: problems.join(' · ') };
  if (p.checks === 'pending')
    return { tone: 'pending', label: 'Checks running' };
  // Same rule as the "Ready to merge" group in Mine: every gate passes.
  if (gatesFor(p).every((g) => g.ok)) {
    return { tone: 'ready', label: 'Ready to merge' };
  }
  return { tone: 'open', label: 'Open' };
}

/** The one status mark for a PR everywhere it shows: a red cross while
 *  something is wrong, otherwise a ring coloured by `prStatus` and filled
 *  with how much of it you've read. */
export function PrStatusIcon({
  summary,
  progress = 0,
}: {
  summary: PrSummary;
  progress?: number;
}) {
  const { tone, label } = prStatus(summary);
  const read = progress ? ` · read ${Math.round(progress * 100)}%` : '';
  switch (tone) {
    case 'problem':
      return <Ring color="var(--danger)" cross title={label} />;
    case 'merged':
      return <Ring color="var(--accent)" check title={label} />;
    case 'closed':
      return <Ring color="var(--muted)" cross title={label} />;
    case 'draft':
      return (
        <Ring
          color="var(--muted)"
          dashed
          fill={progress}
          title={label + read}
        />
      );
    case 'pending':
      return (
        <Ring color="var(--warning)" fill={progress} title={label + read} />
      );
    case 'ready':
      return (
        <Ring color="var(--success)" fill={progress} title={label + read} />
      );
    case 'open':
      return (
        <Ring color="var(--accent)" fill={progress} title={label + read} />
      );
  }
}
