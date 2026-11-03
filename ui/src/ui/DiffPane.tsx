import { openExternal } from '../core/openExternal';
import { useElementWidth } from './useElementWidth';
import { MarkdownDiff } from './MarkdownDiff';
import { isMarkdown } from './mdDiff';
import { ImageDiffCard } from './media/ImageDiffCard';
import { isImageDiffPath } from './media/imageDiff';
import {
  createContext,
  memo,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from 'react';
import {
  ArrowRight,
  Check,
  CircleExclamation,
  ChevronDown,
  ChevronRight,
  ChevronsDown,
  ChevronsUp,
  CircleCheckFill,
  Eye,
  FileCode,
  Plus,
  Sparkles,
  TriangleExclamation,
  Xmark,
} from '@gravity-ui/icons';
import {
  Alert,
  Button,
  Card,
  Checkbox,
  Chip,
  ToggleButton,
  ToggleButtonGroup,
  Tooltip,
  Typography,
} from '@heroui/react';
import { observer } from 'mobx-react-lite';
import {
  Code,
  hunkLineTokens,
  langOf,
  rich,
  tokenizeLines,
  useShikiVersion,
  type Tok,
} from '../highlight';
import { api } from '../core/api';
import { countRender } from '../core/perf';
import type {
  Anchor,
  DiffLine,
  FileDiff,
  Hunk,
  HunkId,
  InterdiffEntry,
  PrId,
  Revision,
  Severity,
  Side,
} from '../gen/contract';
import {
  askAboutLine,
  askPending,
  dismissLineAsk,
  markGroupRead,
  markHunkRead,
  openLineAnswer,
  referenceLine,
  setFileViewed,
} from '../actions';
import { useRootStore, useUi } from '../stores/context';
import { lineAskKey, type LineAsk } from '../stores/EphemeralStore';
import type { ModelRows } from '../sync/types';
import { Btn, Spinner } from './bits';
import { Composer, DraftView, plural, ThreadView } from './Comments';
import { contextMenuHandler, type ContextMenuItem } from './ContextMenu';
import {
  gapsAbove,
  gapTail,
  sinceCountsFor,
  splitRows,
  wordMarks,
  type Gap,
  type SinceKind,
} from './diff';
import { groupsForRevision, type ReviewMode } from './GroupsPane';
import {
  useCheckboxRange,
  useCheckboxRangeEvents,
  type CheckboxRangeModifiers,
} from './useCheckboxRange';
import { nextReviewableGroup } from './review/groups';
import { diffFiles, isCollapsed, showsOnlyOutliers } from './review/hunks';
import { InterdiffPanel } from './review/InterdiffPanel';
import { interdiffByHunk, interdiffView } from './review/interdiff';
import { SymbolPeek, type SymbolPeekTarget } from './SymbolPeek';
import { CodeHoverHost } from './symbols/CodeHoverHost';
import { identifierAt } from './symbols/extract';

export interface ComposerState {
  path: string;
  side: Side;
  line: number;
  /** Set when the comment covers a range; `line` is always the range's end. */
  startLine?: number;
}

/** Exported so `Review`'s `C` shortcut can open the composer on the focused
 *  hunk's first changed line the same way a mouse click does. */
export function lineAnchor(
  rev: Revision['head'],
  path: string,
  l: DiffLine,
  startLine?: number,
): Anchor | null {
  if (l.kind === 'del') {
    if (l.old_no == null) return null;
    return {
      rev,
      path,
      side: 'old',
      line: l.old_no,
      start_line: startLine ?? null,
      start_side: startLine != null ? 'old' : null,
    };
  }
  if (l.new_no == null) return null;
  return {
    rev,
    path,
    side: 'new',
    line: l.new_no,
    start_line: startLine ?? null,
    start_side: startLine != null ? 'new' : null,
  };
}

/** Lines `lines X–Y`, from the hunk's new-side range (deletions-only hunks
 *  have no new lines to anchor on). */
function hunkRange(h: Hunk): string {
  if (h.new_lines <= 0) return 'deletions only';
  return `lines ${h.new_start}–${h.new_start + h.new_lines - 1}`;
}

/** The identifier under the pointer, from the text node the browser hit. */
function symbolAt(e: React.MouseEvent): string | null {
  if ((e.target as HTMLElement).closest('.ln-act, .no, .mark')) return null;
  const range = document.caretRangeFromPoint?.(e.clientX, e.clientY);
  const node = range?.startContainer;
  if (!range || !node || node.nodeType !== 3) return null;
  return identifierAt(node.textContent ?? '', range.startOffset);
}

const sevBg: Record<Severity, string> = {
  high: 'bg-danger',
  warn: 'bg-warning',
  info: 'bg-accent',
};

const CHUNK = 40;
const STEP = 20;
/** Below this, side-by-side halves are too narrow to read — fall back to
 *  unified even if the user asked for split. */
const SPLIT_MIN_WIDTH = 760;

/** The diff's scroll container — the root files measure their distance to. */
/** Set by a file jump just before it changes the view, so the view change
 *  doesn't reset the scroll the jump is about to set. */
export const pendingJump = { current: false };

const ScrollRoot = createContext<{ current: HTMLElement | null }>({
  current: null,
});

/** A file's hunks mount once it is within ~1.5 viewports of the visible
 *  part of the diff, and stay mounted after that. */
const MOUNT_MARGIN = '150% 0px';

/** Files at the top of the list mount right away, up to this many viewports
 *  of estimated height — no placeholder flash on open or group switch. */
const EAGER_VIEWPORTS = 2.5;

// Rendered heights from styles.css / the classes below, for placeholders.
const LINE_PX = 20;
const HUNK_HEAD_PX = 33;
const GAP_PX = 41;
const FILE_HEAD_PX = 61;

/** Estimated height of a file's hunks, so an unmounted file's placeholder
 *  keeps the scrollbar and everything below it roughly where they'll be. */
function estimateBodyHeight(file: FileDiff, split: boolean): number {
  const gaps = gapsAbove(file.hunks);
  let px = file.hunks.length ? GAP_PX : 0;
  for (const h of file.hunks) {
    const rows = split ? splitRows(h.lines).length : h.lines.length;
    px += HUNK_HEAD_PX + LINE_PX * rows + (gaps.has(h.id) ? GAP_PX : 0);
  }
  return px;
}

/** How many files from the top fit in `EAGER_VIEWPORTS` viewports. */
function eagerCount(files: FileDiff[], split: boolean): number {
  const budget =
    EAGER_VIEWPORTS *
    (typeof window !== 'undefined' ? window.innerHeight : 800);
  let px = 0;
  let n = 0;
  for (const f of files) {
    if (px > budget) break;
    px += FILE_HEAD_PX + estimateBodyHeight(f, split);
    n++;
  }
  return n;
}

/** Whether a file's hunks should be in the DOM: once `force`d (it's the
 *  target of a jump) or once it came near the viewport. Without
 *  `IntersectionObserver` (tests) everything mounts, as before. */
function useMountedNear(
  ref: { current: HTMLElement | null },
  force: boolean,
): boolean {
  const root = useContext(ScrollRoot);
  const [mounted, setMounted] = useState(
    () => force || typeof IntersectionObserver === 'undefined',
  );
  // Same render, not an effect: a jump target must be in the DOM by the
  // time the caller's effect scrolls to it.
  if (force && !mounted) setMounted(true);
  useEffect(() => {
    if (mounted) return;
    const el = ref.current;
    if (!el) return;
    const io = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) setMounted(true);
      },
      { root: root.current, rootMargin: MOUNT_MARGIN },
    );
    io.observe(el);
    return () => io.disconnect();
  }, [mounted, ref, root]);
  return mounted || force;
}

