import { afterEach, describe, expect, it } from 'vitest';
import { act, useState } from 'react';
import { click, mount, settle, typeInto, type Mounted } from '../../test/dom';
import type { ChatMessage, Space } from '../../gen/contract';
import { setTransport } from '../../core/transport';
import { FakeTransport } from '../../test/fakeTransport';
import { Composer } from './Composer';
import { QueuedMessages } from './QueuedMessages';

let m: Mounted | null = null;
afterEach(() => {
  m?.unmount();
  m = null;
});

const space = (status: Space['status']): Space =>
  ({
    id: 's1',
    status,
    agent: 'claude',
    mode: 'edit',
  }) as Space;

const queued = (id: string, held = false): ChatMessage => ({
  id,
  pr: 'space:s1',
  author: { kind: 'me' },
  text: `text ${id}`,
  refs: [],
  at: 1,
  queued: { seq: 1, held },
});

const button = (label: string) =>
  [...m!.host.querySelectorAll('button')].find(
    (b) => b.textContent === label,
  ) ?? null;

describe('the queue under a running turn', () => {
  it('offers Edit and Cancel while it waits, Send once held', async () => {
    const transport = new FakeTransport();
    setTransport(transport);
    transport.onCall('space_queued', () => null);
    m = await mount(
      <QueuedMessages space={space('running')} queued={[queued('q1')]} />,
    );
    expect(m.host.textContent).toContain('queued');
    expect(button('Send')).toBeNull();
    await click(button('Cancel'));
    await settle();
    expect(transport.calls.at(-1)).toMatchObject({
      cmd: 'space_queued',
      args: { req: { space: 's1', message: 'q1', action: { kind: 'cancel' } } },
    });
    m.unmount();

    m = await mount(
      <QueuedMessages space={space('idle')} queued={[queued('q2', true)]} />,
    );
    expect(m.host.textContent).toContain('held');
    expect(m.host.textContent).toContain('Send or drop');
    await click(button('Send'));
    await settle();
    expect(transport.calls.at(-1)).toMatchObject({
      args: { req: { message: 'q2', action: { kind: 'send' } } },
    });
  });

  it('edits the text in place', async () => {
    const transport = new FakeTransport();
    setTransport(transport);
    transport.onCall('space_queued', () => null);
    m = await mount(
      <QueuedMessages space={space('running')} queued={[queued('q1')]} />,
    );
    await click(button('Edit'));
    await settle();
    const area = m.host.querySelector('textarea')!;
    await typeInto(area, 'better words');
    await click(button('Save'));
    await settle();
    expect(transport.calls.at(-1)).toMatchObject({
      args: {
        req: { message: 'q1', action: { kind: 'edit', text: 'better words' } },
      },
    });
  });
});

describe('the composer while a turn runs', () => {
  it('⏎ queues, ⌘⏎ sends now', async () => {
    const log: string[] = [];
    function Box() {
      const [text, setText] = useState('more');
      return (
        <Composer
          value={text}
          onChange={setText}
          onSubmit={() => log.push(`queue:${text}`)}
          onSendNow={() => log.push(`now:${text}`)}
          placeholder="Ask"
          running
          queueWhileRunning
        />
      );
    }
    m = await mount(<Box />);
    const box = m.host.querySelector('textarea')!;
    const key = (init: KeyboardEventInit) =>
      act(async () => {
        box.dispatchEvent(
          new KeyboardEvent('keydown', {
            key: 'Enter',
            bubbles: true,
            cancelable: true,
            ...init,
          }),
        );
      });
    await key({});
    await key({ metaKey: true });
    expect(log).toEqual(['queue:more', 'now:more']);
    expect(m.host.querySelector('button[aria-label="Stop"]')).not.toBeNull();
    expect(m.host.querySelector('button[aria-label="Queue"]')).not.toBeNull();
    expect(
      m.host.querySelector('button[aria-label="Send now"]'),
    ).not.toBeNull();
  });
});

describe('the composer of a chat that runs side by side', () => {
  it('has no Queue or Send now while running', async () => {
    m = await mount(
      <Composer
        value="q"
        onChange={() => {}}
        onSubmit={() => {}}
        placeholder="Ask"
        running
        sendWhileRunning
      />,
    );
    expect(m.host.querySelector('button[aria-label="Stop"]')).not.toBeNull();
    expect(m.host.querySelector('button[aria-label="Queue"]')).toBeNull();
    expect(m.host.querySelector('button[aria-label="Send now"]')).toBeNull();
  });
});
