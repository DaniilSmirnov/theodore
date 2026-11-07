/** The AI panel's box carries the same agent chip as a space's (model and
 *  effort, ⌘⇧/). Running skills on several agents is a separate control,
 *  offered only when more than one agent is installed and enabled. */
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { act } from 'react';
import { click, mount, settle, typeInto, type Mounted } from '../test/dom';
import type { AgentId } from '../gen/contract';
import { setTransport } from '../core/transport';
import { RootStore } from '../stores/RootStore';
import { RootStoreProvider } from '../stores/context';
import { FakeTransport } from '../test/fakeTransport';
import { setSetting } from '../actions';
import { resetAgentModels } from './useAgentModels';
import { agentEffort, agentModel } from './agentModel';
import { SidePanel } from './SidePanel';

// jsdom has no `CSS.escape`; an open react-aria popover calls it.
(globalThis as { CSS?: { escape?: (s: string) => string } }).CSS ??= {};
globalThis.CSS.escape ??= (s: string) =>
  s.replace(/[^a-zA-Z0-9_-]/g, (c) => `\\${c}`);
Element.prototype.scrollIntoView ??= function () {};

const PR = 'gh:acme/app#1';
const EFFORTS = ['low', 'medium', 'high'];
const model = (id: string, label: string) => ({
  id,
  label,
  description: null,
  efforts: EFFORTS,
  default_effort: 'medium',
  older: false,
});
const CATALOG: Record<string, unknown[]> = {
  claude: [model('claude-opus-5-5', 'Opus 5.5')],
  codex: [model('gpt-6-sol', 'GPT-6-Sol')],
  gemini: [],
};

let transport: FakeTransport;
let store: RootStore;
let m: Mounted | null = null;

async function open(installed: AgentId[], before?: (s: RootStore) => void) {
  transport = new FakeTransport();
  transport.onCall('detect_agents', () =>
    installed.map((id) => ({
      id,
      installed: true,
      version: '1',
      path: `/bin/${id}`,
    })),
  );
  transport.onCall('agent_models', (a) => CATALOG[String(a?.agent)] ?? []);
  transport.onCall('list_skills', () => []);
  transport.onCall('transact', () => ({ status: 'ok', last_sync_id: 1 }));
  transport.onCall('ask', () => 'q1');
  setTransport(transport);
  store = new RootStore();
  before?.(store);
  m = await mount(
    <RootStoreProvider store={store}>
      <SidePanel pr={PR} />
    </RootStoreProvider>,
  );
  await settle();
  await settle();
}

beforeEach(() => resetAgentModels());
afterEach(() => {
  m?.unmount();
  m = null;
});

const chip = () =>
  m!.host.querySelector<HTMLButtonElement>(
    '.chat-input button[aria-label="Model"]',
  );
const skillAgents = () =>
  m!.host.querySelector<HTMLButtonElement>(
    '.chat-input button[aria-label="Run skills on"]',
  );
const item = (name: string) =>
  [...document.querySelectorAll('[role^="menuitem"]')].find((el) =>
    el.textContent?.trim().startsWith(name),
  ) as HTMLElement | undefined;

const asked = () =>
  transport.calls.filter((c) => c.cmd === 'ask').map((c) => c.args!.req) as {
    agent: AgentId | null;
    question: string;
  }[];

async function send(text: string) {
  await typeInto(m!.host.querySelector('.chat-input textarea'), text);
  await click(m!.host.querySelector('.chat-input button[aria-label="Send"]'));
  await settle();
}

describe('the AI panel agent chip', () => {
  it('one enabled agent: the chip with its model and effort, no multi picker', async () => {
    await open(['claude']);
    expect(chip()?.textContent).toBe('Opus 5.5 Medium');
    expect(skillAgents()).toBeNull();
  });

  it('two enabled agents: the chip, plus the picker for skills', async () => {
    await open(['claude', 'codex']);
    expect(chip()?.textContent).toBe('Opus 5.5 Medium');
    // As before: skills run on every enabled agent until you narrow it.
    expect(skillAgents()?.textContent).toBe('Skills on: Claude + Codex');
    await click(skillAgents());
    await settle();
    await click(item('Codex')!);
    await settle();
    expect(skillAgents()?.textContent).toBe('Skills on: Claude');
  });

  it('a disabled agent is not offered, so one left means no picker', async () => {
    await open(['claude', 'codex'], (s) =>
      setSetting(s, 'user', 'agents.codex.enabled', false),
    );
    expect(chip()).not.toBeNull();
    expect(skillAgents()).toBeNull();
  });

  it('asks go to the agent the chip shows', async () => {
    await open(['claude', 'codex']);
    await send('first?');
    expect(asked()).toMatchObject([{ agent: 'claude', question: 'first?' }]);
  });

  it("picking in the chip changes who the panel asks, with that agent's model", async () => {
    await open(['claude', 'codex']);
    await click(chip());
    await settle();
    await click(item('GPT-6-Sol')!);
    await settle();
    expect(chip()?.textContent).toBe('GPT-6-Sol Medium');
    expect(agentModel(store, 'codex')).toBe('gpt-6-sol');
    await send('second?');
    expect(asked()).toMatchObject([{ agent: 'codex', question: 'second?' }]);
    // The chat agent runs skills too, and leads the list.
    expect(skillAgents()?.textContent).toBe('Skills on: Codex + Claude');
  });

  it('⌘⇧/ in the box cycles the effort, as in a space', async () => {
    await open(['claude']);
    expect(agentEffort(store, 'claude')).toBe('medium');
    m!.host.querySelector<HTMLTextAreaElement>('.chat-input textarea')!.focus();
    await act(async () => {
      window.dispatchEvent(
        new KeyboardEvent('keydown', {
          code: 'Slash',
          key: '?',
          metaKey: true,
          shiftKey: true,
        }),
      );
    });
    await settle();
    expect(agentEffort(store, 'claude')).toBe('high');
  });
});
