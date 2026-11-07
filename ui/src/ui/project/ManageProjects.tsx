/**
 * Manage projects: every project as a plain list in a dialog. Drag to
 * reorder (the switcher and ⌃1…9 follow, `ws.reorder`), a row opens that
 * project's settings, and Add project… sits at the bottom. A menu can't
 * take a drag, so the switcher's "Manage projects…" opens this.
 */
import { Folder, Gear, Grip, Plus } from '@gravity-ui/icons';
import { Button, Card, ListBox } from '@heroui/react';
import { observer } from 'mobx-react-lite';
import { useDragAndDrop, type Key } from 'react-aria-components';
import { FEATURES } from '../../features';
import { useUi, useWorkspaces } from '../../stores/context';
import { projectSettingsRoute } from '../../core/workspaces';
import { Shell } from '../bits';
import { projectLabel } from '../Projects';

export const ManageProjectsModal = observer(function ManageProjectsModal() {
  const ui = useUi();
  const ws = useWorkspaces();
  const projects = ws.projects; // switcher order, not add order
  const { dragAndDropHooks } = useDragAndDrop({
    getItems: (keys) => [...keys].map((key) => ({ 'text/plain': String(key) })),
    onReorder(e) {
      const { dropPosition } = e.target;
      if (dropPosition !== 'before' && dropPosition !== 'after') return;
      ws.reorder([...e.keys].map(String), String(e.target.key), dropPosition);
    },
  });
  return (
    <Shell
      title="Manage projects"
      onClose={() => ui.closeModal()}
      footer={
        <Button
          size="sm"
          variant="secondary"
          className="manage-add"
          isDisabled={!FEATURES.projects}
          onPress={() => ui.openModal('add-project')}
        >
          <Plus className="size-4" />
          Add project…
        </Button>
      }
    >
      {projects.length === 0 ? (
        <span className="text-sm text-muted">
          No projects yet — connect a repository or a local folder.
        </span>
      ) : (
        <>
          <Card variant="secondary" className="gap-0 p-0">
            <ListBox
              aria-label="Projects"
              selectionMode="none"
              dragAndDropHooks={dragAndDropHooks}
              onAction={(k: Key) =>
                ui.go(projectSettingsRoute(String(k), 'general'))
              }
              className="p-0 [&>*+*]:mt-0"
            >
              {projects.map((p) => (
                <ListBox.Item
                  key={p.id}
                  id={p.id}
                  textValue={projectLabel(p)}
                  className="repo-row flex w-full items-center gap-3 rounded-none border-b border-separator px-4 py-3 last:border-b-0 hover:bg-default/50 data-[dragging]:opacity-50"
                >
                  <Grip
                    aria-hidden
                    className="size-4 shrink-0 cursor-grab text-muted"
                  />
                  <Folder className="size-4 text-muted" />
                  <span className="min-w-0 flex-1 truncate text-sm">
                    {projectLabel(p)}
                  </span>
                  <Gear
                    aria-label="Project settings"
                    className="size-4 text-muted"
                  />
                </ListBox.Item>
              ))}
            </ListBox>
          </Card>
          <span className="text-xs text-muted">
            Drag to reorder — the switcher and ⌃1…9 follow this order. A row
            opens the project’s settings.
          </span>
        </>
      )}
    </Shell>
  );
});
