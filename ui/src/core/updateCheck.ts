/** "Check for Updates…" (app menu): one toast that says what's happening —
 *  checking, downloading, up to date, or why the check failed. The shell's
 *  `check_update` downloads and installs before it answers; the `update`
 *  event it sends first (`downloaded: false`) is what flips the toast to
 *  "Downloading…". Once an update is installed the toast goes away: the
 *  "Restart to update" pill (`UpdateReady`) says it, once. */
import { toast as heroToast } from '@heroui/react';
import { reaction } from 'mobx';
import type { RootStore } from '../stores/RootStore';
import { api, CommandFailure } from './api';

/** The slice of HeroUI's `toast` this needs — swapped for a fake in tests. */
export interface Toaster {
  show(text: string, opts: ToastOpts): string;
  update(id: string, text: string, opts: ToastOpts): void;
  close(id: string): void;
}

interface ToastOpts {
  variant?: 'default' | 'success' | 'danger';
  isLoading?: boolean;
  /** 0 keeps the toast up until it is updated. */
  timeout: number;
}

const heroToaster: Toaster = {
  show: (text, opts) => heroToast(text, opts),
  update: (id, text, opts) => void heroToast.update(id, text, opts),
  close: (id) => heroToast.close(id),
};

const DONE_MS = 4000;

const NETWORK =
  /offline|dns|network|connect|timed? ?out|sending request|unreachable/i;

/** Why a check failed, as a toast line. Offline (the browser says so, or
 *  the host is unreachable) is just "You're offline"; otherwise the shell's
 *  first line, minus the "couldn't check for updates:" it already starts
 *  with. */
export function checkFailureText(message: string, offline: boolean): string {
  if (offline) return 'You’re offline';
  const line =
    message
      .split('\n')
      .map((l) => l.trim())
      .find(Boolean) ?? '';
  const reason = line.replace(/^couldn['’]t check for updates:\s*/i, '');
  if (NETWORK.test(reason)) {
    return 'Couldn’t check for updates: the update server is unreachable';
  }
  return reason
    ? `Couldn’t check for updates: ${reason}`
    : 'Couldn’t check for updates';
}

export function isOffline(store: RootStore): boolean {
  return (
    (typeof navigator !== 'undefined' && navigator.onLine === false) ||
    !store.ephemeral.hostStatus.online
  );
}

let running: Promise<void> | null = null;

/** Runs a check with toast feedback; a second call while one runs joins it. */
export function checkForUpdates(
  store: RootStore,
  toaster: Toaster = heroToaster,
): Promise<void> {
  running ??= run(store, toaster).finally(() => (running = null));
  return running;
}

async function run(store: RootStore, toaster: Toaster): Promise<void> {
  // Already installed: the pill is on screen, nothing to add.
  if (store.ephemeral.readyUpdate) return;
  const id = toaster.show('Checking for updates…', {
    isLoading: true,
    timeout: 0,
  });
  // The shell announces an update before downloading it.
  const stop = reaction(
    () => store.ephemeral.appInfo?.update,
    (u) => {
      if (u && !u.downloaded) {
        toaster.update(id, `Downloading Theodore ${u.version}…`, {
          isLoading: true,
          timeout: 0,
        });
      }
    },
  );
  try {
    const info = await api.checkUpdate(true);
    if (info) {
      // The pill takes over from here.
      toaster.close(id);
    } else {
      const v = store.ephemeral.appInfo?.version;
      toaster.update(
        id,
        v ? `Theodore ${v} is up to date` : 'Theodore is up to date',
        { variant: 'success', timeout: DONE_MS },
      );
    }
  } catch (e) {
    const message = e instanceof CommandFailure ? e.message : String(e);
    toaster.update(id, checkFailureText(message, isOffline(store)), {
      variant: 'danger',
      timeout: DONE_MS,
    });
  } finally {
    stop();
  }
}
