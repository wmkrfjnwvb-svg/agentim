// The theme preview (docs/en/design/chat-components.md): pick an accent colour, the corner
// radius and light or dark, see our real components (a chat with a data
// card, a form and steps, and the approval inbox) change, and copy the CSS
// variables for your app. Nothing is saved: the theme lives in your code.

import { useEffect, useMemo, useState } from 'react';
import { Observable } from '../../../sdk/client-web/src/view';
import { AgentIM, Chat, Inbox } from '../../../sdk/react/src/index';
import '../../../sdk/react/theme.css';
import tokens from '../../../sdk/react/tokens.json';

const ME = 'u_maya';
const NOW = Date.parse('2026-10-26T17:30:00Z');
let seq = 0;
const m = (sender: [string, string, string], content: string, iso: string, extra: any = {}) => ({
  key: `m${++seq}`,
  msg_id: `m${seq}`,
  conv_id: 'c',
  sender_id: sender[0],
  sender_name: sender[1],
  sender_type: sender[2],
  content,
  created_at: Date.parse(iso),
  created_seq: seq,
  order: seq,
  status: 'sent',
  state: 'committed',
  version: 1,
  ...extra,
});
const maya: [string, string, string] = [ME, 'Maya Chen', 'human'];
const nova: [string, string, string] = ['u_nova', 'Nova', 'agent'];

class View extends Observable<any[]> {
  hasOlder = false;
  olderEnd = 'start';
  async loadOlder() {}
}

const inboxItems = [
  { type: 'request', request_id: 'q1', kind: 'approve', conv_id: 'c1', conv_title: 'Maya Chen', agent_name: 'Nova', asker_name: 'Maya Chen', created_at: NOW - 4 * 60_000, expires_at: NOW + 3_000_000, title: 'x', context: { operation: { name: 'createRefund', description: 'Refund an order', fields: [{ name: 'order', value: '1002' }, { name: 'amount', value: 120 }], amount: 120, rule: { amount_field: 'amount', over: 30 } }, tool_call: { msg: { content: 'Please refund it.', sender_name: 'Maya Chen' } } } },
  { type: 'draft', draft_id: 'd1', conv_id: 'c3', conv_title: 'Chris Wong', agent_name: 'Aide', content: 'Could we look at your plan together on Thursday at 10?', created_at: NOW - 9 * 60_000 },
  { type: 'handoff', conv_id: 'c4', agent_id: 'u_nova', agent_name: 'Nova', conv_title: 'Priya Shah', note: 'Wants to speak to a manager', created_at: NOW - 30 * 60_000 },
];

function fakeClient() {
  const view = new View([
    m(maya, 'Where is my order 1002?', '2026-10-26T17:20:00Z'),
    m(nova, 'Order 1002', '2026-10-26T17:20:06Z', {
      msg_id: 'r1',
      kind: 'agentim.card',
      payload: { title: 'Order 1002', blocks: [{ type: 'facts', items: [{ label: 'Item', value: 'Bluetooth speaker' }, { label: 'Total', value: 120, format: 'currency:USD' }, { label: 'Status', value: 'Delayed', tone: 'warning' }] }] },
    }),
    m(nova, 'Refund', '2026-10-26T17:21:04Z', {
      kind: 'agentim.card',
      payload: { title: 'Refund order 1002', to: [ME], blocks: [{ type: 'form', id: 'form', submit: 'Request refund', fields: [{ id: 'reason', label: 'Reason', type: 'select', options: ['Arrived late', 'Damaged'], placeholder: 'Choose a reason', required: true }] }] },
    }),
    m(nova, 'The refund needs a manager’s approval. You’ll see the answer here.', '2026-10-26T17:22:40Z', { msg_id: 'r4' }),
  ]);
  const listeners = new Map<string, Set<(e: any) => void>>();
  const noop = async () => ({});
  const client: any = {
    connection: new Observable<any>({ state: 'connected' }),
    conversations: new Observable<any[]>([]),
    messages: () => view,
    send: noop,
    edit: noop,
    recall: noop,
    retry: noop,
    discard: noop,
    feedback: noop,
    respond: noop,
    read: noop,
    focus: noop,
    sendFile: noop,
    call: async (op: string) => (op === 'inbox' ? { items: inboxItems, more: false } : op === 'card_answers' ? { answers: [] } : {}),
    on: (t: string, f: (e: any) => void) => {
      let s = listeners.get(t);
      if (!s) listeners.set(t, (s = new Set()));
      s.add(f);
      return () => s!.delete(f);
    },
  };
  const steps = () => {
    const at = Date.parse('2026-10-26T17:22:31Z');
    const send = (data: any) => listeners.get('side')?.forEach((f) => f({ event: { type: 'progress', conv_id: 'c', created_at: at, data: { kind: data.steps ? 'plan' : 'step', text: data.title ?? 'plan', msg_id: 'r4', data } } }));
    send({ steps: [{ id: 'a', title: 'Check the refund rules' }, { id: 'b', title: 'Refund $120' }] });
    send({ id: 'a', state: 'done', title: 'Check the refund rules' });
    send({ id: 'b', state: 'waiting', title: 'Refund $120', detail: 'Waiting for approval from Sam' });
  };
  return { client, steps };
}

