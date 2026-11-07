/**
 * A space turn as it happened: its text, tool calls, subagents and thinking,
 * oldest first. What folds is `turnRows.ts`; this draws it.
 *
 * ```text
 * running   rows growing, a spinner on each call still going
 *           12s                                ticks without a render
 *              │ the reply lands
 *              ▼
 * finished  ▸ 3 tool calls, 5 messages  >_ ⌕   opens in place
 *           ✗ Run  cargo test                  always a row
 *           the reply
 *           1m 12s · 3:37 PM  copy vote  RS a.rs +3 −1  +2 more
 * ```
 *
 * Opening anything is instant and what you clicked stays where it is. What's
 * closed isn't rendered.
 */
import {
  memo,
  useEffect,
  useMemo,
  useRef,
  type ComponentType,
  type ReactNode,
} from 'react';
import {
  Bulb,
  ChevronRight,
  CircleXmarkFill,
  Cpu,
  FileText,
  LogoMcp,
  Magnifier,
  Pencil,
  Terminal,
  Wrench,
} from '@gravity-ui/icons';
import { observer } from 'mobx-react-lite';
import type { AgentId, TurnFile, TurnStep } from '../gen/contract';
import { useRootStore } from '../stores/context';
import { Md } from '../highlight';
import { Spinner } from './bits';
import { FileTypeIcon } from './FileTypeIcon';
import { useKeptOpen, useSteadyChange } from './rowState';
import { toolRow, type ToolKind } from './toolSummary';
import {
  fileChips,
  foldLine,
  groupLabel,
  splitSteps,
  stepFailed,
  toolView,
  turnItems,
  type FoldOptions,
  type ToolStep,
  type TurnItem,
} from './turnRows';

const KIND_ICON: Record<ToolKind, ComponentType<{ className?: string }>> = {
  read: FileText,
  edit: Pencil,
  run: Terminal,
  search: Magnifier,
  agent: Cpu,
  mcp: LogoMcp,
  other: Wrench,
};

const NO_STEPS: readonly TurnStep[] = [];
const ROW = 'flex min-w-0 items-center gap-1.5 text-[13px]';

