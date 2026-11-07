import { describe, expect, it } from 'vitest';
import type { HostStatus } from '../../stores/EphemeralStore';
import { syncIndicator } from './SyncStatus';

const host = (overrides: Partial<HostStatus> = {}): HostStatus => ({
  online: true,
  rateLimitedUntil: null,
  authOk: true,
  serverTrouble: false,
  ...overrides,
});

describe('syncIndicator', () => {
  it('is synced once online with nothing pending', () => {
    expect(syncIndicator('online', host(), 0)).toMatchObject({
      label: 'Synced',
      dot: 'success',
    });
  });

  it('is offline when disconnected or the host is offline', () => {
    expect(syncIndicator('error', host(), 0).dot).toBe('danger');
    expect(syncIndicator('online', host({ online: false }), 0).dot).toBe(
      'danger',
    );
  });

  it('shows the pending count while transactions are in flight', () => {
    const info = syncIndicator('online', host(), 3);
    expect(info.label).toBe('Syncing (3)');
    expect(info.dot).toBe('warning');
  });

  it('is syncing (no count) while bootstrapping', () => {
    const info = syncIndicator('bootstrapping', host(), 0);
    expect(info.label).toBe('Syncing');
    expect(info.dot).toBe('warning');
  });

  it('offline wins over a pending count', () => {
    expect(syncIndicator('error', host({ online: false }), 2).dot).toBe(
      'danger',
    );
  });
});
