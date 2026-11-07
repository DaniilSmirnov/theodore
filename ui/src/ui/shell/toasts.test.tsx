import { act } from 'react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { Toast, toast } from '@heroui/react';
import { click, mount, settle, type Mounted } from '../../test/dom';
import { HARD_CLOSE_MS, showToast } from './toasts';

let m: Mounted | null = null;

afterEach(async () => {
  act(() => toast.clear());
  await act(async () => {
    await new Promise((r) => setTimeout(r, 400));
  });
  m?.unmount();
  m = null;
});

const button = (label: string) =>
  [...document.body.querySelectorAll('button')].find(
    (b) => b.textContent === label,
  );

describe('showToast', () => {
  it('a two-choice toast shows both and closes on either', async () => {
    m = await mount(<Toast.Provider />);
    const download = vi.fn();
    const later = vi.fn();
    act(() => {
      showToast({
        id: 1,
        level: 'info',
        text: 'C++: download clangd for exact references?',
        action: { label: 'Download', onAction: download },
        secondary: { label: 'Not now', onAction: later },
      });
    });
    await settle();
    expect(document.body.textContent).toContain('download clangd');
    expect(button('Download')).toBeDefined();
    await click(button('Not now'));
    expect(later).toHaveBeenCalledOnce();
    expect(download).not.toHaveBeenCalled();
    // Gone once its exit animation ends.
    await act(async () => {
      await new Promise((r) => setTimeout(r, 400));
    });
    expect(button('Not now')).toBeUndefined();
  });

  it('a one-action toast keeps the plain layout', async () => {
    m = await mount(<Toast.Provider />);
    const undo = vi.fn();
    act(() => {
      showToast({
        id: 2,
        level: 'info',
        text: 'Marked done',
        action: { label: 'Undo', onAction: undo },
      });
    });
    await settle();
    await click(button('Undo'));
    expect(undo).toHaveBeenCalledOnce();
    expect(button('Not now')).toBeUndefined();
  });

  it('an error is a danger toast, not a neutral one', async () => {
    m = await mount(<Toast.Provider />);
    act(() => {
      showToast({ id: 5, level: 'error', text: 'Push failed' });
      showToast({ id: 6, level: 'info', text: 'Pushed' });
    });
    await settle();
    const of = (text: string) =>
      [...document.body.querySelectorAll('[data-slot="toast"]')].find((t) =>
        t.textContent?.includes(text),
      );
    expect(of('Push failed')?.className).toMatch(/danger/);
    expect(of('Pushed')?.className).not.toMatch(/danger|warning/);
  });

  it('a toast closes in the end even while its timers are paused', async () => {
    m = await mount(<Toast.Provider />);
    vi.useFakeTimers();
    try {
      act(() => {
        showToast({ id: 7, level: 'info', text: 'Opened PR #15' });
        // A pointer resting on the stack pauses HeroUI's own countdown.
        toast.pauseAll();
      });
      act(() => vi.advanceTimersByTime(HARD_CLOSE_MS.info + 500));
    } finally {
      vi.useRealTimers();
    }
    await act(async () => {
      await new Promise((r) => setTimeout(r, 400));
    });
    expect(document.body.textContent).not.toContain('Opened PR #15');
  });

  it('a newer toast of a group closes the older one', async () => {
    m = await mount(<Toast.Provider />);
    const show = (id: number, text: string) =>
      act(() => {
        showToast({
          id,
          level: 'info',
          text,
          group: 'g',
          action: { label: 'Yes', onAction: vi.fn() },
          secondary: { label: 'Not now', onAction: vi.fn() },
        });
      });
    show(3, 'First language: download?');
    await settle();
    show(4, 'Both languages: download?');
    await act(async () => {
      await new Promise((r) => setTimeout(r, 400));
    });
    expect(document.body.textContent).not.toContain('First language');
    expect(document.body.textContent).toContain('Both languages');
  });
});
