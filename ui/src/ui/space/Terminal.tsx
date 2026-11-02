/**
 * A space's terminals (docs/spaces.md §3 "Terminal"): a drawer under the
 * page, `xterm.js` tabs over one real PTY per tab in the space's worktree.
 * Toggled from the header and ⌃` (`SpacePage.tsx`); the shells are the
 * core's, not this component's, so they outlive navigating away from the
 * page — reopening the drawer (or the page) just re-lists and re-attaches.
 */
import { useEffect, useLayoutEffect, useRef, useState } from 'react';

import { Plus, TerminalLine, Xmark } from '@gravity-ui/icons';
import { Button, Tooltip } from '@heroui/react';
import { FitAddon } from '@xterm/addon-fit';
import { Terminal as XTerm } from '@xterm/xterm';
import '@xterm/xterm/css/xterm.css';
import { observer } from 'mobx-react-lite';

import type { SpaceTerminalInfo } from '../../gen/contract';
import { api } from '../../core/api';
import { useRootStore } from '../../stores/context';
import {
  type TerminalChunk,
  chunksToApply,
  decodeChunk,
} from './terminalBuffer';

/** A new tab's placeholder size, before its first `fit()` resizes it to
 *  the drawer's actual pixel size. */
const DEFAULT_COLS = 80;
const DEFAULT_ROWS = 24;
/** A stable empty array, so a terminal with no chunks yet doesn't give the
 *  view's effect a new dependency reference every render. */
const NO_CHUNKS: TerminalChunk[] = [];

/** Opens or lists `space`'s terminals when the drawer is shown; tracks
 *  which tab is active. One terminal is opened automatically the first
 *  time a space's drawer has none. */
function useSpaceTerminals(space: string, open: boolean) {
  const store = useRootStore();
  const [terminals, setTerminals] = useState<SpaceTerminalInfo[]>([]);
  const [active, setActive] = useState<string | null>(null);
  const openedFirst = useRef(false);

  useEffect(() => {
    if (!open) return;
    let on = true;
    void api.listSpaceTerminals(space).then((list) => {
      if (!on) return;
      setTerminals(list);
      setActive((a) =>
        a && list.some((t) => t.id === a)
          ? a
          : (list[list.length - 1]?.id ?? null),
      );
      if (list.length === 0 && !openedFirst.current) {
        openedFirst.current = true;
        void api
          .openSpaceTerminal({ space, cols: DEFAULT_COLS, rows: DEFAULT_ROWS })
          .then((t) => {
            if (!on) return;
            setTerminals((cur) => [...cur, t]);
            setActive(t.id);
          });
      }
    });
    return () => {
      on = false;
    };
  }, [space, open]);

  const openTab = () => {
    void api
      .openSpaceTerminal({ space, cols: DEFAULT_COLS, rows: DEFAULT_ROWS })
      .then((t) => {
        setTerminals((cur) => [...cur, t]);
        setActive(t.id);
      });
  };

  const closeTab = (id: string) => {
    setTerminals((cur) => {
      const next = cur.filter((t) => t.id !== id);
      setActive((a) => (a === id ? (next[next.length - 1]?.id ?? null) : a));
      return next;
    });
    store.ephemeral.clearTerminal(id);
    void api.closeSpaceTerminal(id);
  };

  return { terminals, active, setActive, openTab, closeTab };
}

