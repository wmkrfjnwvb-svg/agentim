// Every component in fixed states, with a fake client and a fixed clock
// (docs/12 §3.5): for looking at the design and for the screenshot
// comparison. ?theme=dark, ?dir=rtl, ?view=support|app.

import { createRoot } from 'react-dom/client';
import { Observable } from '@agentimhq/client-web/view';
import { AgentIM, Chat, ConversationList, MemberList } from '../src/index.js';

const q = new URLSearchParams(location.search);
if (q.get('theme')) document.documentElement.dataset.theme = q.get('theme')!;
if (q.get('dir') === 'rtl') document.documentElement.dir = 'rtl';

const NOW = Date.parse('2026-10-26T17:30:00Z');
const at = (iso: string) => Date.parse(iso);
const ME = 'u_maya';

let seq = 0;
const m = (sender: [string, string, string], content: string, iso: string, extra: any = {}) => ({
  key: `m${++seq}`,
  msg_id: `m${seq}`,
  conv_id: 'c_support',
  sender_id: sender[0],
  sender_name: sender[1],
  sender_type: sender[2],
  content,
  created_at: at(iso),
  created_seq: seq,
  order: seq,
  status: 'sent',
  state: 'committed',
  version: 1,
  ...extra,
});
const maya: [string, string, string] = [ME, 'Maya Chen', 'human'];
const nova: [string, string, string] = ['u_nova', 'Nova', 'agent'];
const jordan: [string, string, string] = ['u_jordan', 'Jordan Lee', 'human'];

const messages = [
  m(maya, 'Hi! My order #4821 still says “processing” and it’s been a week.', '2026-10-25T21:10:00Z'),
  m(nova, 'Sorry about the wait, Maya. Let me look that up for you.', '2026-10-25T21:10:20Z'),
  m(nova, 'Your order shipped this morning with UPS. It should arrive by Thursday, Oct 29.', '2026-10-25T21:10:40Z', {
    envelope: { sources: [{ title: 'Order #4821', url: 'https://example.com/orders/4821' }, { title: 'Shipping policy', url: 'https://example.com/shipping' }] },
  }),
  m(maya, 'Great, thanks. Can I still change the delivery address?', '2026-10-26T16:58:00Z'),
  m(nova, 'Updating addresses needs a quick confirmation.', '2026-10-26T16:58:30Z', {
    kind: 'agentim.card',
    payload: {
      title: 'Change delivery address?',
      blocks: [
        { type: 'text', text: 'UPS allows one change before the package is out for delivery.' },
        { type: 'buttons', id: 'b1', options: [{ id: 'yes', label: 'Change address', style: 'primary' }, { id: 'no', label: 'Keep it' }] },
      ],
    },
  }),
  m(jordan, 'Hey Maya, Jordan from support here. I’ve got this one.', '2026-10-26T17:20:00Z'),
  m(jordan, 'What’s the new address?', '2026-10-26T17:20:15Z'),
  m(maya, '500 Market St, San Francisco, CA 94105', '2026-10-26T17:24:00Z'),
  m(maya, 'Apt 12B', '2026-10-26T17:24:10Z', { status: 'failed', key: 'local:f', client_msg_id: 'f' }),
  m(nova, 'Updated. You’ll get a new tracking email shortly', '2026-10-26T17:25:00Z', { state: 'streaming', streamed: 'Updated. You’ll get a new tracking email shortly' }),
];

const conversations = [
  { conv_id: 'c_support', title: 'Acme Support', unread: 0, last_message: messages[messages.length - 1], members: [
    { user_id: ME, name: 'Maya Chen', kind: 'human', role: 'member' },
    { user_id: 'u_jordan', name: 'Jordan Lee', kind: 'human', role: 'admin' },
    { user_id: 'u_nova', name: 'Nova', kind: 'agent', role: 'member' },
  ] },
  { conv_id: 'c_design', title: 'Design team', unread: 3, last_message: { content: 'New mocks are in Figma 🎨', sender_id: 'u_jordan', created_at: at('2026-10-26T15:02:00Z') } },
  { conv_id: 'c_launch', title: 'Launch checklist', unread: 0, last_message: { content: 'Ship it Thursday?', sender_id: ME, created_at: at('2026-10-25T19:00:00Z') } },
  { conv_id: 'c_nova', title: 'Nova', unread: 0, last_message: { content: 'Here’s your weekly summary.', sender_id: 'u_nova', created_at: at('2026-10-22T13:00:00Z') } },
  { conv_id: 'c_alex', title: 'Alex Rivera', unread: 0, last_message: { content: 'See you at the offsite!', sender_id: 'u_alex', created_at: at('2026-09-30T13:00:00Z') } },
];

class View extends Observable<any[]> {
  hasOlder = false;
  olderEnd = 'start';
  async loadOlder() {}
}
const view = new View(messages);
const noop = async () => ({});
const listeners = new Map<string, Set<(e: any) => void>>();
const client: any = {
  connection: new Observable<any>({ state: q.get('offline') ? 'disconnected' : 'connected' }),
  conversations: new Observable<any[]>(conversations),
  messages: () => view,
  send: noop,
  retry: noop,
  discard: noop,
  feedback: noop,
  respond: noop,
  read: noop,
  call: noop,
  on: (t: string, f: (e: any) => void) => {
    let s = listeners.get(t);
    if (!s) listeners.set(t, (s = new Set()));
    s.add(f);
    return () => s!.delete(f);
  },
};
setTimeout(() => listeners.get('signal')?.forEach((f) => f({ conv_id: 'c_support', kind: 'typing', from: 'u_jordan' })), 50);

const support = q.get('view') === 'support';
createRoot(document.getElementById('root')!).render(
  <AgentIM client={client} me={ME} locale="en-US" timeZone="America/Los_Angeles" now={() => NOW}>
    {support ? (
      <div className="support">
        <Chat convId="c_support" title="Acme Support" subtitle="Nova (AI) and our team typically reply in minutes" />
      </div>
    ) : (
      <div className="app">
        <aside className="side">
          <div className="side-head">Messages</div>
          <ConversationList selected="c_support" onSelect={() => {}} />
        </aside>
        <main className="main">
          <Chat convId="c_support" title="Acme Support" subtitle="3 members" />
        </main>
        <aside className="info">
          <div className="side-head">Members</div>
          <MemberList convId="c_support" />
        </aside>
      </div>
    )}
  </AgentIM>,
);
