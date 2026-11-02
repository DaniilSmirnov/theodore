import { listenMenu } from '../core/menu';
import { looksLikePrUrl } from '../core/prUrl';
import { openPr } from '../actions';
import { NewTab } from './NewTab';
import { useEffect } from 'react';
import { Surface, Toast } from '@heroui/react';
import { observer } from 'mobx-react-lite';
import { useRootStore, useUi } from '../stores/context';
import { ask } from '../actions';
import { ColorPreviewHost } from './ColorPreview';
import { ContextMenuHost, useContextMenu } from './ContextMenu';
import { Inbox } from './List';
import { GateModals, Modals } from './Modals';
import { useGlobalKeys } from './Palette';
import { Branches } from './Branches';
import { PullRequests } from './PullRequests';
import { Review } from './Review';
import { Sidebar } from './Sidebar';
import { SettingsNav, SettingsPage } from './Settings';
import { SkillsPage } from './Skills';
import { TabBar } from './TabBar';
import { Tasks } from './Tasks';
import { Onboarding } from './Onboarding';
import { SignInGate } from './AccountCard';
import { UpdateReady } from './shell/UpdateReady';
import { SharingPrompt } from './SharingPrompt';
import { appView } from '../core/accountGate';
import { StatusBanners } from './shell/StatusBanners';
import { showToast } from './shell/toasts';
import { isBuildMode } from './space/mode';
import { isHostOn } from './settings/labs';
import { SpacePage } from './space/SpacePage';
import { useWelcome, Welcome } from './Welcome';

/** Fallback for any surface with no menu of its own: inputs get Cut/Copy/
 *  Paste/Select all, a text selection gets Copy + "Ask agent about
 *  selection", everything else gets nothing (no custom items to show). */
function useRootContextMenu() {
  const store = useRootStore();
  const ui = useUi();
  // ⌘V with a PR link outside a text field opens it.
  useEffect(() => {
    const onPaste = (e: ClipboardEvent) => {
      const t = e.target as HTMLElement | null;
      if (t?.closest('input,textarea,[contenteditable=true]')) return;
      if (store.ephemeral.accountGate !== 'open') return;
      const text = e.clipboardData?.getData('text') ?? '';
      if (!looksLikePrUrl(text, (h) => isHostOn(store, h))) return;
      e.preventDefault();
      void openPr(store, text.trim());
    };
    window.addEventListener('paste', onPaste);
    // ⌘T and the rest of the ⌘ shortcuts come from the native menu.
    const offMenu = listenMenu(store);
    return () => {
      window.removeEventListener('paste', onPaste);
      offMenu();
    };
  }, [store, ui]);

  return useContextMenu(() => {
    const el = document.activeElement as HTMLElement | null;
    const isField =
      !!el && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA');
    if (isField) {
      const input = el as HTMLInputElement | HTMLTextAreaElement;
      return [
        {
          id: 'cut',
          label: 'Cut',
          onAction: () => document.execCommand('cut'),
        },
        {
          id: 'copy',
          label: 'Copy',
          onAction: () => document.execCommand('copy'),
        },
        {
          id: 'paste',
          label: 'Paste',
          onAction: () => document.execCommand('paste'),
        },
        {
          id: 'select-all',
          label: 'Select all',
          onAction: () => input.select(),
        },
      ];
    }
    const text = document.getSelection()?.toString();
    if (text) {
      return [
        {
          id: 'copy',
          label: 'Copy',
          onAction: () => void navigator.clipboard.writeText(text),
        },
        {
          id: 'ask-agent',
          label: 'Ask agent about selection',
          isDisabled: ui.route.name !== 'review',
          onAction: () => {
            if (ui.route.name !== 'review') return;
            void ask(store, {
              pr: ui.route.pr,
              question: text,
              about: null,
              group: null,
              agent: null,
            }).catch(() => {
              // toasted by api.ts
            });
          },
        },
      ];
    }
    return [];
  });
}

const Toasts = observer(function Toasts() {
  const store = useRootStore();
  useEffect(() => {
    // HeroUI's imperative toast API: fire once per new entry, then drop it.
    for (const t of store.ephemeral.toasts) {
      showToast(t);
      store.ephemeral.dismissToast(t.id);
    }
  }, [store, store.ephemeral.toasts]);
  return null;
});

export const App = observer(function App() {
  const ui = useUi();
  const store = useRootStore();
  const buildMode = isBuildMode(store);
  useGlobalKeys();
  const onContextMenu = useRootContextMenu();
  // Onboarding just finished: the logo, then *whoosh* into the app.
  const welcome = useWelcome(ui.onboarded);

  // Signed out where sign-in is required: the sign-in and nothing else
  // (plan §6.12). Onboarding's first step is that sign-in.
  const view = appView(ui.onboarded, store.ephemeral.accountGate);
  if (view === 'loading') {
    return <div className="h-full bg-background" />;
  }
  if (view === 'onboarding' || view === 'sign-in') {
    return (
      <>
        {view === 'onboarding' ? <Onboarding /> : <SignInGate />}
        <UpdateReady />
        <GateModals />
        <Toast.Provider placement="bottom end" maxVisibleToasts={3} />
      </>
    );
  }

  const r = ui.route;
  return (
    <div
      className={`flex h-full flex-col bg-background text-sm text-foreground ${welcome ? 'app-enter' : ''}`}
      onContextMenu={onContextMenu}
    >
      <div className="flex min-h-0 flex-1">
        {/* Settings always shows its own sections: with the sidebar
            collapsed there'd be no way between its pages. */}
        {r.name === 'settings' ? (
          <SettingsNav page={r.page} />
        ) : (
          ui.sidebar && <Sidebar />
        )}
        <Surface className="relative flex min-w-0 flex-1 flex-col rounded-none">
          {/* Settings shows its own nav in the sidebar's place. */}
          <TabBar showOpen={!ui.sidebar && r.name !== 'settings'} />
          <div className="flex min-h-0 flex-1">
            {r.name === 'inbox' ? (
              <Inbox />
            ) : r.name === 'prs' ? (
              <PullRequests />
            ) : r.name === 'branches' ? (
              <Branches key={ui.workspace} />
            ) : r.name === 'tasks' ? (
              <Tasks />
            ) : r.name === 'skills' ? (
              <SkillsPage />
            ) : r.name === 'settings' ? (
              <SettingsPage page={r.page} />
            ) : r.name === 'new' ? (
              <NewTab key={r.id} route={r} />
            ) : r.name === 'space' ? (
              // A tab left over from when build mode was on shows nothing.
              buildMode ? (
                <SpacePage key={r.id} id={r.id} />
              ) : null
            ) : (
              <Review key={r.pr} pr={r.pr} />
            )}
          </div>
          {/* Floats above the hint bar: a banner appearing or clearing
              never moves the page, and never covers the header. */}
          <div className="pointer-events-none absolute inset-x-0 bottom-14 z-30 flex flex-col items-center gap-2 px-4 [&>*]:pointer-events-auto">
            <UpdateReady inline />
            <StatusBanners />
          </div>
        </Surface>
      </div>
      <Modals />
      <SharingPrompt />
      <Toasts />
      <ContextMenuHost />
      <ColorPreviewHost />
      <Toast.Provider placement="bottom end" maxVisibleToasts={3} />
      {welcome && <Welcome />}
    </div>
  );
});
