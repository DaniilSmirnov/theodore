/** The Checks and Commits tabs of a PR — `PrDetail.checks` / `.commits`. */
import { useEffect, useState } from 'react';
import { ArrowRotateRight, PaperPlane, PersonPlus } from '@gravity-ui/icons';
import {
  Accordion,
  Button,
  Chip,
  Dropdown,
  Table,
  Toolbar,
  Tooltip,
} from '@heroui/react';
import { observer } from 'mobx-react-lite';
import type {
  Check,
  CheckStatus,
  Commit,
  Person,
  PrId,
  Reviewer,
} from '../gen/contract';
import { ask, rerunCheck, setReviewers } from '../actions';
import { api } from '../core/api';
import { useRootStore } from '../stores/context';
import { isBuildMode } from './space/mode';
import { spaceTitle } from './space/spaces';
import { ago } from './List';
import { Avatar, Btn, Spinner } from './bits';

const ORDER: Record<CheckStatus, number> = {
  fail: 0,
  pending: 1,
  skipped: 2,
  pass: 3,
};

const STATUS_COLOR: Record<
  CheckStatus,
  'danger' | 'warning' | 'default' | 'success'
> = {
  fail: 'danger',
  pending: 'warning',
  skipped: 'default',
  pass: 'success',
};

const STATUS_LABEL: Record<CheckStatus, string> = {
  fail: 'failed',
  pending: 'running…',
  skipped: 'skipped',
  pass: 'passed',
};

function duration(ms: number | null): string {
  if (ms == null) return '';
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}s`;
  return `${Math.round(s / 60)}m ${s % 60}s`;
}

/** "Fix with agent" (docs/spaces.md "Fix CI"): only on a PR a space owns,
 *  only with build mode on, only once something failed. */
const FixCiButton = observer(function FixCiButton({
  pr,
  anyFailing,
}: {
  pr: PrId;
  anyFailing: boolean;
}) {
  const store = useRootStore();
  const [busy, setBusy] = useState(false);
  if (!isBuildMode(store) || !anyFailing) return null;
  const space = store.models.spaces
    .all()
    .find((s) => s.status !== 'archived' && s.pr === pr);
  if (!space) return null;
  const running = space.status === 'running';
  const send = async () => {
    setBusy(true);
    try {
      await api.spaceFixCi({ space: space.id });
      store.ephemeral.toast(`Sent to ${spaceTitle(space)}`, 'info', {
        label: 'Open',
        onAction: () => store.ui.go({ name: 'space', id: space.id }),
      });
    } catch {
      // toasted by api.ts
    } finally {
      setBusy(false);
    }
  };
  return (
    <Tooltip>
      <Button
        size="sm"
        variant="ghost"
        className="fix-ci gap-1.5"
        isDisabled={running}
        isPending={busy}
        onPress={() => void send()}
      >
        <PaperPlane className="size-3.5" />
        Fix with agent
      </Button>
      <Tooltip.Content>
        {running
          ? `${space.agent} is still working in ${spaceTitle(space)}`
          : `Send the failing checks' logs to ${spaceTitle(space)} as its next turn`}
      </Tooltip.Content>
    </Tooltip>
  );
});

