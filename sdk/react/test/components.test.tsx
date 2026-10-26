// @vitest-environment jsdom
// Every component state (docs/12 §3): empty, loading older, sending,
// failed, streaming, stopped, recalled, the AI mark (kept under a
// replacement), cards, feedback, typing, connection, texts and RTL.

import { afterEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { Observable } from '@agentimhq/client-web';
import { AgentIM, Chat, ConversationList, MemberList, MessageList, type MessageProps } from '../src/index.js';

afterEach(cleanup);

class FakeView extends Observable<any[]> {
  hasOlder = false;
  olderEnd?: string;
  loadOlder = vi.fn(async () => {
    this.hasOlder = false;
    this.olderEnd = 'start';
    this.set([{ ...msg('old', 'older one'), order: 0 }, ...this.get()]);
    return false;
  });
  constructor(list: any[]) {
    super(list);
  }
}

function fake(messages: any[] = [], opts: { hasOlder?: boolean } = {}) {
  const view = new FakeView(messages);
  view.hasOlder = !!opts.hasOlder;
  const listeners = new Map<string, Set<(e: any) => void>>();
  const client = {
    connection: new Observable<any>({ state: 'connected' }),
    conversations: new Observable<any[]>([]),
    messages: vi.fn(() => view),
    send: vi.fn(async () => ({ client_msg_id: 'x', key: 'local:x' })),
    edit: vi.fn(async () => ({})),
    recall: vi.fn(async () => ({})),
    retry: vi.fn(async () => ({})),
    discard: vi.fn(async () => ({})),
    feedback: vi.fn(async () => ({})),
    respond: vi.fn(async () => ({})),
    read: vi.fn(async () => ({})),
    call: vi.fn(async () => ({})),
    on: (t: string, f: (e: any) => void) => {
      let s = listeners.get(t);
      if (!s) listeners.set(t, (s = new Set()));
      s.add(f);
      return () => s!.delete(f);
    },
    emit: (t: string, e: any) => act(() => listeners.get(t)?.forEach((f) => f(e))),
  };
  return { client, view };
}

let n = 0;
function msg(key: string, content: string, extra: any = {}) {
  return { key, msg_id: key, conv_id: 'c1', content, status: 'sent', state: 'committed', sender_id: 'u_bob', sender_name: 'Bob', sender_type: 'human', version: 1, created_seq: ++n, order: n, ...extra };
}

function chat(client: any, props: any = {}) {
  return render(
    <AgentIM client={client} me="u_me" {...props}>
      <Chat convId="c1" />
    </AgentIM>,
  );
}

describe('MessageList', () => {
  it('shows the empty state, then follows the observable', () => {
    const { client, view } = fake();
    chat(client);
    expect(screen.getByText('No messages yet')).toBeTruthy();
    act(() => view.set([msg('a', 'hello')]));
    expect(screen.getByText('hello')).toBeTruthy();
    expect(screen.queryByText('No messages yet')).toBeNull();
    expect(screen.getByText('This is the beginning of the conversation')).toBeTruthy();
  });

  it('loads older messages with the button, then shows the start', async () => {
    const { client, view } = fake([msg('a', 'new')], { hasOlder: true });
    chat(client);
    await act(async () => fireEvent.click(screen.getByText('Load earlier messages')));
    expect(view.loadOlder).toHaveBeenCalledTimes(1);
    expect(screen.getByText('older one')).toBeTruthy();
    expect(screen.getByText('This is the beginning of the conversation')).toBeTruthy();
  });

  it('says when older messages are gone (retention)', () => {
    const { client, view } = fake([msg('a', 'x')]);
    view.olderEnd = 'retention';
    chat(client);
    expect(screen.getByText('Earlier messages are no longer available')).toBeTruthy();
  });

  it('shows sending, and failed with retry and delete', () => {
    const { client } = fake([
      msg('local:1', 'on its way', { status: 'sending', client_msg_id: '1', sender_id: 'u_me' }),
      msg('local:2', 'refused', { status: 'failed', client_msg_id: '2', sender_id: 'u_me' }),
    ]);
    chat(client);
    expect(screen.getByText('Sending…')).toBeTruthy();
    expect(screen.getByRole('alert').textContent).toContain('Not delivered');
    fireEvent.click(screen.getByText('Retry'));
    expect(client.retry).toHaveBeenCalledWith('c1', '2');
    fireEvent.click(screen.getByText('Delete'));
    expect(client.discard).toHaveBeenCalledWith('c1', '2');
  });

  it('streams, then completes, or shows stopped and failed', () => {
    const { client, view } = fake([msg('s', '', { state: 'streaming', streamed: 'Hel', sender_type: 'agent' })]);
    chat(client);
    expect(screen.getByText('Hel')).toBeTruthy();
    expect(screen.getByText('Writing…')).toBeTruthy();
    expect(document.querySelector('[aria-busy="true"]')).toBeTruthy();
    act(() => view.set([msg('s', 'Hello there', { state: 'complete', sender_type: 'agent' })]));
    expect(screen.getByText('Hello there')).toBeTruthy();
    expect(screen.queryByText('Writing…')).toBeNull();
    act(() => view.set([msg('s', 'Hel', { state: 'cancelled', sender_type: 'agent' }), msg('t', 'He', { state: 'failed', sender_type: 'agent' })]));
    expect(screen.getByText('Stopped')).toBeTruthy();
    expect(screen.getByText('Couldn’t finish')).toBeTruthy();
  });

  it('shows recalled and edited', () => {
    const { client } = fake([
      msg('r', 'secret', { state: 'recalled' }),
      msg('e', 'fixed', { version: 2 }),
      msg('n', 'new', { version: 1 }),
      msg('s', 'streamed', { version: 3, state: 'complete', sender_type: 'agent' }),
    ]);
    chat(client);
    expect(screen.queryByText('secret')).toBeNull();
    expect(screen.getByText('This message was deleted')).toBeTruthy();
    expect(screen.getAllByText('Edited')).toHaveLength(1);
  });

  it('marks AI messages, also when the message component is replaced', () => {
    const { client } = fake([msg('h', 'from a person'), msg('g', 'from an agent', { sender_type: 'agent' })]);
    const Mine = ({ message }: MessageProps) => <p>custom: {message.content}</p>;
    chat(client, { components: { Message: Mine } });
    expect(screen.getByText('custom: from an agent')).toBeTruthy();
    const marks = screen.getAllByLabelText('AI agent');
    expect(marks).toHaveLength(1);
    expect(marks[0].closest('[data-sender-type]')?.getAttribute('data-sender-type')).toBe('agent');
  });

  it('asks for feedback on agent messages only', () => {
    const { client } = fake([msg('h', 'person'), msg('g', 'agent', { sender_type: 'agent' })]);
    chat(client);
    const up = screen.getAllByLabelText('Helpful');
    expect(up).toHaveLength(1);
    fireEvent.click(up[0]);
    expect(client.feedback).toHaveBeenCalledWith('c1', 'g', 'up');
    fireEvent.click(screen.getByLabelText('Not helpful'));
    expect(client.feedback).toHaveBeenCalledWith('c1', 'g', 'down');
  });

  it('asks for feedback on a finished stream, not while it streams', () => {
    const { client, view } = fake([msg('s', '', { state: 'streaming', streamed: 'Hel', sender_type: 'agent', sender_id: 'u_bot' })]);
    chat(client);
    expect(screen.queryByLabelText('Helpful')).toBeNull();
    act(() => view.set([msg('s', 'Hello', { state: 'complete', sender_type: 'agent', sender_id: 'u_bot' })]));
    expect(screen.getByLabelText('Helpful')).toBeTruthy();
  });

  it('shows sources as links that open safely', () => {
    const { client } = fake([msg('s', 'see', { sender_type: 'agent', envelope: { sources: [{ title: 'Policy', url: 'https://example.com/p' }] } })]);
    chat(client);
    const a = screen.getByText('Policy').closest('a')!;
    expect(a.getAttribute('href')).toBe('https://example.com/p');
    expect(a.getAttribute('rel')).toContain('noopener');
  });

  it('renders cards and sends button and form answers', () => {
    const payload = {
      title: 'Refund?',
      blocks: [
        { type: 'text', text: 'Order 42' },
        { type: 'buttons', id: 'b', options: [{ id: 'yes', label: 'Approve', style: 'primary' }, { id: 'no', label: 'Reject', style: 'danger' }] },
        { type: 'form', id: 'f', submit: 'Send', fields: [{ id: 'why', label: 'Why', type: 'text' }, { id: 'n', label: 'Amount', type: 'number' }, { id: 'ok', label: 'Sure', type: 'checkbox' }, { id: 'pick', label: 'Pick', type: 'select', options: ['red', 'blue'] }] },
      ],
    };
    const { client } = fake([msg('k', 'Refund? (card)', { kind: 'agentim.card', payload, sender_type: 'agent' })]);
    chat(client);
    expect(screen.getByText('Refund?')).toBeTruthy();
    fireEvent.click(screen.getByText('Approve'));
    expect(client.respond).toHaveBeenCalledWith('c1', 'k', 'b', { action: 'yes' });
    fireEvent.change(screen.getByLabelText('Why'), { target: { value: 'broken' } });
    fireEvent.change(screen.getByLabelText('Amount'), { target: { value: '12' } });
    fireEvent.click(screen.getByLabelText('Sure'));
    fireEvent.click(screen.getByText('Send', { selector: '.aim-card button' }));
    expect(client.respond).toHaveBeenCalledWith('c1', 'k', 'f', { values: { why: 'broken', n: 12, ok: true, pick: 'red' } });
  });

  it('marks the newest message read', () => {
    const { client } = fake([msg('a', 'one', { created_seq: 7 })]);
    chat(client);
    expect(client.read).toHaveBeenCalledWith('c1', 7);
  });

  it('lets text choose its direction (RTL) and replaces texts', () => {
    const { client } = fake([msg('a', 'مرحبا')]);
    chat(client, { strings: { placeholder: 'اكتب رسالة', send: 'إرسال' } });
    expect(screen.getByText('مرحبا').getAttribute('dir')).toBe('auto');
    expect(screen.getByPlaceholderText('اكتب رسالة')).toBeTruthy();
    expect(screen.getByLabelText('إرسال').tagName).toBe('BUTTON');
  });
});

describe('Composer', () => {
  it('sends on Enter, not on Shift+Enter, not when empty, and says I am typing', async () => {
    const { client } = fake();
    chat(client);
    const box = screen.getByLabelText('Message') as HTMLTextAreaElement;
    fireEvent.keyDown(box, { key: 'Enter' });
    expect(client.send).not.toHaveBeenCalled();
    fireEvent.change(box, { target: { value: 'hi there' } });
    expect(client.call).toHaveBeenCalledWith('signal', { conv_id: 'c1', kind: 'typing' });
    fireEvent.keyDown(box, { key: 'Enter', shiftKey: true });
    expect(client.send).not.toHaveBeenCalled();
    await act(async () => fireEvent.keyDown(box, { key: 'Enter' }));
    expect(client.send).toHaveBeenCalledWith('c1', 'hi there', undefined);
    expect(box.value).toBe('');
    // Typing again within 3 s does not signal again.
    fireEvent.change(box, { target: { value: 'x' } });
    expect(client.call).toHaveBeenCalledTimes(1);
  });
});

describe('TypingIndicator and ConnectionBanner', () => {
  it('shows who types, for a while', () => {
    vi.useFakeTimers();
    try {
      const { client } = fake();
      chat(client);
      client.emit('signal', { conv_id: 'other', kind: 'typing', from: 'u_x' });
      expect(screen.queryByText(/is typing/)).toBeNull();
      client.emit('signal', { conv_id: 'c1', kind: 'typing', from: 'u_bob' });
      expect(screen.getByText('u_bob is typing…')).toBeTruthy();
      act(() => vi.advanceTimersByTime(7000));
      expect(screen.queryByText(/is typing/)).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it('says what the connection is doing', () => {
    const { client } = fake();
    chat(client);
    expect(screen.queryByRole('status', { name: '' })?.textContent ?? '').not.toContain('Offline');
    act(() => client.connection.set({ state: 'suspended' }));
    expect(screen.getByText('Waiting for network…')).toBeTruthy();
    act(() => client.connection.set({ state: 'failed', reason: 'unauthenticated' }));
    expect(screen.getByText('Can’t connect')).toBeTruthy();
    act(() => client.connection.set({ state: 'connected' }));
    expect(screen.queryByText('Can’t connect')).toBeNull();
  });
});

describe('ConversationList', () => {
  it('shows empty, then the list with unread counts, and selects', () => {
    const { client } = fake();
    const onSelect = vi.fn();
    render(
      <AgentIM client={client as any}>
        <ConversationList selected="c2" onSelect={onSelect} />
      </AgentIM>,
    );
    expect(screen.getByText('No conversations yet')).toBeTruthy();
    act(() =>
      client.conversations.set([
        { conv_id: 'c1', title: 'Support', unread: 3, last_message: { content: 'hello' } },
        { conv_id: 'c2', title: 'Team', unread: 150 },
      ]),
    );
    expect(screen.getByText('3')).toBeTruthy();
    expect(screen.getByText('99+')).toBeTruthy();
    expect(screen.getByText('Team').closest('button')?.getAttribute('aria-current')).toBe('true');
    fireEvent.click(screen.getByText('Support'));
    expect(onSelect).toHaveBeenCalledWith('c1');
  });

  it('needs a provider', () => {
    const err = vi.spyOn(console, 'error').mockImplementation(() => {});
    expect(() => render(<MessageList convId="c1" />)).toThrow(/AgentIMProvider/);
    err.mockRestore();
  });
});

describe('MemberList and HandoffNotice', () => {
  it('lists members with roles and the AI mark on agents', () => {
    const { client } = fake();
    render(
      <AgentIM client={client as any}>
        <MemberList convId="c1" />
      </AgentIM>,
    );
    expect(screen.queryAllByRole('listitem')).toHaveLength(0);
    act(() =>
      client.conversations.set([
        { conv_id: 'c1', members: [{ user_id: 'u1', name: 'Alice', kind: 'human', role: 'owner' }, { user_id: 'u2', name: 'Helper', kind: 'agent', role: 'member' }] },
      ]),
    );
    expect(screen.getAllByRole('listitem')).toHaveLength(2);
    expect(screen.getByText('Owner')).toBeTruthy();
    const mark = screen.getByLabelText('AI agent');
    expect(mark.closest('li')?.textContent).toContain('Helper');
  });

  it('says when the agent hands over and when it is back, for this conversation only', () => {
    const { client } = fake();
    chat(client);
    client.emit('event', { event: { conv_id: 'other', type: 'agent.state', data: { state: 'taken_over', handoff: true } } });
    expect(screen.queryByText(/connected to a person/)).toBeNull();
    client.emit('event', { event: { conv_id: 'c1', type: 'agent.state', data: { state: 'taken_over', handoff: true, note: 'Refund over 100' } } });
    expect(screen.getByText('You’re being connected to a person')).toBeTruthy();
    expect(screen.getByText('Refund over 100')).toBeTruthy();
    // A takeover that is not a handoff (a person took over) says nothing new.
    client.emit('event', { event: { conv_id: 'c1', type: 'agent.state', data: { state: 'tripped' } } });
    expect(screen.getByText('You’re being connected to a person')).toBeTruthy();
    client.emit('event', { event: { conv_id: 'c1', type: 'agent.state', data: { state: 'active', handback: true } } });
    expect(screen.getByText('The assistant is back')).toBeTruthy();
  });
});

describe('Runs, days, unread and scrolling (docs/12 §3.1)', () => {
  const now = Date.parse('2026-10-26T20:00:00Z');
  const t = (iso: string) => Date.parse(iso);
  const opts = { locale: 'en-US', timeZone: 'UTC', now: () => now };
  const nbsp = (s: string | null | undefined) => (s ?? '').replace(/ /g, ' ');

  it('groups a sender’s messages, with name and avatar once and the time on the last', () => {
    const { client } = fake([
      msg('a', 'one', { created_at: t('2026-10-25T10:00:00Z') }),
      msg('b', 'two', { created_at: t('2026-10-25T10:02:00Z') }),
      msg('c', 'three', { created_at: t('2026-10-26T09:15:00Z') }),
      msg('d', 'mine', { created_at: t('2026-10-26T09:16:00Z'), sender_id: 'u_me', sender_name: 'Me' }),
    ]);
    chat(client, opts);
    const separators = screen.getAllByRole('separator').map((s) => s.textContent);
    expect(separators).toEqual(['Yesterday', 'Today']);
    // Bob twice (two runs), never me.
    expect(document.querySelectorAll('.aim-sender')).toHaveLength(2);
    expect(document.querySelectorAll('.aim-avatar')).toHaveLength(2);
    expect([...document.querySelectorAll('.aim-avatar')].map((a) => a.textContent)).toEqual(['B', 'B']);
    const times = [...document.querySelectorAll('.aim-time')].map((x) => nbsp(x.textContent));
    expect(times).toEqual(['10:02 AM', '9:15 AM', '9:16 AM']);
    const rows = [...document.querySelectorAll('.aim-row')];
    expect(rows.map((r) => `${r.hasAttribute('data-first') ? 'F' : '-'}${r.hasAttribute('data-last') ? 'L' : '-'}`)).toEqual(['F-', '-L', 'FL', 'FL']);
    // My last message says it was sent.
    expect(rows[3].querySelector('.aim-sent')?.textContent).toBe('Sent');
  });

  it('puts “New messages” above the first unread one, and keeps it there after reading', () => {
    const { client, view } = fake([msg('a', 'old'), msg('b', 'mine', { sender_id: 'u_me' }), msg('c', 'new 1'), msg('d', 'new 2')]);
    client.conversations.set([{ conv_id: 'c1', unread: 2 }]);
    chat(client);
    const line = screen.getByRole('separator', { name: 'New messages' });
    expect(line.nextElementSibling?.textContent).toContain('new 1');
    act(() => client.conversations.set([{ conv_id: 'c1', unread: 0 }]));
    act(() => view.set([...view.get(), msg('e', 'newer')]));
    expect(screen.getByRole('separator', { name: 'New messages' }).nextElementSibling?.textContent).toContain('new 1');
  });

  it('shows no unread line when nothing was unread', () => {
    const { client } = fake([msg('a', 'x')]);
    client.conversations.set([{ conv_id: 'c1', unread: 0 }]);
    chat(client);
    expect(screen.queryByRole('separator', { name: 'New messages' })).toBeNull();
  });

  it('offers a way back down that counts what arrived, and follows my own message', () => {
    const { client, view } = fake([msg('a', 'one'), msg('b', 'two')]);
    chat(client);
    const log = screen.getByRole('log');
    let top = 0;
    Object.defineProperty(log, 'scrollHeight', { configurable: true, get: () => 1000 });
    Object.defineProperty(log, 'clientHeight', { configurable: true, get: () => 300 });
    Object.defineProperty(log, 'scrollTop', { configurable: true, get: () => top, set: (v) => (top = v) });
    const scrollTo = vi.fn(({ top: y }) => (top = y));
    (log as any).scrollTo = scrollTo;
    expect(screen.queryByText('Jump to latest')).toBeNull();
    top = 100;
    fireEvent.scroll(log);
    expect(screen.getByText('Jump to latest')).toBeTruthy();
    act(() => view.set([...view.get(), msg('c', 'three')]));
    expect(top).toBe(100);
    expect(screen.getByText('1 new message')).toBeTruthy();
    act(() => view.set([...view.get(), msg('d', 'four')]));
    expect(screen.getByText('2 new messages')).toBeTruthy();
    fireEvent.click(screen.getByText('2 new messages'));
    expect(scrollTo).toHaveBeenCalledWith({ top: 1000, behavior: 'smooth' });
    fireEvent.scroll(log);
    expect(screen.queryByText(/new message|Jump to latest/)).toBeNull();
    // Scrolled up again, my own message brings me back down.
    top = 0;
    fireEvent.scroll(log);
    act(() => view.set([...view.get(), msg('local:z', 'from me', { sender_id: 'u_me', status: 'sending', client_msg_id: 'z' })]));
    expect(top).toBe(1000);
    expect(screen.queryByText('Jump to latest')).toBeNull();
  });

  it('keeps the place when older messages load above', async () => {
    const { client, view } = fake([msg('a', 'new')], { hasOlder: true });
    chat(client);
    const log = screen.getByRole('log');
    let top = 0;
    let height = 500;
    Object.defineProperty(log, 'scrollHeight', { configurable: true, get: () => height });
    Object.defineProperty(log, 'clientHeight', { configurable: true, get: () => 300 });
    Object.defineProperty(log, 'scrollTop', { configurable: true, get: () => top, set: (v) => (top = v) });
    top = 50;
    fireEvent.scroll(log);
    height = 800;
    await act(async () => view.loadOlder());
    // 300 px were added above: the same messages stay in view.
    expect(top).toBe(350);
  });
});

describe('ConversationList details', () => {
  const now = Date.parse('2026-10-26T20:00:00Z');
  it('shows previews (mine as “You:”), times, and filters by search', () => {
    const { client } = fake();
    client.conversations.set([
      { conv_id: 'c1', title: 'Support', unread: 1, last_message: { content: 'Where is my order?', sender_id: 'u_bob', created_at: Date.parse('2026-10-26T14:05:00Z') } },
      { conv_id: 'c2', title: 'Design team', unread: 0, last_message: { content: 'Shipped it', sender_id: 'u_me', created_at: Date.parse('2026-10-22T09:00:00Z') } },
      { conv_id: 'c3', title: 'Old', unread: 0, last_message: { content: 'gone', sender_id: 'u_bob', state: 'recalled', created_at: Date.parse('2025-01-02T09:00:00Z') } },
    ]);
    render(
      <AgentIM client={client as any} me="u_me" locale="en-US" timeZone="UTC" now={() => now}>
        <ConversationList onSelect={() => {}} />
      </AgentIM>,
    );
    expect(screen.getByText('You: Shipped it')).toBeTruthy();
    expect(screen.getByText('This message was deleted')).toBeTruthy();
    const times = [...document.querySelectorAll('.aim-conv-time')].map((x) => (x.textContent ?? '').replace(/ /g, ' '));
    expect(times).toEqual(['2:05 PM', 'Thu', '1/2/25']);
    expect(screen.getByText('Support').closest('button')?.hasAttribute('data-unread')).toBe(true);
    const box = screen.getByLabelText('Search');
    fireEvent.change(box, { target: { value: 'ORDER' } });
    expect(screen.getAllByRole('button').map((b) => b.textContent)).toEqual([expect.stringContaining('Support')]);
    fireEvent.change(box, { target: { value: 'design' } });
    expect(screen.getByText('Design team')).toBeTruthy();
    expect(screen.queryByText('Support')).toBeNull();
    fireEvent.change(box, { target: { value: 'nothing like this' } });
    expect(screen.getByText('No matches')).toBeTruthy();
  });

  it('can do without the search box', () => {
    const { client } = fake();
    client.conversations.set([{ conv_id: 'c1', title: 'A', unread: 0 }]);
    render(
      <AgentIM client={client as any}>
        <ConversationList onSelect={() => {}} search={false} />
      </AgentIM>,
    );
    expect(screen.queryByLabelText('Search')).toBeNull();
  });
});

describe('Typing by name and the send button', () => {
  it('names who types from the members', () => {
    const { client } = fake();
    client.conversations.set([{ conv_id: 'c1', members: [{ user_id: 'u_bob', name: 'Bob' }, { user_id: 'u_amy', name: 'Amy' }] }]);
    chat(client);
    client.emit('signal', { conv_id: 'c1', kind: 'typing', from: 'u_bob' });
    expect(screen.getByText('Bob is typing…')).toBeTruthy();
    client.emit('signal', { conv_id: 'c1', kind: 'typing', from: 'u_amy' });
    expect(screen.getByText('Bob and Amy are typing…')).toBeTruthy();
    client.emit('signal', { conv_id: 'c1', kind: 'typing', from: 'u_zed' });
    expect(screen.getByText('Several people are typing…')).toBeTruthy();
  });

  it('enables the send button only with text, and sends on click', async () => {
    const { client } = fake();
    chat(client);
    const send = screen.getByLabelText('Send') as HTMLButtonElement;
    expect(send.disabled).toBe(true);
    fireEvent.change(screen.getByLabelText('Message'), { target: { value: '   ' } });
    expect(send.disabled).toBe(true);
    fireEvent.change(screen.getByLabelText('Message'), { target: { value: 'Hey' } });
    expect(send.disabled).toBe(false);
    await act(async () => fireEvent.click(send));
    expect(client.send).toHaveBeenCalledWith('c1', 'Hey', undefined);
  });

  it('shows the chat header when given a title', () => {
    const { client } = fake();
    render(
      <AgentIM client={client as any}>
        <Chat convId="c1" title="Acme Support" subtitle="We typically reply in a few minutes" />
      </AgentIM>,
    );
    expect(screen.getByText('Acme Support')).toBeTruthy();
    expect(screen.getByText('We typically reply in a few minutes')).toBeTruthy();
  });
});

describe('Notices, own-message actions and answered cards', () => {
  it('merges notices into the timeline by seq and ends runs around them', () => {
    const { client } = fake([msg('a', 'one', { created_seq: 1 }), msg('b', 'two', { created_seq: 3 }), msg('c', 'three', { created_seq: undefined })]);
    const notices = [
      { key: 'late', seq: 99, content: 'after everything' },
      { key: 'j', seq: 2, content: 'Amy joined', tone: 'info' as const },
      { key: 'w', seq: 3.5, content: 'Whisper to Aria', tone: 'private' as const },
    ];
    render(
      <AgentIM client={client as any} me="u_me">
        <MessageList convId="c1" notices={notices} />
      </AgentIM>,
    );
    const order = [...document.querySelectorAll('.aim-row, .aim-notice')].map((e) => e.textContent?.replace(/\s+/g, ' ').trim());
    expect(order.map((t) => t?.match(/one|two|three|Amy joined|Whisper to Aria|after everything/)?.[0])).toEqual(['one', 'Amy joined', 'two', 'Whisper to Aria', 'after everything', 'three']);
    expect(screen.getByText('Whisper to Aria').getAttribute('data-tone')).toBe('private');
    // Bob's messages are split by the notices: each starts a run with his name.
    expect(document.querySelectorAll('.aim-sender')).toHaveLength(3);
  });

  it('edits my sent message inline: Enter saves, Escape cancels, unchanged does nothing', async () => {
    const { client } = fake([msg('a', 'helo', { sender_id: 'u_me' }), msg('b', 'theirs')]);
    chat(client);
    expect(screen.getAllByText('Edit')).toHaveLength(1);
    fireEvent.click(screen.getByText('Edit'));
    const box = screen.getByLabelText('Edit message') as HTMLTextAreaElement;
    expect(box.value).toBe('helo');
    fireEvent.keyDown(box, { key: 'Escape' });
    expect(screen.queryByLabelText('Edit message')).toBeNull();
    fireEvent.click(screen.getByText('Edit'));
    fireEvent.click(screen.getByText('Save'));
    expect(client.edit).not.toHaveBeenCalled();
    fireEvent.click(screen.getByText('Edit'));
    fireEvent.change(screen.getByLabelText('Edit message'), { target: { value: 'hello' } });
    await act(async () => fireEvent.keyDown(screen.getByLabelText('Edit message'), { key: 'Enter' }));
    expect(client.edit).toHaveBeenCalledWith('c1', { msgId: 'a' }, 'hello');
    expect(screen.queryByLabelText('Edit message')).toBeNull();
  });

  it('deletes only my sent messages, and never a card, a stream or one still sending', () => {
    const { client } = fake([
      msg('a', 'mine', { sender_id: 'u_me' }),
      msg('k', 'card', { sender_id: 'u_me', kind: 'agentim.card', payload: { blocks: [] } }),
      msg('local:s', 'sending', { sender_id: 'u_me', status: 'sending', client_msg_id: 's' }),
      msg('b', 'theirs'),
    ]);
    chat(client);
    // Delete on "mine" and on my card (deletable, not editable); none on the others.
    expect(screen.getAllByText('Edit')).toHaveLength(1);
    const del = screen.getAllByText('Delete');
    expect(del).toHaveLength(2);
    fireEvent.click(del[0]);
    expect(client.recall).toHaveBeenCalledWith('c1', { msgId: 'a' });
  });

  it('offers Stop while an agent streams, and stops it', () => {
    const { client, view } = fake([msg('s', '', { state: 'streaming', streamed: 'Thinking', sender_type: 'agent', sender_id: 'u_bot' })]);
    chat(client);
    fireEvent.click(screen.getByText('Stop'));
    expect(client.call).toHaveBeenCalledWith('stream_cancel', { msg_id: 's' });
    act(() => view.set([msg('s', 'Thinking', { state: 'cancelled', sender_type: 'agent', sender_id: 'u_bot' })]));
    expect(screen.queryByText('Stop')).toBeNull();
    expect(screen.getByText('Stopped')).toBeTruthy();
  });

  it('disables a card once answered here, and my own cards', () => {
    const payload = { blocks: [{ type: 'buttons', id: 'b', options: [{ id: 'y', label: 'Yes' }] }, { type: 'form', id: 'f', fields: [{ id: 'x', label: 'X', type: 'text' }] }] };
    const { client } = fake([msg('k', 'card', { kind: 'agentim.card', payload, sender_type: 'agent', sender_id: 'u_bot' }), msg('m', 'my card', { kind: 'agentim.card', payload, sender_id: 'u_me' })]);
    chat(client);
    const yes = screen.getAllByText('Yes') as HTMLButtonElement[];
    expect(yes.map((b) => b.disabled)).toEqual([false, true]);
    fireEvent.click(yes[0]);
    expect(client.respond).toHaveBeenCalledTimes(1);
    expect((screen.getAllByText('Yes')[0] as HTMLButtonElement).disabled).toBe(true);
    // The form is another block: still open, then done once sent.
    const submit = screen.getAllByText('Submit') as HTMLButtonElement[];
    expect(submit.map((b) => b.disabled)).toEqual([false, true]);
    fireEvent.click(submit[0]);
    expect(client.respond).toHaveBeenLastCalledWith('c1', 'k', 'f', { values: { x: '' } });
    expect((screen.getAllByText('Submit')[0] as HTMLButtonElement).disabled).toBe(true);
  });
});

describe('Focus', () => {
  it('focuses the open conversation, and nothing once it closes', () => {
    const { client } = fake([msg('a', 'x')]);
    const focus = vi.fn(async () => ({}));
    (client as any).focus = focus;
    const r = render(
      <AgentIM client={client as any}>
        <MessageList convId="c1" />
      </AgentIM>,
    );
    expect(focus).toHaveBeenLastCalledWith('c1');
    r.rerender(
      <AgentIM client={client as any}>
        <MessageList convId="c2" />
      </AgentIM>,
    );
    expect(focus.mock.calls.map((c: any[]) => c[0])).toEqual(['c1', null, 'c2']);
    r.unmount();
    expect(focus).toHaveBeenLastCalledWith(null);
  });
});
