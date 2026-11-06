import { SignInModal } from './AccountCard';
import { useState } from 'react';
import {
  Alert,
  Button,
  Label,
  Radio,
  RadioGroup,
  Table,
  TextArea,
} from '@heroui/react';
import { observer } from 'mobx-react-lite';
import type { ReviewEvent } from '../gen/contract';
import { submitReview } from '../actions';
import { FEATURES } from '../features';
import { useRootStore, useUi } from '../stores/context';
import { AgentTag, K, Shell } from './bits';
import { AssignMenu, CommandMenu, SnoozeMenu } from './Palette';
import { AddProjectModal } from './Projects';
import { isBuildMode } from './space/mode';
import { NewSpaceModal } from './space/SpacePage';
import { ReportProblemModal } from './ReportProblem';
import { WhatsCollectedModal } from './WhatsCollected';

const EVENT_OPTS: [ReviewEvent, string, string][] = [
  ['COMMENT', 'Comment', 'no verdict'],
  ['APPROVE', 'Approve', 'ready to merge'],
  ['REQUEST_CHANGES', 'Request changes', 'blocks merge'],
];

const SubmitReviewModal = observer(function SubmitReviewModal() {
  const store = useRootStore();
  const ui = useUi();
  const [event, setEvent] = useState<ReviewEvent>('COMMENT');
  const [body, setBody] = useState('');
  const [sending, setSending] = useState(false);
  if (ui.route.name !== 'review') return null;
  const pr = ui.route.pr;
  const local = store.models.prSummaries.get(pr)?.pr.repo.host === 'local';
  const drafts = store.models.drafts.byPr(pr);
  const reads = new Set(store.models.hunkReads.byPr(pr).map((r) => r.hunk));
  // This PR's groups only — `groups.all()` spans every open PR.
  const unread = store.models
    .groupsOf(pr)
    .filter((g) => g.kind !== 'hidden')
    .flatMap((g) => g.hunks)
    .filter((h) => !reads.has(h)).length;
  return (
    <Shell
      title={`Review · ${prLabel(store.models.prSummaries.get(pr), pr)}`}
      size="lg"
      onClose={() => ui.closeModal()}
    >
      {unread > 0 && (
        <div className="text-xs text-warning">Unread hunks: {unread}</div>
      )}
      <TextArea
        rows={3}
        fullWidth
        aria-label="Overall comment"
        placeholder="Overall comment (optional)"
        value={body}
        onChange={(e) => setBody(e.target.value)}
      />
      {local ? (
        <Alert status="accent">
          <Alert.Content>
            <Alert.Title>Local review — comments stay in Theodore.</Alert.Title>
            <Alert.Description>
              No host to send a verdict to; pending comments below are kept
              here.
            </Alert.Description>
          </Alert.Content>
        </Alert>
      ) : (
        <RadioGroup
          value={event}
          onChange={(v) => setEvent(v as ReviewEvent)}
          orientation="horizontal"
          aria-label="Verdict"
        >
          {EVENT_OPTS.map(([v, t, d]) => (
            <Radio
              key={v}
              value={v}
              className={`event flex-1 rounded-2xl border p-2.5 ${event === v ? 'border-accent bg-accent-soft' : 'border-separator'}`}
            >
              <Radio.Content>
                <Radio.Control>
                  <Radio.Indicator />
                </Radio.Control>
                <div>
                  <b>{t}</b>
                  <div className="text-xs text-muted">{d}</div>
                </div>
              </Radio.Content>
            </Radio>
          ))}
        </RadioGroup>
      )}
      {drafts.length === 0 ? (
        <span className="text-xs text-muted">
          No pending comments: only the verdict will be sent.
        </span>
      ) : (
        <>
          <Label className="text-sm font-medium">
            Pending comments ({drafts.length})
          </Label>
          <Table variant="secondary">
            <Table.ScrollContainer>
              <Table.Content aria-label="Pending comments">
                <Table.Header>
                  <Table.Column isRowHeader>where</Table.Column>
                  <Table.Column>who</Table.Column>
                  <Table.Column>text</Table.Column>
                </Table.Header>
                <Table.Body>
                  {drafts.map((d) => (
                    <Table.Row key={d.id} id={d.id}>
                      <Table.Cell className="max-w-[200px] font-mono text-xs">
                        <span
                          className="block truncate"
                          title={`${d.anchor.path}:${d.anchor.line}`}
                        >
                          {d.anchor.path}:{d.anchor.line}
                        </span>
                      </Table.Cell>
                      <Table.Cell className="shrink-0">
                        {d.source.kind === 'agent' ? (
                          <AgentTag id={d.source.agent} />
                        ) : (
                          <span className="text-xs text-muted">you</span>
                        )}
                      </Table.Cell>
                      <Table.Cell className="max-w-[320px] text-xs">
                        <span className="line-clamp-2">
                          {d.body.slice(0, 110)}
                        </span>
                      </Table.Cell>
                    </Table.Row>
                  ))}
                </Table.Body>
              </Table.Content>
            </Table.ScrollContainer>
          </Table>
        </>
      )}
      <div className="flex justify-end gap-2">
        <Button variant="tertiary" onPress={() => ui.closeModal()}>
          Cancel
        </Button>
        <Button
          variant={!local && event === 'REQUEST_CHANGES' ? 'danger' : 'primary'}
          isDisabled={sending}
          onPress={() => {
            setSending(true);
            // Close only once the host accepted it: on a failure the modal
            // stays open (the toast comes from api.ts) so the overall
            // comment and verdict aren't lost.
            void submitReview(store, pr, local ? 'COMMENT' : event, body.trim())
              .then(() => {
                ui.closeModal();
                ui.goBack();
              })
              .catch(() => {
                setSending(false);
              });
          }}
        >
          {sending ? 'Sending…' : local ? 'Done' : 'Submit'}
        </Button>
      </div>
    </Shell>
  );
});