/** A colour a bit darker (light) or lighter (dark), for text on soft backgrounds. */
function shade(hex: string, amount: number): string {
  const n = parseInt(hex.slice(1), 16);
  const c = [n >> 16, (n >> 8) & 255, n & 255].map((v) => Math.max(0, Math.min(255, Math.round(amount < 0 ? v * (1 + amount) : v + (255 - v) * amount))));
  return `#${c.map((v) => v.toString(16).padStart(2, '0')).join('')}`;
}

const ACCENTS = ['#2563eb', '#0f766e', '#7c3aed', '#be123c', '#c2410c', '#18181b'];

export default function ThemePreview() {
  const { client, steps } = useMemo(fakeClient, []);
  const [accent, setAccent] = useState('#2563eb');
  const [radius, setRadius] = useState(12);
  const [scheme, setScheme] = useState<'light' | 'dark'>('light');
  const [view, setView] = useState<'chat' | 'inbox'>('chat');
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    const t = setTimeout(steps, 50);
    return () => clearTimeout(t);
  }, [steps, view]);
  const dark = scheme === 'dark';
  // The page's own light or dark is the site's; the stage takes the tokens itself.
  const base = Object.fromEntries(Object.entries((tokens as any).color[scheme]).map(([k, v]) => [`--aim-${k}`, String(v)]));
  const derived = (d: boolean): Record<string, string> => ({
    '--aim-accent': accent,
    '--aim-accent-text': d ? shade(accent, 0.45) : shade(accent, -0.15),
    '--aim-accent-soft': d ? shade(accent, -0.7) : shade(accent, 0.92),
    '--aim-focus': d ? shade(accent, 0.35) : accent,
    '--aim-bubble-mine': accent,
  });
  const radii: Record<string, string> = {
    '--aim-radius-card': `${radius}px`,
    '--aim-radius-control': `${Math.max(4, Math.round(radius * 0.66))}px`,
    '--aim-radius-bubble': `${Math.round(radius * 1.5)}px`,
  };
  const vars = { ...derived(dark), ...radii };
  const block = (sel: string, v: Record<string, string>) => `${sel} {\n${Object.entries(v).map(([k, x]) => `  ${k}: ${x};`).join('\n')}\n}`;
  const darkOnly = Object.fromEntries(Object.entries(derived(true)).filter(([k]) => k !== '--aim-accent' && k !== '--aim-bubble-mine'));
  const css = [
    block(':root', { ...derived(false), ...radii }),
    `@media (prefers-color-scheme: dark) {\n${block('  :root:not([data-theme="light"])', darkOnly).replace(/\n/g, '\n  ')}\n}`,
    block(':root[data-theme="dark"]', darkOnly),
  ].join('\n\n');
  return (
    <div className="theme-preview not-content">
      <div className="tp-controls">
        <fieldset>
          <legend>Accent</legend>
          <div className="tp-swatches">
            {ACCENTS.map((c) => (
              <button key={c} type="button" aria-label={c} aria-pressed={accent === c} style={{ background: c }} onClick={() => setAccent(c)} />
            ))}
            <input type="color" aria-label="Custom accent" value={accent} onChange={(e) => setAccent(e.target.value)} />
          </div>
        </fieldset>
        <label>
          Corner radius <span>{radius}px</span>
          <input type="range" min={0} max={20} value={radius} onChange={(e) => setRadius(Number(e.target.value))} />
        </label>
        <fieldset>
          <legend>Mode</legend>
          <div className="tp-seg">
            {(['light', 'dark'] as const).map((s) => (
              <button key={s} type="button" aria-pressed={scheme === s} onClick={() => setScheme(s)}>
                {s === 'light' ? 'Light' : 'Dark'}
              </button>
            ))}
          </div>
        </fieldset>
        <fieldset>
          <legend>Show</legend>
          <div className="tp-seg">
            {(['chat', 'inbox'] as const).map((v) => (
              <button key={v} type="button" aria-pressed={view === v} onClick={() => setView(v)}>
                {v === 'chat' ? 'Chat' : 'Inbox'}
              </button>
            ))}
          </div>
        </fieldset>
      </div>
      <div className="tp-stage" data-theme={scheme} style={{ ...base, ...vars } as any}>
        <AgentIM client={client} me={ME} locale="en-US" timeZone="America/Los_Angeles" now={() => NOW}>
          {view === 'chat' ? <Chat convId="c" title="Acme Support" subtitle="Nova (AI) · orders and refunds" /> : <Inbox onOpen={() => {}} />}
        </AgentIM>
      </div>
      <div className="tp-css">
        <pre>
          <code>{css}</code>
        </pre>
        <button
          type="button"
          onClick={() => {
            navigator.clipboard?.writeText(css).then(() => {
              setCopied(true);
              setTimeout(() => setCopied(false), 1500);
            });
          }}
        >
          {copied ? 'Copied' : 'Copy CSS'}
        </button>
      </div>
    </div>
  );
}
