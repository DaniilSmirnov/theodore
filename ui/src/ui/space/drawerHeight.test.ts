import { describe, expect, it } from 'vitest';
import { CHAT_LOG_MIN, DRAWER_MIN, drawerHeight } from './drawerHeight';

describe('drawerHeight', () => {
  it('gives way at 900×640 so the chat log keeps ~200 px', () => {
    // 640 tall, ~56 px of header: 584 px for the panes row and the drawer;
    // the composer takes 140 of the row.
    const h = drawerHeight(300, 584, 140);
    expect(h).toBe(584 - 140 - CHAT_LOG_MIN);
    expect(584 - h - 140).toBeGreaterThanOrEqual(CHAT_LOG_MIN);
  });

  it('keeps the height it wants in a tall window', () => {
    expect(drawerHeight(300, 1000, 140)).toBe(300);
  });

  it('shrinks with the window, and grows back', () => {
    expect(drawerHeight(300, 500, 140)).toBe(160);
    expect(drawerHeight(300, 900, 140)).toBe(300);
  });

  it('never goes below a usable strip', () => {
    expect(drawerHeight(300, 300, 140)).toBe(DRAWER_MIN);
  });
});
