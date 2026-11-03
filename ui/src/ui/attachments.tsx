/**
 * Files in a chat: images, PDFs, anything. The 📎 button, a paste or a
 * drop adds them to the box; each is saved by the core (`save_attachment`)
 * and goes with the next message, which keeps them (`ChatMessage.
 * attachments`). The agent reads them from disk.
 */
import { useEffect, useRef, useState, type ReactNode } from 'react';
import { FileText, Paperclip, Xmark } from '@gravity-ui/icons';
import { Button, Spinner, Tooltip } from '@heroui/react';
import { api } from '../core/api';
import type { Attachment } from '../gen/contract';

/** A file on its way into the box. */
export type Pending = {
  key: string;
  name: string;
  mime: string;
  /** An object URL while the box holds an image. */
  preview: string | null;
  saved: Attachment | null;
  error: string | null;
};

export type Attachments = {
  items: Pending[];
  add: (files: FileList | File[]) => void;
  /** Already saved by the caller (e.g. `@terminal`'s tail) — straight into
   *  the strip, no upload round trip. */
  addSaved: (a: Attachment) => void;
  remove: (key: string) => void;
  clear: () => void;
  /** Saved ones' ids, for the request. */
  ids: string[];
  /** Saved ones, for the message shown before the core echoes it. */
  saved: Attachment[];
  /** Still saving: hold the send. */
  busy: boolean;
};

const MAX = 25 * 1024 * 1024;

function base64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const r = new FileReader();
    r.onload = () => {
      const url = String(r.result);
      resolve(url.slice(url.indexOf(',') + 1));
    };
    r.onerror = () => reject(r.error ?? new Error('read failed'));
    r.readAsDataURL(file);
  });
}

let seq = 0;

export function useAttachments(): Attachments {
  const [items, setItems] = useState<Pending[]>([]);
  const live = useRef(items);
  useEffect(() => {
    live.current = items;
  }, [items]);
  // Object URLs go when the box does.
  useEffect(
    () => () => {
      for (const p of live.current)
        if (p.preview) URL.revokeObjectURL(p.preview);
    },
    [],
  );

  const patch = (key: string, next: Partial<Pending>) =>
    setItems((list) =>
      list.map((p) => (p.key === key ? { ...p, ...next } : p)),
    );

  const add = (files: FileList | File[]) => {
    for (const file of Array.from(files)) {
      const key = `att-${++seq}`;
      const mime = file.type || 'application/octet-stream';
      const preview = mime.startsWith('image/')
        ? URL.createObjectURL(file)
        : null;
      const name =
        file.name || (mime.startsWith('image/') ? 'image.png' : 'file');
      setItems((list) => [
        ...list,
        { key, name, mime, preview, saved: null, error: null },
      ]);
      if (file.size > MAX) {
        patch(key, { error: 'Over 25 MB' });
        continue;
      }
      void base64(file)
        .then((data) => api.saveAttachment({ name, mime, data }))
        .then((saved) => patch(key, { saved }))
        .catch((e: unknown) =>
          patch(key, { error: e instanceof Error ? e.message : 'Not saved' }),
        );
    }
  };

  const addSaved = (a: Attachment) => {
    const key = `att-${++seq}`;
    setItems((list) => [
      ...list,
      { key, name: a.name, mime: a.mime, preview: null, saved: a, error: null },
    ]);
  };

  const remove = (key: string) =>
    setItems((list) => {
      const gone = list.find((p) => p.key === key);
      if (gone?.preview) URL.revokeObjectURL(gone.preview);
      return list.filter((p) => p.key !== key);
    });

  const clear = () =>
    setItems((list) => {
      for (const p of list) if (p.preview) URL.revokeObjectURL(p.preview);
      return [];
    });

  const saved = items.flatMap((p) => (p.saved ? [p.saved] : []));
  return {
    items,
    add,
    addSaved,
    remove,
    clear,
    ids: saved.map((a) => a.id),
    saved,
    busy: items.some((p) => !p.saved && !p.error),
  };
}

/** The 📎 button: a file picker, several at once. */
export function AttachButton({
  onFiles,
  isDisabled,
}: {
  onFiles: (files: FileList) => void;
  isDisabled?: boolean;
}) {
  const input = useRef<HTMLInputElement>(null);
  return (
    <>
      <Tooltip>
        <Button
          isIconOnly
          size="sm"
          variant="tertiary"
          aria-label="Attach files"
          className="attach-btn shrink-0 rounded-full"
          isDisabled={isDisabled}
          onPress={() => input.current?.click()}
        >
          <Paperclip className="size-4" />
        </Button>
        <Tooltip.Content>Attach files · or paste, or drop</Tooltip.Content>
      </Tooltip>
      <input
        ref={input}
        type="file"
        multiple
        hidden
        onChange={(e) => {
          if (e.currentTarget.files?.length) onFiles(e.currentTarget.files);
          e.currentTarget.value = '';
        }}
      />
    </>
  );
}

