import { act } from 'react';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { click, mount, settle, type Mounted } from '../test/dom';
import { setTransport } from '../core/transport';
import { RootStore } from '../stores/RootStore';
import { RootStoreProvider } from '../stores/context';
import { FakeTransport } from '../test/fakeTransport';
import { setSetting } from '../actions';
import type { AgentId } from '../gen/contract';
import { AgentMenu } from './AgentMenu';
import {
  agentEffort,
  agentLabel,
  agentLabelOf,
  agentModel,
  nextEffort,
  setAgentEffort,
  setAgentModel,
} from './agentModel';
import { resetAgentModels } from './useAgentModels';

let m: Mounted | null = null;

// jsdom has no `CSS.escape`; an open react-aria popover calls it.
(globalThis as { CSS?: { escape?: (s: string) => string } }).CSS ??= {};
globalThis.CSS.escape ??= (s: string) =>
  s.replace(/[^a-zA-Z0-9_-]/g, (c) => `\\${c}`);

beforeEach(() => {
  const transport = new FakeTransport();
  setTransport(transport);
  transport.onCall('transact', () => ({ status: 'ok', last_sync_id: 1 }));
});

afterEach(() => {
  m?.unmount();
  m = null;
});

describe('agentLabel', () => {
  it('names the model, or says it is the default', () => {
    expect(agentLabel('claude', null, null)).toBe('claude · default model');
    expect(agentLabel('claude', 'opus', null)).toBe('claude · Opus');
    expect(agentLabel('claude', 'opus', 'high')).toBe('claude · Opus · High');
    expect(agentLabel('claude', null, 'max')).toBe(
      'claude · default model · Max',
    );
  });

  it('shows a full model name as typed', () => {
    expect(agentLabel('claude', 'claude-opus-4-1', null)).toBe(
      'claude · claude-opus-4-1',
    );
    expect(agentLabel('codex', 'gpt-5', null)).toBe('codex · gpt-5');
  });

  it('never shows an effort for an agent without the flag', () => {
    expect(agentLabel('gemini', null, 'high')).toBe('gemini · default model');
  });
});

describe('agent model and effort settings', () => {
  it('reads agents.<id>.model / .effort, user scope', () => {
    const store = new RootStore();
    expect(agentModel(store, 'claude')).toBeNull();
    setAgentModel(store, 'claude', 'sonnet');
    setAgentEffort(store, 'claude', 'xhigh');
    expect(agentModel(store, 'claude')).toBe('sonnet');
    expect(agentEffort(store, 'claude')).toBe('xhigh');
    expect(agentLabelOf(store, 'claude')).toBe('claude · Sonnet · Extra high');
    // Per agent.
    expect(agentModel(store, 'codex')).toBeNull();
  });

  it('treats null, blank and unknown efforts as unset', () => {
    const store = new RootStore();
    setAgentModel(store, 'claude', '  ');
    setSetting(store, 'user', 'agents.claude.effort', 'ultra');
    expect(agentModel(store, 'claude')).toBeNull();
    expect(agentEffort(store, 'claude')).toBeNull();
    setAgentModel(store, 'claude', 'opus');
    setAgentModel(store, 'claude', null);
    expect(agentModel(store, 'claude')).toBeNull();
  });
});

const CLAUDE = ['low', 'medium', 'high', 'xhigh', 'max'];
const model = (
  id: string,
  label: string,
  efforts: string[],
  default_effort: string | null,
  older = false,
) => ({ id, label, description: null, efforts, default_effort, older });
const CATALOG: Record<string, unknown[]> = {
  claude: [
    model('claude-opus-5-5', 'Opus 5.5', CLAUDE, 'medium'),
    model('claude-fable-5-1', 'Fable 5.1', CLAUDE, 'high'),
    model('claude-opus-4-6', 'Opus 4.6', ['low', 'high'], 'high', true),
  ],
  codex: [
    model('gpt-6-sol', 'GPT-6-Sol', ['low', 'medium', 'ultra'], 'medium'),
    model('gpt-6-luna', 'GPT-6-Luna', ['low', 'medium'], 'low'),
  ],
};

describe('nextEffort', () => {
  it('cycles through the levels and wraps round', () => {
    expect(nextEffort(null)).toBe('low');
    expect(nextEffort('high')).toBe('xhigh');
    expect(nextEffort('max')).toBe('low');
    expect(nextEffort('medium', ['low', 'medium', 'ultra'])).toBe('ultra');
    expect(nextEffort('ultra', ['low', 'medium', 'ultra'])).toBe('low');
    expect(nextEffort('x', [])).toBeNull();
  });
});