export const Checks = observer(function Checks({
  pr,
  checks,
}: {
  pr: PrId;
  checks: Check[];
}) {
  const store = useRootStore();
  const [rerunning, setRerunning] = useState<string | null>(null);
  const sorted = [...checks].sort((a, b) => ORDER[a.status] - ORDER[b.status]);
  const failed = sorted.filter((c) => c.status === 'fail').map((c) => c.id);
  return (
    <main className="diff min-w-0 overflow-auto bg-background px-6 pb-20 pt-5">
      <div className="checks flex max-w-[900px] flex-col gap-2">
        <div className="flex items-center justify-between">
          <h3 className="text-sm font-semibold">Checks</h3>
          <FixCiButton pr={pr} anyFailing={failed.length > 0} />
        </div>
        {sorted.length === 0 ? (
          <p className="text-sm text-muted">No checks reported.</p>
        ) : (
          <Accordion
            variant="surface"
            allowsMultipleExpanded
            defaultExpandedKeys={failed}
          >
            {sorted.map((c) => (
              <Accordion.Item key={c.id} id={c.id} className="check">
                <Accordion.Heading>
                  <Accordion.Trigger className="flex w-full items-center gap-3 px-4 py-3 text-left">
                    <Chip
                      size="sm"
                      variant="soft"
                      color={STATUS_COLOR[c.status]}
                      className="shrink-0"
                    >
                      {c.status}
                    </Chip>
                    <b className="min-w-0 flex-1 truncate" title={c.name}>
                      {c.name}
                    </b>
                    <span className="shrink-0 whitespace-nowrap text-muted">
                      {c.status === 'pending'
                        ? STATUS_LABEL.pending
                        : duration(c.duration_ms)}
                    </span>
                    {c.url && (
                      <a
                        href={c.url}
                        target="_blank"
                        rel="noreferrer"
                        className="shrink-0 text-xs text-accent"
                        onClick={(e) => e.stopPropagation()}
                      >
                        details
                      </a>
                    )}
                  </Accordion.Trigger>
                </Accordion.Heading>
                <Accordion.Panel>
                  <Accordion.Body className="flex flex-col gap-2">
                    <Toolbar aria-label={c.name} className="flex gap-1.5">
                      {c.status === 'fail' && (
                        <>
                          <Btn
                            tone="tertiary"
                            icon={<ArrowRotateRight />}
                            isPending={rerunning === c.id}
                            onPress={() => {
                              setRerunning(c.id);
                              void rerunCheck(store, {
                                pr,
                                check: c.id,
                              }).finally(() => setRerunning(null));
                            }}
                          >
                            Re-run
                          </Btn>
                          <Btn
                            tone="tertiary"
                            onPress={() =>
                              void ask(store, {
                                pr,
                                question: `why did ${c.name} fail?`,
                                about: null,
                                group: null,
                                agent: null,
                              }).catch(() => {
                                // toasted by api.ts
                              })
                            }
                          >
                            Why did it fail
                          </Btn>
                        </>
                      )}
                    </Toolbar>
                  </Accordion.Body>
                </Accordion.Panel>
              </Accordion.Item>
            ))}
          </Accordion>
        )}
      </div>
    </main>
  );
});

/** Commits newer than the viewer's last submitted review (`after_my_review`)
 *  get a highlight and, above the first one, a one-time divider — "new since
 *  you last looked" (plan "Commits tab"). */
export function Commits({ commits }: { commits: Commit[] }) {
  const firstNewSha = commits.find((c) => c.after_my_review)?.sha;
  return (
    <main className="diff min-w-0 overflow-auto bg-background px-6 pb-20 pt-5">
      <Table className="max-w-[900px]">
        <Table.ScrollContainer>
          <Table.Content aria-label="Commits">
            <Table.Header>
              <Table.Column>sha</Table.Column>
              <Table.Column isRowHeader>commit</Table.Column>
              <Table.Column>author</Table.Column>
              <Table.Column>when</Table.Column>
            </Table.Header>
            <Table.Body
              renderEmptyState={() => (
                <span className="px-4 py-3 text-sm text-muted">
                  No commits.
                </span>
              )}
            >
              {commits.flatMap((c) => {
                const rows = [];
                if (c.sha === firstNewSha) {
                  rows.push(
                    <Table.Row
                      key={`${c.sha}-divider`}
                      id={`${c.sha}-divider`}
                      className="commit-divider"
                    >
                      <Table.Cell
                        colSpan={4}
                        className="bg-accent-soft py-1 text-center text-xs font-medium text-accent"
                      >
                        New since your last review
                      </Table.Cell>
                    </Table.Row>,
                  );
                }
                rows.push(
                  <Table.Row
                    key={c.sha}
                    id={c.sha}
                    className={
                      c.after_my_review ? 'bg-accent-soft/40' : undefined
                    }
                  >
                    <Table.Cell className="font-mono text-accent">
                      {c.sha.slice(0, 7)}
                    </Table.Cell>
                    <Table.Cell>{c.title}</Table.Cell>
                    <Table.Cell>
                      <span className="inline-flex items-center gap-1.5">
                        <Avatar person={c.author} size={16} />
                        {c.author.name ?? c.author.login}
                      </span>
                    </Table.Cell>
                    <Table.Cell className="text-muted">{ago(c.at)}</Table.Cell>
                  </Table.Row>,
                );
                return rows;
              })}
            </Table.Body>
          </Table.Content>
        </Table.ScrollContainer>
      </Table>
    </main>
  );
}

