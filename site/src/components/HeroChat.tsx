// The chat window on the homepage: our own React components with a fake
// client (the screenshot gallery's data) and an agent reply that streams
// in, unless the visitor prefers reduced motion.

import { useEffect, useMemo } from 'react';
import { Observable } from '../../../sdk/client-web/src/view';
import { AgentIM, Chat } from '../../../sdk/react/src/index';
import '../../../sdk/react/theme.css';

const ME = 'u_maya';
const NOW = Date.parse('2026-10-26T17:30:00Z');
let seq = 0;
const m = (sender: [string, string, string], content: string, iso: string, extra: any = {}) => ({
  key: `m${++seq}`,
  msg_id: `m${seq}`,
  conv_id: 'c_support',
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

const REPLY = 'It shipped this morning with UPS and should arrive Thursday, Oct 29. Want me to send the tracking link?';

class View extends Observable<any[]> {
  hasOlder = false;
  olderEnd = 'start';
  async loadOlder() {}
}

export default function HeroChat() {
  const { client, view } = useMemo(() => {
    const view = new View([
      m(maya, 'Hi! Where is my order #4821?', '2026-10-26T17:28:00Z'),
      m(nova, 'Let me check that for you.', '2026-10-26T17:28:10Z', {
        envelope: { sources: [{ title: 'Order #4821', url: '#' }] },
      }),
      m(nova, '', '2026-10-26T17:28:20Z', { key: 'reply', msg_id: 'reply', state: 'streaming', streamed: '' }),
    ]);
    const noop = async () => ({});
    const client: any = {
      connection: new Observable<any>({ state: 'connected' }),
      conversations: new Observable<any[]>([{ conv_id: 'c_support', title: 'Acme Support', unread: 0, members: [] }]),
      messages: () => view,
      send: async (_c: string, text: string) => {
        view.set([...view.get(), m(maya, text, new Date(NOW).toISOString())]);
        return {};
      },
      edit: noop,
      recall: noop,
      retry: noop,
      discard: noop,
      feedback: noop,
      respond: noop,
      read: noop,
      call: noop,
      on: () => () => {},
    };
    return { client, view };
  }, []);

  useEffect(() => {
    const finish = () => view.set(view.get().map((x) => (x.key === 'reply' ? { ...x, content: REPLY, state: 'committed', streamed: undefined } : x)));
    if (typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches) {
      finish();
      return;
    }
    const words = REPLY.split(' ');
    let i = 0;
    const t = setInterval(() => {
      i++;
      if (i > words.length) {
        clearInterval(t);
        finish();
        return;
      }
      const text = words.slice(0, i).join(' ');
      view.set(view.get().map((x) => (x.key === 'reply' ? { ...x, content: text, streamed: text } : x)));
    }, 110);
    return () => clearInterval(t);
  }, [view]);

  return (
    <AgentIM client={client} me={ME} locale="en-US" timeZone="America/Los_Angeles" now={() => NOW}>
      <Chat convId="c_support" title="Acme Support" subtitle="Nova (AI) and our team" />
    </AgentIM>
  );
}
