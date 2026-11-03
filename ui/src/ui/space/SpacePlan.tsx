/**
 * Plan mode and `ask_user` in a space's chat (docs/spaces.md §5): the
 * plan card's Approve / Revise, and the card for the agent's questions
 * while its turn waits on you.
 */
import { useState } from 'react';
import { Button, Card, Input, TextField, ToggleButton } from '@heroui/react';
import type {
  Space,
  SpaceAnswerItem,
  SpaceQuestion,
  SpaceQuestionItem,
} from '../../gen/contract';
import { api } from '../../core/api';

/** Under a Plan turn's reply: Approve builds it, Revise puts you in the
 *  composer to say what should change (the next turn plans again). */
export function PlanActions({
  space,
  onRevise,
}: {
  space: Space;
  onRevise: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const build = space.build_mode === 'run' ? 'Run' : 'Edit';
  return (
    <div className="space-plan-actions flex items-center gap-1.5">
      <Button
        size="sm"
        variant="primary"
        isPending={busy}
        onPress={() => {
          setBusy(true);
          void api
            .approveSpacePlan(space.id)
            .catch(() => {})
            .finally(() => setBusy(false));
        }}
      >
        Approve
      </Button>
      <Button size="sm" variant="tertiary" onPress={onRevise}>
        Revise
      </Button>
      <span className="text-xs text-muted">
        Approve implements it in {build}
      </span>
    </div>
  );
}

/** One question's answer while you pick it. */
interface Draft {
  selected: string[];
  other: string;
}

const empty = (): Draft => ({ selected: [], other: '' });

/** Picks `option` in `d`: toggles it where `multi`, else makes it the
 *  only answer (and clears "Other"). */
export function pick(q: SpaceQuestionItem, d: Draft, option: string): Draft {
  if (q.multi) {
    return d.selected.includes(option)
      ? { ...d, selected: d.selected.filter((o) => o !== option) }
      : { ...d, selected: [...d.selected, option] };
  }
  return { selected: [option], other: '' };
}

/** Typing under "Other": a single-answer question drops its option. */
export function typeOther(q: SpaceQuestionItem, d: Draft, text: string): Draft {
  return q.multi ? { ...d, other: text } : { selected: [], other: text };
}

/** Each question has an answer. */
export function answered(drafts: Draft[]): boolean {
  return drafts.every((d) => d.selected.length > 0 || d.other.trim() !== '');
}

export function toAnswers(drafts: Draft[]): SpaceAnswerItem[] {
  return drafts.map((d) => ({
    selected: d.selected,
    other: d.other.trim() ? d.other.trim() : null,
  }));
}

/** The agent's questions; the turn waits until you send the answers (or
 *  Stop it). */
export function QuestionCard({
  space,
  question,
}: {
  space: Space;
  question: SpaceQuestion;
}) {
  const [drafts, setDrafts] = useState<Draft[]>(() =>
    question.questions.map(empty),
  );
  const [busy, setBusy] = useState(false);
  const set = (i: number, d: Draft) =>
    setDrafts((all) => all.map((x, j) => (j === i ? d : x)));
  const send = () => {
    if (!answered(drafts) || busy) return;
    setBusy(true);
    void api
      .answerSpaceQuestion({
        space: space.id,
        question: question.id,
        answers: toAnswers(drafts),
      })
      .catch(() => {})
      .finally(() => setBusy(false));
  };
  return (
    <Card
      variant="secondary"
      className="space-question flex flex-col gap-4 px-4 py-3"
      data-question={question.id}
    >
      <span className="text-xs font-medium text-muted">
        The agent is waiting for your answer
      </span>
      {question.questions.map((q, i) => {
        const d = drafts[i] ?? empty();
        return (
          <div key={i} className="flex flex-col gap-2">
            <span className="text-sm font-medium">{q.question}</span>
            <div className="flex flex-wrap gap-1.5">
              {q.options.map((o) => (
                <ToggleButton
                  key={o}
                  size="sm"
                  className="space-option"
                  isSelected={d.selected.includes(o)}
                  onChange={() => set(i, pick(q, d, o))}
                >
                  {o}
                </ToggleButton>
              ))}
            </div>
            <TextField
              value={d.other}
              onChange={(t) => set(i, typeOther(q, d, t))}
              aria-label={`Other answer to: ${q.question}`}
            >
              <Input
                className="text-sm"
                placeholder="Other…"
                onKeyDown={(e) => e.key === 'Enter' && send()}
              />
            </TextField>
            {q.multi && (
              <span className="text-[11px] text-muted">
                Pick any that apply
              </span>
            )}
          </div>
        );
      })}
      <div className="flex gap-1.5">
        <Button
          size="sm"
          variant="primary"
          className="space-answer"
          isDisabled={!answered(drafts)}
          isPending={busy}
          onPress={send}
        >
          Send answer
        </Button>
      </div>
    </Card>
  );
}
