/** What a space says about one of its messages, as plain values a row can
 *  compare: everything that changes how that message's row looks. */
import type { ChatMessage, Space, SpaceCheck } from '../../gen/contract';
import { checksByReply } from './commands';
import {
  actionablePlan,
  plansByReply,
  undoState,
  undoneMessages,
} from './spaces';

export interface MessageView {
  undone: boolean;
  /** Undo sits here: the reply that ended the last turn. */
  undoHere: boolean;
  /** Why that Undo is disabled. */
  undoReason: string | null;
  plan: boolean;
  /** Approve / Revise sit here. */
  planHere: boolean;
  /** "Back to here" sits here. */
  back: boolean;
  check: SpaceCheck | null;
}

export function messageView(
  space: Space,
  m: Pick<ChatMessage, 'id' | 'author'>,
): MessageView {
  const agent = m.author.kind === 'agent';
  const undone = undoneMessages(space).has(m.id);
  const undo = undoState(space);
  const undoHere = undo.turn?.reply === m.id && space.status !== 'archived';
  return {
    undone,
    undoHere,
    undoReason: undoHere && !undo.ok ? undo.reason : null,
    plan: agent && !undone && plansByReply(space.turn_log).has(m.id),
    planHere: agent && actionablePlan(space) === m.id,
    back: agent && !undone && !undoHere,
    check: agent ? (checksByReply(space.turn_log).get(m.id) ?? null) : null,
  };
}
