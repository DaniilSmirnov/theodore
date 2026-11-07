/**
 * Merge as on GitHub: method, commit message, gates, conflicts, auto-merge —
 * ported from the prototype's `MergeDialog`, now wired to the real
 * `merge_info` / `merge_pr` / `update_branch` commands (`FEATURES.hostWrites`).
 *
 * `PrSummary.conflicts` is a bare bool, so the gates below still use it
 * (it's already on hand for every PR row, not just the one being merged),
 * but the dialog itself now backs the file-level detail with `conflicts`
 * (`git merge-tree`, computed locally — no host round trip): a per-file
 * list instead of a generic "has conflicts" note. On an old local `git`
 * (`CommandFailure.code === 'unsupported'`) `api.conflicts` resolves to
 * `null` and the list silently falls back to the old plain-text note.
 * "Resolve with agent" has no backing command either way — kept disabled.
 */
import { useEffect, useState } from 'react';
import {
  CircleCheckFill,
  CodeMerge,
  TriangleExclamation,
} from '@gravity-ui/icons';
import {
  Alert,
  Button,
  Dropdown,
  Input,
  Label,
  Modal,
  Radio,
  RadioGroup,
  TextArea,
  TextField,
  Tooltip,
} from '@heroui/react';
import { observer } from 'mobx-react-lite';
import type {
  AgentId,
  ConflictFile,
  ConflictKind,
  MergeInfo,
  MergeMethod,
  PrId,
} from '../gen/contract';
import { mergePr, updateBranch } from '../actions';
import { api } from '../core/api';
import { mergeInfoProblem, useMergeInfoState } from './mergeInfo';
import { useRootStore } from '../stores/context';
import { Btn, Spinner } from './bits';

export const CONFLICT_KIND_LABEL: Record<ConflictKind, string> = {
  content: 'content conflict',
  add_add: 'added on both sides',
  modify_delete: 'modified on one side, deleted on the other',
  rename_delete: 'renamed on one side, deleted on the other',
  rename_rename: 'renamed differently on each side',
  directory_file: 'a file on one side, a directory on the other',
  submodule: 'submodule conflict',
  other: 'conflict',
};

export const METHOD: Record<MergeMethod, string> = {
  squash: 'Squash and merge',
  merge: 'Create a merge commit',
  rebase: 'Rebase and merge',
};

/** What `method` does, said of the PR's own base branch: a stacked PR
 *  lands on another branch, not on main. */
export function methodHint(method: MergeMethod, base: string): string {
  const onto = base || 'the base branch';
  switch (method) {
    case 'squash':
      return `one commit on ${onto}; branch history is squashed`;
    case 'merge':
      return `all commits + a merge commit on ${onto}`;
    case 'rebase':
      return `branch commits on top of ${onto}, no merge commit`;
  }
}

const AGENTS: AgentId[] = ['claude', 'codex', 'gemini'];

export const STATE_LABEL: Record<MergeInfo['state'], string> = {
  clean: 'Ready to merge',
  blocked: 'Blocked by required checks or reviews',
  behind: 'Out of date with the base branch',
  dirty: 'Has merge conflicts',
  unstable: "Some checks aren't passing",
  unknown: "GitHub hasn't finished checking yet",
};

interface Gate {
  ok: boolean;
  label: string;
  /** Not ok, but the host merges anyway (failing checks it doesn't
   *  require): shown as a warning, doesn't hold the merge. */
  warn?: boolean;
}

/** The gates that hold the merge: not ok, and not just a warning. */
export const blockingGates = (gates: Gate[]): Gate[] =>
  gates.filter((g) => !g.ok && !g.warn);

