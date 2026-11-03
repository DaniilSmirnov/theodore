/** When each space page was last looked at — local only, like read marks
 *  of the sidebar, so a finished turn can show as "waiting for you". */
import { observable, runInAction } from 'mobx';
import { STORAGE_NS } from '../../stores/UiStore';

const LS_SEEN = `${STORAGE_NS}theodore-space-seen`;

function load(): Record<string, number> {
  try {
    const v = JSON.parse(localStorage.getItem(LS_SEEN) ?? '{}') as unknown;
    return v && typeof v === 'object' ? (v as Record<string, number>) : {};
  } catch {
    return {};
  }
}

export const spaceSeen = observable.map<string, number>(load());

export function markSpaceSeen(id: string, at: number): void {
  if ((spaceSeen.get(id) ?? 0) >= at) return;
  runInAction(() => spaceSeen.set(id, at));
  localStorage.setItem(
    LS_SEEN,
    JSON.stringify(Object.fromEntries(spaceSeen.entries())),
  );
}

/** "Mark as unread" (context menu / ⌘K): forgets the space was seen, so
 *  it shows the ● glyph and counts toward the dock badge again, without
 *  waiting for a new turn. */
export function markSpaceUnseen(id: string): void {
  if (!spaceSeen.has(id)) return;
  runInAction(() => spaceSeen.delete(id));
  localStorage.setItem(
    LS_SEEN,
    JSON.stringify(Object.fromEntries(spaceSeen.entries())),
  );
}
