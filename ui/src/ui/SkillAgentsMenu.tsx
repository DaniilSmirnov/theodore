/**
 * "Skills on: Claude + Codex": which agents a skill runs on, beside the
 * agent chip in the AI panel's box. The chip's agent (the one the panel
 * talks to) always runs skills; the rest are toggles. Only offered when
 * more than one agent is installed and enabled.
 */
import { ChevronDown } from '@gravity-ui/icons';
import { Button, Dropdown } from '@heroui/react';
import type { AgentId } from '../gen/contract';
import { AGENT_NAME } from './agentModel';

export function SkillAgentsMenu({
  agents,
  primary,
  selected,
  onSelect,
}: {
  /** Offered: installed and enabled. */
  agents: AgentId[];
  /** The agent the panel talks to (the chip's): always runs skills. */
  primary: AgentId;
  /** Every agent skills run on; the chat agent is among them. */
  selected: AgentId[];
  onSelect: (next: AgentId[]) => void;
}) {
  if (agents.length < 2) return null;
  const also = agents.filter((a) => a !== primary && selected.includes(a));
  const names = [primary, ...also].map((a) => AGENT_NAME[a]).join(' + ');
  return (
    <Dropdown>
      <Button
        size="sm"
        variant="ghost"
        aria-label="Run skills on"
        className="skill-agents min-w-0 shrink gap-1.5 rounded-full px-2 text-xs text-muted"
      >
        <span className="min-w-0 truncate">Skills on: {names}</span>
        <ChevronDown className="size-3 shrink-0" />
      </Button>
      <Dropdown.Popover placement="top start" className="min-w-52">
        <Dropdown.Menu
          aria-label="Run skills on"
          selectionMode="multiple"
          selectedKeys={new Set([primary, ...also])}
          onSelectionChange={(keys) => {
            // The chat agent stays: it is the one the chip shows.
            const more = agents.filter(
              (a) => a !== primary && (keys === 'all' || keys.has(a)),
            );
            onSelect([primary, ...more]);
          }}
        >
          {agents.map((a) => (
            <Dropdown.Item
              key={a}
              id={a}
              textValue={AGENT_NAME[a]}
              className="gap-2"
            >
              <span className="flex-1">{AGENT_NAME[a]}</span>
              {a === primary && (
                <span className="text-xs text-muted">Asks go here</span>
              )}
              <Dropdown.ItemIndicator />
            </Dropdown.Item>
          ))}
        </Dropdown.Menu>
      </Dropdown.Popover>
    </Dropdown>
  );
}
