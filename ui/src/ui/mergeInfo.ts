/** `merge_info`, asked again on a backoff: the space header's merge state
 *  and the merge dialog both recover from a host having trouble on their
 *  own, and say what's wrong in a sentence rather than the core's words. */
import { useEffect, useState } from 'react';
import type { HostKind, MergeInfo, PrId } from '../gen/contract';
import { api, CommandFailure } from '../core/api';

/** How long before asking `merge_info` again after `attempt` failed (or
 *  the host hadn't finished checking): 2 s, doubling, capped at 30 s. */
export function mergeInfoRetryMs(attempt: number): number {
  return Math.min(2_000 * 2 ** attempt, 30_000);
}

export type MergeInfoState = {
  info: MergeInfo | null;
  /** The last ask failed; cleared by the next answer. */
  error: unknown;
};

/** `merge_info` of an open PR, asked again whenever `version` changes
 *  (the PR row's head and update time); the last answer stays meanwhile.
 *  A failed ask (a host having trouble) or an `unknown` answer is asked
 *  again on a backoff, so "Checking…" doesn't stay until you leave.
 *  Quiet: the caller says what's unknown (`mergeInfoProblem`). */
export function useMergeInfoState(
  pr: PrId | null,
  version: string,
): MergeInfoState {
  const [got, setGot] = useState<{
    pr: PrId;
    info: MergeInfo | null;
    error: unknown;
  } | null>(null);
  const [attempt, setAttempt] = useState({ key: '', n: 0 });
  const key = `${pr}@${version}`;
  const n = attempt.key === key ? attempt.n : 0;
  useEffect(() => {
    if (!pr) return;
    let live = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const again = () => {
      timer = setTimeout(
        () => live && setAttempt({ key, n: n + 1 }),
        mergeInfoRetryMs(n),
      );
    };
    void api
      .mergeInfo(pr, true)
      .then((info) => {
        if (!live) return;
        setGot({ pr, info, error: null });
        if (info.state === 'unknown') again();
      })
      .catch((error: unknown) => {
        if (!live) return;
        // The last answer stays; the failure is said beside it.
        setGot((g) => ({
          pr,
          info: g && g.pr === pr ? g.info : null,
          error,
        }));
        again();
      });
    return () => {
      live = false;
      if (timer) clearTimeout(timer);
    };
  }, [pr, key, n]);
  return got && got.pr === pr
    ? { info: got.info, error: got.error }
    : { info: null, error: null };
}

export function useMergeInfo(
  pr: PrId | null,
  version: string,
): MergeInfo | null {
  return useMergeInfoState(pr, version).info;
}

const HOST_NAME: Record<HostKind, string> = {
  github: 'GitHub',
  gitlab: 'GitLab',
  bitbucket: 'Bitbucket',
  local: 'The host',
};

/** A failed `merge_info` in a sentence, said while it's asked again. */
export function mergeInfoProblem(host: HostKind, error: unknown): string {
  const name = HOST_NAME[host];
  const code = error instanceof CommandFailure ? error.code : '';
  switch (code) {
    case 'offline':
      return `Can't reach ${name}. Trying again when the connection is back.`;
    case 'auth':
      return `${name} turned the sign-in down. Sign in again in Settings; trying again meanwhile.`;
    case 'rate_limited':
      // Already a sentence: how long to wait.
      return (error as CommandFailure).message;
    default:
      return `${name} is having trouble right now. Trying again…`;
  }
}
