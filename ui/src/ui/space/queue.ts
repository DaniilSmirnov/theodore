/**
 * Sending while a space's agent works (docs/spaces.md §8): pure rules for
 * the queue under the running turn and for "Send now".
 */
import type {
  AgentId,
  ChatMessage,
  Space,
  SpaceMode,
} from '../../gen/contract';

/** The transcript by time, and the queue in the order it sends. */
export function splitQueue(messages: readonly ChatMessage[]): {
  sent: ChatMessage[];
  queued: ChatMessage[];
} {
  const sent = messages.filter((m) => !m.queued).sort((a, b) => a.at - b.at);
  const queued = messages
    .filter((m) => !!m.queued)
    .sort((a, b) => a.queued!.seq - b.queued!.seq);
  return { sent, queued };
}

/** Why the queue waits for you, or `null` while it sends on its own. */
export function heldReason(
  space: Pick<Space, 'status'>,
  queued: readonly ChatMessage[],
): string | null {
  if (!queued.some((m) => m.queued?.held)) return null;
  if (space.status === 'archived') return 'Unarchive the space to send these.';
  return 'Not sent: the turn before them stopped or failed, or Theodore restarted. Send or drop each one.';
}

/** "Send now" can go into the running turn: a Claude turn, and the message
 *  changes nothing a running agent can't take (agent, mode, files). */
export function canSendNow(
  space: Pick<Space, 'status' | 'agent' | 'mode'>,
  next: { agent: AgentId; mode: SpaceMode; files: number },
): boolean {
  return (
    space.status === 'running' &&
    space.agent === 'claude' &&
    next.agent === space.agent &&
    next.mode === space.mode &&
    next.files === 0
  );
}
