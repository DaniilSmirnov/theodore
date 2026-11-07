/** Project settings → Languages: the project's two
 *  language-server switches (its local config file), then each of its
 *  languages with grammar, server, index and Install / Remove / Decline
 *  (`languages.ts`). */
import { useEffect, useState } from 'react';
import {
  Button,
  Card,
  Chip,
  ProgressBar,
  Switch,
  Typography,
} from '@heroui/react';
import { observer } from 'mobx-react-lite';
import type {
  CodeLanguage,
  CodeLsp,
  CodeLspScope,
  CodeLspToggle,
  Project,
} from '../../gen/contract';
import { useRootStore } from '../../stores/context';
import { Shell, Spinner } from '../bits';
import {
  INSTALL_POLICY_TEXT,
  LanguagesModel,
  canDecline,
  canInstall,
  canRemove,
  downloadOf,
  downloadPercent,
  downloadText,
  installView,
  isDownloading,
  languageName,
  lspNote,
  removeText,
  runView,
  type InstallView,
  type LanguageProgress,
} from './languages';

const LSP_ROWS: { scope: CodeLspScope; title: string; hint: string }[] = [
  {
    scope: 'review',
    title: 'Language servers in reviews',
    hint: 'Exact definitions and references while a PR is reviewed. Off by default: a PR’s code can run on this Mac while it is reviewed.',
  },
  {
    scope: 'spaces',
    title: 'Language servers in spaces',
    hint: 'A space’s agent gets exact definitions, references and renames in its own worktree.',
  },
];

export const ProjectLanguages = observer(function ProjectLanguages({
  project,
  onConfigWritten,
}: {
  project: Project;
  /** A switch wrote the project's local config file. */
  onConfigWritten?: () => void;
}) {
  const store = useRootStore();
  const [model] = useState(
    () =>
      new LanguagesModel(project.id, {
        progress: store.ephemeral,
        onConfigWritten: () => onConfigWritten?.(),
      }),
  );
  const [removing, setRemoving] = useState<CodeLanguage | null>(null);
  useEffect(() => {
    model.start();
    return () => model.stop();
  }, [model]);
  const { status, error } = model;
  let body;
  if (!status) {
    body = error ? (
      <span className="text-sm text-danger">
        Could not read this project’s languages: {error}
      </span>
    ) : (
      <Spinner />
    );
  } else if (!status.enabled) {
    body = (
      <Card variant="secondary" className="px-4 py-3 text-sm text-muted">
        Code intelligence is off for this project:{' '}
        <code className="font-mono">code_intel.enabled</code> is false in its
        config, or this build has none.
      </Card>
    );
  } else {
    body = (
      <>
        <Card variant="secondary" className="gap-0 p-0">
          {LSP_ROWS.map((r) => (
            <LspRow
              key={r.scope}
              title={r.title}
              hint={r.hint}
              toggle={status.lsp[r.scope]}
              pending={model.pending.has(`lsp:${r.scope}`)}
              onChange={(on) => void model.setLsp(r.scope, on)}
            />
          ))}
        </Card>
        <span className="text-xs text-muted">
          Saved in this project’s local config file (not committed); your global
          file is the default under it. A repo’s own config never turns them on.
        </span>
        {status.languages.length === 0 ? (
          <span className="text-sm text-muted">
            None yet — languages show up once a PR or a space opens a worktree
            of this project.
          </span>
        ) : (
          <Card variant="secondary" className="code-languages gap-0 p-0">
            {status.languages.map((l) => (
              <LanguageRow
                key={l.language}
                lang={l}
                lsp={status.lsp}
                progress={model.progress.get(l.language)}
                pending={model.pending.has(l.language)}
                onInstall={() => void model.install(l.language)}
                onRemove={() => setRemoving(l)}
                onDecline={() => void model.decline(l.language)}
              />
            ))}
          </Card>
        )}
        <span className="text-xs text-muted">
          {INSTALL_POLICY_TEXT[status.install]}
        </span>
        {error && (
          <span className="text-xs text-danger">
            Could not refresh: {error}
          </span>
        )}
      </>
    );
  }
  return (
    <section className="project-languages flex flex-col gap-2">
      <Typography.Heading level={6}>Languages</Typography.Heading>
      {body}
      {removing && (
        <RemoveDialog
          lang={removing}
          onRemove={() => model.remove(removing.language)}
          onClose={() => setRemoving(null)}
        />
      )}
    </section>
  );
});

