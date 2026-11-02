import { openExternal } from '../core/openExternal';
import { useElementWidth } from './useElementWidth';
import { REVIEW_EVENT } from '../core/menu';
import { DiffSkeleton } from './Loading';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  ClockArrowRotateLeft,
  CodeMerge,
  LayoutRows,
  LayoutSideContentLeft,
  LayoutSideContentRight,
  LayoutSplitColumns,
  Xmark,
} from '@gravity-ui/icons';
import { Code, CodeCommit, Eye, FileText } from '@gravity-ui/icons';
import {
  Alert,
  Separator,
  Spinner,
  Tabs,
  ToggleButton,
  Toolbar,
  Tooltip,
  Typography,
} from '@heroui/react';
import { observer } from 'mobx-react-lite';
import type { Anchor, HunkId, PrId } from '../gen/contract';
import { api } from '../core/api';
import { markGroupRead, setDraft, setFileViewed } from '../actions';
import { FEATURES } from '../features';
import { useRootStore, useUi } from '../stores/context';
import { Btn, CheckIcon, Hints, K } from './bits';
import { Checks, Commits, ReviewerPicker } from './Checks';
import {
  DiffPane,
  pendingJump,
  lineAnchor,
  type ComposerState,
} from './DiffPane';
import { STORAGE_NS } from '../stores/UiStore';
import { sinceHunks, type SinceKind } from './diff';
import { GroupsPane, type ReviewMode } from './GroupsPane';
import { MergeDialog } from './Merge';
import { Overview } from './review/Overview';
import { closeBranchReview } from './project/closeReview';
import { isBranchReview } from './project/sections';
import { isBuildMode, reviewTitle } from './space/mode';
import { SendAllToAgentFooter } from './space/SendAllToAgent';
import { spaceOfReview } from './space/spaces';
import {
  groupsInRevision,
  nextReviewableGroup,
  nextUnreadGroup,
  prevReviewableGroup,
  reviewableGroups,
} from './review/groups';
import { hunkAt, hunksInScope, jumpView, moveHunkFocus } from './review/hunks';
import { SidePanel } from './SidePanel';
import { readProgress } from './review/progress';
import { RevisionError } from './review/RevisionError';
import { StatusChip } from './review/StatusChip';
import { latinKey, useViewKeys } from './keys';

type PrTab = 'overview' | 'diff' | 'checks' | 'commits';

/** `a`'s line among the diff lines under `root`: the row whose old (or new)
 *  number cell says its line. */
export function lineIn(
  root: ParentNode,
  a: Pick<Anchor, 'side' | 'line'>,
): HTMLElement | null {
  const col = a.side === 'old' ? 0 : 1;
  const no = String(a.line);
  for (const el of root.querySelectorAll<HTMLElement>('.ln')) {
    if (el.children[col]?.textContent === no) return el;
  }
  return null;
}

/** A short glow on a line the diff just jumped to. */
function flash(el: HTMLElement): void {
  el.classList.remove('ln-flash');
  void el.offsetWidth; // restart the animation on a repeat jump
  el.classList.add('ln-flash');
}

const lastSeenKey = (pr: PrId) => `${STORAGE_NS}theodore-lastseen:${pr}`;
const baselineKeyOf = (pr: PrId) => `${STORAGE_NS}theodore-baseline:${pr}`;

/** Below this width the groups pane and the aside stop being grid columns —
 *  there isn't room for three panes and a diff column no narrower than
 *  `DIFF_MIN`. Pure, so the breakpoints are unit-testable without mounting
 *  anything. */
const GROUPS_BREAKPOINT = 1060;
const ASIDE_BREAKPOINT = 1280;
/** Below this the header's buttons go icon-only and the breadcrumb drops
 *  the repo, so the title keeps some room (the tab carries it anyway). */
const HEADER_BREAKPOINT = 820;
export const DIFF_MIN = 520;

export type ReviewLayout = {
  groups: 'column' | 'drawer';
  aside: 'column' | 'drawer';
  header: 'full' | 'compact';
};

export function layoutFor(width: number): ReviewLayout {
  return {
    groups: width < GROUPS_BREAKPOINT ? 'drawer' : 'column',
    aside: width < ASIDE_BREAKPOINT ? 'drawer' : 'column',
    header: width < HEADER_BREAKPOINT ? 'compact' : 'full',
  };
}

