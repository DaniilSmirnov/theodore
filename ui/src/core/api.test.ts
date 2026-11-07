import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { FakeTransport } from '../test/fakeTransport';
import {
  api,
  CommandFailure,
  setAccountRequiredHandler,
  setCommandFailureHandler,
} from './api';
import { setTransport } from './transport';

describe('a command turned away by the account gate', () => {
  let transport: FakeTransport;
  const toast = vi.fn();
  const recheck = vi.fn();

  beforeEach(() => {
    transport = new FakeTransport();
    setTransport(transport);
    setCommandFailureHandler(toast);
    setAccountRequiredHandler(recheck);
  });

  afterEach(() => {
    toast.mockReset();
    recheck.mockReset();
  });

  it('re-reads the account instead of toasting', async () => {
    transport.onCall('open_pr', () => {
      throw { code: 'account_required', message: 'Sign in' };
    });
    const err = await api
      .openPr({ url: 'https://github.com/a/b/pull/1' })
      .catch((e: unknown) => e);
    expect(err).toBeInstanceOf(CommandFailure);
    expect((err as CommandFailure).code).toBe('account_required');
    expect(recheck).toHaveBeenCalledTimes(1);
    expect(toast).not.toHaveBeenCalled();
  });

  it('leaves other failures to the toast', async () => {
    transport.onCall('open_pr', () => {
      throw { code: 'host', message: 'offline' };
    });
    await api.openPr({ url: 'https://github.com/a/b/pull/1' }).catch(() => {});
    expect(toast).toHaveBeenCalledTimes(1);
    expect(recheck).not.toHaveBeenCalled();
  });
});

describe('operation ids', () => {
  let transport: FakeTransport;

  beforeEach(() => {
    transport = new FakeTransport();
    setTransport(transport);
    setCommandFailureHandler(() => {});
  });

  it('sends a fresh 16-hex op id with every command', async () => {
    transport.onCall('app_info', () => ({ version: '1' }));
    await api.appInfo();
    await api.appInfo();
    const [a, b] = transport.opIds;
    expect(a).toMatch(/^[0-9a-f]{16}$/);
    expect(b).toMatch(/^[0-9a-f]{16}$/);
    expect(a).not.toBe(b);
  });

  it('puts the op id on the failure', async () => {
    transport.onCall('open_pr', () => {
      throw { code: 'host', message: 'offline' };
    });
    const err = (await api
      .openPr({ url: 'https://github.com/a/b/pull/1' })
      .catch((e: unknown) => e)) as CommandFailure;
    expect(err.opId).toBe(transport.opIds[0]);
    expect(err.opId).toMatch(/^[0-9a-f]{16}$/);
  });

  it('never throws or toasts from logClient and trackUi', async () => {
    const toast = vi.fn();
    setCommandFailureHandler(toast);
    transport.onCall('log_client', () => {
      throw { code: 'internal', message: 'down' };
    });
    // track_ui has no handler at all: the fake rejects.
    await expect(
      api.logClient({ level: 'error', source: 'onerror', message: 'x' }),
    ).resolves.toBeNull();
    await expect(
      api.trackUi({ event: 'onboarding_step', step: 'github' }),
    ).resolves.toBeNull();
    expect(toast).not.toHaveBeenCalled();
  });
});

describe('a stale last-seen sha', () => {
  let transport: FakeTransport;
  const toast = vi.fn();

  beforeEach(() => {
    transport = new FakeTransport();
    setTransport(transport);
    setCommandFailureHandler(toast);
  });

  afterEach(() => toast.mockReset());

  it("isn't toasted by interdiff: the review forgets it instead", async () => {
    transport.onCall('interdiff', () => {
      throw { code: 'commit_not_found', message: 'commit b6cef3e not found' };
    });
    const err = await api
      .interdiff({ pr: 'github:a/b#1', base_sha: 'b6cef3e', file: null })
      .catch((e: unknown) => e);
    expect((err as CommandFailure).code).toBe('commit_not_found');
    expect(toast).not.toHaveBeenCalled();
  });

  it('leaves any other interdiff failure to the toast', async () => {
    transport.onCall('interdiff', () => {
      throw { code: 'git', message: 'git diff: exit 1' };
    });
    await api
      .interdiff({ pr: 'github:a/b#1', base_sha: 'b6cef3e', file: null })
      .catch(() => {});
    expect(toast).toHaveBeenCalledTimes(1);
  });
});
