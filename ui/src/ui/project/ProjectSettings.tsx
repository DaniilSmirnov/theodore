/**
 * A project's own settings: the route `project-settings`, reached from the
 * workspace switcher. Same shell as Settings — Back with Esc, the project's
 * name, a left menu — with three sections: General (the settings effective
 * for the project, remove), Languages (`ProjectLanguages`) and Config (the
 * config file's editor, `ConfigEditor`).
 */
import { useState, type ReactNode } from 'react';
import { ArrowLeft, Code, FileText, Sliders } from '@gravity-ui/icons';
import { Button, Card, ListBox, Typography } from '@heroui/react';
import { observer } from 'mobx-react-lite';
import type { Project } from '../../gen/contract';
import { removeProject } from '../../actions';
import { PROJECT_SETTINGS_SECTIONS } from '../../core/workspaces';
import { useRootStore, useUi } from '../../stores/context';
import type { ProjectSettingsSection } from '../../stores/UiStore';
import { K, Spinner } from '../bits';
import { useViewKeys } from '../keys';
import { projectLabel } from '../Projects';
import { AI_KEYS, AI_TITLE, SOURCE_NOTE, effectiveOf } from '../Settings';
import { ConfigEditor } from './ConfigEditor';
import { ProjectLanguages } from './ProjectLanguages';

export const SECTION_INFO: Record<
  ProjectSettingsSection,
  { title: string; sub: string; icon: ReactNode }
> = {
  general: {
    title: 'General',
    sub: 'The settings effective for this project, and where each came from.',
    icon: <Sliders />,
  },
  languages: {
    title: 'Languages',
    sub: 'Language servers in reviews and spaces, and what is downloaded for each language.',
    icon: <Code />,
  },
  config: {
    title: 'Config',
    sub: 'The project’s config files: create, edit, check and save.',
    icon: <FileText />,
  },
};

/** The project a route points at: found, still loading, or gone. */
function useRouteProject(id: string): {
  project: Project | undefined;
  loading: boolean;
} {
  const store = useRootStore();
  const project = store.models.projects.get(id);
  // Projects arrive with the first sync: until then a missing one isn't
  // removed, just not here yet.
  const loading =
    !project &&
    (store.sync.status === 'idle' || store.sync.status === 'bootstrapping');
  return { project, loading };
}

/** The sidebar while in a project's settings: Back, the project, its sections. */
export const ProjectSettingsNav = observer(function ProjectSettingsNav({
  project: id,
  section,
}: {
  project: string;
  section: ProjectSettingsSection;
}) {
  const ui = useUi();
  const { project } = useRouteProject(id);
  // Esc leaves, as the hint by "Back" says (inputs and dialogs keep their
  // own Esc: `useViewKeys` skips them).
  useViewKeys(
    ui,
    (e) => {
      if (e.key !== 'Escape') return false;
      ui.goBack();
    },
    [],
  );
  return (
    <aside className="sidebar settings-nav project-settings-nav flex w-64 shrink-0 flex-col overflow-hidden border-r border-separator bg-background">
      <div
        data-tauri-drag-region
        className="px-3 pb-2 pt-[calc(var(--controls-bottom)+0.75rem)]"
      >
        <Button
          variant="ghost"
          size="sm"
          className="settings-back h-8 w-full justify-start gap-2 px-2 text-muted"
          onPress={() => ui.goBack()}
        >
          <ArrowLeft className="size-4" />
          Back
          <span className="flex-1" />
          <K>Esc</K>
        </Button>
        {/* Titled like Settings; the project is named under it, quietly. */}
        <div className="mt-3 flex min-w-0 flex-col px-2">
          <Typography.Heading level={6}>Project settings</Typography.Heading>
          <span
            className="project-settings-name min-w-0 truncate text-xs text-muted"
            title={project ? projectLabel(project) : id}
          >
            {project ? projectLabel(project) : id}
          </span>
        </div>
      </div>
      {project ? (
        <ListBox
          aria-label="Project settings sections"
          selectionMode="single"
          selectedKeys={[section]}
          onSelectionChange={(keys) => {
            const k = keys === 'all' ? undefined : [...keys][0];
            if (k)
              ui.go({
                name: 'project-settings',
                project: id,
                section: k as ProjectSettingsSection,
              });
          }}
          className="min-h-0 flex-1 overflow-auto px-2 pb-3"
        >
          {PROJECT_SETTINGS_SECTIONS.map((s) => (
            <ListBox.Item
              key={s}
              id={s}
              textValue={SECTION_INFO[s].title}
              className="nav gap-3 px-3 text-sm data-[selected=true]:font-medium"
            >
              <span className="grid size-4 place-items-center text-muted [&>svg]:size-4">
                {SECTION_INFO[s].icon}
              </span>
              {SECTION_INFO[s].title}
            </ListBox.Item>
          ))}
        </ListBox>
      ) : (
        <div className="min-h-0 flex-1" />
      )}
    </aside>
  );
});

