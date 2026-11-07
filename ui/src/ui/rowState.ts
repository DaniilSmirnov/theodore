/** What a chat row needs to outlive being scrolled out of view, and to change
 *  height without moving what you clicked. */
import { createContext, useCallback, useContext, useState } from 'react';

/** Whether something in a row is open, by its key: a row out of view is gone,
 *  and so is leaving the space. In memory, closed by default. */
const kept = new Map<string, boolean>();

/** `useState` for an open flag, kept per `key`. No key: this mount's only. */
export function useKeptOpen(key: string | null) {
  const [open, setOpen] = useState(() =>
    key == null ? false : (kept.get(key) ?? false),
  );
  const set = useCallback(
    (next: boolean) => {
      if (key != null) kept.set(key, next);
      setOpen(next);
    },
    [key],
  );
  return [open, set] as const;
}

export type Steady = (change: () => void) => void;

export const SteadyContext = createContext<Steady | null>(null);
const unsteady: Steady = (change) => change();

/** For a row's own toggle: `steady(() => setOpen(...))` keeps what you clicked
 *  where it is. Outside a virtual chat it just makes the change. */
export function useSteadyChange(): Steady {
  return useContext(SteadyContext) ?? unsteady;
}
