import { describe, expect, it, vi } from 'vitest';
import { TAB_CLOSE_MS, UiStore } from './UiStore';

describe('UiStore tabs', () => {
  it('opens every new tab at the end', () => {
    localStorage.clear();
    const ui = new UiStore();
    ui.go({ name: 'review', pr: 'github:o/r#1' });
    ui.go({ name: 'review', pr: 'github:o/r#2' });
    ui.go({ name: 'inbox' }); // existing tab: just switches
    ui.go({ name: 'review', pr: 'github:o/r#3' });
    ui.newTab();
    expect(ui.tabs.map((t) => (t.name === 'review' ? t.pr : t.name))).toEqual([
      'inbox',
      'github:o/r#1',
      'github:o/r#2',
      'github:o/r#3',
      'new',
    ]);
  });

  it('keeps a closing tab until its collapse animation ends', () => {
    vi.useFakeTimers();
    try {
      localStorage.clear();
      const ui = new UiStore();
      const pr = { name: 'review', pr: 'github:o/r#1' } as const;
      ui.go(pr);
      ui.requestCloseTab(pr);
      expect(ui.closingTabs).toEqual([JSON.stringify(pr)]);
      expect(ui.tabs).toHaveLength(2);
      vi.advanceTimersByTime(TAB_CLOSE_MS);
      expect(ui.closingTabs).toEqual([]);
      expect(ui.tabs).toEqual([{ name: 'inbox' }]);
      expect(ui.route).toEqual({ name: 'inbox' });
    } finally {
      vi.useRealTimers();
    }
  });

  it('navigates to the standalone skills route like any other view', () => {
    localStorage.clear();
    const ui = new UiStore();
    ui.go({ name: 'skills' });
    expect(ui.route).toEqual({ name: 'skills' });
  });
});

describe('UiStore modals', () => {
  it('carries targets through openModal for the modal to read', () => {
    localStorage.clear();
    const ui = new UiStore();
    ui.openModal('assign', ['task-1']);
    expect(ui.modal).toBe('assign');
    expect(ui.modalTargets).toEqual(['task-1']);
  });

  it('clears targets to the default empty list when none are given', () => {
    localStorage.clear();
    const ui = new UiStore();
    ui.openModal('snooze', ['pr-1']);
    ui.openModal('help');
    expect(ui.modalTargets).toEqual([]);
  });

  it('closing a modal does not touch its targets', () => {
    localStorage.clear();
    const ui = new UiStore();
    ui.openModal('assign', ['task-1']);
    ui.closeModal();
    expect(ui.modal).toBeNull();
    expect(ui.modalTargets).toEqual(['task-1']);
  });
});

describe('UiStore.resetOnboarding', () => {
  it('forgets onboarding was finished', () => {
    localStorage.clear();
    const ui = new UiStore();
    ui.finishOnboarding();
    expect(ui.onboarded).toBe(true);

    ui.resetOnboarding();
    expect(ui.onboarded).toBe(false);
    expect(localStorage.getItem('theodore-onboarded')).toBeNull();

    // A fresh load (e.g. app relaunch) should also see it reset.
    expect(new UiStore().onboarded).toBe(false);
  });
});

describe('UiStore aside drawer', () => {
  it('starts closed in drawer layouts and leaves the saved column state alone', () => {
    localStorage.clear();
    const ui = new UiStore();
    ui.toggleAside(true); // the wide-layout column is on
    ui.setAsideIsDrawer(true);
    expect(ui.asideShown).toBe(false);
    ui.toggleAside();
    expect(ui.asideShown).toBe(true);
    expect(ui.aside).toBe(true);
    ui.setAsideIsDrawer(false);
    expect(ui.asideDrawerOpen).toBe(false);
    expect(ui.asideShown).toBe(true);
  });
});

describe('UiStore back', () => {
  it('leaves Settings in one step after moving between its pages', () => {
    localStorage.clear();
    const ui = new UiStore();
    ui.go({ name: 'prs' });
    ui.go({ name: 'settings', page: 'agents' });
    ui.go({ name: 'settings', page: 'account' });
    ui.go({ name: 'settings', page: 'updates' });
    ui.goBack();
    expect(ui.route).toEqual({ name: 'prs' });
  });
});

describe('UiStore settings tab', () => {
  it('keeps one Settings tab, whichever page and tab you open it from', () => {
    localStorage.clear();
    const ui = new UiStore();
    ui.go({ name: 'prs' });
    ui.go({ name: 'settings', page: 'agents' });
    ui.go({ name: 'review', pr: 'github:o/r#1' });
    ui.go({ name: 'settings', page: 'about' });
    expect(ui.tabs.filter((t) => t.name === 'settings')).toEqual([
      { name: 'settings', page: 'about' },
    ]);
    expect(ui.route).toEqual({ name: 'settings', page: 'about' });
  });
});

describe('UiStore project settings', () => {
  const at = (section: 'general' | 'languages' | 'config') =>
    ({ name: 'project-settings', project: 'github:o/r', section }) as const;

  it('leaves in one step after moving between its sections', () => {
    localStorage.clear();
    const ui = new UiStore();
    ui.go({ name: 'prs' });
    ui.go(at('general'));
    ui.go(at('languages'));
    ui.go(at('config'));
    ui.goBack();
    expect(ui.route).toEqual({ name: 'prs' });
  });

  it('closes a removed project’s tabs', () => {
    localStorage.clear();
    const ui = new UiStore();
    ui.go({ name: 'prs' });
    ui.go(at('general'));
    ui.closeProjectSettings('github:o/r');
    expect(ui.tabs.some((t) => t.name === 'project-settings')).toBe(false);
    expect(ui.tabs.length).toBeGreaterThan(0);
  });
});
