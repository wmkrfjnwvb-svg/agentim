// @vitest-environment jsdom
// The chat components of docs/en/design/chat-components.md: data cards (facts, tables, lists, values
// in the viewer's locale), forms (new fields, checks, defaults, `to`, the
// answers after a reload, live answers), your own blocks and fields, steps
// next to a reply, and the approval inbox.

import { afterEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { Observable } from '@agentimhq/client-web';
import { AgentIM, Chat, Inbox, InboxBadge, type CardBlockProps, type FormFieldProps, type StepsProps } from '../src/index.js';
import { useSteps } from '../src/headless.js';
import { foldStep, formatAgo, formatValue, type Step } from '../src/format.js';

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
    sendFile: vi.fn(async () => ({ client_msg_id: 'f', key: 'local:f' })),
    fileUrl: vi.fn(async (_c: string, id: string, variant: string) => {
      if (id === 'gone') throw Object.assign(new Error('gone'), { code: 'not_found' });
      return { url: `https://files.example/${id}/${variant}`, variant, expires_at: 0 };
    }),
    localPreview: vi.fn((k?: string) => (k ? `blob:${k}` : undefined)),
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


const card = (payload: any, extra: any = {}) => msg('k', 'card', { kind: 'agentim.card', payload, sender_type: 'agent', sender_id: 'u_bot', ...extra });

describe('formatValue and foldStep', () => {
  it('formats values in the viewer\'s locale', () => {
    expect(formatValue(1200, 'currency:USD', { locale: 'en-US' })).toBe('$1,200.00');
    expect(formatValue(1200, 'currency:EUR', { locale: 'de-DE' })).toBe('1.200,00\u00a0€');
    expect(formatValue(0.4, 'percent', { locale: 'en-US' })).toBe('40%');
    expect(formatValue(1234.5, 'number', { locale: 'de-DE' })).toBe('1.234,5');
    // A calendar day is the same day everywhere.
    expect(formatValue('2026-03-02', 'date', { locale: 'en-US', timeZone: 'Pacific/Honolulu' })).toBe('Mar 2, 2026');
    expect(formatValue('2026-11-07T10:00:00Z', 'datetime', { locale: 'en-US', timeZone: 'UTC' })).toBe('Nov 7, 2026, 10:00 AM');
    expect(formatValue('not a date', 'date')).toBe('not a date');
    expect(formatValue(5, 'currency:NOPE')).toBe('5');
    expect(formatValue(null, 'number')).toBe('—');
    expect(formatValue(true, undefined)).toBe('✓');
    expect(formatAgo(0, { now: 3 * 60_000, locale: 'en-US' })).toBe('3 min');
    expect(formatAgo(0, { now: 2 * 86_400_000, locale: 'en-US' })).toBe('2 days');
  });
  it('folds progress into steps', () => {
    let s: Step[] = [];
    s = foldStep(s, { kind: 'plan', data: { steps: [{ id: 'a', title: 'Look up' }, { id: 'b', title: 'Refund' }] }, at: 1 });
    expect(s.map((x) => x.state)).toEqual(['pending', 'pending']);
    s = foldStep(s, { kind: 'step', data: { id: 'a', state: 'running' }, at: 2 });
    s = foldStep(s, { kind: 'step', data: { id: 'a', state: 'done' }, at: 3 });
    s = foldStep(s, { kind: 'step', data: { id: 'b', state: 'waiting', detail: 'Waiting for Sam', eta: '5 min' }, at: 4 });
    expect(s.map((x) => [x.title, x.state])).toEqual([['Look up', 'done'], ['Refund', 'waiting']]);
    expect(s[1].detail).toBe('Waiting for Sam');
    expect(s[1].data).toEqual({ eta: '5 min' });
    s = foldStep(s, { kind: 'step', data: { id: 'c', state: 'bogus', title: 'Notify' }, at: 5 });
    expect(s[2].state).toBe('running');
    // Without ids: tool calls become steps.
    let t: Step[] = [];
    t = foldStep(t, { kind: 'tool_call', text: 'search' });
    t = foldStep(t, { kind: 'tool_result', data: { error: true } });
    expect(t.map((x) => x.state)).toEqual(['failed']);
    expect(foldStep(t, { kind: 'thinking', text: 'x' })).toBe(t);
  });
});

describe('Data cards', () => {
  it('shows facts, tables and lists, with tones and formats', () => {
    const { client } = fake([
      card({
        title: 'Order 1002',
        blocks: [
          { type: 'facts', items: [{ label: 'Total', value: 120, format: 'currency:USD' }, { label: 'Status', value: 'Delivered', tone: 'success' }, { label: 'Placed', value: '2026-03-02', format: 'date' }] },
          { type: 'table', caption: 'Recent orders', columns: [{ label: 'Order' }, { label: 'Total', format: 'currency:USD' }], rows: [['1001', 25], ['1002', 120]] },
          { type: 'list', items: [{ id: 'a', title: 'Headphones', subtitle: 'Black', meta: '$25', badge: { label: 'Refunded', tone: 'info' } }] },
        ],
      }),
    ]);
    chat(client, { locale: 'en-US' });
    expect(screen.getByText('Order 1002')).toBeTruthy();
    expect(screen.getAllByText('$120.00')).toHaveLength(2);
    expect(screen.getByText('Delivered').getAttribute('data-tone')).toBe('success');
    expect(screen.getByText('Mar 2, 2026')).toBeTruthy();
    const table = screen.getByRole('table');
    expect(within(table).getAllByRole('columnheader').map((h) => h.textContent)).toEqual(['Order', 'Total']);
    expect(within(table).getAllByRole('rowheader').map((h) => h.textContent)).toEqual(['1001', '1002']);
    expect(within(table).getByText('$25.00').getAttribute('data-align')).toBe('end');
    expect(screen.getByRole('region', { name: 'Recent orders' })).toBeTruthy();
    expect(screen.getByText('Refunded').getAttribute('data-tone')).toBe('info');
    // Display blocks: nothing to answer, no buttons.
    expect(screen.queryByRole('button', { name: /Headphones/ })).toBeNull();
    expect(client.call).not.toHaveBeenCalledWith('card_answers', expect.anything());
  });
  it('lets people pick a row of a list', () => {
    const { client } = fake([card({ blocks: [{ type: 'list', id: 'pick', select: true, items: [{ id: 'o1', title: 'Order 1001' }, { id: 'o2', title: 'Order 1002' }] }] })]);
    chat(client);
    fireEvent.click(screen.getByRole('button', { name: /Order 1002/ }));
    expect(client.respond).toHaveBeenCalledWith('c1', 'k', 'pick', { action: 'o2' });
    expect(screen.getByRole('button', { name: /Order 1002/ }).getAttribute('aria-pressed')).toBe('true');
    expect((screen.getByRole('button', { name: /Order 1001/ }) as HTMLButtonElement).disabled).toBe(true);
  });
  it('formats for another locale', () => {
    const { client } = fake([card({ blocks: [{ type: 'facts', items: [{ label: 'Summe', value: 1200, format: 'currency:EUR' }] }] })]);
    chat(client, { locale: 'de-DE' });
    expect(screen.getByText('1.200,00 €')).toBeTruthy();
  });
});

describe('Form cards', () => {
  const form = (fields: any[], extra: any = {}) => card({ title: 'Refund', ...extra, blocks: [{ type: 'form', id: 'f', submit: 'Send', fields }] });
  it('checks answers like the server before sending', () => {
    const { client } = fake([
      form([
        { id: 'reason', label: 'Reason', type: 'select', options: ['Broken', 'Late'], required: true, placeholder: 'Choose…' },
        { id: 'more', label: 'More', type: 'textarea', max: 10, hint: 'Optional' },
        { id: 'day', label: 'Pickup', type: 'date', min: '2026-01-01' },
        { id: 'at', label: 'At', type: 'time' },
        { id: 'mail', label: 'Email', type: 'email' },
        { id: 'code', label: 'Code', type: 'text', pattern: '[A-Z]{2}[0-9]{4}', error: 'Like AB1234' },
        { id: 'qty', label: 'Quantity', type: 'number', min: 1, max: 5, default: 1 },
      ]),
    ]);
    chat(client);
    expect((screen.getByLabelText('Quantity') as HTMLInputElement).value).toBe('1');
    expect(screen.getByText('Optional')).toBeTruthy();
    expect((screen.getByLabelText('Pickup') as HTMLInputElement).type).toBe('date');
    expect((screen.getByLabelText('At') as HTMLInputElement).type).toBe('time');
    expect(screen.getByLabelText('More').tagName).toBe('TEXTAREA');
    fireEvent.click(screen.getByText('Send'));
    expect(screen.getByText('Required')).toBeTruthy();
    expect(screen.getByLabelText(/Reason/).getAttribute('aria-invalid')).toBe('true');
    fireEvent.change(screen.getByLabelText(/Reason/), { target: { value: 'Broken' } });
    fireEvent.change(screen.getByLabelText('Code'), { target: { value: 'ab1234' } });
    fireEvent.change(screen.getByLabelText('Email'), { target: { value: 'nope' } });
    fireEvent.change(screen.getByLabelText('Pickup'), { target: { value: '2025-12-31' } });
    fireEvent.click(screen.getByText('Send'));
    expect(screen.getByText('Like AB1234')).toBeTruthy();
    expect(screen.getAllByText('Check this value')).toHaveLength(2);
    expect(client.respond).not.toHaveBeenCalled();
    fireEvent.change(screen.getByLabelText('Code'), { target: { value: 'AB1234' } });
    fireEvent.change(screen.getByLabelText('Email'), { target: { value: 'bob@example.com' } });
    fireEvent.change(screen.getByLabelText('Pickup'), { target: { value: '2026-03-02' } });
    fireEvent.click(screen.getByText('Send'));
    expect(client.respond).toHaveBeenCalledWith('c1', 'k', 'f', { values: { reason: 'Broken', day: '2026-03-02', mail: 'bob@example.com', code: 'AB1234', qty: 1 } });
  });
  it('is read-only for people it is not for', () => {
    const { client } = fake([form([{ id: 'x', label: 'X', type: 'text' }], { to: ['u_maya'] })]);
    chat(client);
    expect((screen.getByText('Send') as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByText('Waiting for an answer')).toBeTruthy();
  });
  it('shows what was sent after a reload, and answers that arrive', async () => {
    const { client } = fake([form([{ id: 'reason', label: 'Reason', type: 'select', options: ['Broken'] }, { id: 'day', label: 'Pickup', type: 'date' }], { once: true })]);
    client.call = vi.fn(async (op: string) => (op === 'card_answers' ? { answers: [{ block_id: 'f', by: 'u_me', by_name: 'me', values: { reason: 'Broken', day: '2026-03-02' }, at: 0, seq: 9 }] } : {}));
    chat(client, { locale: 'en-US' });
    await screen.findByText('Sent by you', { exact: false });
    expect(client.call).toHaveBeenCalledWith('card_answers', { msg_id: 'k' });
    expect(screen.getByText('Mar 2, 2026')).toBeTruthy();
    expect(screen.queryByText('Send')).toBeNull();
    cleanup();
    // Someone else answers a `once` card while I look at it.
    const b = fake([card({ once: true, blocks: [{ type: 'buttons', id: 'ok', options: [{ id: 'y', label: 'Yes' }] }] })]);
    chat(b.client);
    expect((screen.getByText('Yes') as HTMLButtonElement).disabled).toBe(false);
    b.client.emit('event', { event: { type: 'card.response', seq: 10, created_at: 0, data: { msg_id: 'k', block_id: 'ok', action: 'y', by: 'u_ann' } } });
    expect((screen.getByText('Yes') as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByText('Yes').getAttribute('data-picked')).toBe('true');
    expect(screen.getByText('Answered', { exact: false })).toBeTruthy();
  });
});

describe('Your own blocks and fields', () => {
  it('draws registered ones, the fallback for others, nothing for drop', () => {
    const Chart = ({ block, respond }: CardBlockProps) => (
      <button type="button" onClick={() => respond({ values: { picked: block.data.points[1] } })}>
        chart of {block.data.points.length}
      </button>
    );
    const Slot = ({ field, value, onChange }: FormFieldProps) => (
      <input aria-label={field.label} value={(value as string) ?? ''} onChange={(e) => onChange(e.target.value.toUpperCase())} />
    );
    const { client } = fake([
      card({
        blocks: [
          { type: 'x-acme.chart', id: 'c', data: { points: [1, 2] }, fallback: 'A chart' },
          { type: 'x-acme.map', fallback: 'See the map in the app' },
          { type: 'x-acme.secret', fallback: 'drop' },
          { type: 'form', id: 'f', submit: 'Book', fields: [{ id: 's', label: 'Slot', type: 'x-acme.slot' }, { id: 'u', label: 'Unknown', type: 'x-other' }] },
        ],
      }),
    ]);
    chat(client, { components: { cardBlocks: { 'x-acme.chart': Chart }, formFields: { 'x-acme.slot': Slot } } });
    fireEvent.click(screen.getByText('chart of 2'));
    expect(client.respond).toHaveBeenCalledWith('c1', 'k', 'c', { values: { picked: 2 } });
    expect(screen.getByText('See the map in the app')).toBeTruthy();
    expect(screen.queryByText('drop')).toBeNull();
    expect(screen.queryByLabelText('Unknown')).toBeNull();
    fireEvent.change(screen.getByLabelText('Slot'), { target: { value: 'mon 9' } });
    fireEvent.click(screen.getByText('Book'));
    expect(client.respond).toHaveBeenCalledWith('c1', 'k', 'f', { values: { s: 'MON 9' } });
  });
});

describe('Steps', () => {
  const progress = (kind: string, data: any, msg_id = 'k') => ({ event: { type: 'progress', conv_id: 'c1', created_at: Date.now(), data: { kind, text: data.title ?? kind, msg_id, data } } });
  it('open while working, folded once done, by reply', () => {
    const { client } = fake([msg('k', 'Here you go', { sender_type: 'agent', sender_id: 'u_bot' }), msg('k2', 'Other', { sender_type: 'agent', sender_id: 'u_bot' })]);
    chat(client);
    client.emit('side', progress('plan', { steps: [{ id: 'a', title: 'Look up order' }, { id: 'b', title: 'Refund' }] }));
    client.emit('side', progress('step', { id: 'a', state: 'done' }));
    client.emit('side', progress('step', { id: 'b', state: 'waiting', detail: 'Waiting for Sam' }));
    const list = screen.getByRole('list', { name: 'Steps' });
    expect(within(list).getAllByRole('listitem').map((li) => li.getAttribute('data-state'))).toEqual(['done', 'waiting']);
    expect(screen.getByText('Waiting for Sam')).toBeTruthy();
    expect(screen.queryByRole('button', { name: /steps/ })).toBeNull();
    client.emit('side', progress('step', { id: 'b', state: 'done' }));
    // All done: one line, opened on a click.
    const toggle = screen.getByRole('button', { name: /2 steps/ });
    expect(toggle.getAttribute('aria-expanded')).toBe('true');
    fireEvent.click(toggle);
    expect(screen.queryByRole('list', { name: 'Steps' })).toBeNull();
    // Another reply's steps and other kinds of progress stay apart.
    client.emit('side', progress('step', { id: 'x', state: 'failed', title: 'Send email', detail: 'Bounced' }, 'k2'));
    expect(screen.getByText('Bounced')).toBeTruthy();
    expect(screen.getAllByRole('list', { name: 'Steps' })).toHaveLength(1);
  });
  it('can be drawn your way, or read with useSteps', () => {
    const Mine = ({ steps }: StepsProps) => <p>mine: {steps.map((s) => `${s.title}=${s.state}`).join(',')}</p>;
    const Count = () => <p>count {useSteps('c1').length}</p>;
    const { client } = fake([msg('k', 'Hi', { sender_type: 'agent', sender_id: 'u_bot' })]);
    render(
      <AgentIM client={client as any} me="u_me" components={{ Steps: Mine }}>
        <Chat convId="c1" />
        <Count />
      </AgentIM>,
    );
    client.emit('side', progress('step', { id: 'a', state: 'running', title: 'Think' }));
    expect(screen.getByText('mine: Think=running')).toBeTruthy();
    // Steps not tied to a message show at the end of the conversation.
    client.emit('side', progress('step', { id: 'z', state: 'running', title: 'Loose' }, null as any));
    expect(screen.getByText('count 1')).toBeTruthy();
    expect(screen.getByText('mine: Loose=running')).toBeTruthy();
  });
});

describe('Inbox', () => {
  const op = { name: 'createRefund', description: 'Refund an order (POST /refunds)', connection: 'orders', fields: [{ name: 'order', value: '1002' }, { name: 'amount', value: 120 }], amount: 120, rule: { tool: 'createRefund', amount_field: 'amount', over: 30 } };
  const items = (now: number) => [
    { type: 'request', request_id: 'q1', kind: 'approve', conv_id: 'c1', conv_title: 'Maya', agent_name: 'helpbot', asker_name: 'maya', created_at: now - 3 * 60_000, expires_at: now + 3_600_000, title: 'x', context: { operation: op, tool_call: { msg: { content: 'refund order 1002', sender_name: 'maya' } } } },
    { type: 'draft', draft_id: 'd1', conv_id: 'c2', conv_title: 'Chris', agent_name: 'aide', content: 'Let us talk Thursday', created_at: now - 60_000 },
    { type: 'handoff', conv_id: 'c3', agent_id: 'u_bot', agent_name: 'helpbot', conv_title: 'Leo', note: 'Wants a manager', created_at: now - 120_000 },
    { type: 'request', request_id: 'q2', kind: 'choose', options: ['Red', 'Blue'], conv_id: 'c4', title: 'Which colour?', created_at: now - 10_000, expires_at: now + 5 * 60_000 },
  ];
  function inbox(props: any = {}, list?: any[]) {
    const now = Date.now();
    const { client } = fake();
    let current = list ?? items(now);
    client.call = vi.fn(async (op: string, a: any) => {
      if (op === 'inbox') return { items: current, more: false };
      current = current.filter((i) => !((a?.request_id && i.request_id === a.request_id) || (a?.draft_id && i.draft_id === a.draft_id) || (op === 'handback' && i.type === 'handoff')));
      return {};
    });
    render(
      <AgentIM client={client as any} me="u_me" locale="en-US">
        <InboxBadge />
        <Inbox {...props} />
      </AgentIM>,
    );
    return client;
  }
  it('lists what waits with counts, and filters', async () => {
    inbox();
    await screen.findByText('Refund an order');
    const tabs = screen.getAllByRole('tab');
    expect(tabs.map((t) => t.textContent)).toEqual(['All4', 'Approvals1', 'Drafts1', 'Hand-offs1', 'Questions1']);
    expect(screen.getAllByText('120').length).toBeGreaterThan(0);
    expect(screen.getByText('3 min')).toBeTruthy();
    expect(screen.getByText('Expires soon')).toBeTruthy();
    expect(screen.getByText('createRefund · Maya · for maya')).toBeTruthy();
    fireEvent.click(screen.getByRole('tab', { name: /Drafts/ }));
    expect(screen.getAllByRole('option')).toHaveLength(1);
    fireEvent.click(screen.getByRole('tab', { name: /All/ }));
    fireEvent.change(screen.getByLabelText('Search the inbox'), { target: { value: 'manager' } });
    expect(screen.getAllByRole('option')).toHaveLength(1);
    fireEvent.change(screen.getByLabelText('Search the inbox'), { target: { value: 'zzz' } });
    expect(screen.getByText('No matches')).toBeTruthy();
    // The badge counts too.
    expect(document.querySelector('.aim-inbox-badge .aim-count')?.textContent).toBe('4');
  });
  it('approves, declines with a reason, edits a draft, hands back, answers', async () => {
    const onOpen = vi.fn();
    const client = inbox({ onOpen });
    fireEvent.click(await screen.findByText('Refund an order'));
    const detail = screen.getByRole('article');
    expect(within(detail).getByText('refund order 1002')).toBeTruthy();
    expect(within(detail).getByText(/amount > 30/)).toBeTruthy();
    fireEvent.click(within(detail).getByText('Decline'));
    fireEvent.change(within(detail).getByLabelText('Reason (optional)'), { target: { value: 'Too much' } });
    fireEvent.click(within(detail).getByText('Decline'));
    await waitFor(() => expect(client.call).toHaveBeenCalledWith('decide', { request_id: 'q1', decision: 'reject', text: 'Too much' }));
    await screen.findAllByText('Declined by you');
    fireEvent.click(screen.getByText('Let us talk Thursday'));
    fireEvent.change(screen.getByRole('textbox', { name: /aide suggests/ }), { target: { value: 'Thursday at 10?' } });
    fireEvent.click(screen.getByText('Send edited'));
    await waitFor(() => expect(client.call).toHaveBeenCalledWith('approve', { draft_id: 'd1', text: 'Thursday at 10?' }));
    fireEvent.click(screen.getByText('Wants a manager'));
    fireEvent.click(within(screen.getByRole('article')).getByText('Open conversation'));
    expect(onOpen).toHaveBeenCalledWith('c3');
    fireEvent.click(screen.getByText('Hand back', { selector: 'button' }));
    await waitFor(() => expect(client.call).toHaveBeenCalledWith('handback', { conv_id: 'c3', agent_id: 'u_bot' }));
    fireEvent.click(screen.getByText('Which colour?', { selector: '.aim-inbox-title' }));
    fireEvent.click(screen.getByText('Blue'));
    await waitFor(() => expect(client.call).toHaveBeenCalledWith('decide', { request_id: 'q2', decision: 'choose', choice: 'Blue' }));
  });
  it('says when someone else decided, and is empty when nothing waits', async () => {
    const client = inbox();
    fireEvent.click(await screen.findByText('Refund an order'));
    // A new draft arriving is not a decision: the items stay open.
    client.emit('side', { event: { type: 'draft.created', conv_id: 'c2', data: { draft_id: 'd1' } } });
    client.emit('side', { event: { type: 'request.created', conv_id: 'c1', data: { request_id: 'q1' } } });
    expect(screen.getByText('Approve', { selector: 'button' })).toBeTruthy();
    expect(screen.queryByText('Answered')).toBeNull();
    client.emit('side', { event: { type: 'request.decided', conv_id: 'c1', data: { request_id: 'q1', state: 'approved', decided_by: 'u_sam' } } });
    await screen.findAllByText('Approved');
    expect(screen.queryByText('Approve', { selector: 'button' })).toBeNull();
    cleanup();
    inbox({}, []);
    await screen.findByText('You’re all caught up');
  });
  it('takes your own items and details', async () => {
    const now = Date.now();
    inbox({ extraItems: [{ type: 'po', id: 'po-7', conv_id: 'c9', created_at: now, title: 'Purchase order 7' }], renderItem: ({ item }: any) => (item.type === 'po' ? <p>my detail {item.id}</p> : undefined), types: ['request', 'po'] }, []);
    fireEvent.click(await screen.findByText('Purchase order 7'));
    expect(screen.getByText('my detail po-7')).toBeTruthy();
    expect(screen.getAllByRole('tab').map((t) => t.textContent)).toEqual(['All1', 'Approvals']);
  });
});
