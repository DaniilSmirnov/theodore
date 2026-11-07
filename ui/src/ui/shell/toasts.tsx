/** One `EphemeralStore` toast onto HeroUI's imperative queue. */
import { Button, toast } from '@heroui/react';
import type { Toast } from '../../stores/EphemeralStore';

/** A toast with two choices waits longer: it asks something. */
export const CHOICE_TIMEOUT_MS = 15000;

/** The open toast of each `group`. */
const openInGroup = new Map<string, string>();

/** The latest a toast that asks nothing stays, whatever paused HeroUI's own
 *  countdown (a pointer resting on the stack, a focus left inside it): news
 *  and a progress note ("Merging #14…") never sit over buttons for minutes. */
export const HARD_CLOSE_MS = {
  info: 8000,
  warn: 15000,
  error: 15000,
} as const satisfies Record<Toast['level'], number>;

export function showToast(t: Toast): string {
  const key = showOne(t);
  if (!t.secondary) {
    const ms = HARD_CLOSE_MS[t.level] + (t.action ? 5000 : 0);
    setTimeout(() => toast.close(key), ms);
  }
  if (t.group) {
    const older = openInGroup.get(t.group);
    if (older && older !== key) toast.close(older);
    openInGroup.set(t.group, key);
  }
  return key;
}

/** A failed command is red, a warning amber; news stays neutral. */
const VARIANT = {
  info: 'default',
  warn: 'warning',
  error: 'danger',
} as const satisfies Record<Toast['level'], string>;

function showOne(t: Toast): string {
  if (!t.secondary) {
    return toast(t.text, {
      variant: VARIANT[t.level],
      timeout: t.action ? 5000 : 3200,
      actionProps: t.action
        ? { children: t.action.label, onPress: t.action.onAction }
        : undefined,
    });
  }
  // HeroUI has one action slot: the second choice sits under the text.
  // Either choice answers the question, so it closes the toast.
  let key = '';
  const choose = (fn: () => void) => () => {
    toast.close(key);
    fn();
  };
  const secondary = t.secondary;
  key = toast(t.text, {
    timeout: CHOICE_TIMEOUT_MS,
    actionProps: t.action
      ? { children: t.action.label, onPress: choose(t.action.onAction) }
      : undefined,
    description: (
      <Button
        size="sm"
        variant="ghost"
        className="toast-secondary -ml-2 h-6 px-2 text-muted"
        onPress={choose(secondary.onAction)}
      >
        {secondary.label}
      </Button>
    ),
  });
  return key;
}
