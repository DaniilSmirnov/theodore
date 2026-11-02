/**
 * Settings → Projects → ⟨project⟩ → Config: create the project's config
 * from what is already set (or open the file that exists), edit it with
 * live checking and the effective-config preview, and save it to the repo
 * (a PR for a host project), your local file, or the global file
 * (docs/config.md "Creating and saving the config").
 */
import { useEffect, useRef, useState } from 'react';

import { ChevronDown, TriangleExclamation } from '@gravity-ui/icons';
import {
  Button,
  Card,
  Dropdown,
  ToggleButton,
  ToggleButtonGroup,
  Typography,
} from '@heroui/react';
import { observer } from 'mobx-react-lite';
import type { ConfigCheck, ConfigDraft, Project } from '../../gen/contract';
import { api } from '../../core/api';
import { useRootStore } from '../../stores/context';
import { Spinner } from '../bits';
import { EffectiveConfigView } from '../space/ConfigSection';
import { LAYER_LABEL } from '../space/commands';
import {
  type ConfigSource,
  hasErrors,
  newlineEdit,
  offsetOfLine,
  previewLayer,
  problemText,
  saveOptions,
  savedText,
  startOf,
  tabEdit,
  textOf,
} from './configFile';

const CHECK_DELAY_MS = 300;

const SOURCES: { id: ConfigSource; label: string }[] = [
  { id: 'generated', label: 'From settings' },
  { id: 'repo', label: 'Repo' },
  { id: 'local', label: 'Local' },
  { id: 'global', label: 'Global' },
];

