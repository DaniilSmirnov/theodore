/**
 * Settings → Labs → Global config: the global config file (open it in the
 * editor) and, per project, the merged config read-only — which files it
 * comes from, every effective value with its layer, unknown keys, and
 * whether the repo's commands are allowed. Also `SpaceSetupAssistHint`,
 * the space page's "no `spaces:` commands yet" nudge.
 */
import { useEffect, useState } from 'react';

import {
  ChevronDown,
  ChevronRight,
  Folder,
  Sparkles,
  TriangleExclamation,
} from '@gravity-ui/icons';
import { Button, Card, Chip, Typography } from '@heroui/react';
import { observer } from 'mobx-react-lite';
import type {
  ConfigLayer,
  EffectiveConfig,
  GlobalConfigInfo,
  Project,
  Space,
  SpaceConfigView,
} from '../../gen/contract';
import { api } from '../../core/api';
import { useWorkspaces } from '../../stores/context';
import { Spinner } from '../bits';
import { projectLabel } from '../Projects';
import { LAYER_LABEL, fileStatus, groupEntries } from './commands';

const LAYER_COLOR: Record<ConfigLayer, 'default' | 'accent' | 'warning'> = {
  global: 'default',
  repo: 'warning',
  local: 'accent',
};

function LayerBadge({ layer }: { layer: ConfigLayer }) {
  return (
    <Chip size="sm" variant="soft" color={LAYER_COLOR[layer]}>
      {LAYER_LABEL[layer]}
    </Chip>
  );
}

/** The space page's empty-state nudge: no `spaces:` commands are
 *  configured for this project yet, so there's nothing to run or check.
 *  "Set up with agent" sends the space's own agent a turn to look at the
 *  repo and write them (docs/config.md §2, §8); the normal trust prompt
 *  applies to whatever it proposes, same as any other repo config. */
export const SpaceSetupAssistHint = observer(function SpaceSetupAssistHint({
  space,
  config,
}: {
  space: Pick<Space, 'id' | 'status'>;
  config: Pick<SpaceConfigView, 'commands'> | null;
}) {
  const [busy, setBusy] = useState(false);
  if (
    space.status === 'archived' ||
    config == null ||
    config.commands.length > 0
  ) {
    return null;
  }
  const run = async () => {
    setBusy(true);
    try {
      await api.spaceSetupAssist(space.id);
    } catch {
      // toasted by api.ts
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="space-setup-assist-hint flex items-center gap-2 text-xs text-muted">
      <span className="flex-1">
        This project has no setup, run or check commands yet.
      </span>
      <Button
        size="sm"
        variant="secondary"
        className="space-setup-assist shrink-0"
        isPending={busy}
        onPress={() => void run()}
      >
        <Sparkles className="size-3.5" />
        Set up with agent
      </Button>
    </div>
  );
});

export const GlobalConfigSection = observer(function GlobalConfigSection() {
  const ws = useWorkspaces();
  const [info, setInfo] = useState<GlobalConfigInfo | null>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    let on = true;
    api
      .globalConfig()
      .then((i) => on && setInfo(i))
      .catch(() => {});
    return () => {
      on = false;
    };
  }, []);
  const open = async () => {
    setBusy(true);
    try {
      setInfo(await api.openGlobalConfig());
    } catch {
      // toasted by api.ts
    } finally {
      setBusy(false);
    }
  };
  return (
    <section className="config-section mt-4 flex flex-col gap-2">
      <Typography.Heading level={6}>Global config</Typography.Heading>
      <Card
        variant="secondary"
        className="flex flex-row items-center gap-3 px-4 py-3"
      >
        <div className="flex min-w-0 flex-1 flex-col gap-0.5">
          <span className="truncate font-mono text-sm">
            {info?.path ?? '…'}
          </span>
          <span className="text-xs text-muted">
            {info == null
              ? 'Loading…'
              : info.exists
                ? 'Applies to every project; a repo’s own config and your local file win over it, key by key.'
                : 'Not created yet — Open in editor creates it from a commented template.'}
          </span>
        </div>
        <Button
          size="sm"
          variant="secondary"
          className="config-open shrink-0"
          isPending={busy}
          onPress={() => void open()}
        >
          Open in editor
        </Button>
      </Card>
      {ws.projects.length > 0 && (
        <Card variant="secondary" className="gap-0 p-0">
          {ws.projects.map((p) => (
            <ProjectConfigRow key={p.id} project={p} />
          ))}
        </Card>
      )}
    </section>
  );
});

