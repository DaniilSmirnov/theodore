/** Typed wrappers for every command in `.context/commands.md`, over `Transport`. */
import { getTransport } from './transport';
import { ACCOUNT_REQUIRED } from './accountGate';
import type {
  AccountSignInRequest,
  AccountSignInStart,
  AccountSignInState,
  AccountStatus,
  AddProjectRequest,
  AgentId,
  AgentModel,
  AgentStatus,
  AppInfo,
  AskRequest,
  Attachment,
  BlobResponse,
  Bootstrap,
  BootstrapScope,
  ClientLog,
  CommandError,
  CommitSuggestionsRequest,
  ConflictFile,
  DelegateRequest,
  DeviceLogin,
  DraftStateRequest,
  EditCommentRequest,
  FileRequest,
  FindSymbolRequest,
  GetBlobRequest,
  HostAuthStatus,
  HostKind,
  InterdiffEntry,
  InterdiffRequest,
  IssuePreview,
  IssueRequest,
  IssueTarget,
  HostRepo,
  LinearIssue,
  LinearIssuesRequest,
  LinearStatus,
  LocalBranch,
  LoginState,
  MergeInfo,
  MergeRequest,
  OpenBranchRequest,
  OpenPrRequest,
  OpenPrResponse,
  OpenTerminalRequest,
  Person,
  ProblemReport,
  PrId,
  Project,
  ProjectIssuesRequest,
  ProjectWorkingState,
  ReactRequest,
  ReplyRequest,
  ReportedIssue,
  RerunCheckRequest,
  ResizeTerminalRequest,
  Revision,
  RevisionRequest,
  ResolveRequest,
  ReviewersRequest,
  RunSkillRequest,
  SaveAttachmentRequest,
  SandboxFaultRequest,
  SandboxStatus,
  CreateSpaceRequest,
  MergeSpaceRequest,
  MergeSpaceResult,
  ShipSpaceRequest,
  Space,
  SpaceBaseStatus,
  SpaceAnswerRequest,
  SpaceFeedbackRequest,
  SpaceFixCiRequest,
  SpaceIssue,
  SpaceReviewRequest,
  SpaceQueuedRequest,
  SpaceSendRequest,
  SpaceConfigView,
  SpaceLogLine,
  SpaceRunRequest,
  SetSpacePinnedRequest,
  SpaceSetupRequest,
  SpaceTerminalInfo,
  SpaceTrustRequest,
  EffectiveConfig,
  GlobalConfigInfo,
  ConfigDraft,
  CheckConfigRequest,
  CodeHover,
  CodeHoverRequest,
  CodeIntelLspRequest,
  CodeIntelStatus,
  ConfigCheck,
  SaveConfigRequest,
  SaveConfigResult,
  SaveSkillRequest,
  SetLinearTokenRequest,
  SetTokenRequest,
  SkillBody,
  SkillInfo,
  SubmitReviewRequest,
  SymbolHit,
  SyncFrame,
  TaskDecision,
  TelemetryChoice,
  TelemetryStatus,
  Transaction,
  TransactionResult,
  UiEvent,
  TriageRequest,
  UnmirrorSpaceRequest,
  UpdateInfo,
  WriteTerminalRequest,
} from '../gen/contract';

/** A fresh operation id: 16 lowercase hex chars. Sent with every command
 *  (`x-theodore-op`) so the core's log records of it share one id. */
export function newOpId(): string {
  const bytes = new Uint8Array(8);
  crypto.getRandomValues(bytes);
  return Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('');
}

/** A command rejected; carries the core's error code, not just a message. */
export class CommandFailure extends Error {
  readonly code: string;
  /** The operation id the command was sent with: matches the core's log. */
  readonly opId: string | undefined;
  constructor(err: CommandError, opId?: string) {
    super(err.message);
    this.name = 'CommandFailure';
    this.code = err.code;
    this.opId = opId;
  }
}

/** Set once by `RootStore.start()`. Every rejected command surfaces here as a
 *  toast — callers that also need to adjust their own state (clear a busy
 *  flag, roll back an optimistic update) still get the rejection to `catch`,
 *  they just don't need to put the message in front of the user themselves. */
let onFailure: ((f: CommandFailure) => void) | null = null;

export function setCommandFailureHandler(
  fn: (f: CommandFailure) => void,
): void {
  onFailure = fn;
}

/** Set once by `RootStore.start()`: a command was turned away because
 *  nobody is signed in (the session ended under the UI). No toast — the
 *  sign-in screen is the message; the handler re-reads the account. */
let onAccountRequired: (() => void) | null = null;