export const ConfigEditor = observer(function ConfigEditor({
  project,
  rev = 0,
}: {
  project: Project;
  /** Bumped when something else wrote a layer's file (the Languages
   *  switches): the files are read again, the text being edited stays. */
  rev?: number;
}) {
  const store = useRootStore();
  const [draft, setDraft] = useState<ConfigDraft | null>(null);
  const [failed, setFailed] = useState(false);
  const [open, setOpen] = useState(false);
  const [source, setSource] = useState<ConfigSource>('generated');
  const [text, setText] = useState('');
  const [check, setCheck] = useState<ConfigCheck | null>(null);
  const [saving, setSaving] = useState(false);
  const area = useRef<HTMLTextAreaElement>(null);

  const load = () =>
    api
      .configDraft(project.id)
      .then((d) => {
        setDraft(d);
        return d;
      })
      .catch(() => {
        setFailed(true);
        return null;
      });

  useEffect(() => {
    let on = true;
    setDraft(null);
    setOpen(false);
    setFailed(false);
    api
      .configDraft(project.id)
      .then((d) => on && setDraft(d))
      .catch(() => on && setFailed(true));
    return () => {
      on = false;
    };
  }, [project.id]);

  useEffect(() => {
    if (rev === 0) return;
    let on = true;
    api
      .configDraft(project.id)
      .then((d) => on && setDraft(d))
      .catch(() => {});
    return () => {
      on = false;
    };
  }, [project.id, rev]);

  // Checked as you type, a beat after the last key.
  useEffect(() => {
    if (!open) return;
    let on = true;
    const t = setTimeout(() => {
      api
        .checkConfig({
          project: project.id,
          layer: previewLayer(source),
          text,
        })
        .then((c) => on && setCheck(c))
        .catch(() => {});
    }, CHECK_DELAY_MS);
    return () => {
      on = false;
      clearTimeout(t);
    };
  }, [open, project.id, source, text]);

  if (failed) {
    return (
      <span className="text-xs text-danger">
        Could not read this project's config.
      </span>
    );
  }
  if (!draft) return <Spinner />;

  const start = () => {
    const s = startOf(draft);
    setSource(s.source);
    setText(s.text);
    setOpen(true);
  };

  const pick = (s: ConfigSource) => {
    const t = textOf(draft, s);
    setSource(s);
    setText(t ?? '');
  };

  const jump = (line: number) => {
    const el = area.current;
    if (!el) return;
    const at = offsetOfLine(text, line);
    el.focus();
    el.setSelectionRange(at, at);
  };

  const apply = (edit: { text: string; start: number; end: number }) => {
    setText(edit.text);
    requestAnimationFrame(() =>
      area.current?.setSelectionRange(edit.start, edit.end),
    );
  };

  const save = async (id: string) => {
    const opt = saveOptions(draft, project.path != null).find(
      (o) => o.id === id,
    );
    if (!opt) return;
    setSaving(true);
    try {
      const r = await api.saveConfig({
        project: project.id,
        layer: opt.layer,
        text,
        commit: opt.commit,
      });
      store.ephemeral.toast(savedText(r), 'info');
      const d = await load();
      if (d) setSource(opt.layer);
    } catch {
      // toasted by api.ts
    } finally {
      setSaving(false);
    }
  };

  const exists = draft.files.some(
    (f) => f.layer !== 'global' && f.text != null,
  );
  const problems = check?.problems ?? [];
  const blocked = hasErrors(problems);
  const options = saveOptions(draft, project.path != null);

  return (
    <section className="config-editor flex flex-col gap-2">
      <Typography.Heading level={6}>Config</Typography.Heading>
      {!open ? (
        <Card
          variant="secondary"
          className="flex flex-row items-center gap-3 px-4 py-3"
        >
          <span className="min-w-0 flex-1 text-xs text-muted">
            {exists
              ? 'This project has a config file. Open it to edit, check and save it.'
              : 'No config file yet. Start one from what is already set for this project: agents per task, plus a commented template for the rest.'}
          </span>
          <Button
            size="sm"
            variant="secondary"
            className="config-create shrink-0"
            onPress={start}
          >
            {exists ? 'Edit config' : 'Create from current settings'}
          </Button>
        </Card>
      ) : (
        <>
          <div className="flex flex-wrap items-center gap-2">
            <span className="text-xs text-muted">Load</span>
            <ToggleButtonGroup
              size="sm"
              selectionMode="single"
              disallowEmptySelection
              aria-label="Load from"
              selectedKeys={new Set([source])}
              onSelectionChange={(keys) => {
                const v = [...keys][0];
                if (v) pick(String(v) as ConfigSource);
              }}
            >
              {SOURCES.map((s, i) => (
                <ToggleButton
                  key={s.id}
                  id={s.id}
                  isDisabled={textOf(draft, s.id) == null}
                  className="whitespace-nowrap"
                >
                  {i > 0 && <ToggleButtonGroup.Separator />}
                  {s.label}
                </ToggleButton>
              ))}
            </ToggleButtonGroup>
            <span className="flex-1" />
            <Dropdown>
              <Button
                size="sm"
                variant="primary"
                className="config-save gap-1"
                isDisabled={blocked}
                isPending={saving}
              >
                Save to…
                <ChevronDown className="size-3.5" />
              </Button>
              <Dropdown.Popover placement="bottom end">
                <Dropdown.Menu
                  aria-label="Save to"
                  onAction={(k) => void save(String(k))}
                >
                  {options.map((o) => (
                    <Dropdown.Item key={o.id} id={o.id} textValue={o.label}>
                      <div className="flex flex-col">
                        <span className="text-sm">{o.label}</span>
                        <span className="max-w-[360px] truncate font-mono text-xs text-muted">
                          {o.replaces ? 'replaces ' : ''}
                          {o.detail}
                        </span>
                      </div>
                    </Dropdown.Item>
                  ))}
                </Dropdown.Menu>
              </Dropdown.Popover>
            </Dropdown>
          </div>
          <textarea
            ref={area}
            aria-label="Config YAML"
            spellCheck={false}
            className="config-text min-h-[320px] w-full resize-y rounded-lg border border-separator bg-surface p-3 font-mono text-xs leading-5 outline-none focus:border-accent"
            value={text}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              const el = e.currentTarget;
              if (e.key === 'Tab') {
                e.preventDefault();
                apply(
                  tabEdit(
                    el.value,
                    el.selectionStart,
                    el.selectionEnd,
                    e.shiftKey,
                  ),
                );
              } else if (
                e.key === 'Enter' &&
                !e.shiftKey &&
                !e.metaKey &&
                !e.ctrlKey
              ) {
                e.preventDefault();
                apply(
                  newlineEdit(el.value, el.selectionStart, el.selectionEnd),
                );
              }
            }}
          />
          {problems.length > 0 ? (
            <ul className="config-problems flex flex-col gap-1">
              {problems.map((p, i) => (
                <li key={i}>
                  <button
                    type="button"
                    className={`flex items-start gap-2 text-left text-xs ${p.level === 'error' ? 'text-danger' : 'text-warning'}`}
                    onClick={() => p.line != null && jump(p.line)}
                  >
                    <TriangleExclamation className="mt-0.5 size-3.5 shrink-0" />
                    {problemText(p)}
                  </button>
                </li>
              ))}
            </ul>
          ) : (
            check && <span className="text-xs text-success">No problems.</span>
          )}
          {project.path != null && !draft.can_commit && draft.commit_note && (
            <span className="text-xs text-muted">
              Saving to the repo won't commit: {draft.commit_note}.
            </span>
          )}
          {check && (
            <div className="flex flex-col gap-2">
              <span className="text-xs font-medium">
                Effective config if saved as {LAYER_LABEL[previewLayer(source)]}
              </span>
              <Card variant="secondary" className="flex flex-col gap-3 p-4">
                <EffectiveConfigView config={check.preview} />
              </Card>
            </div>
          )}
        </>
      )}
    </section>
  );
});
