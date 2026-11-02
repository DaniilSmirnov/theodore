/**
 * The space header's Review control (docs/spaces.md §3 "Review of a
 * space"). "Review" opens the grouped review, as before. Its menu adds
 * "Review with agent": a second opinion, run as a skill on the space's own
 * branch review by a reviewer agent that can differ from the one writing
 * the change — picked here, sticky per machine (`spaces.review_agent`).
 * Runs through `run_skill`, so it never touches `Space.status`: the
 * writing agent stays idle while "Reviewing…" shows here.
 */
import { useEffect, useState } from 'react';
import { ChevronDown, Sparkles } from '@gravity-ui/icons';
import { Button, Popover } from '@heroui/react';
import { observer } from 'mobx-react-lite';
import type { AgentId, Space } from '../../gen/contract';
import { api } from '../../core/api';
import { setSetting } from '../../actions';
import { useRootStore, useUi } from '../../stores/context';
import { Btn, Spinner } from '../bits';
import { AgentChip, useEnabledAgents } from './Composer';
import {
  REVIEW_AGENT_KEY,
  defaultReviewSkill,
  reviewAgentDefault,
  reviewRunning,
} from './spaces';

type Store = ReturnType<typeof useRootStore>;
type Ui = ReturnType<typeof useUi>;

async function openReview(store: Store, ui: Ui, space: Space) {
  const res = await api.spaceReview({ space: space.id, group: true });
  await store.sync.openPr(res.pr);
  ui.go({ name: 'review', pr: res.pr });
}

export const ReviewMenu = observer(function ReviewMenu({
  space,
}: {
  space: Space;
}) {
  const store = useRootStore();
  const ui = useUi();
  const [opening, setOpening] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  const [starting, setStarting] = useState(false);
  const agents = useEnabledAgents();
  const [agent, setAgent] = useState<AgentId | null>(null);
  useEffect(() => {
    if (agent === null && agents.length) {
      setAgent(reviewAgentDefault(store.models.settings.all(), agents));
    }
    // Only once agents finish loading — the user's own pick, once made,
    // isn't second-guessed on every render.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [agents.length]);
  const reviewing = reviewRunning(
    store.models.skillRuns.byPr(space.review_pr ?? ''),
  );
  const archived = space.status === 'archived';

  const runWithAgent = async () => {
    if (!agent) return;
    setStarting(true);
    try {
      const res = await api.spaceReview({ space: space.id, group: true });
      const skills = await api.listSkills(res.pr);
      await api.runSkill({
        pr: res.pr,
        skill: defaultReviewSkill(skills),
        agents: [agent],
      });
      setSetting(store, 'user', REVIEW_AGENT_KEY, agent);
      setMenuOpen(false);
      store.ephemeral.toast(`Reviewing with ${agent}…`, 'info', {
        label: 'Open',
        onAction: () => void openReview(store, ui, space),
      });
    } catch {
      // toasted by api.ts
    } finally {
      setStarting(false);
    }
  };

  return (
    <div className="space-review flex items-center">
      <Button
        size="sm"
        variant="secondary"
        className="gap-1.5 rounded-r-none"
        isDisabled={opening || archived}
        onPress={() => {
          setOpening(true);
          void openReview(store, ui, space)
            .catch(() => {
              // toasted by api.ts
            })
            .finally(() => setOpening(false));
        }}
      >
        {reviewing && <Spinner />}
        Review
      </Button>
      <Popover isOpen={menuOpen} onOpenChange={setMenuOpen}>
        <Button
          size="sm"
          variant="secondary"
          isIconOnly
          className="rounded-l-none border-l border-separator"
          aria-label="Review options"
          isDisabled={archived}
        >
          <ChevronDown className="size-3.5" />
        </Button>
        <Popover.Content placement="bottom end" offset={8} className="w-80">
          <Popover.Dialog className="flex flex-col gap-3 p-4">
            <Popover.Heading className="text-sm font-semibold">
              Review with agent
            </Popover.Heading>
            <span className="text-xs text-muted">
              Runs a review skill on this space&rsquo;s own branch review — a
              second opinion, side by side with the agent writing the change.
              Findings land on the review as usual.
            </span>
            <AgentChip
              value={agent}
              onChange={setAgent}
              isDisabled={starting}
            />
            <div className="flex justify-end">
              <Btn
                tone="primary"
                icon={<Sparkles className="size-3.5" />}
                isDisabled={!agent || reviewing}
                isPending={starting}
                onPress={() => void runWithAgent()}
              >
                {reviewing ? 'Reviewing…' : 'Run'}
              </Btn>
            </div>
          </Popover.Dialog>
        </Popover.Content>
      </Popover>
    </div>
  );
});