/** `<1s`, `42s`, `1m 12s`, `1h 3m`. */
export function formatDuration(ms: number): string {
  if (ms < 1000) return '<1s';
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${s % 60}s`;
  return `${Math.floor(m / 60)}h ${m % 60}m`;
}

function StepIcon({ busy, children }: { busy: boolean; children: ReactNode }) {
  return (
    <span className="grid size-4 shrink-0 place-items-center text-muted">
      {busy ? <Spinner /> : children}
    </span>
  );
}

function Chevron({ open }: { open: boolean }) {
  return (
    <ChevronRight
      className={`size-3.5 shrink-0 text-muted ${open ? 'rotate-90' : ''}`}
    />
  );
}

function Failed() {
  return (
    <span className="turn-tool-failed inline-flex shrink-0 items-center gap-1 text-xs text-danger">
      <CircleXmarkFill className="size-3.5" />
      failed
    </span>
  );
}

function Chip({ text, file = false }: { text: string; file?: boolean }) {
  return (
    <span
      title={text}
      className="turn-chip inline-flex min-w-0 items-center gap-1 rounded bg-default px-1.5 py-0.5 font-mono text-[11px] text-muted"
    >
      {file && <FileTypeIcon path={text} />}
      <span className="min-w-0 truncate">{text}</span>
    </span>
  );
}

function LineCounts({
  added,
  removed,
}: {
  added: number | null | undefined;
  removed: number | null | undefined;
}) {
  return (
    <>
      {added != null && (
        <span className="turn-added shrink-0 font-mono text-[11px] text-success">
          +{added}
        </span>
      )}
      {removed != null && (
        <span className="turn-removed shrink-0 font-mono text-[11px] text-danger">
          −{removed}
        </span>
      )}
    </>
  );
}

const ToolRow = memo(function ToolRow({
  step,
  running,
}: {
  step: ToolStep;
  running: boolean;
}) {
  const view = toolView(step, running);
  const Icon = KIND_ICON[view.kind];
  return (
    <div data-kind={view.kind} className={`turn-tool ${ROW}`}>
      <StepIcon busy={running && step.ok == null}>
        <Icon className="size-3.5" />
      </StepIcon>
      <span title={view.label} className="turn-tool-label min-w-0 truncate">
        {view.label}
      </span>
      {view.chip && <Chip text={view.chip} file={view.isFile} />}
      {view.note && (
        <span className="turn-tool-note shrink-0 text-xs text-muted">
          {view.note}
        </span>
      )}
      <LineCounts added={view.added} removed={view.removed} />
      {stepFailed(step) && <Failed />}
    </div>
  );
});

/** Consecutive calls of one read-only tool or one MCP server. */
const GroupRow = memo(function GroupRow({
  steps,
  keptAs,
}: {
  steps: ToolStep[];
  keptAs: string | null;
}) {
  const [open, setOpen] = useKeptOpen(keptAs);
  const steady = useSteadyChange();
  const first = steps[0]!;
  const Icon = KIND_ICON[toolView(first).kind];
  return (
    <div className="turn-group min-w-0">
      <button
        type="button"
        aria-expanded={open}
        className={`turn-group-line w-full text-left ${ROW}`}
        onClick={() => steady(() => setOpen(!open))}
      >
        <StepIcon busy={false}>
          <Icon className="size-3.5" />
        </StepIcon>
        <span className="min-w-0 truncate">
          {groupLabel(first, steps.length)}
        </span>
        <Chevron open={open} />
      </button>
      {open && (
        <div className="mt-2 flex flex-col gap-2 ps-5">
          {steps.map((step, i) => (
            <ToolRow key={i} step={step} running={false} />
          ))}
        </div>
      )}
    </div>
  );
});

const ThinkingRow = memo(function ThinkingRow({
  text,
  busy,
}: {
  text: string;
  busy: boolean;
}) {
  // By its words: the same row while it runs and once it is stored.
  const [open, setOpen] = useKeptOpen(`thinking:${hash(text)}`);
  const steady = useSteadyChange();
  return (
    <div className="turn-thinking min-w-0">
      <button
        type="button"
        aria-expanded={open}
        className={`turn-thinking-line w-full text-left ${ROW}`}
        onClick={() => steady(() => setOpen(!open))}
      >
        <StepIcon busy={busy}>
          <Bulb className="size-3.5" />
        </StepIcon>
        <span className="shrink-0">Thinking</span>
        {open ? (
          <Chevron open />
        ) : (
          <span className="turn-thinking-text inline-flex min-w-0 rounded bg-default px-1.5 py-0.5 text-[11px] text-muted">
            <span className="min-w-0 truncate">
              {text.replace(/\s+/g, ' ')}
            </span>
          </span>
        )}
      </button>
      {open && (
        <div className="turn-thinking-body mt-1 ps-5 text-xs leading-relaxed text-muted">
          <Md text={text} />
        </div>
      )}
    </div>
  );
});

/** A cheap key for a thought's open state. */
function hash(text: string): string {
  let h = 5381;
  for (let i = 0; i < text.length; i += 1) {
    h = (h * 33) ^ text.charCodeAt(i);
  }
  return `${text.length}:${(h >>> 0).toString(36)}`;
}

const TextRow = memo(function TextRow({ text }: { text: string }) {
  return (
    <div className="turn-text text-sm leading-relaxed">
      <Md text={text} />
    </div>
  );
});

/** What a running subagent is on, in a few words. */
function latestOf(steps: readonly TurnStep[]): string | null {
  const last = steps.at(-1);
  if (!last) return null;
  if (last.kind === 'tool') {
    const view = toolView(last, true);
    return view.chip ? `${view.label} ${view.chip}` : view.label;
  }
  return last.kind === 'thinking' ? 'Thinking' : last.text.replace(/\s+/g, ' ');
}

/** A subagent: its description, and when opened its own steps. While it runs
 *  the newest of them shows beside it. */
const AgentRow = memo(
  function AgentRow({
    step,
    own,
    running,
    keptAs,
  }: {
    step: ToolStep;
    own: readonly TurnStep[];
    running: boolean;
    keptAs: string | null;
  }) {
    const [open, setOpen] = useKeptOpen(
      step.id ? `agent:${step.id}` : keptAs && `agent:${keptAs}`,
    );
    const steady = useSteadyChange();
    // A background launch answers at once, its report comes later or never.
    const waiting = !!step.background && !step.summary;
    const busy = running && (step.ok == null || waiting);
    const view = toolView(step, running);
    const hasMore = own.length > 0 || !!step.summary;
    const beside = busy
      ? (latestOf(own) ?? (waiting ? 'running in background' : null))
      : (step.summary ?? (step.background ? 'ran in background' : null));
    const line = (
      <>
        <StepIcon busy={busy}>
          <Cpu className="size-3.5" />
        </StepIcon>
        <span title={view.label} className="min-w-0 shrink-0 truncate">
          {view.label}
        </span>
        {hasMore && <Chevron open={open} />}
        {beside && !open && (
          <span
            title={beside}
            className="turn-agent-note min-w-0 truncate text-xs text-muted"
          >
            {beside}
          </span>
        )}
        {stepFailed(step) && <Failed />}
      </>
    );
    return (
      <div data-kind="agent" className="turn-agent min-w-0">
        {hasMore ? (
          <button
            type="button"
            aria-expanded={open}
            className={`turn-agent-line w-full text-left ${ROW}`}
            onClick={() => steady(() => setOpen(!open))}
          >
            {line}
          </button>
        ) : (
          <div className={`turn-agent-line ${ROW}`}>{line}</div>
        )}
        {open && (
          <div className="turn-agent-steps mt-2 flex flex-col gap-2 border-l border-separator ps-3">
            <StepList
              own={own}
              under={NOTHING_UNDER}
              options={busy ? { running: true } : { flat: true }}
              keptAs={keptAs && `${keptAs}:a`}
              running={busy}
            />
            {!busy && step.summary && (
              <div className="turn-agent-summary text-xs text-muted">
                {step.summary}
              </div>
            )}
          </div>
        )}
      </div>
    );
  },
  (a, b) =>
    a.step === b.step &&
    a.running === b.running &&
    a.keptAs === b.keptAs &&
    a.own.length === b.own.length &&
    a.own.every((s, i) => s === b.own[i]),
);

const NOTHING_UNDER = new Map<string, TurnStep[]>();

/** Rows that fold away: muted, no border, chevron on the left, then how much
 *  is in it and the kinds of tools. */
const FoldRow = memo(function FoldRow({
  item,
  under,
  keptAs,
}: {
  item: Extract<TurnItem, { kind: 'fold' }>;
  under: ReadonlyMap<string, TurnStep[]>;
  keptAs: string | null;
}) {
  const [open, setOpen] = useKeptOpen(keptAs && `fold:${keptAs}:${item.index}`);
  const steady = useSteadyChange();
  return (
    <div className="turn-fold min-w-0">
      <button
        type="button"
        aria-expanded={open}
        className={`turn-fold-line w-fit max-w-full text-left text-muted ${ROW}`}
        onClick={() => steady(() => setOpen(!open))}
      >
        <Chevron open={open} />
        <span className="min-w-0 truncate">{foldLine(item.counts)}</span>
        {item.icons.map((kind) => {
          const Icon = KIND_ICON[kind];
          return (
            <span key={kind} data-icon={kind} className="shrink-0">
              <Icon className="size-3.5" />
            </span>
          );
        })}
      </button>
      {/* A turn can hold hundreds of rows: none of them while folded. */}
      {open && (
        <div className="turn-fold-rows mt-2 flex flex-col gap-2">
          <Items
            items={item.items}
            under={under}
            keptAs={keptAs}
            running={false}
            newest={-1}
          />
        </div>
      )}
    </div>
  );
});

function Items({
  items,
  under,
  keptAs,
  running,
  newest,
}: {
  items: readonly TurnItem[];
  under: ReadonlyMap<string, TurnStep[]>;
  keptAs: string | null;
  running: boolean;
  /** The newest row's index: it is what thinking is still on. */
  newest: number;
}) {
  return (
    <>
      {items.map((item) => {
        if (item.kind === 'fold') {
          return (
            <FoldRow
              key={`f${item.index}`}
              item={item}
              under={under}
              keptAs={keptAs}
            />
          );
        }
        if (item.kind === 'group') {
          return (
            <GroupRow
              key={`g${item.index}`}
              steps={item.steps}
              keptAs={keptAs && `group:${keptAs}:${item.index}`}
            />
          );
        }
        const { step, index } = item;
        if (step.kind === 'text') {
          return <TextRow key={index} text={step.text} />;
        }
        if (step.kind === 'thinking') {
          return (
            <ThinkingRow
              key={index}
              text={step.text}
              busy={running && index === newest}
            />
          );
        }
        if (toolRow(step.name).kind === 'agent') {
          return (
            <AgentRow
              key={index}
              step={step}
              own={(step.id && under.get(step.id)) || NO_STEPS}
              running={running}
              keptAs={keptAs && `${keptAs}:${index}`}
            />
          );
        }
        return <ToolRow key={index} step={step} running={running} />;
      })}
    </>
  );
}

function StepList({
  own,
  under,
  options,
  keptAs,
  running,
}: {
  own: readonly TurnStep[];
  under: ReadonlyMap<string, TurnStep[]>;
  options: FoldOptions;
  keptAs: string | null;
  running: boolean;
}) {
  // Its fields, not its identity: a new object is the same options.
  const { running: live, flat, pinLastText, keepText } = options;
  const items = useMemo(
    () => turnItems(own, { running: live, flat, pinLastText, keepText }),
    [own, live, flat, pinLastText, keepText],
  );
  return (
    <Items
      items={items}
      under={under}
      keptAs={keptAs}
      running={running}
      newest={own.length - 1}
    />
  );
}

/** The rows. `running`: a call with no result yet spins, nothing folds. */
export function TimelineRows({
  steps,
  running = false,
  keptAs = null,
  pinLastText = false,
  keepText = false,
}: {
  steps: readonly TurnStep[];
  running?: boolean;
  /** A stored reply's id: what's open in here outlives being scrolled away. */
  keptAs?: string | null;
  pinLastText?: boolean;
  keepText?: boolean;
}) {
  const { own, under } = useMemo(() => splitSteps(steps), [steps]);
  return (
    <div className="turn-timeline flex min-w-0 flex-col gap-2">
      <StepList
        own={own}
        under={under}
        options={{ running, pinLastText, keepText }}
        keptAs={keptAs}
        running={running}
      />
    </div>
  );
}

/** A running turn's rows as they arrive. Its own observer: a piece of text
 *  re-renders this, not the chat around it. */
export const LiveTimeline = observer(function LiveTimeline({
  run,
  fallback = null,
}: {
  run: string;
  /** Shown until the first step arrives. */
  fallback?: ReactNode;
}) {
  const store = useRootStore();
  const steps = store.ephemeral.runSteps.get(run);
  if (!steps?.length) return <>{fallback}</>;
  return <TimelineRows steps={steps} running />;
});

/** When a timer with nothing to go by first showed, per run: being scrolled
 *  away doesn't start it over. */
const firstSeen = new Map<string, number>();

/** Elapsed time under a running turn. Written to the node once a second: the
 *  rows above it never hear about it. */
export const LiveTimer = memo(function LiveTimer({
  run,
  since,
}: {
  run: string | null;
  /** When the turn started: your message's time. */
  since?: number;
}) {
  const node = useRef<HTMLSpanElement>(null);
  useEffect(() => {
    let start = since;
    if (start == null) {
      start = firstSeen.get(run ?? '') ?? Date.now();
      firstSeen.set(run ?? '', start);
    }
    const write = () => {
      if (node.current) {
        const s = Math.max(0, Math.floor((Date.now() - start) / 1000));
        node.current.textContent = s < 60 ? `${s}s` : formatDuration(s * 1000);
      }
    };
    write();
    const timer = setInterval(write, 1000);
    return () => clearInterval(timer);
  }, [run, since]);
  return (
    <span
      ref={node}
      className="turn-timer font-mono text-xs tabular-nums text-muted"
    />
  );
});

/** A finished turn's rows above its reply: `turnRows.ts` says what folds. */
export function TurnActivity({
  reply,
  steps,
  pinLastText = false,
  keepText = false,
}: {
  /** The reply's message id. */
  reply: string;
  steps: readonly TurnStep[];
  /** No reply text comes after these. */
  pinLastText?: boolean;
  /** It stopped or failed: all it wrote stays in view. */
  keepText?: boolean;
}) {
  return (
    <div className="turn-activity">
      <TimelineRows
        steps={steps}
        keptAs={reply}
        pinLastText={pinLastText}
        keepText={keepText}
      />
    </div>
  );
}

const clock = (at: number) =>
  new Date(at).toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' });

/** Beside a finished reply's actions: how long its turn ran, and when it
 *  ended. On hover, the model and start → end. */
export function TurnFooter({
  at,
  durationMs,
  model,
  agent,
}: {
  at: number;
  durationMs: number;
  model?: string | null;
  agent?: AgentId | null;
}) {
  const who = model ?? agent;
  const span = `${clock(at - durationMs)} → ${clock(at)}`;
  return (
    <span
      className="turn-footer shrink-0 text-xs text-muted"
      title={who ? `${who} · ${span}` : span}
    >
      {formatDuration(durationMs)} · {clock(at)}
    </span>
  );
}

const CHIP =
  'inline-flex h-6 min-w-0 items-center gap-1 rounded-md border border-separator px-1.5 text-xs text-muted';

/** The files a turn changed. A chip opens the Changes pane on its file;
 *  `+N more` opens the pane. */
export const TurnFiles = memo(function TurnFiles({
  files,
  onOpen,
}: {
  files: readonly TurnFile[];
  onOpen?: (path: string | null) => void;
}) {
  const { shown, rest } = fileChips(files);
  return (
    <div className="turn-files flex min-w-0 flex-wrap items-center gap-1.5">
      {shown.map((f) => (
        <button
          key={f.path}
          type="button"
          title={f.path}
          data-path={f.path}
          className={`turn-file ${CHIP}`}
          onClick={() => onOpen?.(f.path)}
        >
          <FileTypeIcon path={f.path} />
          <span className="min-w-0 max-w-[180px] truncate">
            {f.path.split('/').pop()}
          </span>
          <LineCounts added={f.additions} removed={f.deletions} />
        </button>
      ))}
      {rest && (
        <button
          type="button"
          className={`turn-files-more ${CHIP}`}
          onClick={() => onOpen?.(null)}
        >
          <span>+{rest.files} more</span>
          <LineCounts added={rest.additions} removed={rest.deletions} />
        </button>
      )}
    </div>
  );
});