/** Everything that stands between this PR and a merge, from data we actually have. */
export function gatesFor(
  store: ReturnType<typeof useRootStore>,
  pr: PrId,
  info: MergeInfo | null,
): Gate[] {
  const summary = store.models.prSummaries.get(pr);
  const detail = store.models.prDetails.get(pr);
  const tasks = store.models.tasks.byPr(pr);
  const toVerify = tasks.filter((t) => t.status === 'review').length;
  const checks = detail?.checks ?? [];
  const failing = checks.filter((c) => c.status === 'fail').length;
  const pending = checks.filter((c) => c.status === 'pending').length;
  // `clean`/`unstable`: the host would merge now, failing checks or not —
  // they're none it requires (no protection on the base, say).
  const hostMerges = info?.state === 'clean' || info?.state === 'unstable';
  const gates: Gate[] = [];
  if (summary) {
    gates.push({
      ok: summary.state !== 'draft',
      label:
        summary.state === 'draft'
          ? 'Draft — needs "Ready for review"'
          : 'Open for review',
    });
    gates.push({
      ok: failing === 0,
      warn: failing > 0 && hostMerges,
      label: failing
        ? `${failing} ${failing === 1 ? 'check' : 'checks'} failing`
        : pending
          ? `${pending} ${pending === 1 ? 'check' : 'checks'} still running`
          : 'Checks passing',
    });
    gates.push({
      ok: summary.approvals >= (summary.required_approvals ?? 0),
      label:
        summary.required_approvals != null
          ? `${summary.approvals}/${summary.required_approvals} approvals`
          : `${summary.approvals} approval${summary.approvals === 1 ? '' : 's'}`,
    });
    if (summary.review_decision === 'changes_requested') {
      gates.push({ ok: false, label: 'changes requested' });
    }
    gates.push({
      ok: !summary.conflicts,
      label: summary.conflicts ? 'conflicts with base branch' : 'no conflicts',
    });
  }
  if (toVerify) gates.push({ ok: false, label: `${toVerify} tasks to verify` });
  // `dirty` is the host's word for the conflicts gate above, `unstable`
  // for the checks gate — don't list either twice.
  if (
    info &&
    !hostMerges &&
    info.state !== 'unknown' &&
    !(info.state === 'dirty' && summary?.conflicts)
  ) {
    gates.push({ ok: false, label: STATE_LABEL[info.state] });
  }
  return gates;
}