export const Review = observer(function Review({ pr }: { pr: PrId }) {
  const store = useRootStore();
  const ui = useUi();
  const rootRef = useRef<HTMLDivElement>(null);
  const width = useElementWidth(rootRef, ASIDE_BREAKPOINT);
  const layout = layoutFor(width);
  const compact = layout.header === 'compact';
  // Groups-pane drawer is page-local — unlike `ui.aside`, which the user may
  // also toggle off at wide widths, this only ever matters once collapsed.
  const [groupsOpen, setGroupsOpen] = useState(false);
  useEffect(() => {
    ui.setAsideIsDrawer(layout.aside === 'drawer');
  }, [ui, layout.aside]);
  useEffect(() => () => ui.setAsideIsDrawer(false), [ui]);
  // Grouped by default — the point of the product; Files is the fallback view.
  // Reopening a PR (or switching back to its workspace) resumes where it was.
  const [mode, setMode] = useState<ReviewMode>(() => {
    const at = ui.reviewPos[pr];
    return at
      ? { mode: at.mode, group: at.group, file: at.file }
      : { mode: 'groups', group: null, file: null };
  });
  useEffect(() => ui.setReviewPos(pr, mode), [ui, pr, mode]);
  const [tab, setTab] = useState<PrTab>('diff');
  const tabPicked = useRef(false);
  const markTabPick = () => {
    tabPicked.current = true;
    setTimeout(() => (tabPicked.current = false), 500);
  };
  const [sinceOnly, setSinceOnly] = useState(false);
  const [baselineSha, setBaselineSha] = useState<string | null>(null);
  const [mergeOpen, setMergeOpen] = useState(false);
  const [draftBusy, setDraftBusy] = useState(false);
  // Keyboard-driven hunk focus, the open comment composer, and a one-shot
  // "expand context" request — owned here so `J`/`K`/`C`/`E` can reach into
  // `DiffPane` without it keeping its own, uncontrollable state.
  const [focusHunk, setFocusHunk] = useState<HunkId | null>(null);
  const [composer, setComposer] = useState<ComposerState | null>(null);
  const [expandHunk, setExpandHunk] = useState<{
    hunk: HunkId;
    nonce: number;
  } | null>(null);
  useEffect(() => {
    void store.sync.openPr(pr);
  }, [store, pr]);

  const summary = store.models.prSummaries.get(pr);
  const detail = store.models.prDetails.get(pr);
  const checks = detail?.checks ?? [];
  const failing = checks.filter((c) => c.status === 'fail').length;
  const pending = checks.filter((c) => c.status === 'pending').length;
  const passed = checks.filter((c) => c.status === 'pass').length;
  const revKey = summary ? `${pr}@${summary.head_sha}` : null;
  // Bumped by Retry: `invalidate` isn't observable, so this re-renders the
  // page to call `get()` again.
  const [, setRetries] = useState(0);
  const retryRevision = useCallback(() => {
    if (revKey) store.revisions.invalidate(revKey);
    setRetries((n) => n + 1);
  }, [store, revKey]);
  const revision = revKey
    ? store.revisions.get(revKey, () =>
        api.getRevision({ pr, head: summary!.head_sha }),
      )
    : null;

  // "Since last visit" compares against the head you had caught up on. A
  // push you haven't read yet keeps that baseline across visits; it only
  // moves once every hunk changed since then is read (below).
  useEffect(() => {
    if (!summary) return;
    const seen = localStorage.getItem(lastSeenKey(pr));
    let base = localStorage.getItem(baselineKeyOf(pr));
    if (!base && seen && seen !== summary.head_sha) {
      base = seen;
      localStorage.setItem(baselineKeyOf(pr), base);
    }
    localStorage.setItem(lastSeenKey(pr), summary.head_sha);
    setBaselineSha(base && base !== summary.head_sha ? base : null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pr, summary?.head_sha]);

  const baselineKey = baselineSha ? `${pr}@${baselineSha}` : null;
  const baselineRevision = baselineKey
    ? store.revisions.get(baselineKey, () =>
        api.getRevision({ pr, head: baselineSha! }),
      )
    : null;
  // Memoized so `DiffPane`/`GroupsPane` (both memoized) see the same map
  // until a revision actually changes — revisions are immutable once loaded.
  const revData = revision?.data;
  const baseData =
    baselineRevision?.status === 'ready' ? baselineRevision.data : undefined;
  const since: Map<string, SinceKind> | null = useMemo(
    () =>
      baseData && revData ? sinceHunks(baseData.files, revData.files) : null,
    [baseData, revData],
  );
  const sinceCount = since?.size ?? 0;
  // Caught up: every hunk changed since the baseline is read, so the next
  // visit compares against this head.
  const caughtUp =
    since != null &&
    since.size > 0 &&
    (() => {
      const reads = new Set(store.models.hunkReads.byPr(pr).map((r) => r.hunk));
      return [...since.keys()].every((h) => reads.has(h as HunkId));
    })();
  useEffect(() => {
    if (caughtUp) localStorage.removeItem(baselineKeyOf(pr));
  }, [caughtUp, pr]);

  useEffect(() => {
    if (!mode.file && revision?.data?.files.length) {
      setMode((m) => ({ ...m, file: m.file ?? revision.data!.files[0]!.path }));
    }
  }, [revision?.data, mode.file]);

  // Every group with hunks in this revision, in reading order — includes
  // hidden (lockfile/generated) groups, which `GroupsPane` lists collapsed.
  // Same as `groupsForRevision`, memoized on the grouping row (replaced
  // whenever its groups change) so dependents keep their identity.
  // `groupsOf`, not the row's own order: a group the user opened or partly
  // read keeps its place when a streamed grouping's final pass reorders
  // (`stableOrder`).
  const grouping = store.models.groupings.get(pr);
  const allGroups = useMemo(() => {
    if (!revData) return [];
    const hunkIds = new Set(
      revData.files.flatMap((f) => f.hunks.map((h) => h.id)),
    );
    return groupsInRevision(store.models.groupsOf(pr), hunkIds);
    // `grouping` stands for `groupsOf(pr)`: a new row is a new order.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [grouping, revData, store, pr]);
  useEffect(() => {
    if (mode.mode === 'groups' && mode.group)
      store.models.markGroupOpened(pr, mode.group);
  }, [store, pr, mode.mode, mode.group]);
  const allGroupIds = allGroups.map((g) => g.id).join();
  // The subset `N`/`P` step through — hidden groups are nothing to review,
  // so they're skipped unless opened by hand ("show anyway").
  const reviewable = useMemo(() => reviewableGroups(allGroups), [allGroups]);
  const reviewableIds = reviewable.map((g) => g.id).join();

  const group =
    mode.mode === 'groups' && mode.group
      ? allGroups.find((g) => g.id === mode.group)
      : null;

  // Like the prototype: in grouped mode a group is always selected —
  // preferring a reviewable one so a lockfile group is never the default.
  useEffect(() => {
    const first = reviewable[0] ?? allGroups[0];
    // A resumed group id the PR no longer has counts as none.
    const known = !!mode.group && allGroups.some((g) => g.id === mode.group);
    if (mode.mode === 'groups' && !known && first) {
      setMode((m) => ({ ...m, group: first.id }));
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [mode.mode, mode.group, allGroupIds, reviewableIds]);

  // Marks the current group read and moves on to the next unread one —
  // shared by the `R` shortcut and the menu's "Mark Group as Read".
  const markReadAndAdvance = useCallback(() => {
    const g = allGroups.find((x) => x.id === mode.group);
    if (!g) return;
    markGroupRead(store, pr, g.hunks);
    const reads = new Set(store.models.hunkReads.byPr(pr).map((r) => r.hunk));
    const next = nextUnreadGroup(allGroups, mode.group, reads);
    if (next) setMode((m) => ({ ...m, mode: 'groups', group: next.id }));
  }, [allGroups, mode.group, store, pr]);

  // Clicking a file in a group's file list: open that group and jump to the
  // file's first hunk in it (the jump target is force-mounted by DiffPane).
  const [jumpStart, setJumpStart] = useState(false);
  const [jumpSeq, setJumpSeq] = useState(0);
  const onOpenFile = useCallback(
    (groupId: string, path: string) => {
      const g = allGroups.find((x) => x.id === groupId);
      const file = revision?.data?.files.find((f) => f.path === path);
      const inGroup = g ? new Set(g.hunks) : null;
      const first = file?.hunks.find((h) => !inGroup || inGroup.has(h.id));
      if (first) pendingJump.current = true;
      setMode({ mode: 'groups', group: groupId, file: path });
      if (first) {
        setJumpStart(true);
        setJumpSeq((n) => n + 1);
        setFocusHunk(first.id);
      }
    },
    [allGroups, revision?.data],
  );

  // A file:line chip in the AI panel: show the view that has the line (its
  // group, its file, past the filters that hide it), then land on the line
  // itself (the focus effect below).
  const jumpLine = useRef<Anchor | null>(null);
  const lineJump =
    store.ephemeral.lineJump?.pr === pr ? store.ephemeral.lineJump : null;
  useEffect(() => {
    if (!lineJump || !revision?.data) return;
    store.ephemeral.consumeLineJump(lineJump.seq);
    const at = hunkAt(revision.data, lineJump.anchor);
    if (!at) return;
    const sinceOn = sinceOnly && sinceCount > 0 ? since : null;
    const view = jumpView(
      revision.data,
      mode,
      allGroups,
      (g) => ({ since: sinceOn, ...store.ephemeral.groupView(pr, g) }),
      at.path,
      at.hunk.id,
    );
    const g = view.mode.group;
    if (view.sinceOff) setSinceOnly(false);
    if (view.showHidden && g) store.ephemeral.showHidden(pr, g);
    if (view.showAll && g) store.ephemeral.toggleAllEdits(pr, g);
    const key = (m: ReviewMode) =>
      m.mode === 'files' ? `f:${m.file}` : `g:${m.group}`;
    // Only a view change resets the scroll (DiffPane) — and only then must
    // it be told not to.
    if (key(view.mode) !== key(mode)) pendingJump.current = true;
    if (view.mode !== mode) setMode(view.mode);
    setTab('diff');
    jumpLine.current = lineJump.anchor;
    setJumpStart(true);
    setJumpSeq((n) => n + 1);
    setFocusHunk(at.hunk.id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [lineJump?.seq, revision?.data]);

  // Review items of the native menu bar (see core/menu.ts).
  useEffect(() => {
    const on = (e: Event) => {
      const id = (e as CustomEvent<string>).detail;
      if (id === 'view-grouped') setMode((m) => ({ ...m, mode: 'groups' }));
      else if (id === 'view-files') setMode((m) => ({ ...m, mode: 'files' }));
      else if (id === 'view-split') ui.toggleDiffView();
      else if (id === 'mark-group-read') {
        const g = allGroups.find((x) => x.id === mode.group);
        if (g) markGroupRead(store, pr, g.hunks);
      } else if (id === 'ask-agent') {
        window.dispatchEvent(new KeyboardEvent('keydown', { key: '/' }));
      } else if (id === 'open-on-host') {
        const url = summary?.url;
        if (url) openExternal(url);
      }
    };
    window.addEventListener(REVIEW_EVENT, on);
    return () => window.removeEventListener(REVIEW_EVENT, on);
  });

  useViewKeys(
    ui,
    (e) => {
      const k = latinKey(e);
      if (k === 's') {
        ui.toggleDiffView();
        return;
      }
      if (k === '/') {
        ui.toggleAside(true);
        setTimeout(
          () =>
            (
              document.querySelector(
                '.chat-input textarea',
              ) as HTMLTextAreaElement | null
            )?.focus(),
          30,
        );
        return;
      }
      if (k === 'Escape') {
        if (composer) {
          setComposer(null);
          return;
        }
        ui.goBack();
        return;
      }
      if (k === 'r') {
        markReadAndAdvance();
        return;
      }
      // The rest only make sense over the diff itself.
      if (tab !== 'diff' || !revision?.data) return false;
      switch (k) {
        case 'j':
        case 'k': {
          // Exactly what `DiffPane` renders: the same filters it applies.
          const hunks = hunksInScope(revision.data, mode, group ?? null, {
            since: sinceOnly && sinceCount > 0 ? since : null,
            ...store.ephemeral.groupView(pr, group?.id),
          });
          const next = moveHunkFocus(hunks, focusHunk, k === 'j' ? 1 : -1);
          if (next) setFocusHunk(next);
          return;
        }
        case 'n':
        case 'p': {
          const next =
            k === 'n'
              ? nextReviewableGroup(allGroups, mode.group)
              : prevReviewableGroup(allGroups, mode.group);
          if (next) setMode((m) => ({ ...m, mode: 'groups', group: next.id }));
          return;
        }
        case 'v': {
          if (!focusHunk) return false;
          const file = revision.data.files.find((f) =>
            f.hunks.some((h) => h.id === focusHunk),
          );
          if (!file) return false;
          const viewed = store.models.fileViews.of(pr, file.path);
          void setFileViewed(store, pr, file.path, viewed?.state !== 'viewed');
          return;
        }
        case 'c': {
          if (!focusHunk) return false;
          const file = revision.data.files.find((f) =>
            f.hunks.some((h) => h.id === focusHunk),
          );
          const hunk = file?.hunks.find((h) => h.id === focusHunk);
          if (!file || !hunk) return false;
          const idx = Math.max(
            0,
            hunk.lines.findIndex((l) => l.kind !== 'ctx'),
          );
          const line = hunk.lines[idx];
          const anchor =
            line && lineAnchor(revision.data.head, file.path, line);
          if (anchor) {
            setComposer({
              path: file.path,
              side: anchor.side,
              line: anchor.line,
            });
          }
          return;
        }
        case 'e': {
          if (!focusHunk) return false;
          setExpandHunk({ hunk: focusHunk, nonce: Date.now() });
          return;
        }
        default:
          return false;
      }
    },
    [
      markReadAndAdvance,
      allGroupIds,
      pr,
      tab,
      mode.mode,
      mode.group,
      mode.file,
      focusHunk,
      composer,
      revision?.data,
      group,
      since,
      sinceOnly,
      sinceCount,
    ],
  );

  // Keep the keyboard-focused hunk in view, same id `DiffPane` renders it
  // under (`H-<hunkId>`).
  useEffect(() => {
    if (!focusHunk) return;
    const target = () => document.getElementById(`H-${focusHunk}`);
    if (!jumpStart) {
      // J/K: a small step, smooth is fine and nothing above it moves.
      const id = requestAnimationFrame(() =>
        target()?.scrollIntoView({ block: 'nearest', behavior: 'smooth' }),
      );
      return () => cancelAnimationFrame(id);
    }
    // A jump (file click, group switch): files above the target mount as
    // they come near the viewport and grow from their placeholder height,
    // which moves the target after a smooth scroll has aimed at it. Jump
    // instantly, then re-aim until the layout settles.
    setJumpStart(false);
    // A line jump aims at the line, centred, and lights it up once landed.
    const toLine = jumpLine.current;
    jumpLine.current = null;
    const line = () => {
      const h = target();
      return toLine && h ? lineIn(h.closest('.file') ?? h, toLine) : null;
    };
    const steps = [0, 80, 200, 400, 700];
    const timers = steps.map((ms) =>
      setTimeout(() => {
        const l = line();
        if (l) {
          l.scrollIntoView({ block: 'center', behavior: 'auto' });
          if (ms === steps.at(-1)) flash(l);
          return;
        }
        // The file card, so its name is in view, not just the hunk.
        (target()?.closest('.file') ?? target())?.scrollIntoView({
          block: 'start',
          behavior: 'auto',
        });
      }, ms),
    );
    return () => timers.forEach(clearTimeout);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [focusHunk, jumpSeq]);

  // A branch review (no PR on the host) takes no host actions either.
  const branchReview = !!summary && isBranchReview(summary);
  const local = summary?.pr.repo.host === 'local' || branchReview;
  const finished = summary?.state === 'merged' || summary?.state === 'closed';
  const mine =
    !!summary && store.ephemeral.viewer?.login === summary.author.login;
  // A space's own review, drafting or in review: "Send N to agent"
  // (below) takes the place of "Submit review" there.
  const mySpace = isBuildMode(store)
    ? spaceOfReview(store.models.spaces.all(), pr)
    : undefined;
  const mergeBlock = !summary
    ? ''
    : local
      ? 'local branch'
      : summary.conflicts
        ? 'conflicts with base'
        : failing
          ? `${failing} ${failing === 1 ? 'check' : 'checks'} failing`
          : summary.approvals < (summary.required_approvals ?? 0)
            ? `approvals ${summary.approvals}/${summary.required_approvals}`
            : '';

  return (
    <div ref={rootRef} className="review flex min-w-0 flex-1 flex-col">
      <Toolbar
        aria-label="PR"
        className="flex min-h-14 w-full flex-wrap items-center gap-3 border-b border-separator px-6 py-2"
      >
        {layout.groups === 'drawer' && (
          <Tooltip>
            <ToggleButton
              isIconOnly
              size="sm"
              isSelected={groupsOpen}
              onChange={setGroupsOpen}
              aria-label="Files and groups"
            >
              <LayoutSideContentLeft className="size-4" />
            </ToggleButton>
            <Tooltip.Content>
              {groupsOpen ? 'Hide' : 'Show'} files and groups
            </Tooltip.Content>
          </Tooltip>
        )}
        <div className="rv-title flex min-w-0 flex-1 items-center gap-3 overflow-hidden">
          {summary ? (
            <>
              <span className="shrink-0 text-sm text-muted">
                {branchReview ? summary.head_ref : `#${summary.pr.number}`}
              </span>
              <Typography.Heading
                level={6}
                className="min-w-[12ch] flex-1 truncate"
              >
                {reviewTitle(store, pr, summary.title)}
              </Typography.Heading>
              <StatusChip
                summary={summary}
                progress={readProgress(store, pr)}
                className="shrink-0"
              />
            </>
          ) : (
            <Spinner size="sm" />
          )}
        </div>
        {!local && mine && summary && !finished && (
          <Btn
            tone="tertiary"
            icon={summary.state === 'draft' ? <Eye /> : <FileText />}
            isPending={draftBusy}
            onPress={() => {
              setDraftBusy(true);
              void setDraft(store, {
                pr,
                draft: summary.state !== 'draft',
              }).finally(() => setDraftBusy(false));
            }}
          >
            {summary.state === 'draft'
              ? 'Ready for review'
              : 'Convert to draft'}
          </Btn>
        )}
        {/* A merged or closed PR takes no more reviews, reviewers or merges. */}
        {!finished && !branchReview && !mySpace && (
          <Btn
            tone="primary"
            className="shrink-0"
            onPress={() => ui.openModal('submit')}
          >
            {compact ? 'Submit' : 'Submit review'}
          </Btn>
        )}
        {!local && !finished && FEATURES.hostWrites && (
          <ReviewerPicker pr={pr} compact={compact} />
        )}
        {!local && !finished && (
          <Tooltip>
            <Btn
              tone={mergeBlock ? 'tertiary' : 'primary'}
              icon={<CodeMerge />}
              className="merge-btn shrink-0"
              isIconOnly={compact}
              aria-label="Merge"
              isDisabled={!FEATURES.hostWrites}
              onPress={() => setMergeOpen(true)}
            >
              {!compact && 'Merge'}
            </Btn>
            <Tooltip.Content>
              {!FEATURES.hostWrites
                ? 'Not available yet'
                : mergeBlock
                  ? `Blocked: ${mergeBlock} — you can enable auto-merge`
                  : 'Merge'}
            </Tooltip.Content>
          </Tooltip>
        )}
        {branchReview && (
          <Btn
            tone="tertiary"
            icon={<Xmark />}
            className="close-review shrink-0"
            isIconOnly={compact}
            aria-label="Close review"
            onPress={() => void closeBranchReview(store, pr)}
          >
            {!compact && 'Close review'}
          </Btn>
        )}
        <Tooltip>
          <ToggleButton
            isIconOnly
            size="sm"
            isSelected={ui.asideShown}
            onChange={(v) => ui.toggleAside(v)}
            aria-label="AI panel"
          >
            <LayoutSideContentRight className="size-4" />
          </ToggleButton>
          <Tooltip.Content>
            {ui.asideShown ? 'Hide' : 'Show'} AI, tasks and threads <K>]</K>
          </Tooltip.Content>
        </Tooltip>
      </Toolbar>

      <Toolbar
        aria-label="Sections and people"
        className="flex w-full flex-wrap items-center gap-4 border-b border-separator px-6 py-2"
      >
        {/* Only a click or a key in the tab list switches tabs: focus
            landing on a tab on its own (a dialog closing, ⌘K) must not. */}
        <div
          className="contents"
          onPointerDownCapture={markTabPick}
          onKeyDownCapture={markTabPick}
        >
          <Tabs
            data-plain
            selectedKey={tab}
            onSelectionChange={(k) => tabPicked.current && setTab(k as PrTab)}
            className="rv-tabs"
          >
            <Tabs.ListContainer>
              <Tabs.List aria-label="PR sections">
                <Tabs.Tab id="overview" className="gap-1.5">
                  <FileText className="size-4" />
                  Overview
                  <Tabs.Indicator />
                </Tabs.Tab>
                {!local && (
                  <Tabs.Tab id="checks" className="gap-1.5">
                    <CheckIcon
                      s={failing ? 'fail' : pending ? 'pending' : 'pass'}
                    />
                    Checks
                    <span className="tabular-nums text-muted">
                      {passed}/{checks.length}
                    </span>
                    <Tabs.Indicator />
                  </Tabs.Tab>
                )}
                <Tabs.Tab id="commits" className="gap-1.5">
                  <CodeCommit className="size-4" />
                  Commits
                  <span className="tabular-nums text-muted">
                    {detail?.commits.length ?? 0}
                  </span>
                  <Tabs.Indicator />
                </Tabs.Tab>
                <Tabs.Tab id="diff" className="gap-1.5">
                  <Code className="size-4" />
                  Code
                  <Tabs.Indicator />
                </Tabs.Tab>
              </Tabs.List>
            </Tabs.ListContainer>
          </Tabs>
        </div>
        {baselineRevision?.status === 'ready' && (
          <>
            <Separator orientation="vertical" className="h-5" />
            <Tabs
              data-plain
              selectedKey={sinceOnly ? 'since' : 'all'}
              onSelectionChange={(k) => setSinceOnly(k === 'since')}
              className="since-tabs"
            >
              <Tabs.ListContainer>
                <Tabs.List aria-label="What to show">
                  <Tabs.Tab id="all" className="w-auto whitespace-nowrap">
                    Whole PR
                    <Tabs.Indicator />
                  </Tabs.Tab>
                  <Tabs.Tab
                    id="since"
                    className="w-auto gap-1.5 whitespace-nowrap"
                  >
                    <ClockArrowRotateLeft className="size-4" />
                    Since last visit
                    <span className="tabular-nums text-muted">
                      {sinceCount}
                    </span>
                    <Tabs.Indicator />
                  </Tabs.Tab>
                </Tabs.List>
              </Tabs.ListContainer>
            </Tabs>
          </>
        )}
        {tab === 'diff' && (
          <Tabs
            selectedKey={ui.diffView}
            onSelectionChange={(k) => k !== ui.diffView && ui.toggleDiffView()}
            data-plain
            className="diff-view-tabs ms-auto"
          >
            <Tabs.ListContainer>
              <Tabs.List aria-label="Diff view">
                <Tabs.Tab id="unified" aria-label="Unified" className="px-2.5">
                  <LayoutRows className="size-4" />
                  <Tabs.Indicator />
                </Tabs.Tab>
                <Tabs.Tab
                  id="split"
                  aria-label="Side by side"
                  className="px-2.5"
                >
                  <Tabs.Separator />
                  <LayoutSplitColumns className="size-4" />
                  <Tabs.Indicator />
                </Tabs.Tab>
              </Tabs.List>
            </Tabs.ListContainer>
          </Tabs>
        )}
      </Toolbar>

      {sinceOnly &&
        sinceCount === 0 &&
        baselineRevision?.status === 'ready' && (
          <Alert
            status="accent"
            className="since-banner rounded-none border-b border-separator px-3 py-1.5"
          >
            <Alert.Indicator>
              <ClockArrowRotateLeft className="size-4" />
            </Alert.Indicator>
            <Alert.Content>
              <Alert.Title>Nothing changed since your last visit.</Alert.Title>
            </Alert.Content>
            <Btn tone="tertiary" onPress={() => setSinceOnly(false)}>
              Whole PR
            </Btn>
          </Alert>
        )}

      {!revision || revision.status === 'loading' ? (
        <div className="min-h-0 flex-1 overflow-hidden">
          <DiffSkeleton />
        </div>
      ) : revision.status === 'error' ? (
        <RevisionError error={revision.error} onRetry={retryRevision} />
      ) : (
        (() => {
          const showGroupsColumn = layout.groups === 'column';
          const showAsideColumn = layout.aside === 'column' && ui.aside;
          const gridColsClass = showGroupsColumn
            ? showAsideColumn
              ? 'grid-cols-[256px_minmax(520px,1fr)_352px]'
              : 'grid-cols-[256px_minmax(520px,1fr)]'
            : showAsideColumn
              ? 'grid-cols-[minmax(520px,1fr)_352px]'
              : 'grid-cols-[minmax(520px,1fr)]';
          return (
            <div
              className={`relative grid min-h-0 flex-1 overflow-hidden ${gridColsClass}`}
            >
              {showGroupsColumn && (
                <GroupsPane
                  pr={pr}
                  rev={revision.data!}
                  mode={mode}
                  setMode={setMode}
                  since={since}
                  sinceOnly={sinceOnly && sinceCount > 0}
                  onOpenFile={onOpenFile}
                />
              )}
              {tab === 'overview' && summary ? (
                <Overview
                  id={pr}
                  summary={summary}
                  detail={detail}
                  title={reviewTitle(store, pr, summary.title)}
                  progress={readProgress(store, pr)}
                  branchReview={branchReview}
                  local={local}
                />
              ) : tab === 'checks' ? (
                <Checks pr={pr} checks={checks} />
              ) : tab === 'commits' ? (
                <Commits commits={detail?.commits ?? []} />
              ) : (
                <DiffPane
                  pr={pr}
                  rev={revision.data!}
                  mode={mode}
                  setMode={setMode}
                  since={since}
                  sinceOnly={sinceOnly && sinceCount > 0}
                  baselineSha={baselineSha}
                  focusHunk={focusHunk}
                  setFocusHunk={setFocusHunk}
                  composer={composer}
                  setComposer={setComposer}
                  expandHunk={expandHunk}
                />
              )}
              {showAsideColumn && <SidePanel pr={pr} />}
              {!showGroupsColumn && groupsOpen && (
                <>
                  {/* `grid` (not `flex`/`block`): a single-child grid cell
                   *  stretches that child to fill both axes by default, the
                   *  same way `GroupsPane`'s `Surface` fills its slot as a
                   *  grid column above — no height to hand it otherwise. */}
                  <div className="absolute inset-y-0 left-0 z-20 grid w-64 grid-cols-[minmax(0,1fr)] grid-rows-[minmax(0,1fr)] border-r border-separator bg-background shadow-xl">
                    <GroupsPane
                      pr={pr}
                      rev={revision.data!}
                      mode={mode}
                      setMode={setMode}
                      since={since}
                      sinceOnly={sinceOnly && sinceCount > 0}
                      onOpenFile={onOpenFile}
                    />
                  </div>
                  <div
                    className="absolute inset-0 z-10 bg-black/40"
                    onClick={() => setGroupsOpen(false)}
                  />
                </>
              )}
              {!showAsideColumn &&
                layout.aside === 'drawer' &&
                ui.asideShown && (
                  <>
                    <div className="absolute inset-y-0 right-0 z-20 grid w-[352px] max-w-[85vw] grid-cols-[minmax(0,1fr)] grid-rows-[minmax(0,1fr)] border-l border-separator bg-background shadow-xl">
                      <SidePanel pr={pr} />
                    </div>
                    <div
                      className="absolute inset-0 z-10 bg-black/40"
                      onClick={() => ui.toggleAside(false)}
                    />
                  </>
                )}
            </div>
          );
        })()
      )}
      {mySpace && <SendAllToAgentFooter pr={pr} />}
      {tab === 'diff' && (
        <Hints>
          <K>J</K>
          <K>K</K> hunk <K>N</K>
          <K>P</K> group <K>R</K> mark read <K>V</K> viewed <K>C</K> comment{' '}
          <K>E</K> context <K>S</K> split <K>/</K> ask AI <K>]</K> AI panel{' '}
          <K>Esc</K> back
        </Hints>
      )}
      {mergeOpen && (
        <MergeDialog
          pr={pr}
          onClose={() => setMergeOpen(false)}
          onOpenFile={(path) => {
            setMergeOpen(false);
            setTab('diff');
            setMode({ mode: 'files', group: mode.group, file: path });
          }}
        />
      )}
    </div>
  );
});
