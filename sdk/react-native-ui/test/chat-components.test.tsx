// The chat components of docs/en/design/chat-components.md on React Native: data cards, forms
// (checks, `to`, answers after a reload), your own blocks and fields, steps,
// the inbox.
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react-native';
import { Text, TextInput } from 'react-native';
import { Observable } from '@agentimhq/client-web/view';
import { AgentIM, Chat, Inbox, InboxBadge, type CardBlockProps, type FormFieldProps } from '../src/index';

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
    sendFile: jest.fn(async () => ({ key: 'local:f' })),
    fileUrl: jest.fn(async (_c: string, id: string, variant: string) => {
      if (id === 'gone') throw Object.assign(new Error('gone'), { code: 'not_found' });
      return { url: `https://files.example/${id}/${variant}`, variant, expires_at: 0 };
    }),
    localPreview: jest.fn((k?: string) => (k ? `file://${k}` : undefined)),
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

const card = (payload: any, extra: any = {}) => msg('k', 'card', { kind: 'agentim.card', payload, sender_type: 'agent', sender_id: 'u_bot', ...extra });

describe('Data cards', () => {
  it('shows facts, a table and a list, formatted', async () => {
    const { client } = fake([
      card({
        title: 'Order 1002',
        blocks: [
          { type: 'facts', items: [{ label: 'Total', value: 120, format: 'currency:USD' }, { label: 'Status', value: 'Delayed', tone: 'warning' }, { label: 'Ordered', value: '2026-10-19', format: 'date' }] },
          { type: 'table', caption: 'Recent', columns: [{ label: 'Order' }, { label: 'Total', format: 'currency:USD' }], rows: [['1001', 25]] },
          { type: 'list', id: 'pick', select: true, items: [{ id: 'o1', title: 'Order 1001', badge: { label: 'Refunded', tone: 'info' } }] },
        ],
      }),
    ]);
    await chat(client);
    expect(screen.getByText('$120.00')).toBeTruthy();
    expect(screen.getByText('Delayed')).toBeTruthy();
    expect(screen.getByText('Oct 19, 2026')).toBeTruthy();
    expect(screen.getByText('$25.00')).toBeTruthy();
    expect(screen.getByText('Recent')).toBeTruthy();
    await fireEvent.press(screen.getByText('Order 1001'));
    expect(client.respond).toHaveBeenCalledWith('c1', 'k', 'pick', { action: 'o1' });
  });
});

describe('Form cards', () => {
  it('checks, sends, then shows what was sent', async () => {
    const { client } = fake([
      card({ title: 'Refund', blocks: [{ type: 'form', id: 'f', submit: 'Send', fields: [
        { id: 'reason', label: 'Reason', type: 'select', options: ['Late', 'Broken'], required: true },
        { id: 'day', label: 'Pickup', type: 'date' },
        { id: 'mail', label: 'Email', type: 'email' },
        { id: 'qty', label: 'Quantity', type: 'number', default: 2 },
      ] }] }),
    ]);
    await chat(client);
    expect(screen.getByLabelText('Quantity').props.value).toBe('2');
    await fireEvent.press(screen.getByText('Send'));
    expect(screen.getByText('Required')).toBeTruthy();
    await fireEvent.press(screen.getByText('Broken'));
    await fireEvent.changeText(screen.getByLabelText('Pickup'), '2026-02-30x');
    await fireEvent.changeText(screen.getByLabelText('Email'), 'nope');
    await fireEvent.press(screen.getByText('Send'));
    expect(client.respond).not.toHaveBeenCalled();
    await fireEvent.changeText(screen.getByLabelText('Pickup'), '2026-03-02');
    await fireEvent.changeText(screen.getByLabelText('Email'), 'bob@example.com');
    await fireEvent.press(screen.getByText('Send'));
    expect(client.respond).toHaveBeenCalledWith('c1', 'k', 'f', { values: { reason: 'Broken', day: '2026-03-02', mail: 'bob@example.com', qty: 2 } });
    expect(screen.getByText(/Sent by you/)).toBeTruthy();
    expect(screen.getByText('Mar 2, 2026')).toBeTruthy();
  });
  it('is read-only for others, and answered after a reload', async () => {
    const a = fake([card({ to: ['u_maya'], blocks: [{ type: 'buttons', id: 'b', options: [{ id: 'y', label: 'Yes' }] }] })]);
    await chat(a.client);
    expect(screen.getByText('Waiting for an answer')).toBeTruthy();
    await fireEvent.press(screen.getByText('Yes'));
    expect(a.client.respond).not.toHaveBeenCalled();
    await screen.unmount();
    const b = fake([card({ once: true, blocks: [{ type: 'form', id: 'f', submit: 'Send', fields: [{ id: 'x', label: 'X', type: 'text' }] }] })]);
    b.client.call = jest.fn(async (op: string) => (op === 'card_answers' ? { answers: [{ block_id: 'f', by: 'u_me', values: { x: 'hello' }, at: NOW }] } : {}));
    await chat(b.client);
    await screen.findByText(/Sent by you/);
    expect(screen.getByText('hello')).toBeTruthy();
    expect(screen.queryByText('Send')).toBeNull();
  });
  it('draws your own blocks and fields, and a fallback for others', async () => {
    const Chart = ({ block }: CardBlockProps) => <Text>chart {block.data.n}</Text>;
    const Day = ({ field, onChange }: FormFieldProps) => <TextInput accessibilityLabel={field.label} onChangeText={(v) => onChange(`D:${v}`)} />;
    const { client } = fake([card({ blocks: [{ type: 'x-acme.chart', data: { n: 3 }, fallback: 'A chart' }, { type: 'x-acme.map', fallback: 'Open the map' }, { type: 'form', id: 'f', submit: 'Go', fields: [{ id: 'd', label: 'Day', type: 'date' }] }] })]);
    await chat(client, { components: { cardBlocks: { 'x-acme.chart': Chart }, formFields: { date: Day } } });
    expect(screen.getByText('chart 3')).toBeTruthy();
    expect(screen.getByText('Open the map')).toBeTruthy();
    await fireEvent.changeText(screen.getByLabelText('Day'), 'mon');
    await fireEvent.press(screen.getByText('Go'));
    expect(client.respond).toHaveBeenCalledWith('c1', 'k', 'f', { values: { d: 'D:mon' } });
  });
});

