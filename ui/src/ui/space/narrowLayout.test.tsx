/** What gives way in a narrow header or composer is decided by CSS
 *  container queries (`styles.css`), which jsdom doesn't run. These pin what
 *  jsdom can see: the markup the rules hang on is there, the words are
 *  unchanged when nothing collapses, and the rules still name those
 *  classes. */
import { afterEach, describe, expect, it } from 'vitest';
import { mount, type Mounted } from '../../test/dom';
import { BaseMovedText, baseMovedLabel } from './BaseUpdate';
import { ChipMenu, Composer } from './Composer';

// The stylesheet as text. The test runner skips CSS (a `?raw` import comes
// back empty), and the UI's types have no Node: read it through a typed
// dynamic import instead.
const fs = (await import('node' + ':fs')) as {
  readFileSync(path: string, encoding: 'utf8'): string;
};
// Vitest runs from `ui/`.
const css = fs.readFileSync('src/styles.css', 'utf8');

let m: Mounted | null = null;
afterEach(() => {
  m?.unmount();
  m = null;
});

describe('Base moved, in parts a narrow header can drop', () => {
  it('reads the same as the label when nothing is dropped', async () => {
    const idle = { status: 'idle' as const, updating: null };
    for (const behind of [1, 3]) {
      m = await mount(<BaseMovedText behind={behind} />);
      expect(m.host.textContent).toBe(
        baseMovedLabel(idle, { base_tip: 't', behind }),
      );
      m.unmount();
      m = null;
    }
  });

  it('leaves "2 behind" once the lead and the noun are hidden', async () => {
    m = await mount(<BaseMovedText behind={2} />);
    const copy = m.host.cloneNode(true) as HTMLElement;
    for (const part of ['.base-moved-lead', '.base-moved-long']) {
      copy.querySelector(part)!.remove();
    }
    expect(copy.textContent).toBe('2 behind');
  });
});

describe('the composer footer when a turn runs', () => {
  it('keeps Send now whole as words, with the icon for when they go', async () => {
    m = await mount(
      <Composer
        value="more"
        onChange={() => {}}
        onSubmit={() => {}}
        onSendNow={() => {}}
        placeholder="Ask"
        running
        queueWhileRunning
        chips={
          <ChipMenu
            ariaLabel="Mode"
            label="Edit"
            items={[]}
            selected=""
            onSelect={() => {}}
            keep
          />
        }
      />,
    );
    const now = m.host.querySelector('button[aria-label="Send now"]')!;
    expect(now.textContent).toBe('Send now');
    expect(now.querySelector('.send-now-icon')).not.toBeNull();
    expect(now.querySelector('.send-now-label')?.textContent).toBe('Send now');
    expect(m.host.querySelector('.composer-suffix')).not.toBeNull();
    const mode = m.host.querySelector('button[aria-label="Mode"]')!;
    expect(mode.className).toContain('shrink-0');
    expect(mode.className).not.toContain('min-w-0');
  });

  it('lets another chip shrink as before', async () => {
    m = await mount(
      <ChipMenu
        ariaLabel="Start from"
        label="A goal"
        items={[]}
        selected=""
        onSelect={() => {}}
      />,
    );
    const chip = m.host.querySelector('button[aria-label="Start from"]')!;
    expect(chip.className).toContain('min-w-0');
  });
});

describe('the container rules', () => {
  it('name the classes the header and composer carry', () => {
    for (const rule of [
      '.space-header {',
      '.space-header .btn-label {',
      '.space-header .base-moved-lead,',
      '.space-header .btn-label-narrow {',
      '.composer-suffix {',
      '.composer-suffix .send-now-label {',
      '.composer-suffix .agent-effort {',
    ]) {
      expect(css, rule).toContain(rule);
    }
    expect(css).toContain('@container space-header (max-width: 74rem)');
    // The base chip keeps its words longer than the other labels do.
    const baseRule = css.indexOf('.space-header .base-moved-lead,');
    const query = css.lastIndexOf('@container space-header', baseRule);
    expect(css.slice(query, baseRule)).toContain('(max-width: 56rem)');
    expect(css).toContain('@container composer-suffix (max-width: 32rem)');
  });
});
