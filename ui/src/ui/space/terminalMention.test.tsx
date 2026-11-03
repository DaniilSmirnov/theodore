import { afterEach, describe, expect, it } from 'vitest';
import { useState } from 'react';
import { mount, settle, typeInto, type Mounted } from '../../test/dom';
import { Composer } from './Composer';
import { useAttachments, type Attachments } from '../attachments';
import { setTransport } from '../../core/transport';
import { FakeTransport } from '../../test/fakeTransport';
import { attachTerminalMention, stripTerminalMention } from './terminalMention';

describe('stripTerminalMention', () => {
  it('strips a confirmed mention and the space after it', () => {
    expect(stripTerminalMention('see @terminal for the error')).toEqual({
      text: 'see for the error',
      found: true,
    });
    expect(stripTerminalMention('@terminal fix it')).toEqual({
      text: 'fix it',
      found: true,
    });
  });

  it('leaves an unconfirmed or unrelated mention alone', () => {
    // No trailing whitespace yet: still being typed.
    expect(stripTerminalMention('@terminal')).toEqual({
      text: '@terminal',
      found: false,
    });
    // A longer word, not the exact token.
    expect(stripTerminalMention('@terminals are great')).toEqual({
      text: '@terminals are great',
      found: false,
    });
    expect(stripTerminalMention('nothing here')).toEqual({
      text: 'nothing here',
      found: false,
    });
  });
});

function fakeAttachments(): Attachments & { added: unknown[] } {
  const added: unknown[] = [];
  return {
    items: [],
    add: () => {},
    addSaved: (a) => added.push(a),
    remove: () => {},
    clear: () => {},
    ids: [],
    saved: [],
    busy: false,
    added,
  };
}

function decodeBase64(data: string): string {
  const bytes = Uint8Array.from(atob(data), (c) => c.charCodeAt(0));
  return new TextDecoder().decode(bytes);
}

describe('attachTerminalMention', () => {
  it('saves the tail as a text attachment and hands it to the caller', async () => {
    const transport = new FakeTransport();
    setTransport(transport);
    let savedReq: Record<string, unknown> | undefined;
    transport.onCall('space_terminal_mention', () => ({
      id: 't1',
      title: 'Terminal 1',
      lines: ['$ npm test', 'ok'],
    }));
    transport.onCall('save_attachment', (args) => {
      savedReq = args?.req as Record<string, unknown>;
      return {
        id: 'a1',
        name: 'Terminal 1.log',
        mime: 'text/plain',
        size: 10,
        path: '/data/attachments/a1/Terminal 1.log',
      };
    });
    const files = fakeAttachments();
    const toasts: string[] = [];
    await attachTerminalMention('s1', files, (t) => toasts.push(t));
    expect(savedReq).toMatchObject({ name: 'Terminal 1.log', mime: 'text/plain' });
    expect(decodeBase64(savedReq!.data as string)).toBe('$ npm test\nok');
    expect(files.added).toEqual([
      {
        id: 'a1',
        name: 'Terminal 1.log',
        mime: 'text/plain',
        size: 10,
        path: '/data/attachments/a1/Terminal 1.log',
      },
    ]);
    expect(toasts).toEqual([]);
  });

  it('toasts instead of attaching when the space has no terminal open', async () => {
    const transport = new FakeTransport();
    setTransport(transport);
    transport.onCall('space_terminal_mention', () => null);
    const files = fakeAttachments();
    const toasts: string[] = [];
    await attachTerminalMention('s1', files, (t) => toasts.push(t));
    expect(files.added).toEqual([]);
    expect(toasts).toEqual(['No terminal is open in this space']);
  });
});

describe('@terminal in the composer', () => {
  let m: Mounted | null = null;
  afterEach(() => {
    m?.unmount();
    m = null;
  });

  it('becomes a chip instead of literal text once confirmed with a space', async () => {
    const transport = new FakeTransport();
    setTransport(transport);
    transport.onCall('space_terminal_mention', () => ({
      id: 't1',
      title: 'Terminal 1',
      lines: ['$ npm test', 'ok'],
    }));
    transport.onCall('save_attachment', () => ({
      id: 'a1',
      name: 'Terminal 1.log',
      mime: 'text/plain',
      size: 10,
      path: '/data/attachments/a1/Terminal 1.log',
    }));

    function Box() {
      const [text, setText] = useState('');
      const files = useAttachments();
      const onChange = (next: string) => {
        const stripped = stripTerminalMention(next);
        if (stripped.found) {
          setText(stripped.text);
          void attachTerminalMention('s1', files, () => {});
          return;
        }
        setText(next);
      };
      return (
        <Composer
          value={text}
          onChange={onChange}
          onSubmit={() => {}}
          placeholder="Ask"
          attachments={files}
        />
      );
    }
    globalThis.URL.createObjectURL ??= () => 'blob:x';
    globalThis.URL.revokeObjectURL ??= () => {};
    m = await mount(<Box />);
    const box = m.host.querySelector('textarea')!;
    await typeInto(box, 'fix the bug @terminal ');
    for (let i = 0; i < 5; i++) await settle();
    expect(box.value).toBe('fix the bug ');
    expect(m.host.querySelector('.attachment-strip')?.textContent).toContain(
      'Terminal 1.log',
    );
  });
});