export const DiffPane = observer(function DiffPane({
  pr,
  rev,
  mode,
  setMode,
  since,
  sinceOnly,
  /** The head you had open last visit, shown short in the since-only banner
   *  — `null` on a first visit, when there's nothing to compare against. */
  baselineSha,
  /** Focused hunk and open composer are owned by `Review` — its `J`/`K`/`C`
   *  shortcuts drive them, and the diff pane just renders what they say. */
  focusHunk,
  setFocusHunk,
  composer,
  setComposer,
  /** Bumping `nonce` for a hunk's id fully reveals the context gap directly
   *  above it — `Review`'s `E` shortcut. */
  expandHunk = null,
  /** The "Next: <group>" button under the diff; off where the diff isn't
   *  read group by group (a space's live diff). */
  nextGroup = true,
  /** Asking the AI about a line or hunk (it opens the AI panel); off where
   *  the diff has no AI panel next to it (a space's live diff). */
  askAi = true,
  /** More items for a file's menu (a space's diff: Revert). */
  fileItems,
}: {
  pr: PrId;
  rev: Revision;
  mode: ReviewMode;
  setMode: (m: ReviewMode) => void;
  since?: Map<HunkId, SinceKind> | null;
  sinceOnly?: boolean;
  baselineSha?: string | null;
  focusHunk: HunkId | null;
  setFocusHunk: (h: HunkId | null) => void;
  composer: ComposerState | null;
  setComposer: (c: ComposerState | null) => void;
  expandHunk?: { hunk: HunkId; nonce: number } | null;
  nextGroup?: boolean;
  askAi?: boolean;
  fileItems?: (file: FileDiff) => ContextMenuItem[];
}) {
  const store = useRootStore();
  const ui = useUi();
  const mainRef = useRef<HTMLDivElement>(null);
  const diffWidth = useElementWidth(mainRef, SPLIT_MIN_WIDTH);
  // Split needs room for two readable halves; below that, unified is more
  // useful than two unreadably-thin columns.
  const splitRequested = ui.diffView === 'split';
  const effectiveSplit = splitRequested && diffWidth >= SPLIT_MIN_WIDTH;
  const splitDowngraded = splitRequested && !effectiveSplit;
  // Only read when a line's comment button is shift-clicked — nothing
  // renders from it, so it lives in a ref instead of re-rendering the diff.
  const rangeStart = useRef<RangeStart | null>(null);
  const [limit, setLimit] = useState(CHUNK);
  const [peek, setPeek] = useState<SymbolPeekTarget | null>(null);
  // Which paths the open diff actually has, so a symbol hit elsewhere in
  // the repo opens a read-only viewer instead of trying to scroll to a
  // hunk that isn't there.
  const diffPaths = useMemo(() => new Set(rev.files.map((f) => f.path)), [rev]);
  // The hunk (if any) covering a symbol hit's new-side line, so the peek
  // can scroll to `#H-<id>` (the id every hunk already renders) instead of
  // opening a read-only viewer for a file that's right there in the diff.
  const findHunk = useMemo(() => {
    const byPath = new Map<string, Hunk[]>();
    for (const f of rev.files) byPath.set(f.path, f.hunks);
    return (path: string, line: number): HunkId | null => {
      for (const h of byPath.get(path) ?? []) {
        if (
          h.new_lines > 0 &&
          line >= h.new_start &&
          line < h.new_start + h.new_lines
        ) {
          return h.id;
        }
      }
      return null;
    };
  }, [rev]);
  // One stable object for every line, so a memoized line skips re-rendering
  // when only someone else's composer, peek or range changed.
  const actions = useMemo<LineActions>(
    () => ({
      setComposer,
      setPeek,
      diffPaths,
      findHunk,
      rangeStart: () => rangeStart.current,
      setRangeStart: (r) => {
        rangeStart.current = r;
      },
      askAi,
      fileItems,
    }),
    [setComposer, diffPaths, findHunk, askAi, fileItems],
  );
  const [interdiffEntries, setInterdiffEntries] = useState<
    InterdiffEntry[] | null
  >(null);

  // Re-review: once there's a baseline to compare against, ask the core for
  // the word-level diff of every hunk that changed since then (plan §4).
  useEffect(() => {
    if (!baselineSha) {
      setInterdiffEntries(null);
      return;
    }
    let live = true;
    api
      .interdiff({ pr, base_sha: baselineSha, file: null })
      .then((entries) => live && setInterdiffEntries(entries))
      .catch(() => live && setInterdiffEntries(null));
    return () => {
      live = false;
    };
  }, [pr, baselineSha]);
  const interdiffMap = useMemo(
    () => interdiffByHunk(interdiffEntries ?? []),
    [interdiffEntries],
  );

  // Holding ⌘/Ctrl turns identifiers in the diff into links (styles.css: body.cmd).
  useEffect(() => {
    const on = (e: KeyboardEvent) =>
      document.body.classList.toggle('cmd', e.metaKey || e.ctrlKey);
    const off = () => document.body.classList.remove('cmd');
    window.addEventListener('keydown', on);
    window.addEventListener('keyup', on);
    window.addEventListener('blur', off);
    return () => {
      window.removeEventListener('keydown', on);
      window.removeEventListener('keyup', on);
      window.removeEventListener('blur', off);
      document.body.classList.remove('cmd');
    };
  }, []);

  // Every group with hunks in this revision, in reading order — numbered
  // as the groups pane numbers them. `groupsOf(pr)` keeps its identity
  // until the grouping row changes, and so does this list.
  const allGroups = store.models.groupsOf(pr);
  const revGroups = useMemo(
    () => groupsForRevision(store, pr, rev),
    // `allGroups` stands for `groupsOf(pr)`: a new row is a new list.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [allGroups, store, pr, rev],
  );
  // The same group `Review`'s J/K reads: trimmed to this revision's hunks.
  const group =
    mode.mode === 'groups' && mode.group
      ? revGroups.find((g) => g.id === mode.group)
      : null;
  // Memoized on the group object, which stays the same until the grouping
  // row changes — so the filtered file list below keeps its identity too.
  const groupHunkIds = useMemo(
    () => (group ? new Set(group.hunks) : null),
    [group],
  );
  const { allEdits, hiddenShown } = store.ephemeral.groupView(pr, group?.id);
  const showOnlyOutliers = showsOnlyOutliers(group ?? null, allEdits);
  // Lockfiles and generated code: collapsed by default, same as the
  // prototype — "show anyway" reveals this group's diff like any other.
  const groupCollapsed = isCollapsed(group ?? null, hiddenShown);

  // The same call `Review`'s J/K steps through, so focus only ever lands
  // on what's rendered here, in this order.
  const sinceShown = sinceOnly && since ? since : null;
  const files = useMemo(
    () =>
      diffFiles(rev, { mode: mode.mode, file: mode.file }, group ?? null, {
        since: sinceShown,
        allEdits,
        hiddenShown,
      }),
    [rev, mode.mode, mode.file, group, sinceShown, allEdits, hiddenShown],
  );

  // The other groups each file also has hunks in ("also in …" in its
  // header): one entry per group however many of its hunks the file has,
  // numbered as in the groups pane.
  const othersByPath = useMemo(() => {
    const m = new Map<string, OtherGroup[]>();
    if (mode.mode !== 'groups' || !groupHunkIds) return m;
    const byHunk = new Map<HunkId, OtherGroup>();
    revGroups.forEach((g, i) => {
      const other = { id: g.id, title: g.title, n: i + 1 };
      for (const h of g.hunks) byHunk.set(h, other);
    });
    for (const f of rev.files) {
      const others = new Set<OtherGroup>();
      for (const h of f.hunks) {
        const g = groupHunkIds.has(h.id) ? undefined : byHunk.get(h.id);
        if (g) others.add(g);
      }
      if (others.size) m.set(f.path, [...others]);
    }
    return m;
  }, [rev, mode.mode, groupHunkIds, revGroups]);
  const pathOfHunk = useMemo(() => {
    const m = new Map<HunkId, string>();
    for (const f of rev.files) for (const h of f.hunks) m.set(h.id, f.path);
    return m;
  }, [rev]);
  const focusPath = focusHunk ? pathOfHunk.get(focusHunk) : undefined;
  const expandPath = expandHunk ? pathOfHunk.get(expandHunk.hunk) : undefined;

  // A chunked file list resets whenever the group/file/mode changes under it.
  useEffect(() => setLimit(CHUNK), [mode.mode, mode.group, mode.file]);
  // A new group (N/P, a click in the groups pane) or file starts at its top —
  // its summary card — not wherever the previous one was scrolled to. A file
  // jump inside a group re-aims right after (Review's focus effect).
  const viewKey = mode.mode === 'files' ? `f:${mode.file}` : `g:${mode.group}`;
  // The first view of a resumed PR goes back to its saved scroll, once.
  const resumeScroll = useRef(ui.reviewPos[pr]?.scroll);
  useEffect(() => {
    const top = resumeScroll.current;
    resumeScroll.current = undefined;
    if (top != null && mainRef.current) {
      const el = mainRef.current;
      requestAnimationFrame(() => {
        el.scrollTop = top;
      });
      return;
    }
    // A file jump lands in another group and scrolls itself (Review's focus
    // effect, which runs after this one); resetting to the top here would
    // undo it. `pendingJump` is set by that jump, read and cleared once.
    if (pendingJump.current) {
      pendingJump.current = false;
      return;
    }
    if (mainRef.current) mainRef.current.scrollTop = 0;
  }, [viewKey]);
  // A jump target past the loaded chunk must still render.
  const focusIdx = focusPath
    ? files.findIndex((f) => f.path === focusPath)
    : -1;
  const visibleFiles = files.slice(0, Math.max(limit, focusIdx + 1));
  const eager = useMemo(
    () => eagerCount(files, effectiveSplit),
    [files, effectiveSplit],
  );

  const totalInGroup =
    mode.mode === 'groups' && groupHunkIds
      ? rev.files.flatMap((f) => f.hunks.filter((h) => groupHunkIds.has(h.id)))
          .length
      : 0;

  const sinceTotals = sinceCountsFor(
    rev.files.flatMap((f) => f.hunks),
    since,
  );

  return (
    <ScrollRoot.Provider value={mainRef}>
      <main
        ref={mainRef}
        onScroll={(e) => ui.setReviewScroll(pr, e.currentTarget.scrollTop)}
        className="diff min-w-0 overflow-auto bg-background px-6 pb-20 pt-5"
      >
        <CodeHoverHost pr={pr} rev={rev.head} root={mainRef} />
        {splitDowngraded && (
          <Alert status="default" className="split-note mb-4">
            <Alert.Content>
              <Alert.Title>
                Showing unified — not enough room here for side-by-side.
              </Alert.Title>
            </Alert.Content>
          </Alert>
        )}
        {sinceOnly && (
          <Alert status="accent" className="since-banner mb-4">
            <Alert.Content>
              <Alert.Title>
                Showing only what changed since your last visit
                {baselineSha ? ` (${baselineSha.slice(0, 7)})` : ''}.
              </Alert.Title>
              <Alert.Description>
                {sinceTotals.changed}{' '}
                {sinceTotals.changed === 1 ? 'hunk' : 'hunks'} changed,{' '}
                {sinceTotals.new} new — the rest you've already seen.
              </Alert.Description>
            </Alert.Content>
          </Alert>
        )}
        {mode.mode === 'groups' && mode.group && (
          <GroupCard
            key={`${pr}:${rev.base}:${rev.head}:${mode.group}`}
            pr={pr}
            rev={rev}
            groupId={mode.group}
          />
        )}
        {group && group.kind === 'skim' && group.outliers.length > 0 && (
          <Alert
            status={showOnlyOutliers ? 'warning' : 'default'}
            className="outliers mb-4"
          >
            <Alert.Indicator>
              <Sparkles className="size-4" />
            </Alert.Indicator>
            <Alert.Content>
              <Alert.Title>
                {showOnlyOutliers
                  ? `Showing ${group.outliers.length} off-pattern edit${group.outliers.length === 1 ? '' : 's'} of ${totalInGroup}`
                  : `All ${totalInGroup} edits in the group`}
              </Alert.Title>
              <Alert.Description>
                The agent checked the rest against the pattern: the same
                replacement, no need to read.
              </Alert.Description>
            </Alert.Content>
            <Btn
              tone="tertiary"
              onPress={() => store.ephemeral.toggleAllEdits(pr, group.id)}
            >
              {showOnlyOutliers ? 'Show all' : 'Only outliers'}
            </Btn>
          </Alert>
        )}
        {groupCollapsed ? (
          <div className="hidden-group flex items-center gap-3 rounded-2xl border border-dashed border-separator p-3.5 text-muted">
            <span className="flex-1">
              Hidden: {group!.why || 'mechanical or generated changes'} —
              collapsed by default.
            </span>
            <Btn
              tone="tertiary"
              className="show-hidden"
              onPress={() => store.ephemeral.showHidden(pr, group!.id)}
            >
              Show anyway
            </Btn>
          </div>
        ) : (
          <>
            {visibleFiles.map((f, i) => (
              // Per-file slices only: a file whose composer, peek, focus and
              // expand are all `null` stays memo-equal when another file changes.
              <FileBlock
                key={f.path}
                eager={i < eager}
                pr={pr}
                rev={rev.head}
                base={rev.base}
                file={f}
                since={since}
                interdiff={interdiffMap}
                others={othersByPath.get(f.path) ?? NO_OTHERS}
                setMode={setMode}
                split={effectiveSplit}
                focusHunk={focusPath === f.path ? focusHunk : null}
                setFocusHunk={setFocusHunk}
                expandHunk={expandPath === f.path ? expandHunk : null}
                composer={composer?.path === f.path ? composer : null}
                peek={peek?.path === f.path ? peek : null}
                actions={actions}
              />
            ))}
            {files.length > visibleFiles.length && (
              <div className="more-files flex items-center justify-center gap-3 py-2 text-sm text-muted">
                Showing {visibleFiles.length} of {files.length} files
                <Btn onPress={() => setLimit((n) => n + CHUNK * 2)}>
                  {Math.min(CHUNK * 2, files.length - visibleFiles.length)} more
                </Btn>
                <Btn tone="ghost" onPress={() => setLimit(files.length)}>
                  All
                </Btn>
              </div>
            )}
          </>
        )}
        <div className="flex justify-center py-4">
          {nextGroup && mode.mode === 'groups' && (
            <NextGroup pr={pr} rev={rev} mode={mode} setMode={setMode} />
          )}
        </div>
      </main>
    </ScrollRoot.Provider>
  );
});

