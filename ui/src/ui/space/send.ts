/**
 * What you do to a turn shows before the core answers: starting a turn takes
 * it a while (the worktree, the prompt), and the row must not wait.
 *
 * ```text
 * send ──▶ your message + working dots, at once
 *            ├─ its row arrives ─▶ it takes over, in the same chat row
 *            └─ refused ─────────▶ gone, the text back in the box
 * stop ──▶ "Stopping…" on the row, at once ─▶ the reply takes over
 * ```
 */
import { reaction } from 'mobx';
import { api } from '../../core/api';
import type {
  AgentId,
  Attachment,
  ChatMessage,
  Space,
  SpaceSendRequest,
} from '../../gen/contract';
import type { RootStore } from '../../stores/RootStore';
import type { SpaceSend } from '../../stores/EphemeralStore';
import { spaceChatKey } from './spaces';

/** How long a send the core accepted may wait for its row. */
const LANDS_WITHIN_MS = 30_000;

let sends = 0;

/** A message not stored yet, as the row that shows it. */
export function sendingMessage(space: string, send: SpaceSend): ChatMessage {
  return {
    id: send.key,
    pr: spaceChatKey(space),
    author: { kind: 'me' },
    text: send.text,
    refs: [],
    at: send.at,
    failed: null,
    attachments: send.attachments,
    ...(send.queued ? { queued: { seq: send.at, held: false } } : {}),
  };
}

/** Not stored yet: nothing the core could act on. */
export const isSending = (m: Pick<ChatMessage, 'id'>) =>
  m.id.startsWith('sending-');

/** Sends `req`, showing it at once. Rejects (already toasted by api.ts) when
 *  the core refuses it, with nothing left of it in the chat. */
export function sendToSpace(
  store: RootStore,
  space: Pick<Space, 'id' | 'status'>,
  req: SpaceSendRequest,
  show: { now: boolean; agent: AgentId; attachments: Attachment[] },
): Promise<void> {
  const { ephemeral } = store;
  const chat = spaceChatKey(space.id);
  const rows = () => store.models.chatMessages.byPr(chat);
  const known = new Set(rows().map((m) => m.id));
  const waiting = ephemeral.spaceSends.get(space.id) ?? [];
  const send: SpaceSend = {
    key: `sending-${(sends += 1)}`,
    text: req.text,
    attachments: show.attachments,
    agent: show.agent,
    at: Date.now(),
    // A turn is about to start: this one waits behind it, as the core will.
    queued:
      !show.now &&
      (space.status === 'running' || waiting.some((s) => !s.queued)),
  };
  ephemeral.addSpaceSend(space.id, send);

  let id: string | null = null;
  // Not a row another send of the same text already took.
  const isOurs = (m: ChatMessage) =>
    id != null
      ? m.id === id
      : m.author.kind === 'me' &&
        m.text === send.text &&
        !known.has(m.id) &&
        !ephemeral.turnKeys.has(m.id);
  let timeout: ReturnType<typeof setTimeout> | undefined;
  const stop = reaction(
    () => rows().find(isOurs)?.id ?? null,
    (landed) => {
      if (landed == null) return;
      stop();
      clearTimeout(timeout);
      ephemeral.landSpaceSend(space.id, send.key, landed);
    },
  );
  const drop = () => {
    stop();
    clearTimeout(timeout);
    ephemeral.dropSpaceSend(space.id, send.key);
  };
  return (show.now ? api.spaceSendNow : api.spaceSend)(req).then(
    (stored) => {
      id = stored;
      const row = rows().find((m) => m.id === stored);
      if (row && ephemeral.spaceSends.get(space.id)?.includes(send)) {
        stop();
        ephemeral.landSpaceSend(space.id, send.key, row.id);
      } else if (ephemeral.spaceSends.get(space.id)?.includes(send)) {
        timeout = setTimeout(drop, LANDS_WITHIN_MS);
      }
    },
    (e: unknown) => {
      drop();
      throw e;
    },
  );
}

/** Stops the running turn; the row says so before the turn has ended. */
export function stopSpace(
  store: RootStore,
  space: Pick<Space, 'id' | 'run'>,
): void {
  const { run } = space;
  if (run) store.ephemeral.markStopping(run, true);
  void api.spaceStop(space.id).catch(() => {
    if (run) store.ephemeral.markStopping(run, false);
  });
}