describe('AgentMenu', () => {
  let picked: AgentId | null = null;

  beforeEach(() => {
    resetAgentModels();
    picked = null;
    const transport = new FakeTransport();
    setTransport(transport);
    transport.onCall('transact', () => ({ status: 'ok', last_sync_id: 1 }));
    transport.onCall('agent_models', (a) => CATALOG[String(a?.agent)] ?? []);
  });

  async function render(store: RootStore, selected: AgentId) {
    m = await mount(
      <RootStoreProvider store={store}>
        <div>
          <textarea aria-label="box" />
          <AgentMenu
            agents={['claude', 'codex']}
            selected={selected}
            onSelect={(next) => (picked = next)}
          />
        </div>
      </RootStoreProvider>,
    );
    await settle();
    await settle();
    return m.host.querySelector('button[aria-label="Model"]')!;
  }

  const item = (name: string) =>
    [...document.querySelectorAll('[role^="menuitem"]')].find((el) =>
      el.textContent?.trim().startsWith(name),
    ) as HTMLElement | undefined;

  it('names the model and effort the agent runs', async () => {
    const store = new RootStore();
    setAgentModel(store, 'claude', 'claude-fable-5-1');
    setAgentEffort(store, 'claude', 'max');
    const chip = await render(store, 'claude');
    expect(chip.textContent).toBe('Fable 5.1 Max');
    // A running turn's Stop / Queue / Send now squeeze it, never to a bare
    // chevron: it keeps room for a short name (layout itself is CSS).
    expect(chip.className).toContain('min-w-[6.5rem]');
    expect(chip.parentElement!.className).toContain('min-w-[6.5rem]');
  });

  it("never shows a default: an unset agent runs, and keeps, its first model and that model's effort", async () => {
    const store = new RootStore();
    const chip = await render(store, 'claude');
    expect(chip.textContent).toBe('Opus 5.5 Medium');
    expect(agentModel(store, 'claude')).toBe('claude-opus-5-5');
    expect(agentEffort(store, 'claude')).toBe('medium');
    expect(agentModel(store, 'codex')).toBe('gpt-6-sol');
    expect(agentEffort(store, 'codex')).toBe('medium');
  });

  it('a row per agent with what it runs; a row switches to that agent', async () => {
    const store = new RootStore();
    const chip = await render(store, 'claude');
    await click(chip);
    await settle();
    expect(item('Opus 5.5')?.textContent).toContain('Medium');
    expect(item('GPT-6-Sol')).toBeTruthy();
    expect(document.body.textContent).not.toContain('Default');
    await click(item('GPT-6-Sol')!);
    await settle();
    expect(picked).toBe('codex');
  });

  it("Models lists every CLI's models, earlier versions one level down", async () => {
    const store = new RootStore();
    const chip = await render(store, 'claude');
    await click(chip);
    await settle();
    await click(item('Models')!);
    await settle();
    expect(item('Fable 5.1')).toBeTruthy();
    expect(item('GPT-6-Luna')).toBeTruthy();
    expect(item('Opus 4.6')).toBeUndefined();
    const earlier = item('Earlier versions')!;
    await act(async () => {
      earlier.focus();
      earlier.dispatchEvent(
        new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }),
      );
    });
    await settle();
    await click(item('Opus 4.6')!);
    await settle();
    expect(agentModel(store, 'claude')).toBe('claude-opus-4-6');
    // Medium isn't one of its levels: its own default instead.
    expect(agentEffort(store, 'claude')).toBe('high');
  });

  it("picking another CLI's model switches to that agent", async () => {
    const store = new RootStore();
    const chip = await render(store, 'claude');
    await click(chip);
    await settle();
    await click(item('Models')!);
    await settle();
    await click(item('GPT-6-Luna')!);
    await settle();
    expect(agentModel(store, 'codex')).toBe('gpt-6-luna');
    expect(picked).toBe('codex');
  });

  it("Effort offers the current model's own levels", async () => {
    const store = new RootStore();
    setAgentModel(store, 'codex', 'gpt-6-sol');
    const chip = await render(store, 'codex');
    await click(chip);
    await settle();
    await click(item('Effort')!);
    await settle();
    expect(item('Ultra')).toBeTruthy();
    expect(item('Max')).toBeUndefined();
    await click(item('Ultra')!);
    await settle();
    expect(agentEffort(store, 'codex')).toBe('ultra');
  });

  it('Ultracode is offered in a space and kept everywhere else', async () => {
    const store = new RootStore();
    setAgentModel(store, 'claude', 'claude-opus-5-5');
    m = await mount(
      <RootStoreProvider store={store}>
        <AgentMenu
          agents={['claude']}
          selected="claude"
          onSelect={() => {}}
          ultracode
        />
      </RootStoreProvider>,
    );
    await settle();
    await settle();
    await click(m.host.querySelector('button[aria-label="Model"]'));
    await settle();
    await click(item('Effort')!);
    await settle();
    await click(item('Ultracode')!);
    await settle();
    expect(agentEffort(store, 'claude')).toBe('ultracode');
    m.unmount();
    // Outside a space: not offered, but not reset either.
    const chip = await render(store, 'claude');
    expect(chip.textContent).toBe('Opus 5.5 Ultracode');
    await click(chip);
    await settle();
    await click(item('Effort')!);
    await settle();
    expect(item('Ultracode')).toBeUndefined();
    expect(agentEffort(store, 'claude')).toBe('ultracode');
  });

  it('⌘⇧/ in its box cycles the effort', async () => {
    const store = new RootStore();
    setAgentModel(store, 'claude', 'claude-opus-5-5');
    setAgentEffort(store, 'claude', 'high');
    await render(store, 'claude');
    const box = m!.host.querySelector('textarea')!;
    const press = () =>
      window.dispatchEvent(
        new KeyboardEvent('keydown', {
          code: 'Slash',
          key: '?',
          metaKey: true,
          shiftKey: true,
        }),
      );
    press(); // focus elsewhere: nothing
    expect(agentEffort(store, 'claude')).toBe('high');
    box.focus();
    press();
    await settle();
    expect(agentEffort(store, 'claude')).toBe('xhigh');
  });
});