/** Longer risks wrap on their own line instead of stretching the chip. */
const RISK_CHIP_MAX = 48;

const GroupCard = observer(function GroupCard({
  pr,
  rev,
  groupId,
}: {
  pr: PrId;
  rev: Revision;
  groupId: string;
}) {
  const store = useRootStore();
  const g = store.models.groupsOf(pr).find((x) => x.id === groupId);
  const [checked, setChecked] = useState<Record<string, boolean>>({});
  const occurrences = new Map<string, number>();
  const checks = (g?.check ?? []).map((text) => {
    const occurrence = occurrences.get(text) ?? 0;
    occurrences.set(text, occurrence + 1);
    return { key: JSON.stringify([text, occurrence]), text };
  });
  const range = useCheckboxRange({
    scopeKey: `checks:${pr}:${rev.base}:${rev.head}:${groupId}`,
    orderedKeys: checks.map((check) => check.key),
    onChange: (keys, value) =>
      setChecked((current) => ({
        ...current,
        ...Object.fromEntries(keys.map((key) => [key, value])),
      })),
  });
  if (!g) return null;
  // Same sequence `GroupsPane` numbers groups in — includes hidden groups
  // so a badge doesn't shift when one gets "shown anyway".
  const n = store.models.groupsOf(pr).findIndex((x) => x.id === g.id) + 1;
  const allRead = g.hunks.every((h) => store.models.hunkReads.isRead(pr, h));
  return (
    <Card className="gcard mb-5 gap-4 p-5">
      <div className="flex items-center gap-3">
        <span className="grid size-7 shrink-0 place-items-center rounded-full bg-accent-soft text-sm font-semibold text-accent tabular-nums">
          {n}
        </span>
        <Typography.Heading
          level={5}
          className="min-w-0 truncate"
          title={g.title}
        >
          {g.title}
        </Typography.Heading>
        {g.risk && g.risk.length <= RISK_CHIP_MAX && (
          <Chip
            size="sm"
            variant="soft"
            color="warning"
            className="shrink-0 gap-1"
          >
            <TriangleExclamation className="size-3.5" />
            {rich(g.risk)}
          </Chip>
        )}
        <span className="flex-1" />
        <Btn
          tone={allRead ? 'ghost' : 'tertiary'}
          icon={<Check />}
          className={allRead ? 'text-success' : ''}
          isDisabled={allRead}
          onPress={() => markGroupRead(store, pr, g.hunks)}
        >
          {allRead ? 'Read' : 'Mark as read'}
        </Btn>
      </div>
      {g.risk && g.risk.length > RISK_CHIP_MAX && (
        <p className="flex max-w-[78ch] gap-2 text-sm leading-relaxed text-warning">
          <TriangleExclamation className="mt-1 size-4 shrink-0" />
          <span>{rich(g.risk)}</span>
        </p>
      )}
      <p className="rich flex max-w-[78ch] gap-2 text-sm leading-relaxed">
        <Sparkles
          className="mt-1 size-4 shrink-0 text-accent"
          aria-label="agent explanation"
        />
        <span>{rich(g.why)}</span>
      </p>
      {g.check.length > 0 && (
        <div className="flex flex-col gap-2 rounded-2xl bg-surface-secondary px-4 py-3">
          <span className="text-xs text-muted">What to check</span>
          {checks.map(({ key, text }) => (
            <ChecklistItem
              key={key}
              text={text}
              checked={!!checked[key]}
              onPick={(value, modifiers) =>
                range.activate(key, value, modifiers)
              }
            />
          ))}
        </div>
      )}
    </Card>
  );
});

function ChecklistItem({
  text,
  checked,
  onPick,
}: {
  text: string;
  checked: boolean;
  onPick: (checked: boolean, modifiers: CheckboxRangeModifiers) => void;
}) {
  const rangeEvents = useCheckboxRangeEvents(onPick);
  return (
    <span
      className="contents"
      onClickCapture={rangeEvents.onClickCapture}
      onBlurCapture={rangeEvents.onBlurCapture}
      onKeyDownCapture={rangeEvents.onKeyDownCapture}
    >
      <Checkbox isSelected={checked} onChange={rangeEvents.onCheckboxChange}>
        <Checkbox.Content className={checked ? 'text-muted line-through' : ''}>
          <Checkbox.Control>
            <Checkbox.Indicator />
          </Checkbox.Control>
          {/* `Checkbox.Content` is `select-none` so a click anywhere in the
              row toggles the box; the text itself should still be
              drag-selectable for copy, so it opts back into selection. */}
          <span className="select-text">{rich(text)}</span>
        </Checkbox.Content>
      </Checkbox>
    </span>
  );
}

