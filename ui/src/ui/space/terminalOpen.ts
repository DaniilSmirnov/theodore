/** Whether a space's terminal drawer is open, remembered per space: closing
 *  it (its ×, the toggle, ⌃`) keeps it closed on the next visit. A space
 *  never toggled takes the last choice made in any space. */
import { STORAGE_NS } from '../../stores/UiStore';

const LAST = `${STORAGE_NS}theodore-space-terminal`;
const key = (space: string) => `${LAST}:${space}`;

export function terminalOpenFor(space: string): boolean {
  const own = localStorage.getItem(key(space));
  return (own ?? localStorage.getItem(LAST)) === '1';
}

export function setTerminalOpenFor(space: string, open: boolean): void {
  localStorage.setItem(key(space), open ? '1' : '0');
  localStorage.setItem(LAST, open ? '1' : '0');
}