export const MergeDialog = observer(function MergeDialog({
  pr,
  onClose,
  onOpenFile,
}: {
  pr: PrId;
  onClose: () => void;
  /** Lets a conflicting file's row jump into the diff; omitted, the row
   *  still shows the path and conflict kind, just not as a link. */
  onOpenFile?: (path: string) => void;
}) {
  const store = useRootStore();
  const summary = store.models.prSummaries.get(pr);
  // Asked again on a backoff while the dialog is open: a host having
  // trouble (a 502) recovers on its own, said in a sentence meanwhile.
  const { info, error: loadError } = useMergeInfoState(pr, 'dialog');
  const [method, setMethod] = useState<MergeMethod>('squash');
  const [title, setTitle] = useState(summary ? `${summary.title}` : '');
  const [body, setBody] = useState('');
  const [resolveAgent, setResolveAgent] = useState<AgentId | null>(null);
  const [updating, setUpdating] = useState(false);
  const [merging, setMerging] = useState<'now' | 'auto' | 'queue' | null>(null);
  // `null` covers both "hasn't loaded yet" and "can't tell" (an old local
  // `git`, `api.conflicts` resolving to `null`) — both fall back to the
  // plain-text note below.
  const [conflictFiles, setConflictFiles] = useState<ConflictFile[] | null>(
    null,
  );

  // The host's pick (your last method, else the repo's default), once its
  // first answer is in.
  const [picked, setPicked] = useState(false);
  useEffect(() => {
    if (!info || picked) return;
    setPicked(true);
    const d = info.default_method;
    if (
      d &&
      (!info.allowed_methods.length || info.allowed_methods.includes(d))
    ) {
      setMethod(d);
    } else if (
      info.allowed_methods.length &&
      !info.allowed_methods.includes(method)
    ) {
      setMethod(info.allowed_methods[0]!);
    }
  }, [info, picked, method]);

  useEffect(() => {
    if (!summary?.conflicts) return;
    let live = true;
    void api
      .conflicts(pr)
      .then((files) => live && setConflictFiles(files))
      .catch(() => live && setConflictFiles(null));
    return () => {
      live = false;
    };
  }, [pr, summary?.conflicts]);

  if (!summary) return null;

  const local = summary.pr.repo.host === 'local';
  const gates = gatesFor(store, pr, info);
  const blocked = blockingGates(gates);
  const warnings = gates.filter((g) => !g.ok && g.warn);
  const behindOrDirty = info?.state === 'behind' || info?.state === 'dirty';
  const canMergeNow = !local && !!info && blocked.length === 0;
  const allowedMethods = info?.allowed_methods ?? [];
  const slug = `${summary.pr.repo.owner}/${summary.pr.repo.name}#${summary.pr.number}`;

  const doMerge = (opts: { auto?: boolean; queue?: boolean }) => {
    const kind = opts.queue ? 'queue' : opts.auto ? 'auto' : 'now';
    setMerging(kind);
    void mergePr(store, {
      pr,
      method,
      title: method === 'squash' ? title : null,
      body: method === 'squash' ? body : null,
      auto: !!opts.auto,
      queue: !!opts.queue,
    })
      .then((ok) => {
        if (ok && !opts.queue) onClose();
      })
      .finally(() => setMerging(null));
  };

  const footer = local ? null : !info ? (
    <Spinner />
  ) : info.in_queue ? (
    <Btn tone="tertiary" icon={<CodeMerge />} isDisabled>
      Already in the merge queue
    </Btn>
  ) : (
    <div className="flex flex-wrap items-center justify-end gap-2">
      {/* Only what the repo actually has turned on. */}
      {info.merge_queue && (
        <Btn
          tone="tertiary"
          icon={<CodeMerge />}
          className="merge-queue"
          isPending={merging === 'queue'}
          onPress={() => doMerge({ queue: true })}
        >
          Add to merge queue
        </Btn>
      )}
      {/* Auto-merge is for a PR that can't merge yet: offered while a
          gate is red, and always shown once on, so it can be turned off. */}
      {((info.auto_merge_allowed && blocked.length > 0) ||
        info.auto_merge_enabled) && (
        <Btn
          tone={info.auto_merge_enabled ? 'secondary' : 'tertiary'}
          icon={<CodeMerge />}
          className="enable-auto-merge"
          isPending={merging === 'auto'}
          onPress={() => doMerge({ auto: !info.auto_merge_enabled })}
        >
          {info.auto_merge_enabled ? 'Disable auto-merge' : 'Enable auto-merge'}
        </Btn>
      )}
      <Btn
        tone="primary"
        icon={<CodeMerge />}
        className="do-merge"
        isDisabled={blocked.length > 0}
        isPending={merging === 'now'}
        onPress={() => doMerge({})}
      >
        {METHOD[method]}
      </Btn>
    </div>
  );

  return (
    <Modal.Backdrop isOpen onOpenChange={(open) => !open && onClose()}>
      <Modal.Container
        size="lg"
        placement="top"
        scroll="inside"
        className="mt-[8vh]"
      >
        <Modal.Dialog className="modal w-[640px] max-w-[92vw]">
          <Modal.CloseTrigger />
          <Modal.Header className="modal-h min-w-0">
            <Modal.Heading className="min-w-0 truncate pe-8" title={slug}>
              Merge · {slug}
            </Modal.Heading>
          </Modal.Header>
          <Modal.Body className="flex flex-col gap-3">
            {loadError != null && (
              <Alert status="warning" className="merge-info-problem">
                <Alert.Indicator />
                <Alert.Content>
                  <Alert.Title>Can't check merge status yet</Alert.Title>
                  <Alert.Description>
                    {mergeInfoProblem(summary.pr.repo.host, loadError)}
                  </Alert.Description>
                </Alert.Content>
              </Alert>
            )}
            <div className="merge-gates flex flex-col gap-1.5 text-sm">
              {gates.map((g) => (
                <div
                  key={g.label}
                  className={`flex items-center gap-2 ${g.ok ? 'text-muted' : 'text-foreground'}`}
                >
                  {g.ok ? (
                    <CircleCheckFill className="size-4 text-success" />
                  ) : (
                    <TriangleExclamation className="size-4 text-warning" />
                  )}
                  {g.label}
                </div>
              ))}
              {local && (
                <div className="flex items-center gap-2 text-foreground">
                  <TriangleExclamation className="size-4 text-warning" />
                  Local branch — no host PR to merge
                </div>
              )}
            </div>

            {behindOrDirty && (
              <Alert status="default" className="conflicts">
                <Alert.Indicator />
                <Alert.Content>
                  <Alert.Title>
                    {info?.state === 'dirty'
                      ? conflictFiles && conflictFiles.length > 0
                        ? `Merge conflicts in ${conflictFiles.length} ${conflictFiles.length === 1 ? 'file' : 'files'}`
                        : 'Merge conflicts'
                      : 'Behind the base branch'}
                  </Alert.Title>
                  <Alert.Description className="flex flex-col gap-1.5">
                    {info?.state === 'dirty' &&
                      conflictFiles &&
                      conflictFiles.length > 0 && (
                        <ul className="conflict-files flex flex-col gap-1">
                          {conflictFiles.map((f) => (
                            <li
                              key={f.path}
                              className="flex flex-wrap items-baseline gap-1.5 text-xs"
                            >
                              {onOpenFile ? (
                                <button
                                  type="button"
                                  className="conflict-file-link truncate font-mono text-accent underline-offset-2 hover:underline"
                                  onClick={() => onOpenFile(f.path)}
                                >
                                  {f.path}
                                </button>
                              ) : (
                                <span className="truncate font-mono">
                                  {f.path}
                                </span>
                              )}
                              <span className="text-muted">
                                — {CONFLICT_KIND_LABEL[f.kind]}
                              </span>
                            </li>
                          ))}
                        </ul>
                      )}
                    <div className="flex flex-wrap items-center gap-1.5">
                      {info?.state === 'dirty'
                        ? conflictFiles === null
                          ? 'GitHub reports this PR has conflicts with the base branch.'
                          : conflictFiles.length === 0
                            ? 'No local conflicts found — the base branch may have moved since GitHub last checked.'
                            : 'Updating the branch may resolve some of these conflicts.'
                        : 'Merge the base branch into this one to pick up its latest changes.'}
                      <Tooltip>
                        <Dropdown>
                          <Button
                            size="sm"
                            variant="tertiary"
                            className="resolve-agent"
                            isDisabled
                          >
                            Resolve with agent
                          </Button>
                          <Dropdown.Popover placement="bottom end">
                            <Dropdown.Menu
                              aria-label="Agent"
                              onAction={(k) => setResolveAgent(k as AgentId)}
                            >
                              {AGENTS.map((a) => (
                                <Dropdown.Item key={a} id={a} textValue={a}>
                                  {a}
                                </Dropdown.Item>
                              ))}
                            </Dropdown.Menu>
                          </Dropdown.Popover>
                        </Dropdown>
                        <Tooltip.Content>Not available yet</Tooltip.Content>
                      </Tooltip>
                      <Button
                        size="sm"
                        variant="ghost"
                        isPending={updating}
                        onPress={() => {
                          setUpdating(true);
                          void updateBranch(store, pr).finally(() =>
                            setUpdating(false),
                          );
                        }}
                      >
                        Update branch
                      </Button>
                      {resolveAgent && (
                        <span className="text-xs text-muted">
                          picked {resolveAgent} — nothing to run yet
                        </span>
                      )}
                    </div>
                  </Alert.Description>
                </Alert.Content>
              </Alert>
            )}

            <RadioGroup
              value={method}
              onChange={(v) => setMethod(v as MergeMethod)}
              aria-label="Merge method"
              className="merge-method gap-1.5"
            >
              {(Object.keys(METHOD) as MergeMethod[]).map((m) => (
                <Radio
                  key={m}
                  value={m}
                  isDisabled={
                    allowedMethods.length > 0 && !allowedMethods.includes(m)
                  }
                  className={`rounded-2xl border px-3 py-2 ${method === m ? 'border-accent bg-accent-soft' : 'border-separator'}`}
                >
                  <Radio.Content>
                    <Radio.Control>
                      <Radio.Indicator />
                    </Radio.Control>
                    <div>
                      <b className="text-sm">{METHOD[m]}</b>
                      <div className="text-xs text-muted">
                        {methodHint(m, summary.base_ref)}
                      </div>
                    </div>
                  </Radio.Content>
                </Radio>
              ))}
            </RadioGroup>
            {method === 'squash' && (
              <>
                <TextField value={title} onChange={setTitle}>
                  <Label>Commit message</Label>
                  <Input className="font-mono text-xs" />
                </TextField>
                <TextArea
                  rows={3}
                  aria-label="Commit description"
                  value={body}
                  onChange={(e) => setBody(e.target.value)}
                  className="font-mono text-xs"
                />
              </>
            )}
          </Modal.Body>
          {/* The status line sits outside the scrolling body so it never
           *  ends up half hidden under the buttons. */}
          <Modal.Footer className="flex-col items-stretch gap-2">
            <span className="text-xs text-muted empty:hidden">
              {blocked.length
                ? `Can't merge yet: ${blocked.map((g) => g.label.toLowerCase()).join('; ')}.${info?.auto_merge_allowed ? ' Auto-merge will merge once everything is resolved.' : ''}`
                : canMergeNow && warnings.length
                  ? `GitHub allows the merge: ${warnings.map((g) => g.label.toLowerCase()).join('; ')}.`
                  : canMergeNow
                    ? 'All checks passed.'
                    : local
                      ? 'No PR to merge.'
                      : ''}
            </span>
            <div className="flex flex-wrap justify-end gap-2">{footer}</div>
          </Modal.Footer>
        </Modal.Dialog>
      </Modal.Container>
    </Modal.Backdrop>
  );
});
