/** Build mode against the live store: on/off, Labs visibility, and the
 *  "turn done" notices. No React, so `RootStore` can start the watcher. */
import { reaction } from 'mobx';
import type { PrId, Space } from '../../gen/contract';
import { OPEN_SPACE_EVENT, notifySpace, setBadge } from '../../core/native';
import { getTransport } from '../../core/transport';
import { FEATURES } from '../../features';
import type { RootStore } from '../../stores/RootStore';
import { spaceSeen } from './seen';
import {
  buildModeOn,
  labsVisible,
  nativeNotice,
  notifyOn,
  questionAsked,
  spaceOfReview,
  spaceTitle,
  turnEnded,
  waitingSpaces,
} from './spaces';

export function isBuildMode(store: RootStore): boolean {
  return buildModeOn(FEATURES.buildMode, store.models.settings.all());
}

export function isLabsVisible(store: RootStore): boolean {
  return labsVisible(FEATURES.buildMode, store.models.settings.all());
}

/** Whether you can see the window at all (not just the app's tab). */
const windowFocused = () =>
  typeof document === 'undefined' || document.hasFocus();

/** A click on a space's notification: the shell brought the window
 *  forward, open the space. `opened` runs after it did. */
export function openSpaceOnClick(
  store: RootStore,
  opened: () => void = () => {},
): () => void {
  let off: (() => void) | null = null;
  let cancelled = false;
  void getTransport()
    .listen<string>(OPEN_SPACE_EVENT, (id) => {
      if (!store.models.spaces.get(id) || !isBuildMode(store)) return;
      store.ui.go({ name: 'space', id });
      opened();
    })
    .then((un) => {
      if (cancelled) un();
      else off = un;
    })
    .catch(() => {});
  return () => {
    cancelled = true;
    off?.();
  };
}

/** A finished turn: a toast for a space you're not looking at, and — with
 *  the window in the background and `spaces.notify` on — a system
 *  notification; clicking it opens the space. Where the shell can't report
 *  the click (a dev run, outside macOS), coming back to the window opens
 *  the space the last notification was about instead. The dock badge
 *  counts the spaces waiting for you. */
export function watchSpaceTurns(store: RootStore): () => void {
  let prev = new Map<string, Space>();
  /** The space the last background notification was about, when its
   *  click can't be reported: the next window focus opens it. */
  let pending: string | null = null;

  const turns = reaction(
    () => store.models.spaces.all().map((s) => ({ ...s })),
    (spaces) => {
      if (isBuildMode(store)) {
        for (const s of spaces) {
          const before = prev.get(s.id);
          const route = store.ui.route;
          const looking = route.name === 'space' && route.id === s.id;
          const away = !windowFocused();
          const asked = questionAsked(before, s);
          const native = asked ?? nativeNotice(before, s);
          if (native && away && notifyOn(store.models.settings.all())) {
            const id = s.id;
            void notifySpace(id, native.title, native.body)
              .catch(() => false)
              .then((clicks) => {
                pending = clicks ? null : id;
              });
          }
          const notice = asked
            ? { level: 'info' as const, text: asked.title }
            : turnEnded(before, s);
          if (!notice || (looking && !away)) continue;
          store.ephemeral.toast(notice.text, notice.level, {
            label: 'Open',
            onAction: () => store.ui.go({ name: 'space', id: s.id }),
          });
        }
      }
      prev = new Map(spaces.map((s) => [s.id, s]));
    },
  );

  const onFocus = () => {
    const id = pending;
    pending = null;
    if (id && store.models.spaces.get(id) && isBuildMode(store)) {
      store.ui.go({ name: 'space', id });
    }
  };
  if (typeof window !== 'undefined') window.addEventListener('focus', onFocus);
  const offClick = openSpaceOnClick(store, () => {
    pending = null;
  });

  const badge = reaction(
    () =>
      isBuildMode(store)
        ? waitingSpaces(store.models.spaces.all(), (id) => spaceSeen.get(id))
        : 0,
    (n) => void setBadge(n).catch(() => {}),
    { fireImmediately: true },
  );

  return () => {
    turns();
    badge();
    offClick();
    if (typeof window !== 'undefined') {
      window.removeEventListener('focus', onFocus);
    }
  };
}

/** A review's title, or — for a space's own review, whose row title is the
 *  branch slug — the space's title (what was asked for). */
export function reviewTitle(
  store: RootStore,
  pr: PrId,
  fallback: string,
): string {
  const space = spaceOfReview(store.models.spaces.all(), pr);
  return space && space.review_pr === pr ? spaceTitle(space) : fallback;
}