function NextGroupInner({
  pr,
  rev,
  mode,
  setMode,
}: {
  pr: PrId;
  rev: Revision;
  mode: ReviewMode;
  setMode: (m: ReviewMode) => void;
}) {
  const store = useRootStore();
  const groups = groupsForRevision(store, pr, rev);
  // Skips hidden groups — nothing to review there until shown by hand.
  const next = nextReviewableGroup(groups, mode.group);
  if (!next) return null;
  return (
    <Btn
      icon={<ArrowRight />}
      onPress={() =>
        setMode({ mode: 'groups', group: next.id, file: mode.file })
      }
    >
      Next: {next.title}
    </Btn>
  );
}

/** «Show more» for a gap; lines are revealed from the hunk outwards. */
function Expander({
  left,
  dir,
  onReveal,
}: {
  left: number;
  dir: 'up' | 'down';
  onReveal: (n: number) => void;
}) {
  const Icon = dir === 'up' ? ChevronsUp : ChevronsDown;
  return (
    <span className="expander -ml-2 flex items-center gap-0.5">
      {left > STEP && (
        <Button
          size="sm"
          variant="ghost"
          className="h-6 gap-1 px-1.5 text-xs text-muted"
          onPress={() => onReveal(STEP)}
        >
          <Icon className="size-3.5" />
          {STEP}
        </Button>
      )}
      <Button
        size="sm"
        variant="ghost"
        className="expand-all h-6 gap-1 px-1.5 text-xs text-muted"
        onPress={() => onReveal(Infinity)}
      >
        {left <= STEP && <Icon className="size-3.5" />}
        {left > STEP ? `all ${left}` : plural(left, 'line', 'lines')}
      </Button>
    </span>
  );
}

/** A collapsed run of unchanged lines above a hunk; fetches the file lazily
 *  (unlike the prototype's eagerly-mocked `sources`) and reveals it in
 *  `STEP`-sized bites from the hunk outward, closest lines first. */
const ContextGap = observer(function ContextGap({
  pr,
  rev,
  path,
  gap,
  lang,
  expand,
  onFullyRevealed,
}: {
  pr: PrId;
  rev: Revision['head'];
  path: string;
  gap: Gap;
  lang: ReturnType<typeof langOf>;
  /** A nonce from `Review`'s `E` shortcut — any change fully reveals the
   *  gap once (not on every render, so the manual "load N lines" buttons
   *  below still work undisturbed). */
  expand?: number | null;
  /** Told whenever the gap goes from partly to fully shown (or back, if the
   *  file re-fetches) — just the derived fact the hunk below needs, not the
   *  reveal mechanics themselves (#52: its header hint is redundant once
   *  every line above it is visible). */
  onFullyRevealed?: (full: boolean) => void;
}) {
  const store = useRootStore();
  const [shown, setShown] = useState(0);
  const total = gap.to - gap.from + 1;
  const appliedExpand = useRef<number | null>(null);
  useEffect(() => {
    if (expand != null && expand !== appliedExpand.current) {
      appliedExpand.current = expand;
      setShown(total);
    }
  }, [expand, total]);
  const key = `${pr}@${rev}:${path}`;
  const res =
    shown > 0
      ? store.files.get(key, () => api.getFile({ pr, rev, path }))
      : null;
  const ready = res?.status === 'ready' && res.data != null;
  // The callback prop is read through a ref so a parent re-render (a new
  // closure) doesn't re-fire this for the same reveal state.
  const onFullyRevealedRef = useRef(onFullyRevealed);
  onFullyRevealedRef.current = onFullyRevealed;
  useEffect(() => {
    onFullyRevealedRef.current?.(ready && shown >= total);
  }, [ready, shown, total]);
  const from = gap.to - shown + 1;
  const slice = ready ? res!.data!.split('\n').slice(from - 1, gap.to) : [];
  // Tokenized as one block (not line by line): a gap can land mid-comment
  // or mid-string, same as a hunk's own lines.
  const shikiVersion = useShikiVersion();
  const sideText = slice.join('\n');
  const toks = useMemo(
    () => tokenizeLines(sideText, lang),
    // eslint-disable-next-line react-hooks/exhaustive-deps -- shikiVersion forces a recompute once Shiki/the grammar finishes loading; it isn't read inside
    [sideText, lang, shikiVersion],
  );
  if (ready) {
    const left = total - shown;
    return (
      <>
        {left > 0 && (
          <div className="flex items-center gap-3 border-b border-separator bg-surface-secondary px-4 py-1 text-xs text-muted">
            <Expander
              left={left}
              dir="up"
              onReveal={(n) => setShown((s) => Math.min(total, s + n))}
            />
          </div>
        )}
        {slice.map((text, i) => {
          const newNo = from + i;
          return (
            <div key={newNo} className="ln t-ctx">
              <span className="no">{newNo - gap.shift}</span>
              <span className="no">{newNo}</span>
              <span className="mark"> </span>
              <Code s={text} lang={lang} toks={toks[i]} />
              <span className="ln-act" />
            </div>
          );
        })}
      </>
    );
  }
  return (
    <div className="gap flex items-center justify-center gap-2 border-y border-separator bg-surface-secondary px-4 py-1 text-xs text-muted">
      <Button
        size="sm"
        variant="ghost"
        isPending={res?.status === 'loading'}
        onPress={() => setShown(Math.min(STEP, total))}
      >
        Load {total} line{total === 1 ? '' : 's'} of context
      </Button>
    </div>
  );
});

/** The gap after a file's last shown hunk, fetched the same lazy way. */
const FileTail = observer(function FileTail({
  pr,
  rev,
  path,
  hunks,
  lang,
}: {
  pr: PrId;
  rev: Revision['head'];
  path: string;
  hunks: Hunk[];
  lang: ReturnType<typeof langOf>;
}) {
  const store = useRootStore();
  const [shown, setShown] = useState(0);
  const key = `${pr}@${rev}:${path}`;
  const res =
    shown > 0
      ? store.files.get(key, () => api.getFile({ pr, rev, path }))
      : null;
  const ready = res?.status === 'ready' && res.data != null;
  const allLines = ready ? res!.data!.split('\n') : [];
  const gap = gapTail(hunks, allLines.length);
  const to = gap ? Math.min(gap.to, gap.from + shown - 1) : 0;
  const slice = gap ? allLines.slice(gap.from - 1, to) : [];
  // Tokenized as one block (not line by line): see `ContextGap`.
  const shikiVersion = useShikiVersion();
  const sideText = slice.join('\n');
  const toks = useMemo(
    () => tokenizeLines(sideText, lang),
    // eslint-disable-next-line react-hooks/exhaustive-deps -- shikiVersion forces a recompute once Shiki/the grammar finishes loading; it isn't read inside
    [sideText, lang, shikiVersion],
  );
  if (ready) {
    if (!gap) return null;
    const total = gap.to - gap.from + 1;
    const left = total - shown;
    return (
      <>
        {slice.map((text, i) => {
          const newNo = gap.from + i;
          return (
            <div key={newNo} className="ln t-ctx">
              <span className="no">{newNo - gap.shift}</span>
              <span className="no">{newNo}</span>
              <span className="mark"> </span>
              <Code s={text} lang={lang} toks={toks[i]} />
              <span className="ln-act" />
            </div>
          );
        })}
        {left > 0 && (
          <div className="flex items-center gap-3 border-t border-separator bg-surface-secondary px-4 py-1 text-xs text-muted">
            <Expander
              left={left}
              dir="down"
              onReveal={(n) => setShown((s) => Math.min(total, s + n))}
            />
            <span>to end of file</span>
          </div>
        )}
      </>
    );
  }
  return (
    <div className="gap flex items-center justify-center gap-2 border-t border-separator bg-surface-secondary px-4 py-1 text-xs text-muted">
      <Button
        size="sm"
        variant="ghost"
        isPending={res?.status === 'loading'}
        onPress={() => setShown(STEP)}
      >
        Load to end of file
      </Button>
    </div>
  );
});

function SinceBadge({ kind }: { kind?: SinceKind }) {
  if (!kind) return null;
  return (
    <Chip
      size="sm"
      variant="soft"
      color={kind === 'new' ? 'success' : 'warning'}
    >
      {kind === 'new' ? 'new since' : 'changed since'} last visit
    </Chip>
  );
}

