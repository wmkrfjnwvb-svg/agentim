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

// A picture drawn here (no network): a sunset at 4:3.
const picture = (w: number, h: number) =>
  `data:image/svg+xml,${encodeURIComponent(`<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="${h}"><defs><linearGradient id="s" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#f97316"/><stop offset="0.6" stop-color="#fb7185"/><stop offset="1" stop-color="#312e81"/></linearGradient></defs><rect width="100%" height="100%" fill="url(#s)"/><circle cx="${w * 0.62}" cy="${h * 0.58}" r="${h * 0.16}" fill="#fde68a"/><path d="M0 ${h * 0.78} Q ${w * 0.3} ${h * 0.68} ${w * 0.55} ${h * 0.8} T ${w} ${h * 0.74} V ${h} H 0 Z" fill="#1e1b4b"/></svg>`)}`;
const files = [
  m(jordan, 'Here are the photos from the venue and the contract.', '2026-10-26T17:01:00Z'),
  m(jordan, '[Image]', '2026-10-26T17:01:10Z', { kind: 'image', payload: { file_id: 'f_sunset', name: 'IMG_2041.jpg', width: 4032, height: 3024, thumb: true, caption: 'View from the terrace' } }),
  m(jordan, '[File] Venue contract (signed).pdf', '2026-10-26T17:01:20Z', { kind: 'file', payload: { file_id: 'f_pdf', name: 'Venue contract (signed).pdf', size: 2_412_000, mime: 'application/pdf' } }),
  m(jordan, '[File] Budget Q4.xlsx', '2026-10-26T17:01:25Z', { kind: 'file', payload: { file_id: 'f_xls', name: 'Budget Q4.xlsx', size: 88_400 } }),
  m(maya, '[Image]', '2026-10-26T17:20:00Z', { kind: 'image', payload: { file_id: 'f_tall', name: 'floorplan.png', width: 900, height: 1600, thumb: true } }),
  m(maya, '[File] Seating plan v3.key', '2026-10-26T17:21:00Z', { kind: 'file', payload: { file_id: 'f_key', name: 'Seating plan v3.key', size: 18_200_000 } }),
  m(maya, '[File] All photos.zip', '2026-10-26T17:24:00Z', { kind: 'file', status: 'uploading', key: 'local:z', client_msg_id: 'z', upload: { sent: 148_000_000, size: 412_000_000 }, payload: { name: 'All photos.zip', size: 412_000_000 } }),
  m(maya, '[Image]', '2026-10-26T17:24:30Z', { kind: 'image', status: 'uploading', key: 'local:i', client_msg_id: 'i', upload: { sent: 1_300_000, size: 3_100_000 }, payload: { name: 'IMG_2050.jpg', width: 3024, height: 4032, local_thumb: 'sunset-tall' } }),
];
const thumbs: Record<string, string> = { f_sunset: picture(320, 240), f_tall: picture(180, 320), 'sunset-tall': picture(240, 320) };

class View extends Observable<any[]> {
  hasOlder = false;
  olderEnd = 'start';
  async loadOlder() {}
}
const filesView = q.get('view') === 'files';
const view = new View(filesView ? files : messages);
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
  sendFile: noop,
  fileUrl: async (_c: string, id: string, variant: string) => ({ url: variant === 'thumb' ? (thumbs[id] ?? picture(320, 240)) : picture(1600, 1200), variant, expires_at: 0 }),
  localPreview: (k?: string) => (k ? thumbs[k] : undefined),
  on: (t: string, f: (e: any) => void) => {
    let s = listeners.get(t);
    if (!s) listeners.set(t, (s = new Set()));
    s.add(f);
    return () => s!.delete(f);
  },
};
setTimeout(() => listeners.get('signal')?.forEach((f) => f({ conv_id: 'c_support', kind: 'typing', from: 'u_jordan' })), 50);

const support = q.get('view') === 'support' || filesView;
// ?open=1: the viewer open on the first image.
if (q.get('open')) {
  const tryOpen = () => {
    const b = document.querySelector<HTMLButtonElement>('.aim-image img') ? document.querySelector<HTMLButtonElement>('.aim-image') : null;
    if (b) b.click();
    else setTimeout(tryOpen, 30);
  };
  setTimeout(tryOpen, 250);
}
createRoot(document.getElementById('root')!).render(
  <AgentIM client={client} me={ME} locale="en-US" timeZone="America/Los_Angeles" now={() => NOW}>
    {support ? (
      <div className="support">
        <Chat convId="c_support" title={filesView ? 'Offsite planning' : 'Acme Support'} subtitle={filesView ? 'Maya Chen, Jordan Lee' : 'Nova (AI) and our team typically reply in minutes'} />
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
