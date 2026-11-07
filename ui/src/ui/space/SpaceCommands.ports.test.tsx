/** The header's port chips: "Open app" and an extra port don't look alike. */
import { afterEach, describe, expect, it } from 'vitest';
import { mount, type Mounted } from '../../test/dom';
import type { Space } from '../../gen/contract';
import { PortChips } from './SpaceCommands';

let m: Mounted | null = null;
afterEach(() => {
  m?.unmount();
  m = null;
});

describe('PortChips', () => {
  it('gives Open app and a port their own icons', async () => {
    const space = {
      id: 's1',
      status: 'idle',
      ports: [4100, 4101],
      process: { status: 'running' },
    } as unknown as Space;
    m = await mount(<PortChips space={space} />);
    const icon = (label: string) =>
      m!.host.querySelector(`[aria-label="${label}"] svg`)?.innerHTML;
    expect(icon('Open app')).toBeTruthy();
    expect(icon('Port 4101')).toBeTruthy();
    expect(icon('Open app')).not.toBe(icon('Port 4101'));
  });
});