const KEYS: [string, string][] = [
  ['Everywhere', ''],
  ['⌘K', 'command palette: any action, any PR'],
  [
    `G I${FEATURES.currentUser ? ' / G M' : ''} / G T / G S`,
    `Inbox${FEATURES.currentUser ? ' / my pull requests' : ''} / Tasks / Settings`,
  ],
  ['[', 'collapse sidebar'],
  ['⌥1…9 / ⌥W', 'switch / close tab'],
  ['⌃1…9 / ⌃0', 'switch project / All projects'],
  ['⌘Z / ⌘⇧Z', 'undo / redo'],
  ['PR lists', ''],
  ['J / K', 'move through list'],
  ['⏎', 'open PR'],
  ['Space', 'preview without opening'],
  ['X', 'select (bulk actions)'],
  ['E', 'done: leaves Inbox until new activity'],
  ['H', 'snooze: an hour, until next push, until checks pass'],
  [
    '⌘F',
    'search this list (or the sidebar’s Spaces section, elsewhere) — ↑↓⏎ to open, Esc to clear',
  ],
  ['Tasks', ''],
  ['Y', 'accept fix'],
  ['A', 'assign to a person or agent'],
  ['Review', ''],
  ['J / K', 'next / previous hunk'],
  ['N', 'next group'],
  ['R', 'mark group as read, go to the next one'],
  ['V', 'Viewed for focused file'],
  ['C', 'comment on focused hunk'],
  ['S', 'unified / side by side'],
  [']', 'hide / show AI panel'],
  [
    '⇧\u00a0click',
    'comment on a line range: click the first line, ⇧ click the last',
  ],
  [
    '⌘\u00a0click',
    FEATURES.symbolPeek
      ? 'definition and usages of a symbol'
      : 'definition and usages of a symbol (not available yet)',
  ],
  ['/', 'ask AI'],
  ['Esc', 'close / back'],
];

