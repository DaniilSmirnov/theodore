/** Setup Retry waiting for "Run commands from this repo?": the output on
 *  screen is the failed run's, and says so. */
import { afterEach, describe, expect, it } from 'vitest';
import type { Space } from '../../gen/contract';
import { RootStore } from '../../stores/RootStore';
import { RootStoreProvider } from '../../stores/context';
import { mount, type Mounted } from '../../test/dom';
import { SetupStage } from './SpaceCommands';

const space = (status: 'waiting' | 'failed'): Space => ({
  id: 's1',
  project: 'local:app',
  name: 's1',
  goal: '',
  branch: 'theo/s1',
  base: 'main',
  base_sha: 'b',
  head_sha: 'h',
  worktree: '/tmp/s1',
  status: 'idle',
  agent: 'claude',
  model: null,
  mode: 'edit',
  review_pr: null,
  pr: null,
  turns: 0,
  run: null,
  error: null,
  created_at: 0,
  updated_at: 0,
  setup: { status, goal_pending: false, error: 'exit 1' },
});

let m: Mounted | null = null;
afterEach(() => {
  m?.unmount();
  m = null;
});

async function stage(status: 'waiting' | 'failed') {
  const store = new RootStore();
  store.ephemeral.seedSpaceLogs('s1', [
    { kind: 'setup', line: 'npm ERR! missing script', stderr: true, at: 1 },
  ]);
  return mount(
    <RootStoreProvider store={store}>
      <SetupStage space={space(status)} />
    </RootStoreProvider>,
  );
}

describe('the setup stage', () => {
  it('marks the last run’s output stale while a Retry waits for Allow', async () => {
    m = await stage('waiting');
    expect(m.host.querySelector('.setup-log-stale')?.textContent).toContain(
      'From the last run',
    );
    expect(m.host.textContent).toContain('npm ERR!');
  });

  it('shows a failed run’s output as is', async () => {
    m = await stage('failed');
    expect(m.host.querySelector('.setup-log-stale')).toBeNull();
    expect(m.host.textContent).toContain('npm ERR!');
  });
});
