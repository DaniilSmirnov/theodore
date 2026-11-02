import { autorun } from 'mobx';
import { describe, expect, it } from 'vitest';
import type { AppInfo } from '../gen/contract';
import { setTransport } from '../core/transport';
import { FakeTransport } from '../test/fakeTransport';
import { EphemeralStore } from './EphemeralStore';

const APP_INFO: AppInfo = {
  version: '0.1.0',
  channel: 'stable',
  update: null,
  commit: 'abc',
  build_env: 'production',
  built_at: 0,
};

describe('EphemeralStore config PR', () => {
  it('a config_pr event becomes a toast that opens the PR', () => {
    const e = new EphemeralStore();
    e.handle({
      type: 'config_pr',
      project: 'github:acme/widget',
      number: 7,
      url: 'https://github.com/acme/widget/pull/7',
    });
    expect(e.toasts).toHaveLength(1);
    expect(e.toasts[0]).toMatchObject({
      level: 'info',
      text: 'Config PR #7 is open',
    });
    expect(e.toasts[0]!.action?.label).toBe('Open');
  });
});

describe('EphemeralStore stalled tracking', () => {
  it('marks a run stalled on run_stalled', () => {
    const e = new EphemeralStore();
    e.handle({ type: 'run_stalled', run: 'r1', agent: 'claude' });
    expect(e.stalled.has('r1')).toBe(true);
  });

  it('clears stalled once the run produces more text', () => {
    const e = new EphemeralStore();
    e.handle({ type: 'run_stalled', run: 'r1', agent: 'claude' });
    e.handle({ type: 'agent_delta', run: 'r1', agent: 'claude', text: 'hi' });
    expect(e.stalled.has('r1')).toBe(false);
  });

  it('clears stalled once the run logs a new line', () => {
    const e = new EphemeralStore();
    e.handle({ type: 'run_stalled', run: 'r1', agent: 'claude' });
    e.handle({ type: 'run_log', run: 'r1', agent: 'claude', line: 'tool: x' });
    expect(e.stalled.has('r1')).toBe(false);
  });

  it('does not affect other runs', () => {
    const e = new EphemeralStore();
    e.handle({ type: 'run_stalled', run: 'r1', agent: 'claude' });
    e.handle({ type: 'run_stalled', run: 'r2', agent: 'codex' });
    e.handle({ type: 'agent_delta', run: 'r1', agent: 'claude', text: 'hi' });
    expect(e.stalled.has('r1')).toBe(false);
    expect(e.stalled.has('r2')).toBe(true);
  });

  it('clearRun also clears stalled', () => {
    const e = new EphemeralStore();
    e.handle({ type: 'run_stalled', run: 'r1', agent: 'claude' });
    e.clearRun('r1');
    expect(e.stalled.has('r1')).toBe(false);
  });
});

describe('EphemeralStore account', () => {
  it('starts unknown and follows account_status events', () => {
    const e = new EphemeralStore();
    expect(e.account).toBeNull();
    e.handle({
      type: 'account_status',
      status: {
        available: true,
        required: true,
        session_ended: false,
        user: {
          id: 'u',
          email: 'ada@example.com',
          name: null,
          avatar_url: null,
        },
      },
    });
    expect(e.account?.user?.email).toBe('ada@example.com');
    e.handle({
      type: 'account_status',
      status: {
        available: true,
        required: true,
        user: null,
        session_ended: true,
      },
    });
    expect(e.account?.user).toBeNull();
  });

  it('setAccount stores a fetched status', () => {
    const e = new EphemeralStore();
    e.setAccount({
      available: false,
      required: false,
      user: null,
      session_ended: false,
    });
    expect(e.account?.available).toBe(false);
  });

  it('locks the gate while signed out where sign-in is required', () => {
    const e = new EphemeralStore();
    expect(e.accountGate).toBe('unknown');
    const out = {
      available: true,
      required: true,
      user: null,
      session_ended: false,
    };
    e.setAccount(out);
    expect(e.accountGate).toBe('locked');
    e.setAccount({
      ...out,
      user: { id: 'u', email: 'a@b.co', name: null, avatar_url: null },
    });
    expect(e.accountGate).toBe('open');
    // The session ended: the event closes it again.
    e.handle({
      type: 'account_status',
      status: { ...out, session_ended: true },
    });
    expect(e.accountGate).toBe('locked');
  });
});