type RangeStart = { path: string; side: Side; line: number };

/** Everything a line does to state outside itself — one stable object for
 *  the whole diff (see `DiffPane`), so it never breaks a line's memo. */
interface LineActions {
  setComposer: (c: ComposerState | null) => void;
  setPeek: (p: SymbolPeekTarget | null) => void;
  /** Paths the open diff covers (plan "symbols": jump vs. read-only viewer). */
  diffPaths: ReadonlySet<string>;
  /** The hunk covering `path`'s new-side `line`, `null` outside every hunk. */
  findHunk: (path: string, line: number) => HunkId | null;
  /** Read at click time only; nothing renders from it. */
  rangeStart: () => RangeStart | null;
  setRangeStart: (r: RangeStart | null) => void;
  /** The diff has an AI panel next to it: offer asking about lines and
   *  hunks, and the ways into the panel. */
  askAi: boolean;
  /** More items for a file's menu, after the built-in ones. */
  fileItems?: (file: FileDiff) => ContextMenuItem[];
}

type DraftRow = ModelRows['draft'];
type FindingRow = ModelRows['finding'];

/** Another group a file has hunks in, numbered as in the groups pane. */
interface OtherGroup {
  id: string;
  title: string;
  n: number;
}
const NO_OTHERS: OtherGroup[] = [];
const NO_IDS: readonly string[] = [];
const NO_DRAFTS: readonly DraftRow[] = [];
const NO_FINDINGS: readonly FindingRow[] = [];

/** The side and number a comment on this line anchors to — `lineAnchor`'s
 *  rule, without building the whole anchor. */
function anchorSide(l: DiffLine): { side: Side; line: number } | null {
  if (l.kind === 'del')
    return l.old_no == null ? null : { side: 'old', line: l.old_no };
  return l.new_no == null ? null : { side: 'new', line: l.new_no };
}

/** The line number a symbol peek is pinned to (see `Line`'s ⌘-click). */
const peekLineNo = (l: DiffLine) => l.new_no ?? l.old_no ?? 0;

const hunkHasAnchor = (h: Hunk, side: Side, line: number) =>
  h.lines.some((l) => {
    const a = anchorSide(l);
    return !!a && a.side === side && a.line === line;
  });

const hunkHasPeek = (h: Hunk, line: number) =>
  h.lines.some((l) => peekLineNo(l) === line);

/** One hunk's slices of the file-level props: `null` for every hunk the
 *  composer / peek isn't in, so those hunks stay memo-equal. */
function hunkSlices(
  h: Hunk,
  composer: ComposerState | null,
  peek: SymbolPeekTarget | null,
) {
  return {
    composer:
      composer && hunkHasAnchor(h, composer.side, composer.line)
        ? composer
        : null,
    peek: peek && hunkHasPeek(h, peek.line) ? peek : null,
  };
}

