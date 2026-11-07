/** A turn's rows, its fold above a finished reply, its footer, and the live
 *  chat bubble's two looks: a space's timeline and the AI panel's text and
 *  tools. */
import { act, type ReactNode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { click, mount, type Mounted } from '../test/dom';
import type { ChatMessage as Message, TurnStep } from '../gen/contract';
import { setTransport } from '../core/transport';
import { RootStore } from '../stores/RootStore';
import { RootStoreProvider } from '../stores/context';
import { FakeTransport } from '../test/fakeTransport';
import { ChatLive, ChatMessage } from './ChatTurn';
import { FileTypeIcon } from './FileTypeIcon';
import {
  LiveTimer,
  TimelineRows,
  TurnActivity,
  TurnFiles,
  formatDuration,
} from './TurnTimeline';

let m: Mounted | null = null;
let store: RootStore;

beforeEach(() => {
  setTransport(new FakeTransport());
  store = new RootStore();
});

afterEach(() => {
  m?.unmount();
  m = null;
});

async function render(node: ReactNode) {
  m?.unmount();
  m = await mount(<RootStoreProvider store={store}>{node}</RootStoreProvider>);
}

const tool = (name: string, target: string | null, ok: boolean | null) =>
  ({ kind: 'tool', name, target, ok }) as const;

const steps: TurnStep[] = [
  { kind: 'text', text: 'Reading the config.' },
  tool('Read', 'config/load.rs', true),
  { kind: 'thinking', text: 'Where\nis it?' },
  { kind: 'text', text: 'Now running it.' },
  tool('Bash', 'cargo test', false),
  tool('ask you', null, null),
];

const rowsOf = (root: Element) =>
  [...root.querySelector('.turn-timeline')!.children].map(
    (r) => r.classList[0],
  );

describe('TimelineRows', () => {
  it('lists every step flat while running, in the order given', async () => {
    await render(<TimelineRows steps={steps} running />);
    expect(rowsOf(m!.host)).toEqual([
      'turn-text',
      'turn-tool',
      'turn-thinking',
      'turn-text',
      'turn-tool',
      'turn-tool',
    ]);
  });

  it('a tool row: its kind, a human label, its target, a failure', async () => {
    await render(<TimelineRows steps={steps} running />);
    const [read, bash, ask] = [...m!.host.querySelectorAll('.turn-tool')];
    expect(read!.getAttribute('data-kind')).toBe('read');
    expect(read!.textContent).toBe('Readconfig/load.rs');
    expect(read!.querySelector('[title="config/load.rs"]')).not.toBeNull();
    expect(read!.querySelector('[data-file-type="rs"]')).not.toBeNull();
    expect(read!.querySelector('.turn-tool-failed')).toBeNull();

    expect(bash!.getAttribute('data-kind')).toBe('run');
    expect(bash!.textContent).toContain('Run');
    expect(bash!.querySelector('.turn-tool-failed')).not.toBeNull();

    expect(ask!.textContent).toBe('Waiting for your answer');
    expect(ask!.querySelector('.turn-tool-failed')).toBeNull();
  });

  it('thinking is a muted one-liner until a click opens it as markdown, and stays open', async () => {
    const thought: TurnStep[] = [
      { kind: 'thinking', text: 'Where\nis **it**?' },
    ];
    await render(<TimelineRows steps={thought} running />);
    const line = () =>
      m!.host.querySelector<HTMLButtonElement>('.turn-thinking-line')!;
    expect(line().getAttribute('aria-expanded')).toBe('false');
    expect(m!.host.querySelector('.turn-thinking-text')?.textContent).toBe(
      'Where is **it**?',
    );
    expect(m!.host.querySelector('.turn-thinking-body')).toBeNull();
    await click(line());
    expect(line().getAttribute('aria-expanded')).toBe('true');
    expect(
      m!.host.querySelector('.turn-thinking-body strong')?.textContent,
    ).toBe('it');
    // The same thought once it is stored, or scrolled back to.
    await render(<TimelineRows steps={thought} keptAs="r1" />);
    expect(m!.host.querySelector('.turn-thinking-body')).not.toBeNull();
    await click(line());
  });

  it('every call still going spins, not only the newest', async () => {
    const spinners = () =>
      m!.host.querySelectorAll(
        '[data-slot="spinner"], .spinner, [role="progressbar"]',
      ).length;
    const parallel: TurnStep[] = [
      tool('Bash', 'a', null),
      tool('Bash', 'b', true),
      tool('Bash', 'c', null),
    ];
    await render(<TimelineRows steps={parallel} running />);
    expect(spinners()).toBe(2);
    await render(<TimelineRows steps={parallel} />);
    expect(spinners()).toBe(0);
  });

  it('skips a text step with nothing to show yet', async () => {
    await render(
      <TimelineRows
        steps={[tool('Read', null, true), { kind: 'text', text: '\n\n' }]}
      />,
    );
    expect(rowsOf(m!.host)).toEqual(['turn-tool']);
  });
});

describe('a group of calls', () => {
  it('opens on the chevron, row by row', async () => {
    await render(
      <TimelineRows
        keptAs="group-reply"
        steps={[tool('Read', 'a.rs', true), tool('Read', 'b.rs', true)]}
      />,
    );
    const line = m!.host.querySelector('.turn-group-line')!;
    expect(line.textContent).toBe('Read 2 files');
    expect(m!.host.querySelector('.turn-tool')).toBeNull();
    await click(line);
    expect(m!.host.querySelectorAll('.turn-tool')).toHaveLength(2);
  });
});

describe('a subagent', () => {
  const agent: TurnStep = {
    kind: 'tool',
    name: 'Agent',
    target: null,
    ok: null,
    id: 'ag',
    detail: 'Look around',
  };
  const child = (target: string): TurnStep => ({
    ...tool('Read', target, true),
    parent: 'ag',
  });

  it('is a row with its description, and its latest step while it runs', async () => {
    await render(
      <TimelineRows steps={[agent, child('a.rs'), child('b.rs')]} running />,
    );
    const row = m!.host.querySelector('.turn-agent')!;
    expect(rowsOf(m!.host)).toEqual(['turn-agent']);
    expect(row.textContent).toContain('Look around');
    expect(row.querySelector('.turn-agent-note')?.textContent).toBe(
      'Read b.rs',
    );
    expect(row.querySelector('[role="progressbar"], .spinner')).not.toBeNull();
  });

  it('opens on its own steps, and says what it reported when done', async () => {
    const finished: TurnStep[] = [
      { ...agent, id: 'ag2', ok: true, summary: 'Found it.' },
      { ...child('a.rs'), parent: 'ag2' },
    ];
    await render(<TimelineRows steps={finished} />);
    const row = m!.host.querySelector('.turn-agent')!;
    expect(row.querySelector('.turn-agent-note')?.textContent).toBe(
      'Found it.',
    );
    expect(m!.host.querySelector('.turn-agent-steps')).toBeNull();
    await click(row.querySelector('.turn-agent-line'));
    expect(row.querySelectorAll('.turn-agent-steps .turn-tool')).toHaveLength(
      1,
    );
    expect(row.querySelector('.turn-agent-summary')?.textContent).toBe(
      'Found it.',
    );
  });

  it('with nothing reported is a plain row', async () => {
    await render(<TimelineRows steps={[{ ...agent, id: 'ag3', ok: true }]} />);
    expect(m!.host.querySelector('button.turn-agent-line')).toBeNull();
    expect(m!.host.textContent).toContain('Look around');
  });

  it('launched in the background says so, not what the CLI wrote', async () => {
    const launched: TurnStep = {
      ...agent,
      id: 'bg',
      ok: true,
      background: true,
    };
    await render(<TimelineRows steps={[launched]} running />);
    const row = m!.host.querySelector('.turn-agent')!;
    expect(row.querySelector('.turn-agent-note')?.textContent).toBe(
      'running in background',
    );
    expect(row.querySelector('[role="progressbar"], .spinner')).not.toBeNull();
  });

  it('launched in the background and left behind by its turn does not spin', async () => {
    const launched: TurnStep = {
      ...agent,
      id: 'bg2',
      ok: true,
      background: true,
    };
    await render(<TimelineRows steps={[launched]} />);
    const row = m!.host.querySelector('.turn-agent')!;
    expect(row.querySelector('.turn-agent-note')?.textContent).toBe(
      'ran in background',
    );
    expect(row.querySelector('[role="progressbar"], .spinner')).toBeNull();
  });

  it('launched in the background shows its report once it comes', async () => {
    const reported: TurnStep = {
      ...agent,
      id: 'bg3',
      ok: true,
      background: true,
      summary: 'Heading is Foo.',
    };
    await render(<TimelineRows steps={[reported]} running />);
    const row = m!.host.querySelector('.turn-agent')!;
    expect(row.querySelector('.turn-agent-note')?.textContent).toBe(
      'Heading is Foo.',
    );
    expect(row.querySelector('[role="progressbar"], .spinner')).toBeNull();
  });
});

describe('a finished reply', () => {
  const reply = (over: Partial<Message> = {}): Message => ({
    id: 'a1',
    pr: 'space:s1',
    author: { kind: 'agent', agent: 'claude' },
    text: 'All done.',
    refs: [],
    at: 1_700_000_000_000,
    activity: [
      { kind: 'text', text: 'Reading.' },
      tool('Bash', 'one', true),
      tool('Edit', 'a.rs', true),
      { kind: 'text', text: 'Now running it.' },
      tool('Bash', 'cargo test', false),
    ],
    duration_ms: 72_000,
    ...over,
  });

  it('folds what’s between what stays, above the reply, with no "Worked" line', async () => {
    await render(<ChatMessage m={reply()} />);
    const line = m!.host.querySelector('.turn-fold-line')!;
    expect(line.textContent).toBe('2 tool calls, 2 messages');
    expect(
      [...line.querySelectorAll('[data-icon]')].map((i) =>
        i.getAttribute('data-icon'),
      ),
    ).toEqual(['run', 'edit']);
    expect(m!.host.textContent).not.toContain('Worked');
    // Closed: nothing inside is rendered. The failed one stays out here.
    expect(m!.host.querySelector('.turn-fold-rows')).toBeNull();
    expect(m!.host.querySelectorAll('.turn-tool')).toHaveLength(1);
    expect(m!.host.querySelector('.turn-tool-failed')).not.toBeNull();
    // Above the reply's text.
    const order = line.compareDocumentPosition(
      [...m!.host.querySelectorAll('p')].find(
        (p) => p.textContent === 'All done.',
      )!,
    );
    expect(order & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it('the footer: how long and when, then copy and votes, then its files', async () => {
    await render(
      <ChatMessage
        m={reply({
          model: 'opus',
          changed_files: [{ path: 'src/a.rs', additions: 3, deletions: 1 }],
        })}
      />,
    );
    const footer = m!.host.querySelector('.turn-footer')!;
    expect(footer.textContent).toContain('1m 12s');
    const row = footer.parentElement!;
    const actions = row.querySelector('.message-actions')!;
    const files = row.querySelector('.turn-files')!;
    expect(
      footer.compareDocumentPosition(actions) &
        Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    expect(
      actions.compareDocumentPosition(files) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    // On hover: the model and start → end.
    const title = footer.getAttribute('title')!;
    expect(title).toMatch(/^opus · .+ → .+$/);
  });

  it('opens on a click, and stays open across a remount; another reply stays closed', async () => {
    const many = [
      tool('Bash', 'a', true),
      tool('Bash', 'b', true),
      tool('Bash', 'c', true),
    ];
    await render(<TurnActivity reply="kept-open" steps={many} />);
    await click(m!.host.querySelector('.turn-fold-line'));
    expect(m!.host.querySelector('.turn-fold-rows')).not.toBeNull();

    await render(<TurnActivity reply="kept-open" steps={many} />);
    expect(m!.host.querySelector('.turn-fold-rows')).not.toBeNull();

    await render(<TurnActivity reply="never-opened" steps={many} />);
    expect(m!.host.querySelector('.turn-fold-rows')).toBeNull();
  });

  it('a stopped one keeps all it wrote in order, next to why it ended', async () => {
    await render(
      <ChatMessage
        m={reply({
          failed: 'stopped',
          text: 'Reading.\n\nNow running it.',
          text_in_steps: true,
          activity: [
            { kind: 'text', text: 'Reading.' },
            tool('Bash', 'one', true),
            { kind: 'text', text: 'Now running it.' },
          ],
        })}
      />,
    );
    // A stop you asked for is no failure: a quiet note, no red card.
    expect(m!.host.querySelector('.agent-failed')).toBeNull();
    expect(m!.host.querySelector('.agent-stopped')?.textContent).toBe(
      'Stopped',
    );
    expect(rowsOf(m!.host)).toEqual(['turn-text', 'turn-tool', 'turn-text']);
    // Told once: the steps are the reply.
    const said = [...m!.host.querySelectorAll('p')].filter(
      (p) => p.textContent === 'Now running it.',
    );
    expect(said).toHaveLength(1);
    expect(m!.host.querySelector('.turn-footer')).not.toBeNull();
    expect(m!.host.querySelector('.message-actions')).toBeNull();
  });

  it('a stopped older row still shows its text as the reply', async () => {
    await render(
      <ChatMessage
        m={reply({
          failed: 'stopped',
          activity: [tool('Bash', 'one', true)],
        })}
      />,
    );
    expect(m!.host.textContent).toContain('All done.');
  });

  it('no final text: its last text stays, what’s before it may fold', async () => {
    await render(
      <ChatMessage
        m={reply({
          text: 'One.\n\nTwo.',
          text_in_steps: true,
          activity: [
            tool('Bash', 'a', true),
            tool('Bash', 'b', true),
            { kind: 'text', text: 'One.' },
            { kind: 'text', text: 'Two.' },
          ],
        })}
      />,
    );
    expect(m!.host.querySelector('.turn-fold-line')?.textContent).toBe(
      '2 tool calls, 1 message',
    );
    expect(m!.host.querySelector('.turn-text')?.textContent).toBe('Two.');
  });

  it('an AI panel reply, or an older row: no line, no footer', async () => {
    await render(
      <ChatMessage
        m={reply({ activity: undefined, duration_ms: undefined })}
      />,
    );
    expect(m!.host.querySelector('.turn-activity')).toBeNull();
    expect(m!.host.querySelector('.turn-footer')).toBeNull();
    expect(m!.host.querySelector('.turn-files')).toBeNull();
    expect(m!.host.querySelector('.message-actions')).not.toBeNull();
  });
});

describe('the files a turn changed', () => {
  const file = (path: string, additions: number, deletions: number) => ({
    path,
    additions,
    deletions,
  });
  const files = [
    file('src/a.rs', 3, 1),
    file('ui/b.tsx', 2, 0),
    file('Cargo.lock', 1, 0),
    file('c.md', 10, 2),
    file('d.png', 0, 0),
  ];

  it('shows up to three, each with its type and counts, then +N more with the rest', async () => {
    await render(<TurnFiles files={files} />);
    const chips = [...m!.host.querySelectorAll('.turn-file')];
    expect(chips).toHaveLength(3);
    expect(chips[0]!.textContent).toBe('a.rs+3−1');
    expect(chips[0]!.getAttribute('title')).toBe('src/a.rs');
    expect(chips[0]!.querySelector('[data-file-type="rs"]')).not.toBeNull();
    expect(chips[2]!.querySelector('[data-file-type="lock"]')).not.toBeNull();
    expect(m!.host.querySelector('.turn-files-more')?.textContent).toBe(
      '+2 more+10−2',
    );
  });

  it('no more: no +N', async () => {
    await render(<TurnFiles files={files.slice(0, 3)} />);
    expect(m!.host.querySelector('.turn-files-more')).toBeNull();
  });

  it('a chip opens its file, +N more the pane', async () => {
    const onOpen = vi.fn();
    await render(<TurnFiles files={files} onOpen={onOpen} />);
    await click(m!.host.querySelector('.turn-file[data-path="ui/b.tsx"]'));
    expect(onOpen).toHaveBeenLastCalledWith('ui/b.tsx');
    await click(m!.host.querySelector('.turn-files-more'));
    expect(onOpen).toHaveBeenLastCalledWith(null);
  });
});

describe('FileTypeIcon', () => {
  it('falls back to a plain file', async () => {
    await render(<FileTypeIcon path="notes.txt" />);
    expect(m!.host.querySelector('[data-file-type="file"]')).not.toBeNull();
    await render(<FileTypeIcon path="a.rs" />);
    expect(
      m!.host
        .querySelector('[data-file-type="rs"]')
        ?.getAttribute('data-glyph'),
    ).toBe('RS');
    // Nothing of it is the row's text.
    expect(m!.host.textContent).toBe('');
  });
});

describe('LiveTimer', () => {
  it('ticks through its node, re-rendering nothing', async () => {
    vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval', 'Date'] });
    try {
      const start = Date.now();
      let renders = 0;
      const Probe = () => {
        renders += 1;
        return <LiveTimer run="r1" since={start - 5_000} />;
      };
      m = await mount(<Probe />);
      const timer = m.host.querySelector('.turn-timer')!;
      expect(timer.textContent).toBe('5s');
      vi.advanceTimersByTime(3_000);
      expect(timer.textContent).toBe('8s');
      vi.advanceTimersByTime(60_000);
      expect(timer.textContent).toBe('1m 8s');
      expect(renders).toBe(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it('with no start to go by, counts from when it first showed, across a remount', async () => {
    vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval', 'Date'] });
    try {
      m = await mount(<LiveTimer run="r-first-seen" />);
      vi.advanceTimersByTime(4_000);
      m.unmount();
      m = await mount(<LiveTimer run="r-first-seen" />);
      expect(m.host.querySelector('.turn-timer')?.textContent).toBe('4s');
    } finally {
      vi.useRealTimers();
    }
  });
});

describe('ChatLive', () => {
  const delta = (text: string) =>
    act(() => {
      store.ephemeral.handle({
        type: 'agent_delta',
        run: 'r1',
        agent: 'claude',
        text,
      });
      store.ephemeral.flushDeltas();
    });
  const log = (line: string) =>
    act(() =>
      store.ephemeral.handle({
        type: 'run_log',
        run: 'r1',
        agent: 'claude',
        line,
      }),
    );
  const step = (step: TurnStep) =>
    act(() => store.ephemeral.handle({ type: 'run_step', run: 'r1', step }));
  const textBreak = () =>
    act(() => store.ephemeral.handle({ type: 'run_text_break', run: 'r1' }));
  const stall = () =>
    act(() =>
      store.ephemeral.handle({
        type: 'run_stalled',
        run: 'r1',
        agent: 'claude',
      }),
    );

  it('as a timeline: dots, then the rows growing in the order they came', async () => {
    await render(<ChatLive run="r1" agent="claude" timeline since={1} />);
    expect(
      m!.host.querySelector('[aria-label="Agent is thinking"]'),
    ).not.toBeNull();
    delta('Looking');
    delta(' around.');
    step({ ...tool('Read', 'src/a.rs', null), id: 't1' });
    act(() =>
      store.ephemeral.handle({
        type: 'run_step_done',
        run: 'r1',
        id: 't1',
        ok: true,
        summary: null,
      }),
    );
    delta('Found it.');
    expect(rowsOf(m!.host)).toEqual(['turn-text', 'turn-tool', 'turn-text']);
    expect(m!.host.querySelector('.turn-text')?.textContent).toBe(
      'Looking around.',
    );
    expect(
      m!.host.querySelector('[aria-label="Agent is thinking"]'),
    ).toBeNull();
    // The timer sits under the rows.
    const timer = m!.host.querySelector('.turn-timer')!;
    expect(
      m!.host.querySelector('.turn-timeline')!.compareDocumentPosition(timer) &
        Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
  });

  it('as a timeline: two text blocks are two paragraphs while they stream', async () => {
    await render(<ChatLive run="r1" agent="claude" timeline />);
    textBreak();
    delta('One');
    delta(' block.');
    textBreak();
    delta('Another.');
    expect(rowsOf(m!.host)).toEqual(['turn-text', 'turn-text']);
    expect(m!.host.querySelector('.turn-text')?.textContent).toBe('One block.');
  });

  it('as a timeline, stalled: the note goes under the text, not over it', async () => {
    await render(<ChatLive run="r1" agent="claude" timeline />);
    delta('Half an answer');
    stall();
    const text = m!.host.querySelector('.turn-text')!;
    const hint = m!.host.querySelector('.agent-stalled')!;
    expect(text.textContent).toBe('Half an answer');
    expect(
      text.compareDocumentPosition(hint) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
  });

  it('in the AI panel: text, then folded tools, the stall note under them, no timer', async () => {
    await render(<ChatLive run="r1" agent="claude" />);
    delta('Half an answer');
    log('tool: Read · src/a.rs');
    stall();
    expect(m!.host.querySelector('.live-text')?.textContent).toBe(
      'Half an answer',
    );
    expect(m!.host.querySelector('.turn-timeline')).toBeNull();
    expect(m!.host.querySelector('.runlog, [aria-expanded]')).not.toBeNull();
    expect(m!.host.querySelector('.agent-stalled')).not.toBeNull();
    expect(m!.host.querySelector('.turn-timer')).toBeNull();
  });

  it('stalled before any text: only the note, no dots', async () => {
    await render(<ChatLive run="r1" agent="claude" />);
    stall();
    expect(m!.host.querySelector('.agent-stalled')).not.toBeNull();
    expect(
      m!.host.querySelector('[aria-label="Agent is thinking"]'),
    ).toBeNull();
  });
});

describe('the lines', () => {
  it('formats a duration', () => {
    expect(formatDuration(400)).toBe('<1s');
    expect(formatDuration(42_000)).toBe('42s');
    expect(formatDuration(59_600)).toBe('1m 0s');
    expect(formatDuration(72_000)).toBe('1m 12s');
    expect(formatDuration(3_780_000)).toBe('1h 3m');
  });
});
