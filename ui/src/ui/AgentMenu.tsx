/**
 * The model chip. Every agent shows the one model it runs, with its effort
 * (no "default": what's shown is what runs); a row makes that agent the
 * one this box talks to. "Models" holds every model each CLI offers (older
 * versions one level down), "Effort" the levels the current model takes,
 * and ⌘⇧/ cycles the effort from the box the chip sits in. Model and effort
 * are the agent's settings, shared everywhere. The same chip sits in a
 * space's box and in the AI panel's.
 */
import { useEffect, useMemo, useRef } from 'react';
import { Check, ChevronDown, Gear } from '@gravity-ui/icons';
import { Button, Dropdown, Header, Separator } from '@heroui/react';
import { observer } from 'mobx-react-lite';
import type { AgentId, AgentModel } from '../gen/contract';
import { useRootStore, useUi } from '../stores/context';
import type { RootStore } from '../stores/RootStore';
import {
  AGENT_NAME,
  agentModel,
  effortKey,
  effortLabel,
  nextEffort,
  setAgentEffort,
  setAgentModel,
  takesEffort,
} from './agentModel';
import { useAgentModels } from './useAgentModels';

/** Claude Code's multi-agent mode, past Max: spaces only (`spaces.rs`). */
export const ULTRACODE = 'ultracode';
import { K } from './bits';

/** What an agent runs: its model, the levels that model takes, and the
 *  level in use. `model` is null only when the CLI lists nothing and none
 *  is set (Gemini without a model in Settings). */
export type Running = {
  model: AgentModel | null;
  /** `model` is one of the catalog's (not a name typed in Settings). */
  listed: boolean;
  efforts: string[];
  effort: string | null;
};

function rawEffort(store: RootStore, agent: AgentId): string | null {
  const v = store.models.settings.get(`user:${effortKey(agent)}`)?.value;
  return typeof v === 'string' && v.trim() ? v.trim() : null;
}

/** The concrete model and effort `agent` runs with, given its catalog: the
 *  setting (as typed when the catalog lacks it), else the catalog's first
 *  current model; the effort set when that model takes it, else the
 *  model's own default. */
export function running(
  store: RootStore,
  agent: AgentId,
  catalog: AgentModel[] | undefined,
): Running {
  const id = agentModel(store, agent);
  const list = catalog ?? [];
  const typed: AgentModel | null = id
    ? {
        id,
        label: id,
        description: null,
        efforts: [],
        default_effort: null,
        older: false,
      }
    : null;
  // An alias (`opus`, `fable`) is the newest model of its family.
  const listedModel =
    list.find((m) => m.id === id) ??
    (id
      ? (list.find((m) => !m.older && m.id.includes(id)) ??
        list.find((m) => m.id.includes(id)))
      : (list.find((m) => !m.older) ?? list[0]));
  const model = listedModel ?? typed ?? null;
  const efforts = takesEffort(agent) ? (model?.efforts ?? []) : [];
  const set = rawEffort(store, agent);
  // Picked in a space, kept everywhere (it runs at max outside spaces).
  if (agent === 'claude' && set === ULTRACODE && efforts.length) {
    return { model, listed: !!listedModel, efforts, effort: set };
  }
  const own = model?.default_effort;
  const effort =
    set && efforts.includes(set)
      ? set
      : own && efforts.includes(own)
        ? own
        : (efforts.find((e) => e === 'medium') ?? efforts[0] ?? null);
  return { model, listed: !!listedModel, efforts, effort };
}

const pickKey = (agent: AgentId, id: string) => `${agent}|${id}`;

function ModelItems({
  agent,
  models,
  current,
}: {
  agent: AgentId;
  models: AgentModel[];
  current: string | null;
}) {
  return models.map((m) => (
    <Dropdown.Item
      key={pickKey(agent, m.id)}
      id={pickKey(agent, m.id)}
      textValue={m.label}
      className="gap-2"
    >
      <span className="flex min-w-0 flex-1 flex-col">
        <span className="truncate">{m.label}</span>
        {m.description && (
          <span className="truncate text-xs text-muted">{m.description}</span>
        )}
      </span>
      {m.id === current && <Check className="size-3.5 shrink-0" />}
    </Dropdown.Item>
  ));
}

