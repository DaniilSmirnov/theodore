/** Native notifications and the dock badge, through the desktop shell.
 *  Outside the desktop window (tests, dev in a browser tab) both are
 *  no-ops, like `openExternal`'s fallback. */
import { isTauri } from '@tauri-apps/api/core';
import { getTransport } from './transport';

/** The shell's event for a click on a space's notification; the payload
 *  is the space id. The shell has brought the window forward already. */
export const OPEN_SPACE_EVENT = 'open-space';

/** The space a link asked for before the UI listened for
 *  `OPEN_SPACE_EVENT` (a link that launched the app); later ones come as
 *  that event. */
export async function takePendingSpace(): Promise<string | null> {
  if (!isTauri()) return null;
  return getTransport().invoke<string | null>('take_pending_space');
}

let permission: Promise<boolean> | null = null;

/** Asks once per session; macOS remembers the answer. */
function allowed(): Promise<boolean> {
  permission ??= (async () => {
    const { isPermissionGranted, requestPermission } =
      await import('@tauri-apps/plugin-notification');
    if (await isPermissionGranted()) return true;
    return (await requestPermission()) === 'granted';
  })().catch(() => false);
  return permission;
}

/** Shows a notification about `space`. `true` when a click on it comes
 *  back as `OPEN_SPACE_EVENT` (macOS, from the app bundle); `false` when
 *  it can't, or nothing was shown. */
export async function notifySpace(
  space: string,
  title: string,
  body?: string,
): Promise<boolean> {
  if (!isTauri() || !(await allowed())) return false;
  return getTransport().invoke<boolean>('notify_space', {
    space,
    title,
    body: body ?? null,
  });
}

/** The number on the app's dock icon; 0 clears it. */
export async function setBadge(count: number): Promise<void> {
  if (!isTauri()) return;
  const { getCurrentWindow } = await import('@tauri-apps/api/window');
  await getCurrentWindow().setBadgeCount(count > 0 ? count : undefined);
}
