/**
 * The space's queue (docs/spaces.md §8): messages sent while a turn ran,
 * below it in the order they send, each with Edit and Cancel; a held
 * queue (after Stop, a failed turn, or a restart) also has Send.
 */
import { useState } from 'react';
import { Button, TextArea } from '@heroui/react';
import type { ChatMessage, QueuedAction, Space } from '../../gen/contract';
import { api } from '../../core/api';
import { MessageAttachments } from '../attachments';
import { heldReason } from './queue';

export function QueuedMessages({
  space,
  queued,
}: {
  space: Space;
  queued: ChatMessage[];
}) {
  if (!queued.length) return null;
  const held = heldReason(space, queued);
  return (
    <div className="space-queue flex flex-col gap-3">
      {held && <span className="text-xs text-warning">{held}</span>}
      {queued.map((m) => (
        <QueuedMessage key={m.id} space={space} m={m} />
      ))}
    </div>
  );
}

function QueuedMessage({ space, m }: { space: Space; m: ChatMessage }) {
  const [editing, setEditing] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const held = !!m.queued?.held;
  const archived = space.status === 'archived';
  const act = (action: QueuedAction) => {
    setBusy(true);
    void api
      .spaceQueued({ space: space.id, message: m.id, action })
      .then(() => setEditing(null))
      .catch(() => {})
      .finally(() => setBusy(false));
  };
  const save = () => {
    const text = editing?.trim() ?? '';
    if (text === m.text) setEditing(null);
    else act({ kind: 'edit', text });
  };
  return (
    <div className="queued-message flex flex-col gap-1 opacity-70">
      <span className="flex items-center gap-1.5 text-xs text-muted">
        you
        <span className="rounded-full bg-default px-1.5 text-[10px]">
          {held ? 'held' : 'queued'}
        </span>
        <span className="flex-1" />
        {editing === null && !archived && (
          <>
            {(held || space.status !== 'running') && (
              <Button
                size="sm"
                variant="tertiary"
                className="h-6 px-2 text-xs"
                isDisabled={busy}
                onPress={() => act({ kind: 'send' })}
              >
                Send
              </Button>
            )}
            <Button
              size="sm"
              variant="tertiary"
              className="h-6 px-2 text-xs"
              isDisabled={busy}
              onPress={() => setEditing(m.text)}
            >
              Edit
            </Button>
          </>
        )}
        {editing === null && (
          <Button
            size="sm"
            variant="tertiary"
            className="h-6 px-2 text-xs"
            isDisabled={busy}
            onPress={() => act({ kind: 'cancel' })}
          >
            Cancel
          </Button>
        )}
      </span>
      {editing === null ? (
        <span className="whitespace-pre-wrap text-sm leading-relaxed">
          {m.text}
        </span>
      ) : (
        <div className="flex flex-col gap-1.5">
          <TextArea
            aria-label="Edit queued message"
            rows={2}
            fullWidth
            autoFocus
            value={editing}
            className="text-sm"
            onChange={(e) => setEditing(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && !e.shiftKey) {
                e.preventDefault();
                save();
              } else if (e.key === 'Escape') {
                e.stopPropagation();
                setEditing(null);
              }
            }}
          />
          <span className="flex justify-end gap-1.5">
            <Button
              size="sm"
              variant="tertiary"
              className="h-6 px-2 text-xs"
              onPress={() => setEditing(null)}
            >
              Discard
            </Button>
            <Button
              size="sm"
              className="h-6 px-2 text-xs"
              isDisabled={busy || (!editing.trim() && !m.attachments?.length)}
              onPress={save}
            >
              Save
            </Button>
          </span>
        </div>
      )}
      <MessageAttachments attachments={m.attachments} />
    </div>
  );
}
