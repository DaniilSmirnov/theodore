/** The project's open issues, the viewer's own first, for New space's
 *  "An existing issue" picker (`project_issues`). Empty while `project` is
 *  `null` (nothing picked, or a mode other than "issue"). A host that
 *  can't read issues (`Capabilities::issues`) answers `host.unsupported`:
 *  that is `unsupported`, said calmly by the picker, never a toast. Any
 *  other failure leaves the list empty; a typed `#n` or URL still works. */
import { useEffect, useState } from 'react';
import type { SpaceIssue } from '../../gen/contract';
import { api, CommandFailure, HOST_UNSUPPORTED } from '../../core/api';

export type ProjectIssues = {
  issues: SpaceIssue[];
  /** The host has no issues to start from. */
  unsupported: boolean;
};

const NONE: ProjectIssues = { issues: [], unsupported: false };

export function useProjectIssues(project: string | null): ProjectIssues {
  // Keyed by project: an answer for the previous one never shows.
  const [state, setState] = useState<{ for: string; got: ProjectIssues }>();

  useEffect(() => {
    if (!project) return;
    let cancelled = false;
    void api
      .projectIssues({ project }, true)
      .then((issues) => {
        if (!cancelled)
          setState({ for: project, got: { issues, unsupported: false } });
      })
      .catch((e: unknown) => {
        if (cancelled) return;
        setState({
          for: project,
          got: {
            issues: [],
            unsupported:
              e instanceof CommandFailure && e.code === HOST_UNSUPPORTED,
          },
        });
      });
    return () => {
      cancelled = true;
    };
  }, [project]);

  return project && state?.for === project ? state.got : NONE;
}
