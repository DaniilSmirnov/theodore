import { useEffect, useState } from 'react';
import { ArrowUpRightFromSquare, Xmark } from '@gravity-ui/icons';
import { Button, Card, Chip, Tabs, Tooltip } from '@heroui/react';
import { api } from '../core/api';
import { Code, langOf } from '../highlight';
import {
  countByKind,
  defaultTab,
  groupByFile,
  sourceChip,
} from './symbols/display';
import { SymbolViewer } from './symbols/SymbolViewer';
import type { DiffSide, HunkId, PrId, Sha, SymbolHit } from '../gen/contract';

export interface SymbolPeekTarget {
  name: string;
  path: string;
  /** On `side`: a head line, or a base line for a deletion. */
  line: number;
  /** 1-based char column of the identifier. */
  col: number;
  side: DiffSide;
}

/**
 * VS Code-style peek under the line: go-to-definition / find-references.
 * `find_symbol` asks code intelligence at the clicked head position (a
 * base-side click, which has none in the head worktree, goes by name), or
 * greps the PR's head worktree (plan "symbols"); the chip says which
 * answered, since only a language server's hit is a type-checked jump.
 */
export function SymbolPeek({
  pr,
  rev,
  target,
  diffPaths,
  findHunk,
  onClose,
}: {
  pr: PrId;
  rev: Sha;
  target: SymbolPeekTarget;
  diffPaths: ReadonlySet<string>;
  findHunk: (path: string, line: number) => HunkId | null;
  onClose: () => void;
}) {
  const [hits, setHits] = useState<SymbolHit[] | null>(null);
  const [failed, setFailed] = useState(false);
  const [tab, setTab] = useState<'def' | 'refs'>('def');
  const [viewing, setViewing] = useState<SymbolHit | null>(null);

  // `target` only ever changes by remounting (DiffPane keys SymbolPeek on
  // it), so this runs once per peek with `hits`/`failed` still at their
  // fresh initial values — no need to reset them here first.
  useEffect(() => {
    let live = true;
    const head = target.side === 'head';
    api
      .findSymbol({
        pr,
        name: target.name,
        lang: null,
        from_path: target.path,
        line: head ? target.line : null,
        col: head ? target.col : null,
        side: target.side,
      })
      .then((found) => {
        if (!live) return;
        setHits(found);
        setTab(defaultTab(found));
      })
      .catch(() => live && setFailed(true));
    return () => {
      live = false;
    };
  }, [pr, target]);

  const go = (hit: SymbolHit) => {
    const hunk = diffPaths.has(hit.path) ? findHunk(hit.path, hit.line) : null;
    if (hunk) {
      onClose();
      document
        .getElementById(`H-${hunk}`)
        ?.scrollIntoView({ behavior: 'smooth', block: 'center' });
    } else {
      setViewing(hit);
    }
  };

  const counts = hits ? countByKind(hits) : { definitions: 0, references: 0 };
  const defs = hits?.filter((h) => h.kind === 'definition') ?? [];
  const refs = hits?.filter((h) => h.kind === 'reference') ?? [];
  const shown = tab === 'def' ? defs : refs;
  const chip = sourceChip(shown.length ? shown : (hits ?? []));

  return (
    <>
      <Card className="sym-peek max-w-[760px] gap-0 overflow-hidden p-0">
        <div className="flex items-center gap-2 border-b border-separator px-3 py-2">
          <Chip size="sm" variant="soft" color="accent">
            symbol
          </Chip>
          <code className="min-w-0 flex-1 truncate font-mono text-xs">
            {target.name}
          </code>
          {chip && (
            <Tooltip>
              <Chip size="sm" variant="secondary" className="shrink-0">
                {chip.label}
              </Chip>
              <Tooltip.Content>{chip.tip}</Tooltip.Content>
            </Tooltip>
          )}
          <Button
            isIconOnly
            size="sm"
            variant="ghost"
            aria-label="Close"
            className="size-6 min-w-6"
            onPress={onClose}
          >
            <Xmark className="size-3.5" />
          </Button>
        </div>
        <Tabs
          data-plain
          selectedKey={tab}
          onSelectionChange={(k) => setTab(k as typeof tab)}
          className="px-3 pt-2"
        >
          <Tabs.ListContainer>
            <Tabs.List aria-label="Symbol" className="w-fit">
              <Tabs.Tab id="def" className="h-7 px-3 text-xs">
                Definition {hits ? counts.definitions : ''}
                <Tabs.Indicator />
              </Tabs.Tab>
              <Tabs.Tab id="refs" className="sym-refs h-7 px-3 text-xs">
                <Tabs.Separator />
                References {hits ? counts.references : ''}
                <Tabs.Indicator />
              </Tabs.Tab>
            </Tabs.List>
          </Tabs.ListContainer>
        </Tabs>
        <div className="flex max-h-64 flex-col gap-2 overflow-auto py-2">
          {failed && (
            <div className="px-3 text-xs text-muted">
              Couldn't search for {target.name}.
            </div>
          )}
          {!failed && hits === null && (
            <div className="px-3 text-xs text-muted">Searching…</div>
          )}
          {!failed && hits !== null && shown.length === 0 && (
            <div className="px-3 text-xs text-muted">
              {tab === 'def'
                ? `No declaration of ${target.name} found in this PR's worktree.`
                : `No other references to ${target.name} found.`}
            </div>
          )}
          {!failed &&
            hits !== null &&
            groupByFile(shown).map((group) => (
              <div key={group.path} className="mb-1">
                <div className="truncate px-3 py-1 font-mono text-xs text-muted">
                  {group.path}
                </div>
                {group.hits.map((h) => (
                  <button
                    key={`${h.path}:${h.line}`}
                    type="button"
                    onClick={() => go(h)}
                    className="sym-hit ln w-full cursor-pointer text-left hover:bg-default/60"
                    style={{ gridTemplateColumns: '44px minmax(0, 1fr) auto' }}
                  >
                    <span className="no">{h.line}</span>
                    <Code s={h.preview} lang={langOf(h.path)} />
                    <span className="pe-3">
                      {diffPaths.has(h.path) ? (
                        <Chip size="sm" variant="soft" color="accent">
                          in PR
                        </Chip>
                      ) : (
                        <ArrowUpRightFromSquare className="size-3.5 text-muted" />
                      )}
                    </span>
                  </button>
                ))}
              </div>
            ))}
        </div>
      </Card>
      {viewing && (
        <SymbolViewer
          // A different hit gets a fresh mount, same reasoning as
          // `SymbolPeek`'s own key in `DiffPane`.
          key={`${viewing.path}:${viewing.line}`}
          pr={pr}
          rev={rev}
          path={viewing.path}
          line={viewing.line}
          onClose={() => setViewing(null)}
        />
      )}
    </>
  );
}
