import { afterEach, describe, expect, it } from 'vitest';
import { click, mount, type Mounted } from '../../test/dom';
import type { MarkedLine } from '../diff';
import { InterdiffPanel } from './InterdiffPanel';

let m: Mounted | null = null;

afterEach(() => {
  m?.unmount();
  m = null;
});

const previous: MarkedLine[] = [
  { text: 'let a = 1;', marks: [[8, 9]] },
  { text: 'plain old', marks: [] },
];
const now: MarkedLine[] = [
  { text: 'let a = 22;', marks: [[8, 10]] },
  { text: '', marks: [] },
];

const button = (label: string): HTMLElement => {
  const b = [...m!.host.querySelectorAll('button')].find((x) =>
    x.textContent?.includes(label),
  );
  if (!b) throw new Error(`no button ${label}`);
  return b;
};
const rows = (): HTMLElement[] => [
  ...m!.host.querySelectorAll<HTMLElement>('.font-mono > div'),
];
const marked = (row: HTMLElement): HTMLElement[] => [
  ...row.querySelectorAll<HTMLElement>('span[style]'),
];

describe('InterdiffPanel', () => {
  it('shows the "now" lines by default', async () => {
    m = await mount(<InterdiffPanel previous={previous} now={now} />);
    expect(rows()[0]!.textContent).toBe('let a = 22;');
    expect(m.host.textContent).not.toContain('plain old');
  });

  it('toggles to "Previously" and back to "Now"', async () => {
    m = await mount(<InterdiffPanel previous={previous} now={now} />);
    await click(button('Previously'));
    expect(rows().map((r) => r.textContent)).toEqual([
      'let a = 1;',
      'plain old',
    ]);
    await click(button('Now'));
    expect(rows()[0]!.textContent).toBe('let a = 22;');
  });

  it('highlights marked words with the side-specific CSS var; the rest stays plain', async () => {
    m = await mount(<InterdiffPanel previous={previous} now={now} />);
    const nowSpans = marked(rows()[0]!);
    expect(nowSpans).toHaveLength(1);
    expect(nowSpans[0]!.textContent).toBe('22');
    expect(nowSpans[0]!.getAttribute('style')).toContain('--diff-add-word');
    expect(nowSpans[0]!.getAttribute('style')).not.toContain('--diff-del-word');
    // Text outside the mark is a bare text node, not a span.
    expect(rows()[0]!.firstChild?.textContent).toBe('let a = ');
    expect(rows()[0]!.firstChild?.nodeType).toBe(Node.TEXT_NODE);

    await click(button('Previously'));
    const prevSpans = marked(rows()[0]!);
    expect(prevSpans).toHaveLength(1);
    expect(prevSpans[0]!.textContent).toBe('1');
    expect(prevSpans[0]!.getAttribute('style')).toContain('--diff-del-word');
    // Trailing text after the mark is kept.
    expect(rows()[0]!.lastChild?.textContent).toBe(';');
  });

  it('renders an empty line as a single non-breaking space', async () => {
    m = await mount(<InterdiffPanel previous={previous} now={now} />);
    expect(rows()[1]!.textContent).toBe('\u00a0');
    expect(marked(rows()[1]!)).toHaveLength(0);
  });

  it('renders lines without marks as plain text', async () => {
    m = await mount(<InterdiffPanel previous={previous} now={now} />);
    await click(button('Previously'));
    expect(rows()[1]!.textContent).toBe('plain old');
    expect(marked(rows()[1]!)).toHaveLength(0);
  });
});