function FileBlockInner({
  pr,
  rev,
  base,
  file,
  since,
  interdiff,
  others,
  setMode,
  split,
  focusHunk,
  setFocusHunk,
  expandHunk,
  composer,
  peek,
  actions,
  eager,
}: {
  pr: PrId;
  rev: Revision['head'];
  base: Revision['base'];
  file: FileDiff;
  since?: Map<HunkId, SinceKind> | null;
  /** The re-review word-diff, by the hunk id it's keyed on (plan §4) —
   *  `null`/absent outside "since last visit". */
  interdiff?: Map<HunkId, InterdiffEntry> | null;
  /** The other groups this file has hunks in (grouped mode only). */
  others: OtherGroup[];
  setMode: (m: ReviewMode) => void;
  split: boolean;
  /** Only set when the focused hunk is in this file. */
  focusHunk: HunkId | null;
  setFocusHunk: (h: HunkId | null) => void;
  /** Only set when the hunk to expand is in this file. */
  expandHunk?: { hunk: HunkId; nonce: number } | null;
  /** Only set when the composer is on this file. */
  composer: ComposerState | null;
  /** Only set when the symbol peek is on this file. */
  peek: SymbolPeekTarget | null;
  actions: LineActions;
  /** Near the top of the list: mount without waiting for the observer. */
  eager: boolean;
}) {
  countRender('FileBlock');
  const store = useRootStore();
  // Which hunks' preceding gap is fully revealed (#52) — just enough lifted
  // from `ContextGap` for `HunkComponent` to drop its now-redundant header
  // hint; the reveal mechanics themselves stay local to `ContextGap`.
  const [gapFullyRevealed, setGapFullyRevealed] = useState<
    Partial<Record<HunkId, boolean>>
  >({});
  const reportGapRevealed = useCallback((id: HunkId, full: boolean) => {
    setGapFullyRevealed((current) =>
      // `undefined` (never reported) and `false` both mean "show the
      // hint" — treated the same so the first, unrevealed report right
      // after mount doesn't force an extra render of every file.
      !!current[id] === full ? current : { ...current, [id]: full },
    );
  }, []);
  const cardRef = useRef<HTMLDivElement>(null);
  // Anything that jumps here (J/K focus, `E`, `C`, a peek) forces the hunks
  // in first so the jump finds its `H-<id>` element.
  const mounted = useMountedNear(
    cardRef,
    eager || !!focusHunk || !!expandHunk || !!composer || !!peek,
  );
  const lang = langOf(file.path);
  const viewed = store.models.fileViews.of(pr, file.path);
  const isViewed = viewed?.state === 'viewed';
  const local = store.models.prSummaries.get(pr)?.pr.repo.host === 'local';
  const fileSince = sinceCountsFor(file.hunks, since);
  const fileSinceCount = fileSince.new + fileSince.changed;
  // A jump here (J/K, `E`, `C`, a peek) needs its hunk in the DOM.
  const arrival =
    [
      focusHunk,
      expandHunk && `${expandHunk.hunk}:${expandHunk.nonce}`,
      composer && `c:${composer.side}:${composer.line}`,
      peek && `p:${peek.line}`,
    ]
      .filter(Boolean)
      .join('|') || null;
  // Generated, deleted and Viewed files start folded (#51); the chevron,
  // menu and "Show diff" override that until Viewed flips, or a jump moves
  // on a hand-folded file. Checked in render: a plain focus move costs no
  // extra render.
  const [fold, setFold] = useState<{
    closed: boolean;
    viewed: boolean;
    arrival: string | null;
  } | null>(null);
  if (
    fold &&
    (fold.viewed !== isViewed || (fold.closed && fold.arrival !== arrival))
  ) {
    setFold(null);
  }
  const foldTo = (closed: boolean) =>
    setFold({ closed, viewed: isViewed, arrival });
  // Where focus was when Viewed last flipped: a file marked Viewed while
  // one of its hunks has focus (a click in it, J/K, `V`) still folds; only
  // a jump after that opens it. On mount nothing is "seen" yet, so a jump
  // that brings a Viewed file into view opens it.
  const [seen, setSeen] = useState<{
    viewed: boolean;
    arrival: string | null;
  }>({ viewed: isViewed, arrival: null });
  if (seen.viewed !== isViewed) setSeen({ viewed: isViewed, arrival });
  const jumpedIn = arrival !== null && arrival !== seen.arrival;
  const md = isMarkdown(file.path) && file.status !== 'deleted';
  const [preview, setPreview] = useState(false);
  const generatedFlag = file.flags.find((f) => f.kind === 'generated');
  const collapsedFile = fold
    ? fold.closed
    : ((!!generatedFlag || file.status === 'deleted') && !arrival) ||
      (isViewed && !jumpedIn);
  // Built when the menu opens, not on every render.
  const fileMenu = (e: React.MouseEvent) =>
    contextMenuHandler(store.ui, (): ContextMenuItem[] => {
      const summaryForMenu = store.models.prSummaries.get(pr);
      const items: ContextMenuItem[] = [
        {
          id: 'mark-viewed',
          label: isViewed ? 'Mark not viewed' : 'Mark viewed',
          onAction: () => setFileViewed(store, pr, file.path, !isViewed),
        },
        {
          id: 'copy-path',
          label: 'Copy path',
          onAction: () => void navigator.clipboard.writeText(file.path),
        },
      ];
      if (summaryForMenu) {
        items.push({
          id: 'open-github',
          label: 'Open on GitHub',
          onAction: () => openExternal(`${summaryForMenu.url}/files`),
        });
      }
      items.push({
        id: 'collapse',
        label: collapsedFile ? 'Expand' : 'Collapse',
        onAction: () => foldTo(!collapsedFile),
      });
      items.push(...(actions.fileItems?.(file) ?? []));
      return items;
    })(e);

  const flag = file.flags.find(
    (f) => f.kind === 'oversize' || f.kind === 'binary',
  );
  if (isImageDiffPath(file.path) || flag?.kind === 'binary') {
    return <ImageDiffCard pr={pr} base={base} rev={rev} file={file} />;
  }
  if (flag) {
    return (
      <Card className="file mb-4 gap-0 overflow-hidden p-4 text-sm text-muted">
        {file.path}: not shown (
        {flag.kind === 'oversize'
          ? `${flag.lines.toLocaleString('en')} lines, over the size limit`
          : 'binary file'}
        )
      </Card>
    );
  }

  const gaps = gapsAbove(file.hunks);
  const last = file.hunks[file.hunks.length - 1];

  return (
    <Card ref={cardRef} className="file mb-4 gap-0 overflow-hidden p-0">
      <header
        className="file-h sticky top-0 z-[2] flex items-center gap-3 border-b border-separator bg-surface px-4 py-2.5 text-sm"
        onContextMenu={fileMenu}
      >
        <Button
          isIconOnly
          size="sm"
          variant="ghost"
          aria-label={collapsedFile ? 'Expand file' : 'Collapse file'}
          aria-expanded={!collapsedFile}
          className="fold-file -ml-1 size-6 min-w-6 shrink-0 text-muted"
          onPress={() => foldTo(!collapsedFile)}
        >
          {collapsedFile ? (
            <ChevronRight className="size-3.5" />
          ) : (
            <ChevronDown className="size-3.5" />
          )}
        </Button>
        <FileCode className="size-4 shrink-0 text-muted" />
        {/* `dir="rtl"` truncates from the start so the filename (the end of
         *  the path) stays visible; the inner `dir="ltr"` keeps old→new
         *  order correct for renames regardless of the outer direction. */}
        <span
          className="min-w-0 truncate font-mono text-[13px]"
          dir="rtl"
          title={file.old_path ? `${file.old_path} → ${file.path}` : file.path}
        >
          <span dir="ltr">
            {file.old_path && (
              <span className="text-muted line-through decoration-muted/60">
                {file.old_path}
              </span>
            )}
            {file.old_path && <span className="px-1.5 text-muted">→</span>}
            <span className="font-medium">{file.path}</span>
          </span>
        </span>
        {file.status === 'added' && (
          <Chip size="sm" variant="soft" color="success" className="shrink-0">
            new file
          </Chip>
        )}
        {file.status === 'renamed' && (
          <Chip
            size="sm"
            variant="soft"
            color="accent"
            className="renamed shrink-0"
          >
            renamed
            {file.similarity != null ? ` · ${file.similarity}% similar` : ''}
          </Chip>
        )}
        {file.status === 'deleted' && (
          <Chip size="sm" variant="soft" color="danger" className="shrink-0">
            deleted
          </Chip>
        )}
        {generatedFlag && (
          <Chip size="sm" variant="secondary" className="generated shrink-0">
            generated · {generatedFlag.reason}
          </Chip>
        )}
        <span className="shrink-0 whitespace-nowrap text-xs tabular-nums">
          <span className="text-success">+{file.additions}</span>{' '}
          <span className="text-danger">−{file.deletions}</span>
        </span>
        {fileSinceCount > 0 && (
          <Tooltip>
            <Chip
              size="sm"
              variant="soft"
              color={fileSince.new > 0 ? 'success' : 'warning'}
              className="since-file shrink-0"
            >
              {fileSinceCount} since last visit
            </Chip>
            <Tooltip.Content>
              {fileSince.new > 0 && `${fileSince.new} new`}
              {fileSince.new > 0 && fileSince.changed > 0 && ', '}
              {fileSince.changed > 0 && `${fileSince.changed} changed`}
            </Tooltip.Content>
          </Tooltip>
        )}
        {others.length > 0 && (
          // Each group as just its number, as the groups pane shows it (the
          // title is in the tooltip and accessible name), so the file name
          // and the Viewed toggle keep their room. Past a handful the row
          // gives way first (heavy shrink weight) and clips — never them.
          <span className="also-in flex min-w-0 shrink-[100] items-center gap-0.5 overflow-hidden">
            <span aria-hidden className="shrink-0 pr-0.5 text-xs text-muted">
              also in
            </span>
            {others.map((g) => (
              <Tooltip key={g.id}>
                <Button
                  size="sm"
                  variant="ghost"
                  aria-label={`Also in group ${g.n} “${g.title}”`}
                  className="size-6 min-w-6 shrink-0 rounded-full p-0 text-xs tabular-nums text-accent"
                  onPress={() =>
                    setMode({ mode: 'groups', group: g.id, file: file.path })
                  }
                >
                  {g.n}
                </Button>
                <Tooltip.Content>
                  More of this file in group {g.n} “{g.title}”
                </Tooltip.Content>
              </Tooltip>
            ))}
          </span>
        )}
        <span className="flex-1" />
        {viewed?.state === 'dismissed' && (
          <Chip size="sm" variant="soft" color="warning" className="shrink-0">
            changed since viewed
          </Chip>
        )}
        {md && (
          <ToggleButtonGroup
            size="sm"
            selectionMode="single"
            disallowEmptySelection
            aria-label="Markdown view"
            className="shrink-0"
            selectedKeys={new Set([preview ? 'preview' : 'code'])}
            onSelectionChange={(k) => setPreview([...k][0] === 'preview')}
          >
            <ToggleButton id="code">Code</ToggleButton>
            <ToggleButton id="preview">
              <ToggleButtonGroup.Separator />
              Preview
            </ToggleButton>
          </ToggleButtonGroup>
        )}
        <ToggleButton
          size="sm"
          isSelected={isViewed}
          onChange={() => setFileViewed(store, pr, file.path, !isViewed)}
          className="viewed-toggle shrink-0 gap-1.5"
          aria-label={local ? 'read' : 'Viewed — syncs with GitHub'}
        >
          {isViewed ? (
            <CircleCheckFill className="size-4 text-success" />
          ) : (
            <Eye className="size-4" />
          )}
          {local ? 'Read' : 'Viewed'}
        </ToggleButton>
      </header>
      {collapsedFile ? (
        <div className="collapsed-file flex items-center gap-3 bg-surface-secondary px-4 py-3 text-sm text-muted">
          <span className="flex-1">
            {generatedFlag
              ? `Generated file — ${generatedFlag.reason}`
              : file.status === 'deleted'
                ? `Whole file deleted: ${plural(file.deletions, 'line', 'lines')}`
                : `${plural(file.hunks.length, 'hunk', 'hunks')} hidden`}
          </span>
          <Button
            size="sm"
            variant="tertiary"
            className="show-file"
            onPress={() => foldTo(false)}
          >
            Show diff
          </Button>
        </div>
      ) : md && preview ? (
        <MarkdownDiff
          pr={pr}
          base={base}
          head={rev}
          path={file.path}
          oldPath={file.old_path}
        />
      ) : !mounted ? (
        <div
          aria-hidden
          className="file-pending"
          style={{ height: estimateBodyHeight(file, split) }}
        />
      ) : (
        <>
          {file.hunks.map((h) => {
            const HunkComponent = split ? SplitHunkView : HunkView;
            const slices = hunkSlices(h, composer, peek);
            return (
              <div key={h.id}>
                {gaps.has(h.id) && (
                  <ContextGap
                    pr={pr}
                    rev={rev}
                    path={file.path}
                    gap={gaps.get(h.id)!}
                    lang={lang}
                    expand={expandHunk?.hunk === h.id ? expandHunk.nonce : null}
                    onFullyRevealed={(full) => reportGapRevealed(h.id, full)}
                  />
                )}
                <HunkComponent
                  pr={pr}
                  rev={rev}
                  path={file.path}
                  h={h}
                  lang={lang}
                  since={since?.get(h.id)}
                  interdiff={interdiff?.get(h.id)}
                  focused={focusHunk === h.id}
                  setFocusHunk={setFocusHunk}
                  composer={slices.composer}
                  peek={slices.peek}
                  actions={actions}
                  gapFullyRevealed={!!gapFullyRevealed[h.id]}
                />
              </div>
            );
          })}
          {last && (
            <FileTail
              pr={pr}
              rev={rev}
              path={file.path}
              hunks={file.hunks}
              lang={lang}
            />
          )}
        </>
      )}
    </Card>
  );
}

interface HunkProps {
  pr: PrId;
  rev: Revision['head'];
  path: string;
  h: Hunk;
  lang: ReturnType<typeof langOf>;
  since?: SinceKind;
  interdiff?: InterdiffEntry;
  focused: boolean;
  setFocusHunk: (h: HunkId | null) => void;
  /** Only set when the composer is on one of this hunk's lines. */
  composer: ComposerState | null;
  /** Only set when the symbol peek is on one of this hunk's lines. */
  peek: SymbolPeekTarget | null;
  actions: LineActions;
  /** The gap just above this hunk (if any) is fully revealed, so its own
   *  header hint (#52) would just repeat what's now visible above it. */
  gapFullyRevealed?: boolean;
}