describe('EphemeralStore space logs', () => {
  const line = (at: number, text: string) => ({
    kind: 'setup' as const,
    line: text,
    stderr: false,
    at,
  });

  it('buffers space_log lines per space and seeds from the tail', () => {
    const e = new EphemeralStore();
    e.handle({ type: 'space_log', space: 's1', line: line(3, 'live') });
    e.handle({ type: 'space_log', space: 's2', line: line(1, 'other') });
    e.seedSpaceLogs('s1', [line(1, 'a'), line(2, 'b')]);
    expect(e.spaceLogs.get('s1')?.map((l) => l.line)).toEqual([
      'a',
      'b',
      'live',
    ]);
    expect(e.spaceLogs.get('s2')).toHaveLength(1);
  });
});

describe('EphemeralStore app info', () => {
  it('marks the page with the build, for the per-build accent', () => {
    const e = new EphemeralStore();
    e.setAppInfo({ ...APP_INFO, build_env: 'staging' });
    expect(document.documentElement.dataset.build).toBe('staging');
    e.setAppInfo({ ...APP_INFO, build_env: 'production' });
    expect(document.documentElement.dataset.build).toBe('production');
  });
});

describe('EphemeralStore code intelligence suggestion', () => {
  const suggest = {
    type: 'code_intel_suggest' as const,
    project: 'github:acme/widget',
    language: 'cpp',
    server: 'clangd',
  };

  it('asks with Download and Not now', () => {
    const e = new EphemeralStore();
    e.handle(suggest);
    expect(e.toasts).toHaveLength(1);
    expect(e.toasts[0]).toMatchObject({
      level: 'info',
      text: 'C++: download clangd for exact references?',
    });
    expect(e.toasts[0]!.action?.label).toBe('Download');
    expect(e.toasts[0]!.secondary?.label).toBe('Not now');
  });

  it('Download installs, Not now declines, for that language', async () => {
    const t = new FakeTransport();
    setTransport(t);
    t.onCall('code_intel_install', () => null);
    t.onCall('code_intel_decline', () => null);
    const e = new EphemeralStore();
    e.handle(suggest);
    e.toasts[0]!.action!.onAction();
    e.toasts[0]!.secondary!.onAction();
    await Promise.resolve();
    expect(t.calls).toEqual([
      { cmd: 'code_intel_install', args: { language: 'cpp' } },
      { cmd: 'code_intel_decline', args: { language: 'cpp' } },
    ]);
  });

  it('a failed install does not throw out of the toast', async () => {
    const t = new FakeTransport();
    setTransport(t);
    t.onCall('code_intel_install', () => {
      throw { code: 'invalid', message: 'off' };
    });
    const e = new EphemeralStore();
    e.handle(suggest);
    expect(() => e.toasts[0]!.action!.onAction()).not.toThrow();
    await new Promise((r) => setTimeout(r, 0));
  });
});

describe('EphemeralStore agent text', () => {
  const delta = (run: string, text: string) =>
    ({ type: 'agent_delta', run, agent: 'claude', text }) as const;

  it('appends each piece to its own run', () => {
    const e = new EphemeralStore();
    e.handle(delta('r1', 'Hel'));
    e.handle(delta('r2', 'Other'));
    e.handle(delta('r1', 'lo'));
    expect(e.agentText.get('r1')).toBe('Hello');
    expect(e.agentText.get('r2')).toBe('Other');
  });

  it('a reader of one run reacts to that run only', () => {
    const e = new EphemeralStore();
    let runs = 0;
    const stop = autorun(() => {
      e.agentText.get('r1');
      runs += 1;
    });
    e.handle(delta('r2', 'a'));
    e.handle(delta('r2', 'b'));
    expect(runs).toBe(1);
    e.handle(delta('r1', 'c'));
    expect(runs).toBe(2);
    stop();
  });

  it('clearRun drops the text and log of that run only', () => {
    const e = new EphemeralStore();
    e.handle(delta('r1', 'a'));
    e.handle(delta('r2', 'b'));
    e.handle({ type: 'run_log', run: 'r1', agent: 'claude', line: 'tool: x' });
    e.clearRun('r1');
    expect(e.agentText.has('r1')).toBe(false);
    expect(e.runLog.has('r1')).toBe(false);
    expect(e.agentText.get('r2')).toBe('b');
  });
});
