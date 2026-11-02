/** One `EphemeralStore` toast onto HeroUI's imperative queue. */
import { Button, toast } from '@heroui/react';
import type { Toast } from '../../stores/EphemeralStore';

/** A toast with two choices waits longer: it asks something. */
export const CHOICE_TIMEOUT_MS = 15000;

export function showToast(t: Toast): string {
  if (!t.secondary) {
    return toast(t.text, {
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