/** A line's annotations and per-line slices of the hunk's props, read from
 *  the per-line indexes — the hunk observes them, the line just renders. */
function lineSlice(
  store: ReturnType<typeof useRootStore>,
  { pr, path, composer, peek, actions }: HunkProps,
  l: DiffLine,
) {
  const a = anchorSide(l);
  return {
    threads: a ? store.models.threadIdsAt(pr, path, a.side, a.line) : NO_IDS,
    drafts: a ? store.models.drafts.at(pr, path, a.side, a.line) : NO_DRAFTS,
    findings: a
      ? store.models.findings.shownAt(pr, path, a.side, a.line)
      : NO_FINDINGS,
    // No AI panel, no answer to view or ask to retry: no line ask chip.
    lineAsk:
      a && actions.askAi
        ? store.ephemeral.lineAskAt(pr, path, a.side, a.line)
        : undefined,
    composer:
      a &&
      composer &&
      composer.path === path &&
      composer.side === a.side &&
      composer.line === a.line
        ? composer
        : null,
    peek:
      peek && peek.path === path && peek.line === peekLineNo(l) ? peek : null,
  };
}

/** The hunk header's menu, built when it opens. */
function openHunkMenu(
  e: React.MouseEvent,
  store: ReturnType<typeof useRootStore>,
  { pr, path, h, actions }: HunkProps,
) {
  contextMenuHandler(store.ui, (): ContextMenuItem[] => [
    {
      id: 'mark-read',
      label: 'Mark read',
      isDisabled: store.models.hunkReads.isRead(pr, h.id),
      disabledReason: 'Already read',
      onAction: () => markHunkRead(store, pr, h.id),
    },
    ...(actions.askAi
      ? [
          {
            id: 'ask-agent',
            label: 'Ask agent',
            onAction: () =>
              void askPending(store, {
                pr,
                question: `about the hunk at ${path}:${h.new_start}`,
                about: null,
                group: null,
                agent: null,
              }).catch(() => {
                // toasted by api.ts
              }),
          },
        ]
      : []),
    {
      id: 'copy-hunk',
      label: 'Copy hunk',
      onAction: () =>
        void navigator.clipboard.writeText(
          h.lines.map((l) => l.text).join('\n'),
        ),
    },
  ])(e);
}

function HunkViewInner(props: HunkProps) {
  countRender('Hunk');
  const {
    pr,
    rev,
    path,
    h,
    lang,
    since,
    interdiff,
    focused,
    setFocusHunk,
    gapFullyRevealed,
  } = props;
  const store = useRootStore();
  const read = store.models.hunkReads.isRead(pr, h.id);
  const marks = useMemo(() => wordMarks(h.lines), [h.lines]);
  const view = useMemo(() => interdiffView(interdiff), [interdiff]);
  const [showInterdiff, setShowInterdiff] = useState(false);
  // One Shiki call per side (not per line), so grammar state — an open
  // comment, a template literal — carries across lines; recomputed once
  // Shiki (or this hunk's grammar) finishes loading.
  const shikiVersion = useShikiVersion();
  const toks = useMemo(
    () => hunkLineTokens(h.lines, lang),
    // eslint-disable-next-line react-hooks/exhaustive-deps -- shikiVersion forces a recompute once Shiki/the grammar finishes loading; it isn't read inside
    [h.lines, lang, shikiVersion],
  );
  return (
    <div
      id={`H-${h.id}`}
      className={`hunk border-t border-separator first:border-t-0 ${focused ? 'focus ring-2 ring-inset ring-accent' : ''} ${read ? 'read' : ''}`}
      onMouseDown={() => !focused && setFocusHunk(h.id)}
    >
      <div
        className={`flex min-h-8 items-center gap-3 px-4 py-1 text-xs text-muted ${focused ? 'bg-accent-soft/60' : 'bg-surface-secondary'}`}
        onContextMenu={(e) => openHunkMenu(e, store, props)}
      >
        <span className="tabular-nums">{hunkRange(h)}</span>
        {h.header && !gapFullyRevealed && (
          <span className="min-w-0 flex-1 truncate text-muted/70">
            {h.header}
          </span>
        )}
        <SinceBadge kind={since} />
        {view && (
          <Button
            size="sm"
            variant="ghost"
            className="what-changed h-6 gap-1 px-1.5 text-xs text-accent"
            onPress={() => setShowInterdiff((v) => !v)}
          >
            {showInterdiff ? 'Hide what changed' : 'What changed'}
          </Button>
        )}
        <span className="flex-1" />
        {read && (
          <span className="inline-flex items-center gap-1 text-success">
            <Check className="size-3.5" /> read
          </span>
        )}
      </div>
      {view && showInterdiff && (
        <InterdiffPanel previous={view.previous} now={view.now} />
      )}
      <div className="lines">
        {h.lines.map((l, i) => (
          <Line
            key={i}
            pr={pr}
            rev={rev}
            path={path}
            l={l}
            lang={lang}
            marks={marks.get(i)}
            toks={toks[i]}
            actions={props.actions}
            {...lineSlice(store, props, l)}
          />
        ))}
      </div>
    </div>
  );
}

/** Side-by-side rendering of one hunk, using `splitRows` to pair del/add lines. */
function SplitHunkViewInner(props: HunkProps) {
  countRender('Hunk');
  const {
    pr,
    rev,
    path,
    h,
    lang,
    since,
    interdiff,
    focused,
    setFocusHunk,
    gapFullyRevealed,
  } = props;
  const store = useRootStore();
  const read = store.models.hunkReads.isRead(pr, h.id);
  const marks = useMemo(() => wordMarks(h.lines), [h.lines]);
  const rows = useMemo(() => splitRows(h.lines), [h.lines]);
  const view = useMemo(() => interdiffView(interdiff), [interdiff]);
  const [showInterdiff, setShowInterdiff] = useState(false);
  // Same per-side tokenizing as the unified view (see `HunkViewInner`) — the
  // old/new split here is purely a layout difference, not a second pass.
  const shikiVersion = useShikiVersion();
  const toks = useMemo(
    () => hunkLineTokens(h.lines, lang),
    // eslint-disable-next-line react-hooks/exhaustive-deps -- shikiVersion forces a recompute once Shiki/the grammar finishes loading; it isn't read inside
    [h.lines, lang, shikiVersion],
  );
  const cell = (i: number | null | undefined) => {
    if (i == null) return null;
    const l = h.lines[i]!;
    return (
      <Line
        pr={pr}
        rev={rev}
        path={path}
        l={l}
        lang={lang}
        marks={marks.get(i)}
        toks={toks[i]}
        actions={props.actions}
        {...lineSlice(store, props, l)}
      />
    );
  };
  return (
    <div
      id={`H-${h.id}`}
      className={`hunk split border-t border-separator first:border-t-0 ${focused ? 'focus ring-2 ring-inset ring-accent' : ''} ${read ? 'read' : ''}`}
      onMouseDown={() => !focused && setFocusHunk(h.id)}
    >
      <div
        className={`flex min-h-8 items-center gap-3 px-4 py-1 text-xs text-muted ${focused ? 'bg-accent-soft/60' : 'bg-surface-secondary'}`}
      >
        <span className="tabular-nums">{hunkRange(h)}</span>
        {h.header && !gapFullyRevealed && (
          <span className="min-w-0 flex-1 truncate text-muted/70">
            {h.header}
          </span>
        )}
        <SinceBadge kind={since} />
        {view && (
          <Button
            size="sm"
            variant="ghost"
            className="what-changed h-6 gap-1 px-1.5 text-xs text-accent"
            onPress={() => setShowInterdiff((v) => !v)}
          >
            {showInterdiff ? 'Hide what changed' : 'What changed'}
          </Button>
        )}
        <span className="flex-1" />
        {read && (
          <span className="inline-flex items-center gap-1 text-success">
            <Check className="size-3.5" /> read
          </span>
        )}
      </div>
      {view && showInterdiff && (
        <InterdiffPanel previous={view.previous} now={view.now} />
      )}
      <div className="lines split-lines">
        {rows.map((row, i) => (
          <div key={i} className="split-row grid grid-cols-2">
            <div className="split-col">{cell(row.l)}</div>
            <div className="split-col">{cell(row.r)}</div>
          </div>
        ))}
      </div>
    </div>
  );
}

