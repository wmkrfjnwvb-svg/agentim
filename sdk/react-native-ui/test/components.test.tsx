// Every component state (docs/12 §3.5), with a fake client and a fixed
// clock, in the React Native test renderer: the same cases as the web
// components, plus what is native here (long press, switches, chips).

import { act, fireEvent, render, screen, within } from '@testing-library/react-native';
import { StyleSheet } from 'react-native';
import { Observable } from '@agentimhq/client-web/view';
import { AgentIM, Chat, ConversationList, MemberList, MessageList, dark, light, type MessageProps } from '../src/index';
import { Text } from 'react-native';

class FakeView extends Observable<any[]> {
  hasOlder = false;
  olderEnd?: string;
  loadOlder = jest.fn(async () => {
    this.hasOlder = false;
    this.olderEnd = 'start';
    this.set([{ ...msg('old', 'older one'), order: 0 }, ...this.get()]);
    return false;
  });
}

function fake(messages: any[] = [], opts: { hasOlder?: boolean } = {}) {
  const view = new FakeView(messages);
  view.hasOlder = !!opts.hasOlder;
  const listeners = new Map<string, Set<(e: any) => void>>();
  const client = {
    connection: new Observable<any>({ state: 'connected' }),
    conversations: new Observable<any[]>([]),
    messages: jest.fn(() => view),
    send: jest.fn(async () => ({ client_msg_id: 'x', key: 'local:x' })),
    edit: jest.fn(async () => ({})),
    recall: jest.fn(async () => ({})),
    retry: jest.fn(async () => ({})),
    discard: jest.fn(async () => ({})),
    feedback: jest.fn(async () => ({})),
    respond: jest.fn(async () => ({})),
    read: jest.fn(async () => ({})),
    call: jest.fn(async () => ({})),
    focus: jest.fn(async () => ({})),
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

const NOW = Date.parse('2026-10-26T20:00:00Z');
const when = { locale: 'en-US', timeZone: 'UTC', now: () => NOW };
const nbsp = (s: string) => s.replace(/\u202f/g, ' ');

function chat(client: any, props: any = {}) {
  return render(
    <AgentIM client={client} me="u_me" {...when} {...props}>
      <Chat convId="c1" />
    </AgentIM>,
  );
}

const flat = (style: any) => StyleSheet.flatten(style) ?? {};

describe('MessageList', () => {
  it('shows the empty state, then the messages and the start', async () => {
    const { client, view } = fake();
    await chat(client);
    expect(screen.getByText('No messages yet')).toBeTruthy();
    await act(() => view.set([msg('a', 'hello')]));
    expect(screen.getByText('hello')).toBeTruthy();
    expect(screen.getByText('This is the beginning of the conversation')).toBeTruthy();
    expect(client.focus).toHaveBeenCalledWith('c1');
  });

  it('loads older messages, then says it reached the start or the retention limit', async () => {
    const { client, view } = fake([msg('a', 'new')], { hasOlder: true });
    await chat(client);
    await fireEvent.press(screen.getByText('Load earlier messages'));
    expect(view.loadOlder).toHaveBeenCalled();
    expect(screen.getByText('older one')).toBeTruthy();
    const r = fake([msg('b', 'x')]);
    r.view.olderEnd = 'retention';
    await chat(r.client);
    expect(screen.getByText('Earlier messages are no longer available')).toBeTruthy();
  });

  it('groups runs with the name and avatar once, day lines and the time on the last', async () => {
    const t = (iso: string) => Date.parse(iso);
    const { client } = fake([
      msg('a', 'one', { created_at: t('2026-10-25T10:00:00Z') }),
      msg('b', 'two', { created_at: t('2026-10-25T10:02:00Z') }),
      msg('c', 'three', { created_at: t('2026-10-26T09:15:00Z') }),
      msg('d', 'mine', { created_at: t('2026-10-26T09:16:00Z'), sender_id: 'u_me', sender_name: 'Me' }),
    ]);
    await chat(client);
    expect(screen.getByText('Yesterday')).toBeTruthy();
    expect(screen.getByText('Today')).toBeTruthy();
    expect(screen.getAllByText('Bob')).toHaveLength(2);
    expect(screen.getAllByText('B', { includeHiddenElements: true })).toHaveLength(2);
    expect(screen.getAllByText(/\d:\d\d [AP]M|\d:\d\d\u202f[AP]M/).map((x) => nbsp(String(x.props.children))).reverse()).toEqual(['10:02 AM', '9:15 AM', '9:16 AM']);
    expect(screen.getByLabelText('Sent')).toBeTruthy();
    // The bubble corners tighten inside a run.
    expect(flat(screen.getByTestId('bubble-a').props.style).borderBottomLeftRadius).toBe(6);
    expect(flat(screen.getByTestId('bubble-b').props.style).borderTopLeftRadius).toBe(6);
    expect(flat(screen.getByTestId('bubble-b').props.style).borderBottomLeftRadius).toBe(18);
  });

  it('renders dates on Hermes, which has no Intl.RelativeTimeFormat', async () => {
    const R = (Intl as any).RelativeTimeFormat;
    (Intl as any).RelativeTimeFormat = undefined;
    try {
      const { client } = fake([msg('a', 'old', { created_at: Date.parse('2026-10-25T10:00:00Z') }), msg('b', 'new', { created_at: Date.parse('2026-10-26T10:00:00Z') })]);
      await chat(client);
      expect(screen.getByText('Yesterday')).toBeTruthy();
      expect(screen.getByText('Today')).toBeTruthy();
    } finally {
      (Intl as any).RelativeTimeFormat = R;
    }
  });

  it('shows sending, and not delivered with retry and delete', async () => {
    const { client } = fake([
      msg('local:1', 'on its way', { status: 'sending', client_msg_id: '1', sender_id: 'u_me' }),
      msg('local:2', 'refused', { status: 'failed', client_msg_id: '2', sender_id: 'u_me' }),
    ]);
    await chat(client);
    expect(screen.getByText('Sending…')).toBeTruthy();
    expect(screen.getByText('Not delivered')).toBeTruthy();
    await fireEvent.press(screen.getByText('Retry'));
    expect(client.retry).toHaveBeenCalledWith('c1', '2');
    await fireEvent.press(screen.getByText('Delete'));
    expect(client.discard).toHaveBeenCalledWith('c1', '2');
  });

  it('streams with Stop, then completes, or shows stopped and could not finish', async () => {
    const { client, view } = fake([msg('s', '', { state: 'streaming', streamed: 'Hel', sender_type: 'agent', sender_id: 'u_bot', sender_name: 'Nova' })]);
    await chat(client);
    expect(screen.getByText('Writing…')).toBeTruthy();
    expect(screen.queryByLabelText('Helpful')).toBeNull();
    await fireEvent.press(screen.getByText('■ Stop'));
    expect(client.call).toHaveBeenCalledWith('stream_cancel', { msg_id: 's' });
    await act(() => view.set([msg('s', 'Hello there', { state: 'complete', sender_type: 'agent', sender_id: 'u_bot', sender_name: 'Nova' })]));
    expect(screen.getByText('Hello there')).toBeTruthy();
    expect(screen.queryByText('■ Stop')).toBeNull();
    expect(screen.getByLabelText('Helpful')).toBeTruthy();
    await act(() => view.set([msg('s', 'Hel', { state: 'cancelled', sender_type: 'agent', sender_id: 'u_bot' }), msg('t', 'He', { state: 'failed', sender_type: 'agent', sender_id: 'u_bot' })]));
    expect(screen.getByText('Stopped')).toBeTruthy();
    expect(screen.getByText('Couldn’t finish')).toBeTruthy();
  });

  it('shows deleted and edited', async () => {
    const { client } = fake([msg('r', 'secret', { state: 'recalled' }), msg('e', 'fixed', { version: 2 }), msg('s', 'streamed', { version: 3, state: 'complete', sender_type: 'agent', sender_id: 'u_bot' })]);
    await chat(client);
    expect(screen.queryByText('secret')).toBeNull();
    expect(screen.getByText('This message was deleted')).toBeTruthy();
    expect(screen.getAllByText('Edited')).toHaveLength(1);
  });

  it('marks agents, also when the message component is replaced', async () => {
    const { client } = fake([msg('h', 'from a person'), msg('g', 'from an agent', { sender_type: 'agent', sender_id: 'u_bot', sender_name: 'Nova' })]);
    const Mine = ({ message }: MessageProps) => <Text>custom: {message.content}</Text>;
    await chat(client, { components: { Message: Mine } });
    expect(screen.getByText('custom: from an agent')).toBeTruthy();
    const marks = screen.getAllByLabelText('AI agent');
    expect(marks).toHaveLength(1);
    expect(within(screen.getByTestId('row-g')).getByLabelText('AI agent')).toBeTruthy();
    expect(within(screen.getByTestId('row-h')).queryByLabelText('AI agent')).toBeNull();
  });

  it('gives feedback on agent messages only', async () => {
    const { client } = fake([msg('h', 'person'), msg('g', 'agent', { sender_type: 'agent', sender_id: 'u_bot' })]);
    await chat(client);
    expect(screen.getAllByLabelText('Helpful')).toHaveLength(1);
    await fireEvent.press(screen.getByLabelText('Helpful'));
    expect(client.feedback).toHaveBeenCalledWith('c1', 'g', 'up');
    await fireEvent.press(screen.getByLabelText('Not helpful'));
    expect(client.feedback).toHaveBeenCalledWith('c1', 'g', 'down');
  });

  it('answers cards: buttons and a form, each block once; my own cards are done', async () => {
    const payload = {
      title: 'Refund?',
      blocks: [
        { type: 'text', text: 'Order 42' },
        { type: 'buttons', id: 'b', options: [{ id: 'yes', label: 'Approve', style: 'primary' }, { id: 'no', label: 'Reject', style: 'danger' }] },
        { type: 'form', id: 'f', submit: 'Send it', fields: [{ id: 'why', label: 'Why', type: 'text' }, { id: 'n', label: 'Amount', type: 'number' }, { id: 'ok', label: 'Sure', type: 'checkbox' }, { id: 'pick', label: 'Pick', type: 'select', options: ['red', 'blue'] }] },
      ],
    };
    const { client } = fake([msg('k', 'card', { kind: 'agentim.card', payload, sender_type: 'agent', sender_id: 'u_bot' }), msg('m', 'my card', { kind: 'agentim.card', payload: { blocks: [payload.blocks[1]] }, sender_id: 'u_me' })]);
    await chat(client);
    expect(screen.getByText('Refund?')).toBeTruthy();
    const approve = () => within(screen.getByTestId('row-k')).getByRole('button', { name: 'Approve' });
    expect(within(screen.getByTestId('row-m')).getByRole('button', { name: 'Approve' }).props.accessibilityState?.disabled).toBe(true);
    expect(approve().props.accessibilityState?.disabled).toBe(false);
    await fireEvent.press(approve());
    expect(client.respond).toHaveBeenCalledWith('c1', 'k', 'b', { action: 'yes' });
    expect(approve().props.accessibilityState?.disabled).toBe(true);
    await fireEvent.changeText(screen.getByLabelText('Why'), 'broken');
    await fireEvent.changeText(screen.getByLabelText('Amount'), '12');
    await fireEvent(screen.getByLabelText('Sure'), 'valueChange', true);
    await fireEvent.press(screen.getByRole('radio', { name: 'blue' }));
    await fireEvent.press(screen.getByRole('button', { name: 'Send it' }));
    expect(client.respond).toHaveBeenLastCalledWith('c1', 'k', 'f', { values: { why: 'broken', n: 12, ok: true, pick: 'blue' } });
    expect(screen.getByRole('button', { name: 'Send it' }).props.accessibilityState?.disabled).toBe(true);
  });

  it('edits and deletes my sent message from a long press', async () => {
    const { client } = fake([msg('a', 'helo', { sender_id: 'u_me' }), msg('b', 'theirs')]);
    await chat(client);
    expect(screen.queryByText('Edit')).toBeNull();
    // Theirs has no actions.
    await fireEvent(screen.getByTestId('bubble-b'), 'longPress');
    expect(screen.queryByText('Edit')).toBeNull();
    await fireEvent(screen.getByTestId('bubble-a'), 'longPress');
    await fireEvent.press(screen.getByText('Edit'));
    await fireEvent.changeText(screen.getByLabelText('Edit message'), 'hello');
    await fireEvent.press(screen.getByRole('button', { name: 'Save' }));
    expect(client.edit).toHaveBeenCalledWith('c1', { msgId: 'a' }, 'hello');
    await fireEvent(screen.getByTestId('bubble-a'), 'longPress');
    await fireEvent.press(screen.getByText('Delete'));
    expect(client.recall).toHaveBeenCalledWith('c1', { msgId: 'a' });
    // Unchanged text saves nothing.
    await fireEvent(screen.getByTestId('bubble-a'), 'longPress');
    await fireEvent.press(screen.getByText('Edit'));
    await fireEvent.press(screen.getByRole('button', { name: 'Save' }));
    expect(client.edit).toHaveBeenCalledTimes(1);
    expect(screen.queryByLabelText('Edit message')).toBeNull();
  });

  it('merges notices by seq and puts “New messages” above the first unread', async () => {
    const { client } = fake([msg('a', 'old', { created_seq: 1 }), msg('b', 'mine', { sender_id: 'u_me', created_seq: 2 }), msg('c', 'new 1', { created_seq: 4 }), msg('d', 'new 2', { created_seq: 5 })]);
    client.conversations.set([{ conv_id: 'c1', unread: 2 }]);
    await render(
      <AgentIM client={client as any} me="u_me">
        <MessageList convId="c1" notices={[{ key: 'j', seq: 3, content: 'Amy joined' }, { key: 'w', seq: 4.5, content: 'Whisper', tone: 'private' }]} />
      </AgentIM>,
    );
    // The list is inverted: newest cell first in the tree.
    const cells = screen.getAllByTestId(/^(row|notice)-/).map((c) => c.props.testID);
    expect(cells.reverse()).toEqual(['row-a', 'row-b', 'notice-j', 'row-c', 'notice-w', 'row-d']);
    expect(within(screen.getByTestId('row-c')).getByText('New messages')).toBeTruthy();
    expect(screen.getAllByText('New messages')).toHaveLength(1);
    expect(client.read).toHaveBeenCalledWith('c1', 5);
  });
});

describe('Composer, typing, connection, hand-off', () => {
  it('sends with the button only when there is text, and says I am typing', async () => {
    const { client } = fake();
    await chat(client);
    const send = screen.getByRole('button', { name: 'Send' });
    expect(send.props.accessibilityState?.disabled).toBe(true);
    await fireEvent.changeText(screen.getByLabelText('Message'), '  ');
    expect(screen.getByRole('button', { name: 'Send' }).props.accessibilityState?.disabled).toBe(true);
    await fireEvent.changeText(screen.getByLabelText('Message'), 'Hey there');
    expect(client.call).toHaveBeenCalledWith('signal', { conv_id: 'c1', kind: 'typing' });
    await fireEvent.press(screen.getByRole('button', { name: 'Send' }));
    expect(client.send).toHaveBeenCalledWith('c1', 'Hey there', undefined);
    expect(screen.getByLabelText('Message').props.value).toBe('');
  });

  it('names who is typing from the members', async () => {
    const { client } = fake();
    client.conversations.set([{ conv_id: 'c1', members: [{ user_id: 'u_bob', name: 'Bob' }, { user_id: 'u_amy', name: 'Amy' }] }]);
    await chat(client);
    await client.emit('signal', { conv_id: 'other', kind: 'typing', from: 'u_bob' });
    expect(screen.queryByText(/typing/)).toBeNull();
    await client.emit('signal', { conv_id: 'c1', kind: 'typing', from: 'u_bob' });
    expect(screen.getByText('••• Bob is typing…')).toBeTruthy();
    await client.emit('signal', { conv_id: 'c1', kind: 'typing', from: 'u_amy' });
    expect(screen.getByText('••• Bob and Amy are typing…')).toBeTruthy();
  });

  it('says what the connection is doing', async () => {
    const { client } = fake();
    await chat(client);
    expect(screen.queryByRole('alert')).toBeNull();
    await act(() => client.connection.set({ state: 'suspended' }));
    expect(screen.getByText('Waiting for network…')).toBeTruthy();
    await act(() => client.connection.set({ state: 'failed' }));
    expect(screen.getByText('Can’t connect')).toBeTruthy();
    await act(() => client.connection.set({ state: 'connected' }));
    expect(screen.queryByText('Can’t connect')).toBeNull();
  });

  it('announces a hand-off for this conversation only', async () => {
    const { client } = fake();
    await chat(client);
    await client.emit('event', { event: { conv_id: 'other', type: 'agent.state', data: { state: 'taken_over', handoff: true } } });
    expect(screen.queryByText('You’re being connected to a person')).toBeNull();
    await client.emit('event', { event: { conv_id: 'c1', type: 'agent.state', data: { state: 'taken_over', handoff: true, note: 'Refund over $100' } } });
    expect(screen.getByText('You’re being connected to a person')).toBeTruthy();
    expect(screen.getByText('Refund over $100')).toBeTruthy();
    await client.emit('event', { event: { conv_id: 'c1', type: 'agent.state', data: { state: 'active', handback: true } } });
    expect(screen.getByText('The assistant is back')).toBeTruthy();
  });
});

describe('ConversationList and MemberList', () => {
  it('shows previews, times and unread counts, filters by search, and selects', async () => {
    const { client } = fake();
    const onSelect = jest.fn();
    await render(
      <AgentIM client={client as any} me="u_me" {...when}>
        <ConversationList selected="c2" onSelect={onSelect} />
      </AgentIM>,
    );
    expect(screen.getByText('No conversations yet')).toBeTruthy();
    await act(() =>
      client.conversations.set([
        { conv_id: 'c1', title: 'Support', unread: 3, last_message: { content: 'Where is my order?', sender_id: 'u_bob', created_at: Date.parse('2026-10-26T14:05:00Z') } },
        { conv_id: 'c2', title: 'Design team', unread: 150, last_message: { content: 'Shipped it', sender_id: 'u_me', created_at: Date.parse('2026-10-22T09:00:00Z') } },
      ]),
    );
    expect(screen.getByText('3')).toBeTruthy();
    expect(screen.getByText('99+')).toBeTruthy();
    expect(screen.getByText('You: Shipped it')).toBeTruthy();
    expect(nbsp(String(screen.getByText(/2:05/).props.children))).toBe('2:05 PM');
    expect(screen.getByText('Thu')).toBeTruthy();
    expect(screen.getByRole('button', { name: /Design team/ }).props.accessibilityState?.selected).toBe(true);
    await fireEvent.press(screen.getByText('Support'));
    expect(onSelect).toHaveBeenCalledWith('c1');
    await fireEvent.changeText(screen.getByLabelText('Search'), 'ORDER');
    expect(screen.queryByText('Design team')).toBeNull();
    expect(screen.getByText('Support')).toBeTruthy();
    await fireEvent.changeText(screen.getByLabelText('Search'), 'nothing like it');
    expect(screen.getByText('No matches')).toBeTruthy();
  });

  it('lists members with roles and the AI mark on agents', async () => {
    const { client } = fake();
    client.conversations.set([{ conv_id: 'c1', members: [{ user_id: 'u1', name: 'Alice', kind: 'human', role: 'owner' }, { user_id: 'u2', name: 'Helper', kind: 'agent', role: 'member' }] }]);
    await render(
      <AgentIM client={client as any}>
        <MemberList convId="c1" />
      </AgentIM>,
    );
    expect(screen.getByText('Owner')).toBeTruthy();
    expect(within(screen.getByTestId('member-u2')).getByLabelText('AI agent')).toBeTruthy();
    expect(within(screen.getByTestId('member-u1')).queryByLabelText('AI agent')).toBeNull();
  });
});

describe('Theme and texts', () => {
  it('uses the shared tokens, light or dark, and takes overrides and new texts', async () => {
    const { client } = fake([msg('a', 'mine', { sender_id: 'u_me' })]);
    await chat(client, { scheme: 'dark' });
    expect(flat(screen.getByTestId('bubble-a').props.style).backgroundColor).toBe(dark.bubbleMine);
    await chat(client, { scheme: 'light', colors: { bubbleMine: '#111111' }, strings: { placeholder: 'اكتب رسالة', send: 'إرسال' } });
    expect(flat(screen.getByTestId('bubble-a').props.style).backgroundColor).toBe('#111111');
    expect(screen.getByLabelText('اكتب رسالة')).toBeTruthy();
    expect(screen.getByRole('button', { name: 'إرسال' })).toBeTruthy();
    expect(light.bubbleMine).toBe('#2563eb');
  });

  it('needs a provider for the client', async () => {
    const err = jest.spyOn(console, 'error').mockImplementation(() => {});
    await expect(render(<MessageList convId="c1" />)).rejects.toThrow(/AgentIMProvider/);
    err.mockRestore();
  });
});

describe('a conversation this person is no longer in (L4)', () => {
  it('says how it ended in the chat and replaces the composer; added back, it is normal again', async () => {
    const { client } = fake([msg('a', 'earlier talk')]);
    const conv = (ended?: any) => ({ conv_id: 'c1', title: 'Team', members: [{ user_id: 'u_bob', name: 'Bob' }], ended });
    client.conversations.set([conv({ reason: 'removed', by: 'u_bob' })]);
    await chat(client);
    expect(screen.getByText('earlier talk')).toBeTruthy();
    expect(screen.getByText('Bob removed you')).toBeTruthy();
    expect(screen.getByText('You can’t send messages because you’re no longer a member.')).toBeTruthy();
    expect(screen.queryByLabelText('Message')).toBeNull();
    await act(() => client.conversations.set([conv({ reason: 'dissolved' })]));
    expect(screen.getByText('This conversation was closed')).toBeTruthy();
    expect(screen.getByText('This conversation is closed. You can still read its history.')).toBeTruthy();
    await act(() => client.conversations.set([conv(undefined)]));
    expect(screen.queryByText('This conversation was closed')).toBeNull();
    expect(screen.getByLabelText('Message')).toBeTruthy();
  });
});
