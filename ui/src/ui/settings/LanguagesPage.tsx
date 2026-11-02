/** Settings → Languages: language servers on or off, then each language
 *  of a project with its grammar, server and index (`languages.ts`). */
import { useEffect, useState } from 'react';
import {
  Button,
  Card,
  Chip,
  ListBox,
  Select,
  Switch,
  Typography,
} from '@heroui/react';
import { observer } from 'mobx-react-lite';
import type {
  CodeLanguage,
  CodeLspScope,
  CodeLspToggle,
  Project,
} from '../../gen/contract';
import { useWorkspaces } from '../../stores/context';
import { Spinner } from '../bits';
import { projectLabel } from '../Projects';
import {
  INSTALL_POLICY_TEXT,
  LanguagesModel,
  SERVER_STATE,
  canDecline,
  canInstall,
  installView,
  isDownloading,
  languageName,
  lspNote,
  type InstallView,
} from './languages';

const LSP_ROWS: { scope: CodeLspScope; title: string; hint: string }[] = [
  {
    scope: 'review',
    title: 'Language servers in reviews',
    hint: 'Exact definitions and references while a PR is reviewed. Turning it on lets the PR’s code run on this Mac: rust-analyzer and the TypeScript servers execute the project’s config.',
  },
  {
    scope: 'spaces',
    title: 'Language servers in spaces',
    hint: 'A space’s agent gets exact definitions, references and renames in its own worktree.',
  },
];

export const LanguagesPage = observer(function LanguagesPage() {
  const ws = useWorkspaces();
  const projects = ws.projects;
  const [sel, setSel] = useState<string | null>(null);
  const project =
    projects.find((p) => p.id === sel) ?? ws.active ?? projects[0] ?? null;
  if (!project) {
    return (
      <span className="text-sm text-muted">
        No projects yet — connect a repository to see its languages.
      </span>
    );
  }
  return (
    <>
      {projects.length > 1 && (
        <Card
          variant="secondary"
          className="flex-row items-center gap-4 px-4 py-3"
        >
          <span className="flex-1 text-sm text-muted">Project</span>
          <Select
            aria-label="Project"
            value={project.id}
            onChange={(v) => v != null && setSel(String(v))}
            className="w-64"
          >
            <Select.Trigger>
              <Select.Value />
              <Select.Indicator />
            </Select.Trigger>
            <Select.Popover>
              <ListBox selectionMode="single">
                {projects.map((p) => (
                  <ListBox.Item
                    key={p.id}
                    id={p.id}
                    textValue={projectLabel(p)}
                  >
                    {projectLabel(p)}
                    <ListBox.ItemIndicator />
                  </ListBox.Item>
                ))}
              </ListBox>
            </Select.Popover>
          </Select>
        </Card>
      )}
      <ProjectLanguages key={project.id} project={project} />
    </>
  );
});

const ProjectLanguages = observer(function ProjectLanguages({
  project,
}: {
  project: Project;
}) {
  const [model] = useState(() => new LanguagesModel(project.id));
  useEffect(() => {
    model.start();
    return () => model.stop();
  }, [model]);
  const { status, error } = model;
  if (!status) {
    return error ? (
      <span className="text-sm text-danger">
        Could not read this project’s languages: {error}
      </span>
    ) : (
      <Spinner />
    );
  }
  if (!status.enabled) {
    return (
      <Card variant="secondary" className="px-4 py-3 text-sm text-muted">
        Code intelligence is off for this project:{' '}
        <code className="font-mono">code_intel.enabled</code> is false in its
        config, or this build has none.
      </Card>
    );
  }
  return (
    <>
      <section className="flex flex-col gap-2">
        <Typography.Heading level={6}>Language servers</Typography.Heading>
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
          Both apply to every project. A line in your global or local config
          file wins over these switches; a repo’s own config never turns them
          on.
        </span>
      </section>
      <section className="flex flex-col gap-2">
        <Typography.Heading level={6}>Languages</Typography.Heading>
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
                pending={model.pending.has(l.language)}
                onInstall={() => void model.install(l.language)}
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
      </section>
    </>
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
        // A config file fixes it until the line is removed.
        isDisabled={note !== null || pending}
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

function StateChip({ view }: { view: Pick<InstallView, 'label' | 'tone'> }) {
  return (
    <Chip size="sm" variant="soft" color={view.tone} className="shrink-0">
      {view.label}
    </Chip>
  );
}

function LanguageRow({
  lang,
  pending,
  onInstall,
  onDecline,
}: {
  lang: CodeLanguage;
  pending: boolean;
  onInstall: () => void;
  onDecline: () => void;
}) {
  const grammar = installView(lang.grammar);
  const server = lang.server ? installView(lang.server.install) : null;
  const running = lang.server ? SERVER_STATE[lang.server.state] : null;
  const busy = pending || isDownloading(lang);
  const errors = [grammar, server].flatMap((v) =>
    v?.tone === 'danger' && v.detail ? [v.detail] : [],
  );
  const serverReady =
    lang.server &&
    (lang.server.install.state === 'system' ||
      lang.server.install.state === 'installed');
  return (
    <div className="code-language set-row flex items-center gap-4 border-b border-separator px-4 py-3 last:border-b-0">
      <div className="flex min-w-0 flex-1 flex-col gap-1">
        <span className="text-sm font-medium">
          {languageName(lang.language)}{' '}
          <span className="font-normal text-muted">
            · {lang.files} {lang.files === 1 ? 'file' : 'files'}
          </span>
        </span>
        <span className="flex flex-wrap items-center gap-1.5 text-xs text-muted">
          Grammar
          <StateChip view={grammar} />
          {lang.server ? (
            <>
              <span className="ml-1.5 font-mono">{lang.server.name}</span>
              <StateChip view={server!} />
              {serverReady && running && <StateChip view={running} />}
            </>
          ) : (
            <span className="ml-1.5">no language server</span>
          )}
        </span>
        {server?.tone === 'success' && server.detail && (
          <span
            className="truncate font-mono text-xs text-muted"
            title={server.detail}
          >
            {server.detail}
          </span>
        )}
        {server?.tone === 'default' && server.detail && (
          <span className="text-xs text-muted">{server.detail}</span>
        )}
        {errors.map((e) => (
          <span key={e} className="text-xs break-words text-danger">
            {e}
          </span>
        ))}
      </div>
      <span
        className="w-20 shrink-0 text-right text-xs tabular-nums text-muted"
        title="Files indexed"
      >
        {lang.index_percent}% indexed
      </span>
      <div className="flex w-36 shrink-0 items-center justify-end gap-1">
        {busy ? (
          <span className="flex items-center gap-1.5 text-xs text-muted">
            <Spinner /> {isDownloading(lang) && 'Downloading…'}
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