/** The line's menu, built when it opens — not a hook per line. */
function openLineMenu(
  e: React.MouseEvent,
  store: ReturnType<typeof useRootStore>,
  { pr, path, l, actions }: LineProps,
  anchor: Anchor | null,
) {
  contextMenuHandler(store.ui, (): ContextMenuItem[] => {
    if (!anchor) return [];
    const lineNo = peekLineNo(l);
    const summary = store.models.prSummaries.get(pr);
    const items: ContextMenuItem[] = [
      {
        id: 'comment',
        label: 'Comment',
        onAction: () =>
          actions.setComposer({ path, side: anchor.side, line: anchor.line }),
      },
      {
        id: 'comment-range',
        label: 'Start a comment range here',
        onAction: () =>
          actions.setRangeStart({ path, side: anchor.side, line: anchor.line }),
      },
      ...(actions.askAi
        ? [
            {
              id: 'ask-agent',
              label: 'Ask agent',
              onAction: () => referenceLine(store, pr, anchor),
            },
          ]
        : []),
      {
        id: 'copy-line',
        label: 'Copy line',
        onAction: () => void navigator.clipboard.writeText(l.text),
      },
    ];
    if (summary) {
      items.push({
        id: 'copy-permalink',
        label: 'Copy permalink',
        onAction: () =>
          void navigator.clipboard.writeText(
            `${summary.url}#diff-${path}L${lineNo}`,
          ),
      });
    }
    return items;
  })(e);
}

interface LineProps {
  pr: PrId;
  rev: Revision['head'];
  path: string;
  l: DiffLine;
  lang: ReturnType<typeof langOf>;
  marks?: [number, number][];
  /** This line's tokens, computed once for the whole hunk/gap — carries
   *  cross-line grammar state. `undefined` only while the hunk's own
   *  `useMemo` hasn't run yet, which `Code` handles by tokenizing alone. */
  toks?: Tok[];
  threads: readonly string[];
  drafts: readonly DraftRow[];
  findings: readonly FindingRow[];
  /** A question asked about this very line, until opened or dismissed. */
  lineAsk: LineAsk | undefined;
  /** Only set when the composer is open on this very line. */
  composer: ComposerState | null;
  /** Only set when the symbol peek is pinned to this very line. */
  peek: SymbolPeekTarget | null;
  actions: LineActions;
}

/** Plain `memo`: reads no observables itself — the hunk hands it its
 *  annotations — so it re-renders only when its own props change. */
const Line = memo(function Line(props: LineProps) {
  countRender('Line');
  const {
    pr,
    rev,
    path,
    l,
    lang,
    marks,
    toks,
    threads,
    drafts,
    findings,
    lineAsk,
    composer,
    peek,
    actions,
  } = props;
  const store = useRootStore();
  const anchor = lineAnchor(rev, path, l);
  const composerAnchor =
    composer && anchor ? lineAnchor(rev, path, l, composer.startLine) : null;
  const lineNo = peekLineNo(l);
  return (
    <>
      <div
        className={`ln t-${l.kind}${lineAsk?.status === 'asking' ? ' ln-asking' : ''}`}
        // For the hover (`CodeHoverHost`): head-side lines only.
        data-path={l.new_no != null ? path : undefined}
        data-line={l.new_no ?? undefined}
        onClick={(e) => {
          if (!(e.metaKey || e.ctrlKey)) return;
          const name = symbolAt(e);
          if (!name) return;
          e.preventDefault();
          actions.setPeek({ name, path, line: lineNo });
        }}
        onContextMenu={(e) => openLineMenu(e, store, props, anchor)}
      >
        <span className="no">{l.old_no ?? ''}</span>
        <span className="no">{l.new_no ?? ''}</span>
        <span className="mark">
          {findings.length > 0 ? (
            <span
              className={`inline-block size-[9px] rounded-full ${actions.askAi ? 'cursor-pointer' : ''} ${sevBg[findings[0]!.severity]}`}
              title={findings.map((f) => f.title).join('\n')}
              onClick={
                actions.askAi ? () => store.ui.toggleAside(true) : undefined
              }
            />
          ) : (
            <span>{l.kind === 'add' ? '+' : l.kind === 'del' ? '−' : ' '}</span>
          )}
        </span>
        <Code s={l.text} lang={lang} marks={marks} toks={toks} />
        <span className="ln-act">
          {anchor && (
            <>
              <Tooltip>
                <Button
                  isIconOnly
                  size="sm"
                  aria-label="comment"
                  className="h-5 min-w-5 w-5 rounded-xl text-xs"
                  onPress={(e) => {
                    const rangeStart = actions.rangeStart();
                    if (
                      e.shiftKey &&
                      rangeStart &&
                      rangeStart.path === path &&
                      rangeStart.side === anchor.side
                    ) {
                      const startLine = Math.min(rangeStart.line, anchor.line);
                      const endLine = Math.max(rangeStart.line, anchor.line);
                      actions.setComposer({
                        path,
                        side: anchor.side,
                        line: endLine,
                        startLine:
                          startLine !== endLine ? startLine : undefined,
                      });
                    } else {
                      actions.setRangeStart({
                        path,
                        side: anchor.side,
                        line: anchor.line,
                      });
                      actions.setComposer({
                        path,
                        side: anchor.side,
                        line: anchor.line,
                      });
                    }
                  }}
                >
                  <Plus className="size-3" />
                </Button>
                <Tooltip.Content>
                  Comment · shift-click another line for a range
                </Tooltip.Content>
              </Tooltip>
              {actions.askAi && (
                <Button
                  isIconOnly
                  size="sm"
                  aria-label="ask AI about this line"
                  className="h-5 min-w-5 w-5 rounded-xl text-xs"
                  onPress={() => referenceLine(store, pr, anchor)}
                >
                  <Sparkles className="size-3" />
                </Button>
              )}
            </>
          )}
        </span>
      </div>
      {(threads.length > 0 ||
        drafts.length > 0 ||
        lineAsk ||
        composer ||
        peek) && (
        <div className="flex flex-col gap-2 border-y border-separator bg-background py-2 pl-[108px] pr-3 font-sans text-sm">
          {peek && (
            <SymbolPeek
              // A fresh click target gets a fresh mount, so its search
              // state starts clean instead of being reset imperatively.
              key={`${peek.path}:${peek.line}:${peek.name}`}
              pr={pr}
              rev={rev}
              target={peek}
              diffPaths={actions.diffPaths}
              findHunk={actions.findHunk}
              onClose={() => actions.setPeek(null)}
            />
          )}
          {lineAsk && (
            <LineAskChip
              askKey={lineAskKey(
                pr,
                path,
                lineAsk.anchor.side,
                lineAsk.anchor.line,
              )}
              a={lineAsk}
            />
          )}
          {threads.map((id) => (
            <ThreadView key={id} pr={pr} id={id} />
          ))}
          {drafts.map((d) => (
            <DraftView key={d.id} d={d} />
          ))}
          {composerAnchor && (
            <Composer
              key={`${composerAnchor.path}:${composerAnchor.start_line}:${composerAnchor.line}`}
              pr={pr}
              anchor={composerAnchor}
              lineText={l.kind !== 'del' ? l.text : ''}
              askAi={actions.askAi}
              onClose={() => actions.setComposer(null)}
            />
          )}
        </div>
      )}
    </>
  );
});

/** A line ask's progress on the line itself: "Asking…" while it runs,
 *  then "Answered · View" or "Failed · Retry" until opened or dismissed. */
function LineAskChip({ askKey, a }: { askKey: string; a: LineAsk }) {
  const store = useRootStore();
  const tone =
    a.status === 'failed'
      ? 'border-danger/40 text-danger'
      : a.status === 'answered'
        ? 'border-success/40 text-success'
        : 'border-accent/40 text-accent';
  return (
    <div
      className={`line-ask line-ask-${a.status} inline-flex h-7 w-fit items-center gap-1 rounded-full border bg-surface pl-2.5 pr-1 text-xs ${tone}`}
      role="status"
    >
      {a.status === 'asking' ? (
        <button
          type="button"
          className="inline-flex items-center gap-1.5"
          onClick={() => openLineAnswer(store, askKey)}
        >
          <Spinner />
          <span className="shimmer-text">
            Asking <span className="font-mono">{a.agent}</span>…
          </span>
        </button>
      ) : a.status === 'answered' ? (
        <button
          type="button"
          className="inline-flex items-center gap-1.5"
          onClick={() => openLineAnswer(store, askKey)}
        >
          <Check className="size-3.5" />
          <span>
            Answered · <span className="underline">View</span>
          </span>
        </button>
      ) : (
        <button
          type="button"
          className="inline-flex items-center gap-1.5"
          onClick={() => void askAboutLine(store, a)}
        >
          <CircleExclamation className="size-3.5" />
          <span>
            Failed · <span className="underline">Retry</span>
          </span>
        </button>
      )}
      <button
        type="button"
        aria-label={a.status === 'asking' ? 'Stop' : 'Dismiss'}
        className="grid size-5 place-items-center rounded-full text-muted hover:bg-default"
        onClick={() => dismissLineAsk(store, askKey)}
      >
        <Xmark className="size-3" />
      </button>
    </div>
  );
}

const NextGroup = observer(NextGroupInner);
const FileBlock = observer(FileBlockInner);
const HunkView = observer(HunkViewInner);
const SplitHunkView = observer(SplitHunkViewInner);