/** Header: opens and closes the terminal drawer. */
export function TerminalToggle({
  open,
  onToggle,
}: {
  open: boolean;
  onToggle: () => void;
}) {
  return (
    <Tooltip>
      <Button
        size="sm"
        variant={open ? 'secondary' : 'ghost'}
        aria-label="Terminal"
        aria-pressed={open}
        className="space-terminal-toggle"
        onPress={onToggle}
      >
        <TerminalLine className="size-3.5" />
        Terminal
      </Button>
      <Tooltip.Content>Terminal ⌃`</Tooltip.Content>
    </Tooltip>
  );
}

/** The drawer itself: a tab strip and one `xterm.js` view per tab, all
 *  mounted at once (hidden, not unmounted, when not active) so switching
 *  tabs never loses a terminal's live state. */
export const TerminalDrawer = observer(function TerminalDrawer({
  space,
  open,
  onClose,
}: {
  space: string;
  open: boolean;
  onClose: () => void;
}) {
  const { terminals, active, setActive, openTab, closeTab } = useSpaceTerminals(
    space,
    open,
  );
  if (!open) return null;
  return (
    <section className="space-terminal flex h-[300px] shrink-0 flex-col border-t border-separator">
      <div className="flex items-center gap-1 border-b border-separator px-2 py-1">
        {terminals.map((t) => (
          <TerminalTab
            key={t.id}
            info={t}
            active={t.id === active}
            onSelect={() => setActive(t.id)}
            onClose={() => closeTab(t.id)}
          />
        ))}
        <Tooltip>
          <Button
            size="sm"
            variant="ghost"
            isIconOnly
            aria-label="New terminal"
            className="space-terminal-new size-6 min-w-0"
            onPress={openTab}
          >
            <Plus className="size-3.5" />
          </Button>
          <Tooltip.Content>New terminal</Tooltip.Content>
        </Tooltip>
        <span className="flex-1" />
        <Button
          size="sm"
          variant="ghost"
          isIconOnly
          aria-label="Close terminal drawer"
          onPress={onClose}
        >
          <Xmark className="size-3.5" />
        </Button>
      </div>
      <div className="relative min-h-0 flex-1 bg-background">
        {terminals.length === 0 && (
          <div className="grid h-full place-items-center text-xs text-muted">
            Starting…
          </div>
        )}
        {terminals.map((t) => (
          <TerminalView key={t.id} info={t} visible={t.id === active} />
        ))}
      </div>
    </section>
  );
});

/** One tab: title, click to select, × to close. Dims once its shell has
 *  exited — closing it is still the only way to drop it. */
const TerminalTab = observer(function TerminalTab({
  info,
  active,
  onSelect,
  onClose,
}: {
  info: SpaceTerminalInfo;
  active: boolean;
  onSelect: () => void;
  onClose: () => void;
}) {
  const store = useRootStore();
  const exited =
    info.status === 'exited' || store.ephemeral.terminalExit.has(info.id);
  return (
    <div
      className={`flex items-center gap-1 rounded-md px-2 py-1 text-xs ${
        active ? 'bg-default' : 'hover:bg-default/60'
      } ${exited ? 'text-muted' : ''}`}
    >
      <button
        type="button"
        className="flex items-center gap-1.5"
        onClick={onSelect}
      >
        <TerminalLine className="size-3" />
        {info.title}
      </button>
      <button
        type="button"
        aria-label={`Close ${info.title}`}
        onClick={onClose}
        className="rounded-sm p-0.5 hover:bg-default"
      >
        <Xmark className="size-2.5" />
      </button>
    </div>
  );
});

/** Reads the app's HeroUI tokens for an `xterm.js` theme that follows
 *  light/dark (`ui/src/styles.css`'s `--background`/`--foreground`/…, set
 *  on `:root`/`.dark`) — no terminal-specific palette of its own. */
function terminalTheme(): NonNullable<
  ConstructorParameters<typeof XTerm>[0]
>['theme'] {
  const v = (name: string, fallback: string) => {
    const value = getComputedStyle(document.documentElement)
      .getPropertyValue(name)
      .trim();
    return value || fallback;
  };
  const background = v('--background', '#1e1e1e');
  const foreground = v('--foreground', '#e5e5e5');
  const accent = v('--accent', '#4ea1ff');
  const danger = v('--danger', '#e5484d');
  const success = v('--success', '#30a46c');
  const warning = v('--warning', '#f5a623');
  const muted = v('--muted', '#8a8f98');
  return {
    background,
    foreground,
    cursor: accent,
    cursorAccent: background,
    selectionBackground: `color-mix(in oklab, ${accent} 35%, transparent)`,
    black: background,
    red: danger,
    green: success,
    yellow: warning,
    blue: accent,
    magenta: accent,
    cyan: success,
    white: foreground,
    brightBlack: muted,
    brightRed: danger,
    brightGreen: success,
    brightYellow: warning,
    brightBlue: accent,
    brightMagenta: accent,
    brightCyan: success,
    brightWhite: foreground,
  };
}

/** One `xterm.js` instance for the whole life of a terminal tab: created
 *  once (keyed by `info.id`), seeded with the core's scrollback, fed live
 *  chunks from `EphemeralStore.terminalChunks` as they arrive. Hidden with
 *  CSS rather than unmounted while another tab is active. */
const TerminalView = observer(function TerminalView({
  info,
  visible,
}: {
  info: SpaceTerminalInfo;
  visible: boolean;
}) {
  const store = useRootStore();
  const container = useRef<HTMLDivElement>(null);
  const term = useRef<XTerm | null>(null);
  const fit = useRef<FitAddon | null>(null);
  const appliedSeq = useRef(info.seq);
  const lastSize = useRef({ cols: info.cols, rows: info.rows });

  useLayoutEffect(() => {
    const el = container.current;
    if (!el) return;
    const t = new XTerm({
      cursorBlink: true,
      fontFamily: 'var(--font-mono)',
      fontSize: 12,
      theme: terminalTheme(),
    });
    const f = new FitAddon();
    t.loadAddon(f);
    t.open(el);
    term.current = t;
    fit.current = f;

    if (info.scrollback) t.write(decodeChunk(info.scrollback));

    const offData = t.onData((data) => {
      void api.writeSpaceTerminal({ terminal: info.id, data });
    });
    const offResize = t.onResize(({ cols, rows }) => {
      if (lastSize.current.cols === cols && lastSize.current.rows === rows) {
        return;
      }
      lastSize.current = { cols, rows };
      void api.resizeSpaceTerminal({ terminal: info.id, cols, rows });
    });
    const ro = new ResizeObserver(() => f.fit());
    ro.observe(el);
    f.fit();

    const onTheme = () => {
      t.options.theme = terminalTheme();
    };
    window.addEventListener('theodore-theme', onTheme);

    return () => {
      offData.dispose();
      offResize.dispose();
      ro.disconnect();
      window.removeEventListener('theodore-theme', onTheme);
      t.dispose();
    };
    // One instance per terminal id, kept for the tab's whole life.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [info.id]);

  // New output since this view mounted — including whatever the store
  // already buffered before it existed (the drawer was closed, or this is
  // a fresh mount after navigating back to the space).
  const chunks = store.ephemeral.terminalChunks.get(info.id) ?? NO_CHUNKS;
  useEffect(() => {
    const t = term.current;
    if (!t) return;
    for (const c of chunksToApply(chunks, info.seq, appliedSeq.current)) {
      t.write(decodeChunk(c.data));
      appliedSeq.current = c.seq;
    }
  }, [chunks, info.seq]);

  useEffect(() => {
    if (visible) fit.current?.fit();
  }, [visible]);

  return (
    <div
      ref={container}
      className="absolute inset-0 p-1"
      style={{ display: visible ? 'block' : 'none' }}
    />
  );
});
