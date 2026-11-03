/**
 * `@terminal` in the space composer (docs/spaces.md §3, §11): typing it
 * (confirmed by a trailing space, like `/skill`) attaches the space's most
 * recently active terminal's last ~200 lines — the same way a dropped
 * file does (`save_attachment`, then a chip in the strip and, once sent,
 * in the transcript). No popover: one fixed word, always available.
 */
import { api } from '../../core/api';
import type { Attachments } from '../attachments';

/** `@terminal` followed by whitespace — typing it is the confirmation,
 *  the way `/skill ` already works in this composer. */
const MENTION = /@terminal(?=\s)/;

/** `text` with the first confirmed `@terminal` (and the space after it)
 *  removed, or `text` unchanged if there isn't one. */
export function stripTerminalMention(text: string): {
  text: string;
  found: boolean;
} {
  const m = MENTION.exec(text);
  if (!m) return { text, found: false };
  const start = m.index;
  const end = start + m[0].length + 1; // + the trailing whitespace char
  return { text: text.slice(0, start) + text.slice(end), found: true };
}

/** Bytes → base64 (standard alphabet), as `save_attachment` wants. */
function base64FromText(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let binary = '';
  for (const b of bytes) binary += String.fromCharCode(b);
  return btoa(binary);
}

/** Resolves the mention against `space` and, if it has an open terminal,
 *  saves its tail as a text attachment into `files`. A toast, not a
 *  silent no-op, when the space has none open. */
export async function attachTerminalMention(
  space: string,
  files: Attachments,
  toast: (text: string) => void,
): Promise<void> {
  const mention = await api.spaceTerminalMention(space).catch(() => null);
  if (!mention) {
    toast('No terminal is open in this space');
    return;
  }
  const saved = await api
    .saveAttachment({
      name: `${mention.title}.log`,
      mime: 'text/plain',
      data: base64FromText(mention.lines.join('\n')),
    })
    .catch(() => null);
  if (saved) files.addSaved(saved);
}
