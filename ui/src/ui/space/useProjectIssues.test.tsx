/** New space's issue picker on a host without issues: the core answers
 *  `host.unsupported`, and the field says so calmly — on load, and when
 *  Create with a typed `#12` is turned away — never a toast. */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { setCommandFailureHandler, api } from '../../core/api';
import { setTransport } from '../../core/transport';
import { FakeTransport } from '../../test/fakeTransport';
import { mount, settle, type Mounted } from '../../test/dom';
import { useProjectIssues } from './useProjectIssues';

const UNSUPPORTED = {
  code: 'host.unsupported',
  message: 'host: not supported by this host: open_issues',
};

function Probe({ project }: { project: string | null }) {
  const got = useProjectIssues(project);
  return <span>{got.unsupported ? 'unsupported' : 'issues'}</span>;
}

describe('a host without issues', () => {
  let transport: FakeTransport;
  let m: Mounted | null = null;
  const toast = vi.fn();

  beforeEach(() => {
    transport = new FakeTransport();
    setTransport(transport);
    setCommandFailureHandler(toast);
  });

  afterEach(() => {
    m?.unmount();
    m = null;
    toast.mockReset();
  });

  it('is unsupported on load, with no toast', async () => {
    transport.onCall('project_issues', () => {
      throw UNSUPPORTED;
    });
    m = await mount(<Probe project="local:app" />);
    await settle();
    expect(m.host.textContent).toBe('unsupported');
    expect(toast).not.toHaveBeenCalled();
  });

  it('turns Create with #12 away without a toast', async () => {
    transport.onCall('create_space', () => {
      throw UNSUPPORTED;
    });
    const err = await api
      .createSpace({
        project: 'gitlab:acme/app',
        goal: '',
        name: null,
        branch: null,
        base: null,
        agent: null,
        model: null,
        start_from: { kind: 'issue', value: '12' },
      })
      .catch((e: unknown) => e);
    expect((err as { code: string }).code).toBe('host.unsupported');
    expect(toast).not.toHaveBeenCalled();
  });

  it('still toasts any other Create failure', async () => {
    transport.onCall('create_space', () => {
      throw { code: 'host', message: 'host: host server error: 502' };
    });
    await api
      .createSpace({
        project: 'gitlab:acme/app',
        goal: 'x',
        name: null,
        branch: null,
        base: null,
        agent: null,
        model: null,
        start_from: null,
      })
      .catch(() => {});
    expect(toast).toHaveBeenCalledTimes(1);
  });
});
