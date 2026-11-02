/** The project's own open issues assigned to the viewer, for New space's
 *  "From an issue" picker (`project_issues`). Empty while `project` is
 *  `null` (nothing picked, or a mode other than "issue"), or when the
 *  host doesn't support reading issues (`Capabilities::issues`) —
 *  `project_issues` then answers `Unsupported` and the picker just stays
 *  empty rather than erroring. */
import { useEffect, useState } from 'react';
import type { SpaceIssue } from '../../gen/contract';
import { api } from '../../core/api';

export function useProjectIssues(project: string | null): SpaceIssue[] {
  const [issues, setIssues] = useState<SpaceIssue[]>([]);

  useEffect(() => {
    if (!project) return;
    let cancelled = false;
    void api
      .projectIssues({ project })
      .then((r) => {
        if (!cancelled) setIssues(r);
      })
      .catch(() => {
        if (!cancelled) setIssues([]);
      });
    return () => {
      cancelled = true;
    };
  }, [project]);

  return issues;
}
