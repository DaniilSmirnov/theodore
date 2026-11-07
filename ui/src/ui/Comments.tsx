import { isBranchReview } from './project/sections';
import { stripHtmlComments } from '../highlight';
/**
 * The inline comment-thread UI used under a diff line: existing review
 * threads, pending drafts, and the composer that creates new ones. Ported
 * from the prototype's `Comments.tsx`.
 */
import { useEffect, useRef, useState } from 'react';
import {
  ArrowUp,
  Check,
  CircleCheck,
  ChevronDown,
  ChevronUp,
  CodeCommit,
  Ellipsis,
  FaceSmile,
  Layers,
  Plus,
} from '@gravity-ui/icons';
import {
  AvatarGroup,
  Button,
  Card,
  Chip,
  Dropdown,
  Kbd,
  Popover,
  Tabs,
  TextArea,
} from '@heroui/react';
import { observer } from 'mobx-react-lite';
import { Md } from '../highlight';
import { api } from '../core/api';
import type {
  Anchor,
  Comment,
  Draft,
  Person,
  PrId,
  PrSummary,
  ReactionCount,
} from '../gen/contract';
import {
  addDraft,
  askAboutLine,
  commitSuggestions,
  deleteComment,
  editComment,
  editDraft,
  reactToComment,
  referenceLine,
  removeDraft,
  replyThread,
  resolveThread,
} from '../actions';
import { useRootStore } from '../stores/context';
import {
  filterMentions,
  insertMention,
  mentionQueryAt,
  moveMentionIndex,
  type MentionQuery,
} from './review/mention';
import { AgentTag, Avatar, Btn } from './bits';
import { SendToAgent } from './space/SendToAgent';

export const plural = (n: number, one: string, many: string) =>
  `${n} ${n === 1 ? one : many}`;