export const AgentMenu = observer(function AgentMenu({
  agents,
  selected,
  onSelect,
  isDisabled,
  ultracode,
}: {
  /** Offered: installed and enabled. */
  agents: AgentId[];
  /** The agent this box talks to (the first offered when null). */
  selected: AgentId | null;
  onSelect: (next: AgentId) => void;
  isDisabled?: boolean;
  /** A space's box: Claude also offers Ultracode, past Max. */
  ultracode?: boolean;
}) {
  const store = useRootStore();
  const ui = useUi();
  const chip = useRef<HTMLSpanElement>(null);
  const catalog = useAgentModels(agents);
  const primary = selected ?? agents[0] ?? null;
  const now: Partial<Record<AgentId, Running>> = {};
  for (const a of agents) now[a] = running(store, a, catalog[a]);
  const cur = primary ? now[primary] : undefined;
  const levels = useMemo(
    () =>
      ultracode && primary === 'claude' && cur?.efforts.length
        ? [...cur.efforts, ULTRACODE]
        : (cur?.efforts ?? []),
    [cur?.efforts, primary, ultracode],
  );

  // What's shown is what runs: once its catalog is in, an agent with no
  // model or effort set gets the one shown.
  const fill = agents
    .map((a) => {
      const r = now[a];
      return r?.listed && r.model ? `${a}|${r.model.id}|${r.effort ?? ''}` : '';
    })
    .join(',');
  useEffect(() => {
    for (const part of fill.split(',')) {
      const [a, id, effort] = part.split('|') as [AgentId, string, string];
      if (!a || !id) continue;
      if (agentModel(store, a) !== id) setAgentModel(store, a, id);
      if (effort && rawEffort(store, a) !== effort) {
        setAgentEffort(store, a, effort);
      }
    }
  }, [fill, store]);

  // ⌘⇧/ in the box this chip sits in: the next effort.
  useEffect(() => {
    if (!primary || !levels.length || isDisabled) return;
    const onKey = (e: KeyboardEvent) => {
      if (!(e.metaKey && e.shiftKey && e.code === 'Slash')) return;
      const active = document.activeElement;
      if (!active || active === document.body) return;
      let scope = chip.current?.parentElement ?? null;
      for (let i = 0; scope && i < 4; i++, scope = scope.parentElement) {
        if (scope.contains(active)) {
          e.preventDefault();
          const at = rawEffort(store, primary);
          setAgentEffort(store, primary, nextEffort(at, levels));
          return;
        }
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [store, primary, levels, isDisabled]);

  const switchTo = (agent: AgentId) => {
    if (agent !== primary) onSelect(agent);
  };
  const pick = (key: string) => {
    const [agent, id] = key.split('|') as [AgentId, string];
    if (!agent || !id) return;
    setAgentModel(store, agent, id);
    // A level the new model doesn't take: its own default instead.
    setAgentEffort(store, agent, running(store, agent, catalog[agent]).effort);
    switchTo(agent);
  };

  return (
    <span ref={chip} className="flex min-w-[6.5rem] shrink">
      <Dropdown>
        <Button
          size="sm"
          variant="tertiary"
          aria-label="Model"
          isDisabled={isDisabled}
          // Squeezed by a running turn's Stop / Queue / Send now, it keeps
          // enough room for a short model name, never a bare chevron.
          className="min-w-[6.5rem] shrink gap-1.5 rounded-full px-3 text-xs"
        >
          <span className="min-w-0 truncate">
            {primary ? (cur?.model?.label ?? AGENT_NAME[primary]) : 'agent'}
            {cur?.effort && (
              <span className="agent-effort text-muted">
                {' '}
                {effortLabel(cur.effort)}
              </span>
            )}
          </span>
          <ChevronDown className="size-3 shrink-0 text-muted" />
        </Button>
        <Dropdown.Popover placement="top start" className="min-w-64">
          <Dropdown.Menu
            aria-label="Model and effort"
            onAction={(key) => {
              const k = String(key);
              if (k === 'settings') {
                ui.go({ name: 'settings', page: 'agents' });
              } else if (k.startsWith('use:')) switchTo(k.slice(4) as AgentId);
            }}
          >
            {agents
              .filter((a) => now[a]?.model)
              .map((a) => {
                const r = now[a]!;
                return (
                  <Dropdown.Item
                    key={a}
                    id={`use:${a}`}
                    textValue={r.model!.label}
                    className="items-baseline gap-2"
                  >
                    <span className="min-w-0 truncate">{r.model!.label}</span>
                    <span className="min-w-0 flex-1 truncate text-xs text-muted">
                      {r.effort ? effortLabel(r.effort) : ''}
                    </span>
                    {a === primary && (
                      <Check className="size-3.5 shrink-0 self-center" />
                    )}
                  </Dropdown.Item>
                );
              })}
            <Separator className="my-1" />
            <Dropdown.SubmenuTrigger>
              <Dropdown.Item id="models" textValue="Models" className="gap-2">
                <span className="flex-1">Models</span>
                <Dropdown.SubmenuIndicator />
              </Dropdown.Item>
              <Dropdown.Popover placement="right top" className="min-w-64">
                <Dropdown.Menu
                  aria-label="Models"
                  onAction={(k) => pick(String(k))}
                >
                  {agents
                    .filter((a) => (catalog[a] ?? []).length > 0)
                    .map((a) => {
                      const list = catalog[a] ?? [];
                      const current = now[a]?.model?.id ?? null;
                      const older = list.filter((m) => m.older);
                      return (
                        <Dropdown.Section key={a} id={`models:${a}`}>
                          <Header className="flex items-center gap-1.5 px-2 pt-1.5 text-xs text-muted">
                            {AGENT_NAME[a]}
                          </Header>
                          <ModelItems
                            agent={a}
                            models={list.filter((m) => !m.older)}
                            current={current}
                          />
                          {older.length ? (
                            <Dropdown.SubmenuTrigger>
                              <Dropdown.Item
                                id={`older:${a}`}
                                textValue="Earlier versions"
                                className="gap-2"
                              >
                                <span className="flex-1 text-muted">
                                  Earlier versions
                                </span>
                                <Dropdown.SubmenuIndicator />
                              </Dropdown.Item>
                              <Dropdown.Popover placement="right top">
                                <Dropdown.Menu
                                  aria-label={`Earlier ${AGENT_NAME[a]} models`}
                                  onAction={(k) => pick(String(k))}
                                >
                                  <ModelItems
                                    agent={a}
                                    models={older}
                                    current={current}
                                  />
                                </Dropdown.Menu>
                              </Dropdown.Popover>
                            </Dropdown.SubmenuTrigger>
                          ) : null}
                        </Dropdown.Section>
                      );
                    })}
                </Dropdown.Menu>
              </Dropdown.Popover>
            </Dropdown.SubmenuTrigger>
            {primary && levels.length ? (
              <Dropdown.SubmenuTrigger>
                <Dropdown.Item id="effort" textValue="Effort" className="gap-2">
                  <span className="flex-1">Effort</span>
                  <span className="text-xs text-muted">
                    {cur?.effort ? effortLabel(cur.effort) : ''}
                  </span>
                  <Dropdown.SubmenuIndicator />
                </Dropdown.Item>
                <Dropdown.Popover placement="right top">
                  <Dropdown.Menu
                    aria-label="Effort"
                    onAction={(k) => setAgentEffort(store, primary, String(k))}
                  >
                    {levels.map((l) => (
                      <Dropdown.Item
                        key={l}
                        id={l}
                        textValue={effortLabel(l)}
                        className="gap-2"
                      >
                        <span className="flex min-w-0 flex-1 flex-col">
                          <span>{effortLabel(l)}</span>
                          {l === ULTRACODE && (
                            <span className="text-xs text-muted">
                              Several agents · uses far more
                            </span>
                          )}
                        </span>
                        {l === cur?.effort && (
                          <Check className="size-3.5 shrink-0" />
                        )}
                      </Dropdown.Item>
                    ))}
                  </Dropdown.Menu>
                </Dropdown.Popover>
              </Dropdown.SubmenuTrigger>
            ) : null}
            <Separator className="my-1" />
            <Dropdown.Item id="settings" textValue="Edit" className="gap-2">
              <Gear className="size-3.5 text-muted" />
              <span className="flex-1 text-muted">Edit</span>
              {levels.length > 0 && (
                <span className="flex items-center gap-1.5 text-xs text-muted">
                  <K>⌘⇧/</K> Cycle effort
                </span>
              )}
            </Dropdown.Item>
          </Dropdown.Menu>
        </Dropdown.Popover>
      </Dropdown>
    </span>
  );
});