describe('Steps', () => {
  it('open while working, folded when done', async () => {
    const { client } = fake([msg('r', 'Done', { sender_type: 'agent', sender_id: 'u_bot' })]);
    await chat(client);
    const p = (data: any) => client.emit('side', { event: { type: 'progress', conv_id: 'c1', created_at: NOW, data: { kind: 'step', text: data.title ?? 'x', msg_id: 'r', data } } });
    await p({ id: 'a', state: 'done', title: 'Look up' });
    await p({ id: 'b', state: 'waiting', title: 'Refund', detail: 'Waiting for Sam' });
    expect(screen.getByText('Waiting for Sam')).toBeTruthy();
    await p({ id: 'b', state: 'done', title: 'Refund' });
    expect(screen.getByText(/2 steps/)).toBeTruthy();
    expect(screen.getByText('Refund')).toBeTruthy();
    await fireEvent.press(screen.getByText(/2 steps/));
    expect(screen.queryByText('Refund')).toBeNull();
  });
});

describe('Inbox', () => {
  it('lists, opens an item, approves; counts in the badge', async () => {
    const { client } = fake();
    let items: any[] = [
      { type: 'request', request_id: 'q1', kind: 'approve', conv_id: 'c1', conv_title: 'Maya', created_at: NOW - 180_000, expires_at: NOW + 3_600_000, context: { operation: { name: 'createRefund', description: 'Refund an order (POST /refunds)', fields: [{ name: 'amount', value: 120 }], amount: 120 } } },
      { type: 'draft', draft_id: 'd1', conv_id: 'c2', agent_name: 'Aide', content: 'Thursday at 10?', created_at: NOW - 60_000 },
    ];
    client.call = jest.fn(async (op: string, a: any) => {
      if (op === 'inbox') return { items };
      items = items.filter((i) => i.request_id !== a?.request_id);
      return {};
    });
    await render(
      <AgentIM client={client as any} me="u_me" {...when}>
        <InboxBadge />
        <Inbox />
      </AgentIM>,
    );
    await screen.findByText('Refund an order');
    expect(screen.getByText('2')).toBeTruthy();
    expect(screen.getByText('3 min')).toBeTruthy();
    await fireEvent.press(screen.getByText('Drafts 1'));
    expect(screen.queryByText('Refund an order')).toBeNull();
    await fireEvent.press(screen.getByText('All 2'));
    await fireEvent.press(screen.getByText('Refund an order'));
    await fireEvent.press(screen.getByText('Approve'));
    await waitFor(() => expect(client.call).toHaveBeenCalledWith('decide', { request_id: 'q1', decision: 'approve' }));
    await screen.findByText('Approved by you');
    await fireEvent.press(screen.getByText('Back'));
    expect(screen.getByText('Thursday at 10?')).toBeTruthy();
    // Unmounted, the inbox stops refreshing (no timer left behind).
    await screen.unmount();
  });
});