/** General: the settings effective for this project and where each came
 *  from, then Remove. */
const General = observer(function General({ project }: { project: Project }) {
  const store = useRootStore();
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const scope = `repo:${project.repo.owner}/${project.repo.name}`;
  return (
    <>
      <section className="flex flex-col gap-2">
        <Typography.Heading level={6}>Effective settings</Typography.Heading>
        <Card variant="secondary" className="gap-0 p-0">
          {AI_KEYS.map((key) => {
            const eff = effectiveOf(store, scope, key);
            return (
              <div
                key={key}
                className="set-row flex items-center gap-4 border-b border-separator px-4 py-3 last:border-b-0"
              >
                <div className="flex min-w-0 flex-1 flex-col gap-0.5">
                  <span className="text-sm font-medium">{AI_TITLE[key]}</span>
                  <span className="text-xs text-muted">
                    {SOURCE_NOTE[eff.source]}
                  </span>
                </div>
                <span className="text-sm tabular-nums text-muted">
                  {eff.value == null ? '—' : String(eff.value)}
                </span>
              </div>
            );
          })}
        </Card>
        <span className="text-xs text-muted">
          A value from the project’s config wins over your user settings for
          this repo — change it in Config.
        </span>
      </section>
      <section className="flex flex-col gap-2">
        <Typography.Heading level={6}>Remove project</Typography.Heading>
        <Card
          variant="secondary"
          className="flex flex-row items-center gap-3 p-4"
        >
          <span className="flex-1 text-sm text-muted">
            {project.repo.host === 'local'
              ? 'Theodore stops tracking this folder. The folder itself stays where it is.'
              : 'Theodore stops tracking this repository. Nothing changes on the host.'}
          </span>
          {confirming ? (
            <>
              <Button
                size="sm"
                variant="ghost"
                isDisabled={busy}
                onPress={() => setConfirming(false)}
              >
                Cancel
              </Button>
              <Button
                size="sm"
                variant="danger"
                className="project-remove-confirm"
                isDisabled={busy}
                onPress={() => {
                  setBusy(true);
                  void removeProject(store, project.id).finally(() =>
                    setBusy(false),
                  );
                }}
              >
                Remove {projectLabel(project)}
              </Button>
            </>
          ) : (
            <Button
              size="sm"
              variant="danger-soft"
              className="project-remove"
              onPress={() => setConfirming(true)}
            >
              Remove project…
            </Button>
          )}
        </Card>
      </section>
    </>
  );
});

/** The content of one section of a project's settings. */
export const ProjectSettingsPage = observer(function ProjectSettingsPage({
  project: id,
  section,
}: {
  project: string;
  section: ProjectSettingsSection;
}) {
  const ui = useUi();
  const { project, loading } = useRouteProject(id);
  const info = SECTION_INFO[section];
  return (
    <div className="view settings-page project-settings-page flex min-w-0 flex-1 flex-col overflow-auto">
      <div className="mx-auto flex w-full max-w-[760px] flex-col gap-6 px-8 py-8">
        {project ? (
          <>
            <div>
              <Typography.Heading level={4}>{info.title}</Typography.Heading>
              <span className="text-sm text-muted">{info.sub}</span>
            </div>
            {section === 'general' ? (
              <General project={project} />
            ) : section === 'languages' ? (
              <ProjectLanguages key={project.id} project={project} />
            ) : (
              <ConfigEditor key={project.id} project={project} />
            )}
          </>
        ) : loading ? (
          <Spinner />
        ) : (
          // Removed (or never added on this Mac): a link or a restored tab
          // outliving its project.
          <div className="project-gone flex flex-col items-start gap-3">
            <Typography.Heading level={4}>
              This project is no longer here
            </Typography.Heading>
            <span className="text-sm text-muted">
              It was removed, or it was never added on this Mac.
            </span>
            <div className="flex gap-2">
              <Button size="sm" onPress={() => ui.goBack()}>
                Back
              </Button>
              <Button
                size="sm"
                variant="secondary"
                onPress={() => ui.openModal('manage-projects')}
              >
                Manage projects…
              </Button>
            </div>
          </div>
        )}
      </div>
    </div>
  );
});