function timeAgo(ms: number): string {
  const s = Math.max(0, Math.floor((Date.now() - ms) / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h`;
  return `${Math.floor(h / 24)}d`;
}

/** The lines of a trailing ```suggestion fence, or `null` if there is none. */
export function suggestionOf(body: string): string[] | null {
  const m = body.match(/```suggestion\n([\s\S]*?)\n```/);
  return m ? m[1]!.split('\n') : null;
}

/** `commit_suggestions` is own-PRs-only: a local branch has no other author
 *  to gate on; a host PR needs the signed-in viewer to be its author. */
export function canCommitSuggestions(
  summary: Pick<PrSummary, 'pr' | 'author'> | undefined,
  viewerLogin: string | undefined,
): boolean {
  if (!summary) return false;
  if (summary.pr.repo.host === 'local') return true;
  return !!viewerLogin && summary.author.login === viewerLogin;
}

const REACTIONS: [emoji: string, name: string][] = [
  ['👍', '+1'],
  ['👎', '-1'],
  ['😄', 'laugh'],
  ['🎉', 'hooray'],
  ['😕', 'confused'],
  ['❤️', 'heart'],
  ['🚀', 'rocket'],
  ['👀', 'eyes'],
];

// ── @-mention autocomplete ───────────────────────────────────────────────

/** `assignableUsers` barely changes within one PR's review session, so one
 *  fetch is cached and shared by every composer open on it (plan §6.3). */
const assignableCache = new Map<PrId, Promise<Person[]>>();
function assignableUsers(pr: PrId): Promise<Person[]> {
  let p = assignableCache.get(pr);
  if (!p) {
    p = api.assignableUsers(pr).catch(() => []);
    assignableCache.set(pr, p);
  }
  return p;
}

interface MentionMenuState {
  query: MentionQuery;
  options: Person[];
  index: number;
}

/** Drives the `@`-mention menu for one composer: tracks the candidate list
 *  for `pr` (cached), re-derives the menu from the textarea's own caret on
 *  every keystroke, and handles the keys the menu owns while open —
 *  callers fall back to their own `Enter`/`Escape` handling when this
 *  returns `false`. */
function useMentionMenu(pr: PrId, text: string, setText: (t: string) => void) {
  const store = useRootStore();
  const viewerLogin = store.ephemeral.viewer?.login;
  const [people, setPeople] = useState<Person[] | null>(null);
  const [state, setState] = useState<MentionMenuState | null>(null);

  // A branch review or a local folder has no host to mention people on.
  const summary = store.models.prSummaries.get(pr);
  const noHost =
    !!summary && (summary.pr.repo.host === 'local' || isBranchReview(summary));
  useEffect(() => {
    if (noHost) return;
    let live = true;
    void assignableUsers(pr).then((p) => {
      if (live) setPeople(p);
    });
    return () => {
      live = false;
    };
  }, [pr, noHost]);

  const sync = (value: string, caret: number) => {
    const q = mentionQueryAt(value, caret);
    if (!q || !people) {
      setState(null);
      return;
    }
    const options = filterMentions(people, q.query, viewerLogin);
    setState(options.length ? { query: q, options, index: 0 } : null);
  };

  const pick = (login: string, target: HTMLTextAreaElement | null) => {
    if (!state) return;
    const next = insertMention(text, state.query, login);
    setText(next.text);
    setState(null);
    // The textarea's value updates on the next render; queue the caret
    // move just after, so a direct keystroke doesn't land before the
    // inserted mention.
    setTimeout(() => target?.setSelectionRange(next.caret, next.caret), 0);
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>): boolean => {
    if (!state) return false;
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      const delta = e.key === 'ArrowDown' ? 1 : -1;
      setState((s) =>
        s
          ? { ...s, index: moveMentionIndex(s.options.length, s.index, delta) }
          : s,
      );
      return true;
    }
    if ((e.key === 'Enter' || e.key === 'Tab') && state.options.length) {
      e.preventDefault();
      pick(state.options[state.index]!.login, e.currentTarget);
      return true;
    }
    if (e.key === 'Escape') {
      setState(null);
      return true;
    }
    return false;
  };

  return { state, sync, onKeyDown, pick };
}

/** The open menu itself — styled like the prototype's inline mention bar:
 *  one soft button per candidate, the keyboard-selected one highlighted. */
function MentionMenu({
  options,
  index,
  onPick,
}: {
  options: Person[];
  index: number;
  onPick: (login: string) => void;
}) {
  return (
    <div className="mention-menu flex flex-wrap items-center gap-1 px-1 text-xs text-muted">
      <span>mention:</span>
      {options.map((p, i) => (
        <Button
          key={p.login}
          size="sm"
          variant={i === index ? 'secondary' : 'ghost'}
          className="h-6 gap-1 rounded-full px-2 text-xs"
          onPress={() => onPick(p.login)}
        >
          @{p.login}
        </Button>
      ))}
      <span>↑↓ · Tab/⏎ select · Esc close</span>
    </div>
  );
}

/** Markdown, plus a GitHub-style suggestion block rendered as a diff line. */
export function Body({ text }: { text: string }) {
  const m = text.match(/^([\s\S]*?)```suggestion\n([\s\S]*?)\n```([\s\S]*)$/);
  if (!m) return <Md text={text} />;
  return (
    <div className="flex flex-col">
      {m[1]?.trim() && <Md text={m[1].trim()} />}
      <div className="suggestion my-1 overflow-hidden rounded-xl border border-separator font-mono">
        <div className="sug-h bg-surface-secondary px-2 py-0.5 font-sans text-xs text-muted">
          Suggested change
        </div>
        {m[2]!.split('\n').map((s, i) => (
          <div key={i} className="ln t-add">
            <span className="mark">+</span>
            <code className="src">{s}</code>
          </div>
        ))}
      </div>
      {m[3]?.trim() && <Md text={m[3].trim()} />}
    </div>
  );
}

export interface ReactionView {
  emoji: string;
  count: number;
  mine: boolean;
}

/** Merges the server's per-comment reaction counts with a pending local
 *  override (plan "Comments & threads"): `overrideOf` wins over the
 *  server's `viewer_reacted` until the next sync agrees with it — the
 *  override is additive/subtractive on `count` too, so a just-added
 *  reaction shows up immediately instead of waiting on a round trip.
 *  Returns one row per emoji in `REACTIONS`' canonical order, including
 *  zero-count ones (callers filter those out for display, but keep them
 *  for the "add a reaction" picker). */
export function mergeReactions(
  server: ReactionCount[] | undefined,
  overrideOf: (emoji: string) => boolean | undefined,
): ReactionView[] {
  return REACTIONS.map(([, emoji]) => {
    const row = server?.find((r) => r.emoji === emoji);
    const serverCount = row?.count ?? 0;
    const serverMine = row?.viewer_reacted ?? false;
    const override = overrideOf(emoji);
    const mine = override ?? serverMine;
    const delta = mine === serverMine ? 0 : mine ? 1 : -1;
    return { emoji, count: Math.max(0, serverCount + delta), mine };
  });
}

const Reactions = observer(function Reactions({
  pr,
  comment,
}: {
  pr: PrId;
  comment: Comment;
}) {
  const store = useRootStore();
  const [open, setOpen] = useState(false);
  const views = mergeReactions(comment.reactions, (emoji) =>
    store.ephemeral.reactionOverride(comment.id, emoji),
  );
  const shown = views.filter((v) => v.count > 0);

  // The override's only job is to bridge the gap until a synced `Comment`
  // reflects it; once it does, drop it rather than shadow real data.
  useEffect(() => {
    for (const { emoji, mine } of views) {
      const override = store.ephemeral.reactionOverride(comment.id, emoji);
      if (override === undefined) continue;
      const serverMine =
        comment.reactions?.find((r) => r.emoji === emoji)?.viewer_reacted ??
        false;
      if (serverMine === mine)
        store.ephemeral.clearReactionOverride(comment.id, emoji);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [comment.reactions, comment.id]);

  return (
    <div className="reactions flex flex-wrap items-center gap-1">
      {shown.map((v) => {
        const icon = REACTIONS.find(([, name]) => name === v.emoji)?.[0];
        return (
          <Button
            key={v.emoji}
            size="sm"
            variant="secondary"
            aria-label={v.emoji}
            className={`reaction h-6 gap-1 rounded-full px-2 text-xs ${v.mine ? 'ring-1 ring-accent' : ''}`}
            onPress={() =>
              void reactToComment(store, pr, comment.id, v.emoji, v.mine)
            }
          >
            {icon}
            <span className="tabular-nums">{v.count}</span>
          </Button>
        );
      })}
      <Popover isOpen={open} onOpenChange={setOpen}>
        <Button
          isIconOnly
          size="sm"
          variant="ghost"
          aria-label="Add reaction"
          className={`add-reaction size-6 min-w-6 text-muted ${shown.length ? '' : 'opacity-0 group-hover:opacity-100'} data-[pressed=true]:opacity-100`}
        >
          <FaceSmile className="size-3.5" />
        </Button>
        <Popover.Content placement="top start">
          <Popover.Dialog className="flex gap-0.5 p-1">
            {REACTIONS.map(([emoji, name]) => {
              const mine = views.find((v) => v.emoji === name)?.mine ?? false;
              return (
                <Button
                  key={name}
                  isIconOnly
                  size="sm"
                  variant="ghost"
                  aria-label={name}
                  className="size-8 min-w-8 text-base"
                  onPress={() => {
                    void reactToComment(store, pr, comment.id, name, mine);
                    setOpen(false);
                  }}
                >
                  {emoji}
                </Button>
              );
            })}
          </Popover.Dialog>
        </Popover.Content>
      </Popover>
    </div>
  );
});

/** One comment of a thread: reactions for everyone, edit/delete for your
 *  own, and "add to batch" / "commit" on a comment with a suggestion. */
export const CommentView = observer(function CommentView({
  pr,
  c,
}: {
  pr: PrId;
  c: Comment;
}) {
  const store = useRootStore();
  const [editing, setEditing] = useState(false);
  const [text, setText] = useState(c.body);
  const [busy, setBusy] = useState(false);
  const viewer = store.ephemeral.viewer;
  const mine = !!viewer && c.author.login === viewer.login;
  const sug = suggestionOf(c.body);
  const summary = store.models.prSummaries.get(pr);
  const writable = canCommitSuggestions(summary, viewer?.login);
  const inBatch = store.ephemeral.batchOf(pr).has(c.id);
  const mention = useMentionMenu(pr, text, setText);
  const editRef = useRef<HTMLTextAreaElement>(null);
  return (
    <div className="comment group flex gap-3">
      <Avatar person={c.author} size={24} />
      <div className="flex min-w-0 flex-1 flex-col gap-1">
        <div className="flex items-center gap-2 text-sm">
          <span className="font-medium">{c.author.name ?? c.author.login}</span>
          <span className="text-xs text-muted">
            {timeAgo(c.created_at)}
            {c.edited && ' · edited'}
          </span>
          <span className="flex-1" />
          {mine && !editing && (
            <Dropdown>
              <Button
                isIconOnly
                size="sm"
                variant="ghost"
                aria-label="Comment actions"
                className="comment-menu size-6 min-w-6 text-muted opacity-0 group-hover:opacity-100 data-[pressed=true]:opacity-100"
              >
                <Ellipsis className="size-4" />
              </Button>
              <Dropdown.Popover placement="bottom end">
                <Dropdown.Menu
                  aria-label="Comment"
                  onAction={(k) => {
                    if (k === 'edit') setEditing(true);
                    if (k === 'delete') void deleteComment(store, pr, c.id);
                  }}
                >
                  <Dropdown.Item id="edit" textValue="Edit">
                    Edit
                  </Dropdown.Item>
                  <Dropdown.Item
                    id="delete"
                    textValue="Delete"
                    className="text-danger"
                  >
                    Delete
                  </Dropdown.Item>
                </Dropdown.Menu>
              </Dropdown.Popover>
            </Dropdown>
          )}
        </div>
        {editing ? (
          <div className="flex flex-col gap-1.5">
            <TextArea
              ref={editRef}
              autoFocus
              rows={3}
              aria-label="Edit comment"
              value={text}
              onChange={(e) => {
                setText(e.target.value);
                mention.sync(e.target.value, e.target.selectionStart ?? 0);
              }}
              onKeyDown={(e) => {
                mention.onKeyDown(e);
              }}
              className="comment-edit text-sm"
            />
            {mention.state && (
              <MentionMenu
                options={mention.state.options}
                index={mention.state.index}
                onPick={(login) => mention.pick(login, editRef.current)}
              />
            )}
            <div className="flex justify-end gap-1.5">
              <Button
                size="sm"
                variant="ghost"
                onPress={() => {
                  setText(c.body);
                  setEditing(false);
                }}
              >
                Cancel
              </Button>
              <Button
                size="sm"
                isDisabled={!text.trim() || busy}
                onPress={() => {
                  setBusy(true);
                  void editComment(store, pr, c.id, text.trim())
                    .then(() => setEditing(false))
                    .finally(() => setBusy(false));
                }}
              >
                Save
              </Button>
            </div>
          </div>
        ) : (
          <div className="text-sm leading-relaxed">
            <Body text={c.body} />
          </div>
        )}
        {sug && writable && (
          <div className="sug-actions flex items-center gap-1.5">
            <Btn
              icon={<CodeCommit />}
              className="commit-suggestion"
              onPress={() => void commitSuggestions(store, pr, [c.id])}
            >
              Commit
            </Btn>
            <Btn
              tone="ghost"
              icon={<Layers />}
              className="batch-suggestion"
              onPress={() => store.ephemeral.toggleBatch(pr, c.id)}
            >
              {inBatch ? 'Remove from batch' : 'Add to batch'}
            </Btn>
          </div>
        )}
        <Reactions pr={pr} comment={c} />
      </div>
    </div>
  );
});

/** A review thread: collapsed to one line once resolved, same as the
 *  prototype; expands to the full comment list and a reply box. */
export const ThreadView = observer(function ThreadView({
  pr,
  id,
}: {
  pr: PrId;
  id: string;
}) {
  const store = useRootStore();
  const t = store.models.prDetails.get(pr)?.threads.find((th) => th.id === id);
  const [open, setOpen] = useState(() => !t?.resolved);
  const [text, setText] = useState('');
  const [busy, setBusy] = useState(false);
  const mention = useMentionMenu(pr, text, setText);
  const replyRef = useRef<HTMLTextAreaElement>(null);
  if (!t) return null;
  const people = [
    ...new Map(t.comments.map((c) => [c.author.login, c.author])).values(),
  ];
  const send = () => {
    const body = text.trim();
    if (!body) return;
    setBusy(true);
    void replyThread(store, pr, id, body)
      .then(() => setText(''))
      .finally(() => setBusy(false));
  };
  if (!open)
    return (
      <Card
        variant="secondary"
        className="thread collapsed max-w-[760px] cursor-pointer flex-row items-center gap-3 px-4 py-2.5 text-sm"
        onClick={() => setOpen(true)}
      >
        <CircleCheck className="size-4 shrink-0 text-success" />
        <AvatarGroup>
          {people.map((p) => (
            <Avatar key={p.login} person={p} size={20} />
          ))}
        </AvatarGroup>
        <span className="min-w-0 flex-1 truncate text-muted">
          {t.comments[0]!.author.login}:{' '}
          {stripHtmlComments(t.comments[0]!.body)}
        </span>
        <span className="shrink-0 text-xs text-muted">
          {plural(t.comments.length, 'comment', 'comments')}
        </span>
        <ChevronDown className="size-4 shrink-0 text-muted" />
      </Card>
    );
  return (
    <Card
      className={`thread max-w-[760px] gap-0 overflow-hidden p-0 ${t.resolved ? 'resolved' : ''}`}
    >
      <div className="flex w-full items-center gap-2 border-b border-separator px-4 py-2">
        <Chip
          size="sm"
          variant="soft"
          color={t.resolved ? 'success' : 'warning'}
          className="gap-1"
        >
          {t.resolved && <CircleCheck className="size-3.5" />}
          {t.resolved ? 'Resolved' : 'Open'}
        </Chip>
        <span className="text-xs text-muted">
          {plural(t.comments.length, 'comment', 'comments')}
        </span>
        <span className="flex-1" />
        <SendToAgent pr={pr} item={{ kind: 'thread', id }} />
        <Button
          size="sm"
          variant="ghost"
          className="gap-1.5"
          onPress={() => void resolveThread(store, pr, id, !t.resolved)}
        >
          <Check className="size-3.5" />
          {t.resolved ? 'Unresolve' : 'Resolve'}
        </Button>
        {t.resolved && (
          <Button
            isIconOnly
            size="sm"
            variant="ghost"
            aria-label="Collapse"
            onPress={() => setOpen(false)}
          >
            <ChevronUp className="size-4" />
          </Button>
        )}
      </div>
      <div className="flex flex-col gap-4 px-4 py-3">
        {t.comments.map((c, i) => (
          <CommentView key={c.id ?? i} pr={pr} c={c} />
        ))}
      </div>
      <div className="flex flex-col gap-1.5 border-t border-separator px-3 py-2">
        {mention.state && (
          <MentionMenu
            options={mention.state.options}
            index={mention.state.index}
            onPick={(login) => mention.pick(login, replyRef.current)}
          />
        )}
        <div className="flex items-center gap-1.5">
          <TextArea
            ref={replyRef}
            aria-label="Reply"
            rows={1}
            fullWidth
            value={text}
            placeholder="Reply…"
            className="min-w-0 flex-1 font-sans text-xs"
            onChange={(e) => {
              setText(e.target.value);
              mention.sync(e.target.value, e.target.selectionStart ?? 0);
            }}
            onKeyDown={(e) => {
              if (mention.onKeyDown(e)) return;
              if (
                e.key === 'Enter' &&
                (e.metaKey || e.ctrlKey) &&
                text.trim()
              ) {
                send();
              }
            }}
          />
          <Button
            isIconOnly
            size="sm"
            aria-label="Send reply"
            isPending={busy}
            isDisabled={!text.trim()}
            onPress={send}
          >
            <ArrowUp className="size-4" />
          </Button>
        </div>
      </div>
    </Card>
  );
});

/** A not-yet-submitted comment, held locally until "Submit review". */
function DraftViewInner({ d }: { d: Draft }) {
  const store = useRootStore();
  const [editing, setEditing] = useState(false);
  const [text, setText] = useState(d.body);
  const mention = useMentionMenu(d.pr, text, setText);
  const draftRef = useRef<HTMLTextAreaElement>(null);
  const label =
    d.anchor.start_line != null
      ? `${d.anchor.start_line}–${d.anchor.line}`
      : `${d.anchor.line}`;
  return (
    <Card className="draft min-w-0 max-w-[760px] gap-2 border border-warning/40 px-4 py-3">
      {/* In a narrow column (a split diff's half) the actions drop under
       *  the label instead of running off the card's edge. */}
      <div className="flex flex-wrap items-center gap-x-1.5 gap-y-1 text-xs text-muted">
        <span className="draft-label flex items-center gap-1.5 whitespace-nowrap">
          <Chip size="sm" variant="soft" color="warning">
            pending
          </Chip>
          draft{d.source.kind === 'agent' && <AgentTag id={d.source.agent} />}
          <span className="tabular-nums">
            · {d.anchor.start_line != null ? 'lines' : 'line'} {label}
          </span>
        </span>
        <span className="draft-actions ml-auto flex items-center gap-1.5">
          {!editing && (
            <SendToAgent pr={d.pr} item={{ kind: 'draft', id: d.id }} />
          )}
          {editing ? (
            <Button
              size="sm"
              variant="ghost"
              onPress={() => {
                editDraft(store, d.id, text);
                setEditing(false);
              }}
            >
              save
            </Button>
          ) : (
            <Button size="sm" variant="ghost" onPress={() => setEditing(true)}>
              edit
            </Button>
          )}
          <Button
            size="sm"
            variant="ghost"
            onPress={() => removeDraft(store, d.id)}
          >
            delete
          </Button>
        </span>
      </div>
      {editing ? (
        <div className="flex flex-col gap-1.5">
          <TextArea
            ref={draftRef}
            fullWidth
            aria-label="draft"
            value={text}
            onChange={(e) => {
              setText(e.target.value);
              mention.sync(e.target.value, e.target.selectionStart ?? 0);
            }}
            onKeyDown={(e) => mention.onKeyDown(e)}
            rows={4}
          />
          {mention.state && (
            <MentionMenu
              options={mention.state.options}
              index={mention.state.index}
              onPick={(login) => mention.pick(login, draftRef.current)}
            />
          )}
        </div>
      ) : (
        <Body text={d.body} />
      )}
    </Card>
  );
}

/** Composer for a new draft, anchored to one line or a shift-click range. */
function ComposerInner({
  pr,
  anchor,
  lineText,
  askAi = true,
  onClose,
}: {
  pr: PrId;
  anchor: Anchor;
  /** The code the anchor covers, every line of a range: what Suggestion
   *  starts from, since committing it replaces exactly those lines. */
  lineText: string;
  /** Offer "Ask AI"; off where there's no AI panel to answer in. */
  askAi?: boolean;
  onClose: () => void;
}) {
  const store = useRootStore();
  const [text, setText] = useState('');
  const [preview, setPreview] = useState(false);
  const mention = useMentionMenu(pr, text, setText);
  const textRef = useRef<HTMLTextAreaElement>(null);
  const submit = () => {
    const body = text.trim();
    if (!body) return;
    addDraft(store, pr, anchor, body);
    onClose();
  };
  return (
    <Card className="composer max-w-[760px] gap-1.5 p-2">
      <div className="flex flex-wrap items-center gap-2 px-1 text-xs text-muted">
        <Tabs
          data-plain
          selectedKey={preview ? 'preview' : 'write'}
          onSelectionChange={(k) => setPreview(k === 'preview')}
          className="w-fit"
        >
          <Tabs.ListContainer>
            <Tabs.List aria-label="Comment" className="composer-tabs">
              <Tabs.Tab id="write" className="h-6 px-2.5 text-xs">
                Write
                <Tabs.Indicator />
              </Tabs.Tab>
              <Tabs.Tab id="preview" className="h-6 px-2.5 text-xs">
                <Tabs.Separator />
                Preview
                <Tabs.Indicator />
              </Tabs.Tab>
            </Tabs.List>
          </Tabs.ListContainer>
        </Tabs>
        <span className="composer-range font-medium text-foreground">
          {anchor.start_line != null ? 'Lines' : 'Line'}{' '}
          {anchor.start_line != null
            ? `${anchor.start_line}–${anchor.line}`
            : anchor.line}
        </span>
        <span>
          <Kbd className="text-[10px]">⇧</Kbd> + click{' '}
          <Plus className="inline size-3" /> on another line —{' '}
          {anchor.start_line != null ? 'extend' : 'range'}
        </span>
      </div>
      {preview ? (
        <div className="composer-preview min-h-[76px] rounded-xl border border-separator px-3 py-2 text-sm">
          {text.trim() ? (
            <Body text={text} />
          ) : (
            <span className="text-muted">Nothing to preview</span>
          )}
        </div>
      ) : (
        <TextArea
          ref={textRef}
          autoFocus
          fullWidth
          rows={3}
          aria-label="comment"
          value={text}
          placeholder="Comment… markdown, ⌘⏎ to add to review, @ to mention"
          className="font-mono text-xs"
          onChange={(e) => {
            setText(e.target.value);
            mention.sync(e.target.value, e.target.selectionStart ?? 0);
          }}
          onKeyDown={(e) => {
            if (mention.onKeyDown(e)) return;
            if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) submit();
            if (e.key === 'Escape') onClose();
          }}
        />
      )}
      {!preview && mention.state && (
        <MentionMenu
          options={mention.state.options}
          index={mention.state.index}
          onPick={(login) => mention.pick(login, textRef.current)}
        />
      )}
      <div className="flex items-center gap-1.5">
        <Button
          size="sm"
          variant="ghost"
          onPress={() =>
            setText(
              (t) =>
                `${t}${t ? '\n' : ''}\`\`\`suggestion\n${lineText}\n\`\`\``,
            )
          }
        >
          Suggestion
        </Button>
        {askAi && (
          <Button
            size="sm"
            variant="ghost"
            onPress={() => {
              // The box's text is the question; with none, the line goes to
              // the AI panel's composer to ask there — nothing canned.
              const question = text.trim();
              if (question) void askAboutLine(store, { pr, anchor, question });
              else referenceLine(store, pr, anchor);
            }}
          >
            Ask AI
          </Button>
        )}
        <span className="flex-1" />
        <Button size="sm" variant="ghost" onPress={onClose}>
          Cancel
        </Button>
        <Btn tone="primary" kbd="⌘⏎" isDisabled={!text.trim()} onPress={submit}>
          Add to review
        </Btn>
      </div>
    </Card>
  );
}

export const DraftView = observer(DraftViewInner);
export const Composer = observer(ComposerInner);
