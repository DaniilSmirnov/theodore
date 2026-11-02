import { afterEach, describe, expect, it } from 'vitest';
import { mount, type Mounted } from '../../test/dom';
import type { Space } from '../../gen/contract';
import { SpaceProgress, shownProgress } from './SpaceProgress';

const space = (over: Partial<Space>): Space => ({
  id: 's1',
  project: 'local:app',
  name: 's1',
  goal: '',
  branch: 'space/s1',
  base: 'main',
  base_sha: 'base',
  head_sha: 'base',
  worktree: '/tmp/s1',
  status: 'running',
  agent: 'claude',
  model: null,
  mode: 'edit',
  review_pr: null,
  pr: null,
  turns: 0,
  run: 'space:s1:m1',
  error: null,
  created_at: 0,
  updated_at: 0,
  ...over,
});

const progress = {
  steps: [
    { label: 'Read the login flow', state: 'done' as const },
    { label: 'Fix the redirect', state: 'active' as const },
    { label: 'Run the check', state: 'pending' as const },
  ],
  note: 'redirect found',
  at: 1,
};

let m: Mounted | null = null;
afterEach(() => {
  m?.unmount();
  m = null;
});

describe('SpaceProgress', () => {
  it('shows the steps of the running turn', async () => {
    m = await mount(<SpaceProgress space={space({ progress })} />);
    const items = [...m.host.querySelectorAll('li')];
    expect(items.map((li) => [li.textContent, li.dataset.state])).toEqual([
      ['Read the login flow', 'done'],
      ['Fix the redirect', 'active'],
      ['Run the check', 'pending'],
    ]);
    expect(m.host.textContent).toContain('1 of 3 done · redirect found');
  });

  it('is gone without a turn or without steps', () => {
    expect(shownProgress(space({ progress, status: 'idle' }))).toBeNull();
    expect(shownProgress(space({ progress: null }))).toBeNull();
    expect(
      shownProgress(space({ progress: { ...progress, steps: [] } })),
    ).toBeNull();
    expect(shownProgress(space({ progress }))).toBe(progress);
  });
});