/** Spaces' keys — listed only with build mode on. */
const SPACE_KEYS: [string, string][] = [
  ['Spaces', ''],
  ['G N', 'new space'],
  [
    'right-click',
    'pin / unpin, mark as unread (sidebar row, or type "pin"/"unread" in ⌘K)',
  ],
  ['C or /', 'message the agent (Esc leaves the box)'],
  ['⏎ / ⇧⏎', 'send / new line'],
  ['M', 'mode: Ask → Edit → Run'],
  ['⌘.', 'stop the turn'],
  ['R', 'review the changes, grouped'],
  ['D', 'show / hide the changes'],
  ['⇧R', "start / stop the space's run command"],
  [
    'P / ⇧P',
    'open PR / as draft (local folder: merge / keep as branch); push once a PR exists',
  ],
  ['O', "open the space's PR"],
  ['⇧E', 'archive'],
];

function HelpModalInner() {
  const ui = useUi();
  const store = useRootStore();
  const rows = isBuildMode(store) ? [...KEYS, ...SPACE_KEYS] : KEYS;
  return (
    <Shell title="Shortcuts" onClose={() => ui.closeModal()}>
      <Table variant="secondary">
        <Table.ScrollContainer>
          <Table.Content aria-label="Shortcuts">
            <Table.Header>
              <Table.Column isRowHeader>shortcut</Table.Column>
              <Table.Column>action</Table.Column>
            </Table.Header>
            <Table.Body>
              {rows.map(([k, v], i) => (
                <Table.Row key={i} id={i}>
                  <Table.Cell>
                    {v ? (
                      // One keycap per key or chord: "G I / G T" reads as
                      // two sequences, not one squashed "GI/GT".
                      <span className="flex flex-wrap items-center gap-1">
                        {k.split(' / ').map((chord, i) => (
                          <span key={i} className="flex items-center gap-1">
                            {i > 0 && <span className="text-muted">/</span>}
                            {chord.split(' ').map((key, j) => (
                              <K key={j}>{key}</K>
                            ))}
                          </span>
                        ))}
                      </span>
                    ) : (
                      <b className="text-sm font-medium">{k}</b>
                    )}
                  </Table.Cell>
                  <Table.Cell>{v}</Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table.Content>
        </Table.ScrollContainer>
      </Table>
    </Shell>
  );
}

/** "migratro #77" for a host PR, the branch for a branch review. */
function prLabel(
  p:
    | {
        pr: { repo: { name: string }; number: number };
        url: string;
        head_ref: string;
      }
    | undefined,
  id: string,
): string {
  if (!p) return id;
  return p.url ? `${p.pr.repo.name} #${p.pr.number}` : p.head_ref;
}

export const Modals = observer(function Modals() {
  const ui = useUi();
  const store = useRootStore();
  switch (ui.modal) {
    case 'cmdk':
      return <CommandMenu />;
    case 'snooze':
      return <SnoozeMenu />;
    case 'assign':
      return <AssignMenu />;
    case 'submit':
      return <SubmitReviewModal />;
    case 'help':
      return <HelpModal />;
    case 'add-project':
      return <AddProjectModal />;
    case 'new-space':
      return isBuildMode(store) ? <NewSpaceModal /> : null;
    case 'sign-in':
      return <SignInModal />;
    case 'report-problem':
      return <ReportProblemModal />;
    case 'whats-collected':
      return <WhatsCollectedModal />;
    default:
      return null;
  }
});

/** Onboarding and the sign-in screen: only the modals that work before
 *  the app opens (Help → Report a Problem…, "What's collected"). */
export const GateModals = observer(function GateModals() {
  const ui = useUi();
  switch (ui.modal) {
    case 'report-problem':
      return <ReportProblemModal />;
    case 'whats-collected':
      return <WhatsCollectedModal />;
    default:
      return null;
  }
});

const HelpModal = observer(HelpModalInner);
