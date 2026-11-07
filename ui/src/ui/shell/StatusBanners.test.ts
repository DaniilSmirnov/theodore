import { describe, expect, it } from 'vitest';
import type { HostStatus } from '../../stores/EphemeralStore';
import { formatCountdown, statusKind } from './StatusBanners';

const host = (overrides: Partial<HostStatus> = {}): HostStatus => ({
  online: true,
  rateLimitedUntil: null,
  authOk: true,
  serverTrouble: false,
  ...overrides,
});

describe('statusKind', () => {
  it('shows nothing when everything is fine', () => {
    expect(statusKind('online', host(), Date.now())).toBeNull();
  });

  it('is offline when the sync client errored, even if the host looks fine', () => {
    expect(statusKind('error', host(), Date.now())).toBe('offline');
  });

  it('is offline when the host itself reports offline', () => {
    expect(statusKind('online', host({ online: false }), Date.now())).toBe(
      'offline',
    );
  });

  it('prefers offline over an auth failure', () => {
    expect(
      statusKind('error', host({ online: false, authOk: false }), Date.now()),
    ).toBe('offline');
  });

  it('is an auth failure once online and the token was rejected', () => {
    expect(statusKind('online', host({ authOk: false }), Date.now())).toBe(
      'auth',
    );
  });

  it('prefers auth failure over a rate limit', () => {
    const now = Date.now();
    expect(
      statusKind(
        'online',
        host({ authOk: false, rateLimitedUntil: now + 60_000 }),
        now,
      ),
    ).toBe('auth');
  });

  it('is rate-limited only while the reset time is still in the future', () => {
    const now = Date.now();
    expect(
      statusKind('online', host({ rateLimitedUntil: now + 60_000 }), now),
    ).toBe('rate');
    expect(
      statusKind('online', host({ rateLimitedUntil: now - 1 }), now),
    ).toBeNull();
  });
});

describe('a host answering with server errors', () => {
  it('is trouble, not offline', () => {
    expect(
      statusKind('online', host({ serverTrouble: true }), Date.now()),
    ).toBe('trouble');
  });

  it('gives way to offline and to a rate limit', () => {
    const now = Date.now();
    expect(
      statusKind('online', host({ serverTrouble: true, online: false }), now),
    ).toBe('offline');
    expect(
      statusKind(
        'online',
        host({ serverTrouble: true, rateLimitedUntil: now + 1000 }),
        now,
      ),
    ).toBe('rate');
  });
});

describe('formatCountdown', () => {
  it('says seconds under two minutes', () => {
    expect(formatCountdown(82_000)).toBe('1 min 22 s');
    expect(formatCountdown(45_000)).toBe('45 s');
    expect(formatCountdown(60_000)).toBe('1 min');
  });

  it('rounds minutes, not up', () => {
    expect(formatCountdown(150_000)).toBe('3 min');
    expect(formatCountdown(140_000)).toBe('2 min');
    expect(formatCountdown(3_900_000)).toBe('1h 5m');
  });
});
