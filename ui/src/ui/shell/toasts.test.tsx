import { act } from 'react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { Toast, toast } from '@heroui/react';
import { click, mount, settle, type Mounted } from '../../test/dom';
import { showToast } from './toasts';

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
});
