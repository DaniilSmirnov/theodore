/** `useSpaceConfig` asks again when what the config depends on moved: a
 *  Continue or an Update (new base), a turn's end. Else the trust card and
 *  Run only showed after leaving the space and coming back. */
import { afterEach, describe, expect, it } from 'vitest';
import { mount, settle, type Mounted } from '../../test/dom';
import type { Space } from '../../gen/contract';
import { setTransport } from '../../core/transport';
import { FakeTransport } from '../../test/fakeTransport';
import { useSpaceConfig } from './SpaceCommands';

type Bits = Pick<Space, 'status' | 'commands_denied' | 'setup' | 'base_sha'>;

function Probe({ space }: { space: Bits }) {
  useSpaceConfig('s1', space);
  return null;
}

let m: Mounted | null = null;
afterEach(() => {
  m?.unmount();
  m = null;
});

describe('useSpaceConfig', () => {
  it('refetches on a new base and when a turn ends', async () => {
    const transport = new FakeTransport();
    transport.onCall('space_config', () => ({
      commands: [],
      ports: [],
      repo_commands: null,
      denied: false,
    }));
    setTransport(transport);
    const calls = () =>
      transport.calls.filter((c) => c.cmd === 'space_config').length;
    const base: Bits = {
      status: 'running',
      commands_denied: false,
      setup: null,
      base_sha: 'b1',
    };
    m = await mount(<Probe space={base} />);
    await settle();
    expect(calls()).toBe(1);

    const rerender = async (space: Bits) => {
      m!.root.render(<Probe space={space} />);
      await settle();
    };
    await rerender({ ...base, status: 'idle' });
    expect(calls()).toBe(2);
    await rerender({ ...base, status: 'idle', base_sha: 'b2' });
    expect(calls()).toBe(3);
    await rerender({ ...base, status: 'idle', base_sha: 'b2' });
    expect(calls()).toBe(3);
  });
});
