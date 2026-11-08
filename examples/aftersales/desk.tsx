// The manager's desk: the approval inbox (refunds over $30, hand-offs,
// docs/en/design/chat-components.md) next to the conversation, with take over. Everything Sam
// decides here is decided in the inbox; the chat is to read and answer.
import { useState } from 'react';
import { Chat, Inbox } from '@agentimhq/react';
import { mount, usePoll } from '../lib/session.js';

mount('sam', (s, client) => <Desk s={s} client={client} />);

function Desk({ s, client }: { s: any; client: any }) {
  const [busy, setBusy] = useState(false);
  // Who has the conversation: a hand-off waits in the inbox.
  const inbox = usePoll(() => client.call('inbox'), 1500) as any;
  const handedToMe = inbox?.items?.some((i: any) => i.type === 'handoff' && i.conv_id === s.conv);
  const takeOver = async () => {
    setBusy(true);
    try {
      await client.call('takeover', { conv_id: s.conv, agent_id: s.agent });
    } finally {
      setBusy(false);
    }
  };
  return (
    <>
      <header className="ex-top">
        <strong>Acme · After-sales desk</strong>
        <span className="ex-muted">Signed in as {s.name}</span>
      </header>
      <main className="ex-desk">
        <section className="ex-pane" aria-label="Inbox">
          <div className="ex-pane-head">
            <strong>Inbox</strong>
            <span>Refunds over $30 wait for you; up to $30 the customer confirms them. Address changes run at once.</span>
          </div>
          <Inbox onOpen={() => document.querySelector<HTMLTextAreaElement>('.aim-composer textarea')?.focus()} />
        </section>
        <section className="ex-pane" aria-label="Conversation">
          <div className="ex-pane-head" data-testid="status">
            <strong>Maya Chen</strong>
            <span>{handedToMe ? 'You have it' : 'The assistant is answering'}</span>
            {handedToMe ? null : (
              <button className="ex-btn" disabled={busy} onClick={takeOver}>
                Take over
              </button>
            )}
          </div>
          <div className="ex-chat">
            <Chat convId={s.conv} />
          </div>
        </section>
      </main>
    </>
  );
}