/** One project: collapsed to its name; expanded, its merged config
 *  (fetched when opened). */
function ProjectConfigRow({ project }: { project: Project }) {
  const [open, setOpen] = useState(false);
  const [config, setConfig] = useState<EffectiveConfig | null>(null);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    if (!open) return;
    let on = true;
    api
      .projectConfig(project.id)
      .then((c) => on && setConfig(c))
      .catch(() => on && setFailed(true));
    return () => {
      on = false;
    };
  }, [open, project.id]);
  return (
    <div className="config-project border-b border-separator last:border-b-0">
      <button
        type="button"
        aria-expanded={open}
        className="flex w-full items-center gap-3 px-4 py-3 text-left hover:bg-default/50"
        onClick={() => {
          setFailed(false);
          setOpen((v) => !v);
        }}
      >
        <Folder className="size-4 text-muted" />
        <span className="min-w-0 flex-1 truncate text-sm">
          {projectLabel(project)}
        </span>
        {open ? (
          <ChevronDown className="size-4 text-muted" />
        ) : (
          <ChevronRight className="size-4 text-muted" />
        )}
      </button>
      {open && (
        <div className="flex flex-col gap-3 px-4 pb-4">
          {failed ? (
            <span className="text-xs text-danger">
              Could not read this project's config.
            </span>
          ) : !config ? (
            <Spinner />
          ) : (
            <EffectiveConfigView config={config} />
          )}
        </div>
      )}
    </div>
  );
}

export function EffectiveConfigView({ config }: { config: EffectiveConfig }) {
  const repo = config.repo_commands;
  return (
    <>
      <ul className="flex flex-col gap-1">
        {config.files.map((f) => {
          const status = fileStatus(f);
          return (
            <li
              key={`${f.layer}:${f.path}`}
              className="flex min-w-0 items-center gap-2 text-xs"
            >
              <LayerBadge layer={f.layer} />
              <span
                className={`min-w-0 truncate font-mono ${f.exists ? '' : 'text-muted'}`}
                title={f.path}
              >
                {f.path}
              </span>
              {status && (
                <span
                  className={
                    f.error || f.legacy ? 'text-warning' : 'text-muted'
                  }
                >
                  {status}
                </span>
              )}
            </li>
          );
        })}
      </ul>
      {config.unknown.length > 0 && (
        <div className="flex items-start gap-2 text-xs text-warning">
          <TriangleExclamation className="mt-0.5 size-3.5 shrink-0" />
          <span>
            Unknown keys (typos, or from a newer version):{' '}
            <span className="font-mono">{config.unknown.join(', ')}</span>
          </span>
        </div>
      )}
      {repo && (
        <span className="text-xs text-muted">
          Commands from this repo ({repo.items.length}):{' '}
          {repo.trusted ? (
            <span className="text-success">allowed</span>
          ) : (
            <span className="text-warning">not allowed yet</span>
          )}
        </span>
      )}
      {config.entries.length === 0 ? (
        <span className="text-xs text-muted">No values set.</span>
      ) : (
        <div className="flex flex-col gap-2">
          {groupEntries(config.entries).map((g) => (
            <div key={g.section} className="flex flex-col">
              <span className="pb-1 text-xs font-medium">{g.section}</span>
              {g.entries.map((e) => (
                <div
                  key={e.key}
                  className="config-entry grid grid-cols-[minmax(0,2fr)_minmax(0,3fr)_auto] items-center gap-2 border-t border-separator py-1 text-xs"
                >
                  <span className="truncate font-mono" title={e.key}>
                    {e.key}
                  </span>
                  <span
                    className="truncate font-mono text-muted"
                    title={e.value}
                  >
                    {e.value}
                  </span>
                  <LayerBadge layer={e.layer} />
                </div>
              ))}
            </div>
          ))}
        </div>
      )}
    </>
  );
}
