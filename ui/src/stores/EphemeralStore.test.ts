import { autorun } from 'mobx';
import { describe, expect, it, vi } from 'vitest';
import type { AppInfo, TurnStep } from '../gen/contract';
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

  it('several languages become one prompt that answers for all of them', async () => {
    const t = new FakeTransport();
    setTransport(t);
    t.onCall('code_intel_install', () => null);
    t.onCall('code_intel_decline', () => null);
    const e = new EphemeralStore();
    e.handle(suggest);
    e.handle({
      ...suggest,
      language: 'tsx',
      server: 'typescript-language-server',
    });
    e.handle({ ...suggest, language: 'cpp' }); // asked again: not twice
    const last = e.toasts[e.toasts.length - 1]!;
    expect(last.group).toBe('code_intel_suggest');
    expect(last.text).toBe(
      'TSX and C++: download language servers for exact references?',
    );
    last.action!.onAction();
    await Promise.resolve();
    expect(t.calls).toEqual([
      { cmd: 'code_intel_install', args: { language: 'tsx' } },
      { cmd: 'code_intel_install', args: { language: 'cpp' } },
    ]);
    // Answered: the next language starts a fresh prompt of its own.
    e.handle({ ...suggest, language: 'rust', server: 'rust-analyzer' });
    expect(e.toasts[e.toasts.length - 1]!.text).toBe(
      'Rust: download rust-analyzer for exact references?',
    );
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
    e.flushDeltas();
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
    e.flushDeltas();
    expect(runs).toBe(1);
    e.handle(delta('r1', 'c'));
    e.flushDeltas();
    expect(runs).toBe(2);
    stop();
  });

  it('clearRun drops the text and log of that run only', () => {
    const e = new EphemeralStore();
    e.handle(delta('r1', 'a'));
    e.handle(delta('r2', 'b'));
    e.handle({ type: 'run_log', run: 'r1', agent: 'claude', line: 'tool: x' });
    e.clearRun('r1');
    e.flushDeltas();
    expect(e.agentText.has('r1')).toBe(false);
    expect(e.runLog.has('r1')).toBe(false);
    expect(e.agentText.get('r2')).toBe('b');
  });

  it('a burst shows once per frame, the first piece at once', () => {
    const frames: FrameRequestCallback[] = [];
    vi.stubGlobal('requestAnimationFrame', (cb: FrameRequestCallback) => {
      frames.push(cb);
      return frames.length;
    });
    try {
      const e = new EphemeralStore();
      let runs = 0;
      const stop = autorun(() => {
        e.agentText.get('r1');
        runs += 1;
      });
      e.handle(delta('r1', 'a'));
      expect(e.agentText.get('r1')).toBe('a');
      expect(runs).toBe(2);
      for (const piece of ['b', 'c', 'd']) e.handle(delta('r1', piece));
      expect(e.agentText.get('r1')).toBe('a');
      expect(runs).toBe(2);
      frames.shift()!(0);
      expect(e.agentText.get('r1')).toBe('abcd');
      expect(runs).toBe(3);
      stop();
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it('a hidden window, with no frames, still gets its text soon', () => {
    vi.useFakeTimers();
    vi.stubGlobal('requestAnimationFrame', () => 1);
    try {
      const e = new EphemeralStore();
      e.handle(delta('r1', 'a'));
      e.handle(delta('r1', 'b'));
      vi.advanceTimersByTime(100);
      expect(e.agentText.get('r1')).toBe('ab');
    } finally {
      vi.unstubAllGlobals();
      vi.useRealTimers();
    }
  });

  it('clearRun drops what still waits for a frame', () => {
    const e = new EphemeralStore();
    e.handle(delta('r1', 'a'));
    e.handle(delta('r1', 'b'));
    e.clearRun('r1');
    e.flushDeltas();
    expect(e.agentText.has('r1')).toBe(false);
  });
});

describe('EphemeralStore run steps', () => {
  const delta = (text: string, run = 'r1') =>
    ({ type: 'agent_delta', run, agent: 'claude', text }) as const;
  const log = (line: string, run = 'r1') =>
    ({ type: 'run_log', run, agent: 'claude', line }) as const;
  const step = (step: TurnStep, run = 'r1') =>
    ({ type: 'run_step', run, step }) as const;
  const tool = (id: string, parent?: string): TurnStep => ({
    kind: 'tool',
    name: 'Bash',
    target: id,
    ok: null,
    id,
    parent,
  });
  const done = (id: string, ok: boolean | null, summary?: string) =>
    ({
      type: 'run_step_done',
      run: 'r1',
      id,
      ok,
      summary: summary ?? null,
    }) as const;
  const textBreak = { type: 'run_text_break', run: 'r1' } as const;

  it('a background launch stays marked when its report comes', () => {
    const e = new EphemeralStore();
    e.handle(step(tool('ag')));
    e.handle({ ...done('ag', true), background: true });
    e.handle(done('ag', null, 'Found it.'));
    expect(e.runSteps.get('r1')).toEqual([
      { ...tool('ag'), ok: true, summary: 'Found it.', background: true },
    ]);
  });

  it('keeps text, tools and thinking in the order they came', () => {
    const e = new EphemeralStore();
    e.handle(delta('Look'));
    e.handle(delta('ing.'));
    e.handle(step(tool('a')));
    e.handle(done('a', true));
    e.handle(step({ kind: 'thinking', text: 'Where next?' }));
    e.handle(delta('Found it.'));
    e.handle(delta('Done', 'r2'));
    e.flushDeltas();
    expect(e.runSteps.get('r1')).toEqual([
      { kind: 'text', text: 'Looking.' },
      { ...tool('a'), ok: true, summary: undefined },
      { kind: 'thinking', text: 'Where next?' },
      { kind: 'text', text: 'Found it.' },
    ]);
    expect(e.runSteps.get('r2')).toEqual([{ kind: 'text', text: 'Done' }]);
  });

  it('a block boundary starts a new row, with no tool between them', () => {
    const e = new EphemeralStore();
    e.handle(textBreak);
    e.handle(delta('One.'));
    e.handle(delta(' Still one.'));
    e.handle(textBreak);
    e.handle(delta('Two.'));
    e.flushDeltas();
    expect(e.runSteps.get('r1')).toEqual([
      { kind: 'text', text: 'One. Still one.' },
      { kind: 'text', text: 'Two.' },
    ]);
    expect(e.agentText.get('r1')).toBe('One. Still one.Two.');
  });

  it('a result marks its own call, whichever is newest', () => {
    const e = new EphemeralStore();
    e.handle(step(tool('a')));
    e.handle(step(tool('b')));
    e.handle(delta('Still going.'));
    e.handle(done('a', false));
    e.flushDeltas();
    const [a, b] = e.runSteps.get('r1')!;
    expect(a).toMatchObject({ id: 'a', ok: false });
    expect(b).toMatchObject({ id: 'b', ok: null });
  });

  it('a result can bring what it found; a report alone leaves it running', () => {
    const e = new EphemeralStore();
    e.handle(step(tool('a')));
    e.handle(done('a', null, 'Found it.'));
    expect(e.runSteps.get('r1')![0]).toMatchObject({
      ok: null,
      summary: 'Found it.',
    });
    e.handle(done('a', true));
    expect(e.runSteps.get('r1')![0]).toMatchObject({
      ok: true,
      summary: 'Found it.',
    });
  });

  it('a result with no call is no step', () => {
    const e = new EphemeralStore();
    e.handle(done('gone', true));
    e.handle(log('stalled'));
    expect(e.runSteps.get('r1') ?? []).toEqual([]);
    expect(e.runLog.get('r1')).toEqual(['stalled']);
  });

  it('log lines stay the tool log and make no steps', () => {
    const e = new EphemeralStore();
    e.handle({ type: 'run_stalled', run: 'r1', agent: 'claude' });
    e.handle(log('tool: Bash'));
    e.handle(log('tool result ok=true'));
    expect(e.runSteps.get('r1') ?? []).toEqual([]);
    expect(e.runLog.get('r1')).toEqual(['tool: Bash', 'tool result ok=true']);
    expect(e.stalled.has('r1')).toBe(false);
  });

  it('a step still counts as output', () => {
    const e = new EphemeralStore();
    e.handle({ type: 'run_stalled', run: 'r1', agent: 'claude' });
    e.handle(step({ kind: 'thinking', text: 'hmm' }));
    expect(e.stalled.has('r1')).toBe(false);
  });

  it('keeps agentText and the tool log as they were', () => {
    const e = new EphemeralStore();
    e.handle(delta('A'));
    e.handle(log('tool: Bash · a'));
    e.handle(step(tool('a')));
    e.handle(delta('B'));
    e.flushDeltas();
    expect(e.agentText.get('r1')).toBe('AB');
    expect(e.runLog.get('r1')).toEqual(['tool: Bash · a']);
  });

  it('text goes on past what a subagent did meanwhile', () => {
    const e = new EphemeralStore();
    e.handle(delta('Half'));
    e.handle(step(tool('c', 'ag')));
    e.handle(delta(' done.'));
    e.flushDeltas();
    expect(e.runSteps.get('r1')).toEqual([
      { kind: 'text', text: 'Half done.' },
      tool('c', 'ag'),
    ]);
  });

  it('stays bounded, the newest kept; a subagent gives way to itself', () => {
    const e = new EphemeralStore();
    for (let i = 0; i < 510; i += 1) e.handle(step(tool(`${i}`)));
    const steps = e.runSteps.get('r1')!;
    expect(steps).toHaveLength(500);
    expect(steps.at(-1)).toMatchObject({ target: '509' });

    const f = new EphemeralStore();
    f.handle(step(tool('mine')));
    for (let i = 0; i < 120; i += 1) f.handle(step(tool(`c${i}`, 'ag')));
    const kept = f.runSteps.get('r1')!;
    expect(kept).toHaveLength(101);
    expect(kept[0]).toMatchObject({ id: 'mine' });
    expect(kept[1]).toMatchObject({ id: 'c20' });
  });

  it('a reader of one run reacts to that run only', () => {
    const e = new EphemeralStore();
    let runs = 0;
    const stop = autorun(() => {
      e.runSteps.get('r1');
      runs += 1;
    });
    e.handle(delta('a', 'r2'));
    e.flushDeltas();
    expect(runs).toBe(1);
    e.handle(delta('b'));
    e.flushDeltas();
    expect(runs).toBe(2);
    stop();
  });

  it('clearRun drops them', () => {
    const e = new EphemeralStore();
    e.handle(delta('a'));
    e.handle(textBreak);
    e.clearRun('r1');
    expect(e.runSteps.has('r1')).toBe(false);
    e.handle(delta('b'));
    e.handle(delta('c'));
    e.flushDeltas();
    expect(e.runSteps.get('r1')).toEqual([{ kind: 'text', text: 'bc' }]);
  });
});
