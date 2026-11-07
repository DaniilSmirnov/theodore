import { getTransport } from '../core/transport';
import { resourceCache, type ResourceCache } from '../core/resource';
import { SyncClient, modelAccessors, type ModelAccessors } from '../sync';
import type { Ephemeral, Revision, UpdateInfo } from '../gen/contract';
import { reaction } from 'mobx';
import { loadAccount, loadAppInfo, loadViewer } from '../actions';
import { windowTitle } from '../ui/sandbox/sandbox';
import {
  setAccountRequiredHandler,
  setCommandFailureHandler,
} from '../core/api';
import { EphemeralStore } from './EphemeralStore';
import { UiStore } from './UiStore';
import { WorkspaceStore } from './WorkspaceStore';
import { ALL } from '../core/workspaces';
import { watchSpaceTurns } from '../ui/space/mode';

export class RootStore {
  readonly sync = new SyncClient();
  readonly models: ModelAccessors = modelAccessors(this.sync.pool);
  readonly ephemeral = new EphemeralStore();
  readonly ui = new UiStore();
  readonly workspaces = new WorkspaceStore(this.models, this.ui);

  /** Revision at `<pr>@<head>`; immutable once fetched. */
  readonly revisions: ResourceCache<Revision> = resourceCache({ limit: 50 });
  /** File contents at `<rev>:<path>`; immutable once fetched. */
  readonly files: ResourceCache<string | null> = resourceCache({ limit: 500 });

  private unlistenEphemeral: (() => void) | undefined;
  private unlistenUpdate: (() => void) | undefined;
  private disposeTitle: (() => void) | undefined;
  private disposeViewerRetry: (() => void) | undefined;
  private disposeGoneWorkspace: (() => void) | undefined;
  private disposeSpaceTurns: (() => void) | undefined;
  private disposeGateOpen: (() => void) | undefined;

  async start(): Promise<void> {
    setCommandFailureHandler((f) =>
      this.ephemeral.handle({ type: 'toast', level: 'error', text: f.message }),
    );
    // A command turned away for want of a sign-in: the session ended and
    // its event got lost, so ask again; the gate then shows.
    setAccountRequiredHandler(() => void loadAccount(this));
    this.unlistenEphemeral = await getTransport().listen<Ephemeral>(
      'ephemeral',
      (e) => this.ephemeral.handle(e),
    );
    // The shell downloaded and installed an update: offer the restart.
    this.unlistenUpdate = await getTransport().listen<UpdateInfo>(
      'update',
      (u) => this.ephemeral.setUpdate(u),
    );
    // `[SANDBOX] ` in front of the window title while in sandbox mode.
    this.disposeTitle = reaction(
      () => this.ephemeral.sandbox,
      (sandbox) => {
        document.title = windowTitle(document.title, sandbox);
      },
      { fireImmediately: true },
    );
    // The window shows nothing but the sign-in until this says whether it
    // may (plan §6.12).
    void loadAccount(this);
    void loadAppInfo(this);
    await this.sync.start();
    // Spaces (docs/spaces.md): a no-op with build mode off.
    this.disposeSpaceTurns = watchSpaceTurns(this);
    // Host calls wait for the account gate: at once without a required
    // sign-in, else on signing in.
    this.disposeGateOpen = reaction(
      () => this.ephemeral.accountGate === 'open',
      (open) => {
        if (open) void loadViewer(this);
      },
      { fireImmediately: true },
    );
    // The active project is gone (removed here or on another device) once
    // the pool is loaded: fall back to All.
    this.disposeGoneWorkspace = reaction(
      () =>
        this.sync.status === 'online' &&
        this.ui.workspace !== ALL &&
        !this.models.projects.get(this.ui.workspace),
      (gone) => {
        if (gone) this.workspaces.forget(this.ui.workspace);
      },
      { fireImmediately: true },
    );
    // The viewer fetch fails while the host rejects the token or is
    // offline; once it's healthy again, fetch it then.
    this.disposeViewerRetry = reaction(
      () =>
        this.ephemeral.hostStatus.authOk && this.ephemeral.hostStatus.online,
      (healthy) => {
        if (healthy && !this.ephemeral.viewer) void loadViewer(this);
      },
    );
  }

  dispose(): void {
    this.sync.dispose();
    this.unlistenEphemeral?.();
    this.unlistenUpdate?.();
    this.disposeTitle?.();
    this.disposeViewerRetry?.();
    this.disposeGoneWorkspace?.();
    this.disposeSpaceTurns?.();
    this.disposeGateOpen?.();
  }
}