function LspRow({
  title,
  hint,
  toggle,
  pending,
  onChange,
}: {
  title: string;
  hint: string;
  toggle: CodeLspToggle;
  pending: boolean;
  onChange: (on: boolean) => void;
}) {
  const note = lspNote(toggle);
  return (
    <div className="set-row flex items-center gap-4 border-b border-separator px-4 py-3 last:border-b-0">
      <div className="flex min-w-0 flex-1 flex-col gap-0.5">
        <span className="text-sm font-medium">{title}</span>
        <span className="text-xs text-muted">{hint}</span>
        {note && (
          <span className="truncate font-mono text-xs text-muted" title={note}>
            {note}
          </span>
        )}
      </div>
      <Switch
        size="sm"
        aria-label={title}
        isSelected={toggle.on}
        // The switch writes the local file, which wins over the global
        // one: it is never fixed by a file.
        isDisabled={pending}
        onChange={onChange}
      >
        <Switch.Content>
          <Switch.Control>
            <Switch.Thumb />
          </Switch.Control>
        </Switch.Content>
      </Switch>
    </div>
  );
}

function StateChip({
  view,
}: {
  view: Pick<InstallView, 'label' | 'tone'> & { title?: string | null };
}) {
  const chip = (
    <Chip size="sm" variant="soft" color={view.tone} className="shrink-0">
      {view.label}
    </Chip>
  );
  return view.title ? <span title={view.title}>{chip}</span> : chip;
}