function Tile({
  name,
  image,
  children,
  onRemove,
  title,
}: {
  name: string;
  image: string | null;
  children?: ReactNode;
  onRemove?: () => void;
  title?: string;
}) {
  return (
    <span
      className="attachment group relative flex h-14 max-w-48 shrink-0 items-center gap-2 overflow-hidden rounded-xl border border-separator bg-surface pe-2"
      title={title ?? name}
    >
      {image ? (
        <img src={image} alt={name} className="h-full w-14 object-cover" />
      ) : (
        <span className="grid h-full w-12 place-items-center bg-default">
          <FileText className="size-5 text-muted" />
        </span>
      )}
      <span className="min-w-0 truncate text-xs">{name}</span>
      {children}
      {onRemove && (
        <button
          type="button"
          aria-label={`Remove ${name}`}
          className="absolute end-1 top-1 hidden size-5 place-items-center rounded-full bg-foreground/70 text-background group-hover:grid"
          onClick={onRemove}
        >
          <Xmark className="size-3" />
        </button>
      )}
    </span>
  );
}

/** What's in the box, above the text: each with ×. */
export function AttachmentStrip({ at }: { at: Attachments }) {
  if (!at.items.length) return null;
  return (
    <div className="attachment-strip flex w-full gap-2 overflow-x-auto px-3 pb-1">
      {at.items.map((p) => (
        <Tile
          key={p.key}
          name={p.name}
          image={p.preview}
          title={p.error ? `${p.name}: ${p.error}` : p.name}
          onRemove={() => at.remove(p.key)}
        >
          {!p.saved && !p.error && <Spinner size="sm" />}
          {p.error && <span className="text-xs text-danger">!</span>}
        </Tile>
      ))}
    </div>
  );
}

const dataUrls = new Map<string, Promise<string>>();

function useDataUrl(a: Attachment): string | null {
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    if (!a.mime.startsWith('image/')) return;
    let live = true;
    let p = dataUrls.get(a.id);
    if (!p) {
      const fetched = api.attachmentData(a.id);
      fetched.catch(() => dataUrls.delete(a.id));
      dataUrls.set(a.id, fetched);
      p = fetched;
    }
    void p.then((u) => live && setUrl(u)).catch(() => {});
    return () => {
      live = false;
    };
  }, [a.id, a.mime]);
  return url;
}

function SentTile({ a }: { a: Attachment }) {
  const url = useDataUrl(a);
  const [big, setBig] = useState(false);
  return (
    <>
      <button
        type="button"
        className="text-start"
        onClick={() => url && setBig(true)}
        aria-label={a.name}
      >
        <Tile name={a.name} image={url} />
      </button>
      {big && url && (
        <div
          role="dialog"
          aria-label={a.name}
          className="fixed inset-0 z-50 grid place-items-center bg-black/70 p-8"
          onClick={() => setBig(false)}
        >
          <img
            src={url}
            alt={a.name}
            className="max-h-full max-w-full rounded-xl"
          />
        </div>
      )}
    </>
  );
}

/** A sent message's files. */
export function MessageAttachments({
  attachments,
  align = 'start',
}: {
  attachments: Attachment[] | null | undefined;
  align?: 'start' | 'end';
}) {
  if (!attachments?.length) return null;
  return (
    <div
      className={`message-attachments flex flex-wrap gap-2 ${align === 'end' ? 'justify-end' : ''}`}
    >
      {attachments.map((a) => (
        <SentTile key={a.id} a={a} />
      ))}
    </div>
  );
}

/** Paste and drop onto a box: files become attachments, text stays text. */
export function fileHandlers(add: (files: File[]) => void) {
  return {
    onPaste: (e: React.ClipboardEvent) => {
      const files = Array.from(e.clipboardData.files);
      if (files.length) {
        e.preventDefault();
        add(files);
      }
    },
    onDragOver: (e: React.DragEvent) => {
      if (e.dataTransfer.types.includes('Files')) e.preventDefault();
    },
    onDrop: (e: React.DragEvent) => {
      const files = Array.from(e.dataTransfer.files);
      if (files.length) {
        e.preventDefault();
        add(files);
      }
    },
  };
}
