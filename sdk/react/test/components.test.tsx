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
    expect(screen.getByText('This is the start of the conversation')).toBeTruthy();
  });

  it('loads older messages with the button, then shows the start', async () => {
    const { client, view } = fake([msg('a', 'new')], { hasOlder: true });
    chat(client);
    await act(async () => fireEvent.click(screen.getByText('Load earlier messages')));
    expect(view.loadOlder).toHaveBeenCalledTimes(1);
    expect(screen.getByText('older one')).toBeTruthy();
    expect(screen.getByText('This is the start of the conversation')).toBeTruthy();
  });

  it('says when older messages are gone (retention)', () => {
    const { client, view } = fake([msg('a', 'x')]);
    view.olderEnd = 'retention';
    chat(client);
    expect(screen.getByText('Earlier messages are no longer kept')).toBeTruthy();
  });

  it('shows sending, and failed with retry and delete', () => {
    const { client } = fake([
      msg('local:1', 'on its way', { status: 'sending', client_msg_id: '1', sender_id: 'u_me' }),
      msg('local:2', 'refused', { status: 'failed', client_msg_id: '2', sender_id: 'u_me' }),
    ]);
    chat(client);
    expect(screen.getByText('Sending…')).toBeTruthy();
    expect(screen.getByRole('alert').textContent).toContain('Not sent');
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
    expect(screen.getByText('Could not finish')).toBeTruthy();
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
    expect(screen.getByText('This message was recalled')).toBeTruthy();
    expect(screen.getAllByText('edited')).toHaveLength(1);
  });

  it('marks AI messages, also when the message component is replaced', () => {
    const { client } = fake([msg('h', 'from a person'), msg('g', 'from an agent', { sender_type: 'agent' })]);
    const Mine = ({ message }: MessageProps) => <p>custom: {message.content}</p>;
    chat(client, { components: { Message: Mine } });
    expect(screen.getByText('custom: from an agent')).toBeTruthy();
    const marks = screen.getAllByLabelText('Written by an AI agent');
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
    expect(screen.getByText('إرسال')).toBeTruthy();
  });
});

describe('Composer', () => {
  it('sends on Enter, not on Shift+Enter, not when empty, and says I am typing', async () => {
    const { client } = fake();
    chat(client);
    const box = screen.getByLabelText('Write a message') as HTMLTextAreaElement;
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
    expect(screen.getByText('Waiting for the network…')).toBeTruthy();
    act(() => client.connection.set({ state: 'failed', reason: 'unauthenticated' }));
    expect(screen.getByText('Cannot connect')).toBeTruthy();
    act(() => client.connection.set({ state: 'connected' }));
    expect(screen.queryByText('Cannot connect')).toBeNull();
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
    const mark = screen.getByLabelText('Written by an AI agent');
    expect(mark.closest('li')?.textContent).toContain('Helper');
  });

  it('says when the agent hands over and when it is back, for this conversation only', () => {
    const { client } = fake();
    chat(client);
    client.emit('event', { event: { conv_id: 'other', type: 'agent.state', data: { state: 'taken_over', handoff: true } } });
    expect(screen.queryByText(/handed this conversation/)).toBeNull();
    client.emit('event', { event: { conv_id: 'c1', type: 'agent.state', data: { state: 'taken_over', handoff: true, note: 'Refund over 100' } } });
    expect(screen.getByText('The assistant handed this conversation to a person')).toBeTruthy();
    expect(screen.getByText('Refund over 100')).toBeTruthy();
    // A takeover that is not a handoff (a person took over) says nothing new.
    client.emit('event', { event: { conv_id: 'c1', type: 'agent.state', data: { state: 'tripped' } } });
    expect(screen.getByText('The assistant handed this conversation to a person')).toBeTruthy();
    client.emit('event', { event: { conv_id: 'c1', type: 'agent.state', data: { state: 'active', handback: true } } });
    expect(screen.getByText('The assistant is back')).toBeTruthy();
  });
});