function LanguageRow({
  lang,
  lsp,
  progress,
  pending,
  onInstall,
  onRemove,
  onDecline,
}: {
  lang: CodeLanguage;
  lsp: CodeLsp;
  progress: LanguageProgress | undefined;
  pending: boolean;
  onInstall: () => void;
  onRemove: () => void;
  onDecline: () => void;
}) {
  // No grammar to get: not a problem to report, text search serves it.
  const textOnly = lang.grammar.state === 'unavailable';
  const grammar: InstallView = textOnly
    ? {
        label: 'Text search',
        tone: 'default',
        detail: null,
        downloading: false,
      }
    : installView(lang.grammar);
  const server = lang.server
    ? installView(lang.server.install, lang.server.hint)
    : null;
  const running = lang.server ? runView(lang.server.state, lsp) : null;
  const download = downloadOf(progress);
  const downloading = isDownloading(lang, progress);
  const busy = pending || downloading;
  const errors = [grammar, server].flatMap((v) =>
    v?.tone === 'danger' && v.detail ? [v.detail] : [],
  );
  const serverReady =
    lang.server &&
    (lang.server.install.state === 'system' ||
      lang.server.install.state === 'installed');
  return (
    // The buttons wrap under the text when the row is too narrow for both:
    // three of them (Decline, Remove, Install) are wider than any fixed
    // column, and the text column has a floor of its own.
    <div className="code-language set-row flex flex-wrap items-center gap-x-4 gap-y-2 border-b border-separator px-4 py-3 last:border-b-0">
      <div className="flex min-w-[16rem] flex-1 flex-col gap-1">
        <span className="text-sm font-medium">
          {languageName(lang.language)}{' '}
          <span className="font-normal text-muted">
            · {lang.files} {lang.files === 1 ? 'file' : 'files'}
            {!textOnly && (
              <span className="tabular-nums" title="Files indexed">
                {' '}
                · {lang.index_percent}% indexed
              </span>
            )}
          </span>
        </span>
        <span className="flex flex-wrap items-center gap-1.5 text-xs text-muted">
          Grammar
          <StateChip view={grammar} />
          {lang.server ? (
            <>
              <span className="ml-1.5 min-w-0 break-all font-mono">
                {lang.server.name}
              </span>
              <StateChip view={server!} />
              {serverReady && running && <StateChip view={running} />}
            </>
          ) : (
            <span className="ml-1.5">no language server</span>
          )}
        </span>
        {download && <DownloadBar download={download} />}
        {server?.tone === 'success' && server.detail && (
          <span
            className="truncate font-mono text-xs text-muted"
            title={server.detail}
          >
            {server.detail}
          </span>
        )}
        {(server?.tone === 'default' || server?.tone === 'warning') &&
          server.detail && (
            <span className="text-xs break-words text-muted">
              {server.detail}
            </span>
          )}
        {errors.map((e) => (
          <span key={e} className="text-xs break-words text-danger">
            {e}
          </span>
        ))}
      </div>
      <div className="ml-auto flex shrink-0 items-center justify-end gap-1">
        {busy ? (
          <span className="flex items-center gap-1.5 text-xs text-muted">
            <Spinner /> {downloading && !download && 'Downloading…'}
          </span>
        ) : (
          <>
            {canDecline(lang) && (
              <Button
                size="sm"
                variant="ghost"
                className="code-decline"
                onPress={onDecline}
              >
                Decline
              </Button>
            )}
            {canRemove(lang) && (
              <Button
                size="sm"
                variant="ghost"
                className="code-remove"
                onPress={onRemove}
              >
                Remove
              </Button>
            )}
            {canInstall(lang) && (
              <Button
                size="sm"
                variant="secondary"
                className="code-install"
                onPress={onInstall}
              >
                Install
              </Button>
            )}
          </>
        )}
      </div>
    </div>
  );
}

/** "rust-analyzer · 12.3 of 40 MB" over a bar; indeterminate without a
 *  size. */
function DownloadBar({
  download,
}: {
  download: NonNullable<ReturnType<typeof downloadOf>>;
}) {
  const percent = downloadPercent(download);
  const text = `${download.name} · ${downloadText(download)}`;
  return (
    <div className="code-download flex flex-col gap-1">
      <span className="text-xs tabular-nums text-muted">{text}</span>
      <ProgressBar
        size="sm"
        aria-label={`Downloading ${text}`}
        value={percent ?? undefined}
        isIndeterminate={percent === null}
        className="w-full max-w-64"
      >
        <ProgressBar.Track>
          <ProgressBar.Fill />
        </ProgressBar.Track>
      </ProgressBar>
    </div>
  );
}

/** Remove's confirmation; the core's refusal stays in it. */
function RemoveDialog({
  lang,
  onRemove,
  onClose,
}: {
  lang: CodeLanguage;
  onRemove: () => Promise<string | null>;
  onClose: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [refused, setRefused] = useState<string | null>(null);
  const confirm = async () => {
    setBusy(true);
    setRefused(null);
    const why = await onRemove();
    setBusy(false);
    if (why === null) onClose();
    else setRefused(why);
  };
  return (
    <Shell
      title={`Remove ${languageName(lang.language)}?`}
      onClose={onClose}
      footer={
        <>
          <Button size="sm" variant="ghost" onPress={onClose}>
            Cancel
          </Button>
          <Button
            size="sm"
            variant="danger"
            className="code-remove-confirm"
            isDisabled={busy}
            onPress={() => void confirm()}
          >
            {busy ? 'Removing…' : 'Remove'}
          </Button>
        </>
      }
    >
      <span className="text-sm">{removeText(lang)}</span>
      {refused && (
        <span className="code-remove-error text-sm break-words text-danger">
          {refused}
        </span>
      )}
    </Shell>
  );
}