export function setAccountRequiredHandler(fn: () => void): void {
  onAccountRequired = fn;
}

async function call<T>(
  cmd: string,
  args?: Record<string, unknown>,
  /** The caller shows the failure itself (the sign-in dialog): no toast. */
  quiet = false,
): Promise<T> {
  const opId = newOpId();
  try {
    return await getTransport().invoke<T>(cmd, args, { opId });
  } catch (e) {
    if (e && typeof e === 'object' && 'code' in e && 'message' in e) {
      const failure = new CommandFailure(e as CommandError, opId);
      if (failure.code === ACCOUNT_REQUIRED) onAccountRequired?.();
      else if (!quiet) onFailure?.(failure);
      throw failure;
    }
    throw e;
  }
}

/** Fire-and-forget: bypasses the failure handler and swallows every error
 *  (a transport that throws synchronously included). */
function quietCall(cmd: string, args: Record<string, unknown>): Promise<null> {
  try {
    return getTransport()
      .invoke<null>(cmd, args, { opId: newOpId() })
      .then(
        () => null,
        () => null,
      );
  } catch {
    return Promise.resolve(null);
  }
}

export const api = {
  bootstrap: (scope: BootstrapScope) => call<Bootstrap>('bootstrap', { scope }),
  catchUp: (since: number) => call<SyncFrame>('catch_up', { since }),
  transact: (tx: Transaction) => call<TransactionResult>('transact', { tx }),
  openPr: (req: OpenPrRequest) => call<OpenPrResponse>('open_pr', { req }),
  getRevision: (req: RevisionRequest) =>
    call<Revision>('get_revision', { req }),
  getFile: (req: FileRequest) => call<string | null>('get_file', { req }),
  regroup: (pr: PrId) => call<null>('regroup', { pr }),
  runSkill: (req: RunSkillRequest) => call<string>('run_skill', { req }),
  ask: (req: AskRequest) => call<string>('ask', { req }),
  /** Saves a file for a chat message; its `id` goes in `attachments`. */
  saveAttachment: (req: SaveAttachmentRequest) =>
    call<Attachment>('save_attachment', { req }),
  /** A saved attachment as a `data:<mime>;base64,…` URL. */
  attachmentData: (id: string) => call<string>('attachment_data', { id }),
  cancelRun: (run: string) => call<null>('cancel_run', { run }),
  submitReview: (req: SubmitReviewRequest) =>
    call<null>('submit_review', { req }),
  detectAgents: () => call<AgentStatus[]>('detect_agents'),
  agentModels: (agent: AgentId) =>
    call<AgentModel[]>('agent_models', { agent }),
  appInfo: () => call<AppInfo>('app_info'),
  /** `quiet`: the caller says what failed itself (the update toast). */
  checkUpdate: (quiet = false) =>
    call<UpdateInfo | null>('check_update', undefined, quiet),
  installUpdate: () => call<null>('install_update'),
  refreshInbox: () => call<null>('refresh_inbox'),
  viewer: () => call<Person>('viewer'),
  hostStatus: () => call<HostAuthStatus>('host_status'),
  hostStatusFor: (host: HostKind) =>
    call<HostAuthStatus>('host_status_for', { host }),
  setHostToken: (req: SetTokenRequest) =>
    call<HostAuthStatus>('set_host_token', { req }),
  replyThread: (req: ReplyRequest) => call<null>('reply_thread', { req }),
  resolveThread: (req: ResolveRequest) => call<null>('resolve_thread', { req }),
  listSkills: (pr: PrId | null) => call<SkillInfo[]>('list_skills', { pr }),
  delegateTask: (req: DelegateRequest) =>
    call<string>('delegate_task', { req }),
  decideTask: (req: TaskDecision) => call<null>('decide_task', { req }),
  listHostRepos: () => call<HostRepo[]>('list_host_repos'),
  addProject: (req: AddProjectRequest) => call<Project>('add_project', { req }),
  removeProject: (id: string) => call<null>('remove_project', { id }),
  refreshProject: (id: string) => call<null>('refresh_project', { id }),
  listBranches: (project: string) =>
    call<LocalBranch[]>('list_branches', { project }),
  openBranch: (req: OpenBranchRequest) =>
    call<OpenPrResponse>('open_branch', { req }),
  /** `null` when the folder can't be read (moved, not a repo anymore, a
   *  core without git): the header just leaves the working state out, no
   *  toast for a passive read. */
  closeBranchReview: (pr: PrId) => call<null>('close_branch_review', { pr }),
  projectWorkingState: (project: string): Promise<ProjectWorkingState | null> =>
    getTransport()
      .invoke<ProjectWorkingState>('project_working_state', { project })
      .catch(() => null),
  /** `quiet`: the caller shows the failure itself (a status line). */
  mergeInfo: (pr: PrId, quiet = false) =>
    call<MergeInfo>('merge_info', { pr }, quiet),
  mergePr: (req: MergeRequest) => call<null>('merge_pr', { req }),
  /** A passive read (the @-mention list, the reviewer picker): a failure
   *  just leaves the list empty, it isn't toasted. */
  assignableUsers: (pr: PrId) =>
    call<Person[]>('assignable_users', { pr }, true),
  setReviewers: (req: ReviewersRequest) => call<null>('set_reviewers', { req }),
  setDraft: (req: DraftStateRequest) => call<null>('set_draft', { req }),
  updateBranch: (pr: PrId) => call<null>('update_branch', { pr }),
  react: (req: ReactRequest) => call<null>('react', { req }),
  editComment: (req: EditCommentRequest) => call<null>('edit_comment', { req }),
  commitSuggestions: (req: CommitSuggestionsRequest) =>
    call<null>('commit_suggestions', { req }),
  getSkill: (id: string, pr: PrId | null) =>
    call<SkillBody>('get_skill', { id, pr }),
  saveSkill: (req: SaveSkillRequest) => call<SkillInfo>('save_skill', { req }),
  pickFolder: () => call<string | null>('pick_folder'),
  interdiff: (req: InterdiffRequest) =>
    call<InterdiffEntry[]>('interdiff', { req }),
  loginStart: () => call<DeviceLogin>('login_start'),
  loginPoll: () => call<LoginState>('login_poll'),
  rerunCheck: (req: RerunCheckRequest) => call<null>('rerun_check', { req }),
  triage: (req: TriageRequest) => call<null>('triage', { req }),
  setLinearToken: (req: SetLinearTokenRequest) =>
    call<LinearStatus>('set_linear_token', { req }),
  linearStatus: () => call<LinearStatus>('linear_status'),
  linearIssues: (req: LinearIssuesRequest) =>
    call<LinearIssue[]>('linear_issues', { req }),
  findSymbol: (req: FindSymbolRequest) =>
    call<SymbolHit[]>('find_symbol', { req }),
  getBlob: (req: GetBlobRequest) => call<BlobResponse>('get_blob', { req }),
  /** Settings → Sandbox: `null` outside sandbox mode. */
  sandboxStatus: () => call<SandboxStatus | null>('sandbox_status'),
  sandboxSetFault: (req: SandboxFaultRequest) =>
    call<SandboxStatus>('sandbox_set_fault', { req }),
  sandboxFireEvent: (id: string) =>
    call<SandboxStatus>('sandbox_fire_event', { id }),
  // ── spaces (docs/spaces.md; hidden behind build mode) ──
  createSpace: (req: CreateSpaceRequest) =>
    call<Space>('create_space', { req }),
  /** New space's "From an issue" picker. */
  projectIssues: (req: ProjectIssuesRequest) =>
    call<SpaceIssue[]>('project_issues', { req }),
  listSpaces: (project: string | null) =>
    call<Space[]>('list_spaces', { project }),
  spaceSend: (req: SpaceSendRequest) => call<string>('space_send', { req }),
  /** "Send now": into the running turn when it can take it, else queued. */
  spaceSendNow: (req: SpaceSendRequest) =>
    call<string>('space_send_now', { req }),
  /** Send / Cancel / Edit on a queued message (docs/spaces.md §8). */
  spaceQueued: (req: SpaceQueuedRequest) => call<null>('space_queued', { req }),
  spaceStop: (space: string) => call<null>('space_stop', { space }),
  spaceSkills: (space: string) => call<SkillInfo[]>('space_skills', { space }),
  spaceReview: (req: SpaceReviewRequest) =>
    call<OpenPrResponse>('space_review', { req }),
  archiveSpace: (space: string) => call<null>('archive_space', { space }),
  shipSpace: (req: ShipSpaceRequest) => call<Space>('ship_space', { req }),
  undoSpaceTurn: (space: string) => call<Space>('undo_space_turn', { space }),
  /** Merge the space's PR: the host's default method, auto-merge when it
   *  can't merge yet (docs/spaces.md §3). */
  mergeSpace: (req: MergeSpaceRequest) =>
    call<MergeSpaceResult>('merge_space', { req }),
  /** Continue after merge: a new branch on the base's fresh tip (§7). */
  continueSpace: (space: string) => call<Space>('continue_space', { space }),
  unarchiveSpace: (space: string) => call<Space>('unarchive_space', { space }),
  /** Pin/unpin a space: pinned ones sort first (docs/spaces.md §3). */
  setSpacePinned: (req: SetSpacePinnedRequest) =>
    call<Space>('set_space_pinned', { req }),
  spaceSendFeedback: (req: SpaceFeedbackRequest) =>
    call<string>('space_send_feedback', { req }),
  /** "Base moved: N commits behind", from the copy on this Mac. A passive
   *  read: a failure just hides it, it isn't toasted. */
  spaceBaseStatus: (space: string) =>
    call<SpaceBaseStatus>('space_base_status', { space }, true),
  /** Update from base; while conflicts are resolving, completes the merge. */
  updateSpaceFromBase: (space: string) =>
    call<Space>('update_space_from_base', { space }),
  abortSpaceUpdate: (space: string) =>
    call<Space>('abort_space_update', { space }),
  /** "Fix with agent": the space PR's failing checks' logs as the next
   *  turn. */
  spaceFixCi: (req: SpaceFixCiRequest) => call<string>('space_fix_ci', { req }),
  /** Approve on a plan card: the next turn implements the plan. */
  approveSpacePlan: (space: string) =>
    call<string>('approve_space_plan', { space }),
  /** Your answers to the agent's `ask_user` questions. */
  answerSpaceQuestion: (req: SpaceAnswerRequest) =>
    call<null>('answer_space_question', { req }),
  /** "Mirror into `<folder>`": on fresh, or resuming a paused one. */
  mirrorSpace: (space: string) => call<Space>('mirror_space', { space }),
  /** "Sync now": re-syncs a mirrored space into its folder by hand. */
  syncMirror: (space: string) => call<Space>('sync_mirror', { space }),
  /** "Mirror off": refused (unless `discard`) if the folder was edited.
   *  Quiet: the caller (the mirror banner) shows the refusal itself, with
   *  a Discard button, instead of a toast. */
  unmirrorSpace: (req: UnmirrorSpaceRequest) =>
    call<Space>('unmirror_space', { req }, true),
  /** What a space runs, where each command comes from, and trust. */
  spaceConfig: (space: string) =>
    call<SpaceConfigView>('space_config', { space }),
  /** "Run commands from this repo?": Allow (per project) / Not now. */
  spaceTrust: (req: SpaceTrustRequest) => call<Space>('space_trust', { req }),
  /** Retry a failed setup, or continue without it. */
  spaceSetup: (req: SpaceSetupRequest) => call<Space>('space_setup', { req }),
  /** "Set up with agent": no `spaces:` commands yet — ask the agent to
   *  look at the repo and write them. */
  spaceSetupAssist: (space: string) =>
    call<Space>('space_setup_assist', { space }),
  /** "Fix with agent": setup failed — ask the agent to fix the cause. */
  spaceFixSetup: (space: string) => call<Space>('space_fix_setup', { space }),
  /** Starts or stops the space's `run` command. */
  spaceRun: (req: SpaceRunRequest) => call<Space>('space_run', { req }),
  spaceLogs: (space: string) => call<SpaceLogLine[]>('space_logs', { space }),
  /** Starts a terminal in the space's worktree (docs/spaces.md "Terminal"). */
  openSpaceTerminal: (req: OpenTerminalRequest) =>
    call<SpaceTerminalInfo>('open_space_terminal', { req }),
  /** Bytes typed or pasted into it. */
  writeSpaceTerminal: (req: WriteTerminalRequest) =>
    call<null>('write_space_terminal', { req }),
  /** A new size in cells. */
  resizeSpaceTerminal: (req: ResizeTerminalRequest) =>
    call<null>('resize_space_terminal', { req }),
  /** Kills its shell. */
  closeSpaceTerminal: (terminal: string) =>
    call<null>('close_space_terminal', { terminal }),
  /** The space's open (or just-exited) terminals, with scrollback. */
  listSpaceTerminals: (space: string) =>
    call<SpaceTerminalInfo[]>('list_space_terminals', { space }),
  /** A project's merged config, read-only (Settings → Labs). */
  projectConfig: (project: string) =>
    call<EffectiveConfig>('project_config', { project }),
  /** Settings → Projects → Config: each layer's file and YAML generated
   *  from the project's settings. */
  configDraft: (project: string) =>
    call<ConfigDraft>('config_draft', { project }),
  /** Live check while typing: shown inline, never toasted. */
  checkConfig: (req: CheckConfigRequest) =>
    call<ConfigCheck>('check_config', { req }, true),
  saveConfig: (req: SaveConfigRequest) =>
    call<SaveConfigResult>('save_config', { req }),
  /** Project settings → Languages, read again as installs settle: shown
   *  inline, never toasted. */
  codeIntelStatus: (project: string) =>
    call<CodeIntelStatus>('code_intel_status', { project }, true),
  /** A language's grammar and server: project settings → Languages →
   *  Install, or an install suggestion's Download. */
  codeIntelInstall: (language: string) =>
    call<null>('code_intel_install', { language }),
  /** An install suggestion's Not now: not suggested again. */
  codeIntelDecline: (language: string) =>
    call<null>('code_intel_decline', { language }),
  /** Deletes a language's downloaded server and grammar. Its refusal (a
   *  review or a space uses it) shows in the confirmation, not a toast. */
  codeIntelRemove: (language: string) =>
    call<null>('code_intel_remove', { language }, true),
  /** One project's language servers in reviews or in spaces, on or off:
   *  written to its local config file. */
  codeIntelSetLsp: (req: CodeIntelLspRequest) =>
    call<null>('code_intel_set_lsp', { req }),
  /** Hover text for the identifier under the pointer in a PR's diff;
   *  `null` when there is none. Never toasted: hovering is incidental. */
  codeIntelHover: (req: CodeHoverRequest) =>
    call<CodeHover | null>('code_intel_hover', { req }, true),
  globalConfig: () => call<GlobalConfigInfo>('global_config'),
  /** Creates the global config from a template when missing, then opens
   *  it in the default editor. */
  openGlobalConfig: () => call<GlobalConfigInfo>('open_global_config'),
  /** Theodore account (Settings → Account, Onboarding). The sign-in
   *  dialog shows its own failures, so starting and polling don't toast. */
  accountStatus: () => call<AccountStatus>('account_status'),
  accountSignInStart: (req: AccountSignInRequest) =>
    call<AccountSignInStart>('account_sign_in_start', { req }, true),
  accountSignInPoll: () =>
    call<AccountSignInState>('account_sign_in_poll', undefined, true),
  accountSignInCancel: () => call<null>('account_sign_in_cancel'),
  accountSignOut: () => call<AccountStatus>('account_sign_out'),
  /** Brings the window forward after the browser hands the sign-in back.
   *  Cosmetic: a failure is ignored, never toasted. */
  focusWindow: (): Promise<null> =>
    getTransport()
      .invoke<null>('focus_window')
      .catch(() => null),
  /** A UI error for the core's log. Never throws, never toasts: a lost
   *  log line must not turn into another error. */
  logClient: (entry: ClientLog): Promise<null> =>
    quietCall('log_client', { entry }),
  /** A product event (onboarding steps). Never throws, never toasts. */
  trackUi: (event: UiEvent): Promise<null> => quietCall('track_ui', { event }),
  telemetryStatus: () => call<TelemetryStatus>('telemetry_status'),
  /** Records both sharing choices; the core applies them at once. */
  setTelemetry: (req: TelemetryChoice) =>
    call<TelemetryStatus>('set_telemetry', { req }),
  /** Builds a local zip of redacted logs and app info. Nothing is sent.
   *  The report view shows its own failure: no toast. */
  reportProblem: () => call<ProblemReport>('report_problem', undefined, true),
  revealReport: (path: string) => call<null>('reveal_report', { path }),
  /** "Report a problem" as a public GitHub issue: where it goes, as whom,
   *  or why it can't. The report view shows every failure: no toasts. */
  issueTarget: () => call<IssueTarget>('issue_target', undefined, true),
  /** Exactly the issue `submitIssue` posts for the same request. */
  previewIssue: (req: IssueRequest) =>
    call<IssuePreview>('preview_issue', { req }, true),
  submitIssue: (req: IssueRequest) =>
    call<ReportedIssue>('submit_issue', { req }, true),
  /** `null` means "can't tell" (an old local `git`, code `unsupported`) —
   *  the Merge dialog hides the list instead of toasting a routine
   *  degrade; any other failure still toasts like every other command. */
  conflicts: async (pr: PrId): Promise<ConflictFile[] | null> => {
    const opId = newOpId();
    try {
      return await getTransport().invoke<ConflictFile[]>(
        'conflicts',
        { pr },
        { opId },
      );
    } catch (e) {
      if (
        e &&
        typeof e === 'object' &&
        'code' in e &&
        (e as CommandError).code === 'unsupported'
      ) {
        return null;
      }
      if (e && typeof e === 'object' && 'code' in e && 'message' in e) {
        const failure = new CommandFailure(e as CommandError, opId);
        onFailure?.(failure);
        throw failure;
      }
      throw e;
    }
  },
};