const REVIEW_LABEL: Record<Reviewer['state'], string> = {
  requested: 'not reviewed yet',
  approved: 'approved',
  changes_requested: 'requested changes',
  commented: 'commented',
};

const REVIEW_COLOR: Record<
  Reviewer['state'],
  'default' | 'success' | 'danger'
> = {
  requested: 'default',
  approved: 'success',
  changes_requested: 'danger',
  commented: 'default',
};

/** Who reviews, and where they stand — chips with a name and a state, not just a dot. */
export function ReviewerStrip({ reviewers }: { reviewers: Reviewer[] }) {
  if (!reviewers.length) return null;
  return (
    <div className="reviewer-strip flex min-w-0 items-center gap-1.5 overflow-x-auto scrollbar-none">
      {reviewers.map((r, i) => (
        <Tooltip key={r.team ?? r.person.login ?? i}>
          <Chip
            size="sm"
            variant="soft"
            color={REVIEW_COLOR[r.state]}
            className="person shrink-0 gap-1.5 ps-0.5"
          >
            <Avatar person={r.person} size={18} />
            <span className="font-medium">
              {r.team ? `team:${r.team}` : r.person.login}
            </span>
          </Chip>
          <Tooltip.Content>{REVIEW_LABEL[r.state]}</Tooltip.Content>
        </Tooltip>
      ))}
    </div>
  );
}

/** Add/remove reviewers from `assignable_users`, via `set_reviewers`. Teams
 *  (`Reviewer.team`) aren't offered here — `assignable_users` returns
 *  people, not teams. */
export const ReviewerPicker = observer(function ReviewerPicker({
  pr,
  compact = false,
}: {
  pr: PrId;
  /** Icon only, for a narrow header. */
  compact?: boolean;
}) {
  const store = useRootStore();
  const [open, setOpen] = useState(false);
  const [users, setUsers] = useState<Person[] | 'loading' | null>(null);
  const detail = store.models.prDetails.get(pr);
  const requested = new Set(
    (detail?.reviewers ?? []).filter((r) => !r.team).map((r) => r.person.login),
  );
  // The host refuses a review request from the PR's author.
  const candidates = Array.isArray(users)
    ? users.filter((u) => u.login !== detail?.summary?.author?.login)
    : [];

  useEffect(() => {
    if (!open || users) return;
    setUsers('loading');
    void api
      .assignableUsers(pr)
      .then(setUsers)
      .catch(() => setUsers([]));
  }, [open, pr, users]);

  return (
    <Dropdown isOpen={open} onOpenChange={setOpen}>
      <Button
        size="sm"
        variant="tertiary"
        className="reviewer-picker shrink-0 gap-1.5"
        isIconOnly={compact}
        aria-label="Reviewers"
      >
        <PersonPlus className="size-4" />
        {!compact && 'Reviewers'}
      </Button>
      <Dropdown.Popover placement="bottom end">
        {users === 'loading' || users === null ? (
          <div className="flex items-center gap-2 px-4 py-3 text-sm text-muted">
            <Spinner /> Loading…
          </div>
        ) : candidates.length === 0 ? (
          <div className="px-4 py-3 text-sm text-muted">
            No one else can review this PR
          </div>
        ) : (
          <Dropdown.Menu
            aria-label="Reviewers"
            selectionMode="multiple"
            selectedKeys={requested}
            onSelectionChange={(keys) => {
              const next = new Set([...keys].map(String));
              const add = [...next].filter((l) => !requested.has(l));
              const remove = [...requested].filter((l) => !next.has(l));
              if (add.length || remove.length) {
                void setReviewers(store, {
                  pr,
                  add,
                  add_teams: [],
                  remove,
                });
              }
            }}
          >
            {candidates.map((u) => (
              <Dropdown.Item key={u.login} id={u.login} textValue={u.login}>
                {u.login}
              </Dropdown.Item>
            ))}
          </Dropdown.Menu>
        )}
      </Dropdown.Popover>
    </Dropdown>
  );
});
