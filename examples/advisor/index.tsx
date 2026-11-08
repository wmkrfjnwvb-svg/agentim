// The advisor's page: the conversation, the aide's drafts in the inbox
// (docs/en/design/chat-components.md), and its private notes. The client's page (?as=chris) is the conversation only.
import { useEffect, useState } from 'react';
import { Chat, Inbox } from '@agentimhq/react';
import { mount } from '../lib/session.js';

mount('ava', (s, client) => (
  <>
    <header className="ex-top">
      <strong>{s.advisor ? 'Ava Finance — advisor desk' : 'Ava Finance'}</strong>
      <span className="ex-muted">Signed in as {s.name}</span>
    </header>
    <main className={s.advisor ? 'ex-layout' : 'ex-layout ex-single'}>
      {s.advisor ? null : <p className="ex-muted">Messages with your advisor, Ava. An assistant helps Ava answer; Ava reviews every reply.</p>}
      <div className="ex-chat">
        <Chat convId={s.conv} />
      </div>
      {s.advisor ? (
        <aside className="ex-side">
          <Notes client={client} conv={s.conv} />
          <div className="ex-inbox" data-testid="drafts">
            <Inbox types={['draft']} />
          </div>
        </aside>
      ) : null}
    </main>
  </>
));

/** The aide's private notes (side events of type `ephemeral`, to Ava only). */
function Notes({ client, conv }: { client: any; conv: string }) {
  const [notes, setNotes] = useState<{ id: number; text: string }[]>([]);
  useEffect(
    () =>
      client.on('side', (e: any) => {
        const ev = e?.event ?? e;
        if (ev?.conv_id === conv && ev.type === 'ephemeral') setNotes((n) => [...n, { id: ev.side_seq ?? n.length, text: ev.data?.text ?? '' }]);
      }),
    [client, conv],
  );
  return (
    <div className="ex-card" data-testid="notes">
      <h3>Notes from your aide</h3>
      <p className="ex-muted">Only you see these.</p>
      {notes.length === 0 ? <p className="ex-muted">None yet.</p> : null}
      {notes.map((n) => (
        <p key={n.id} data-testid="note">
          {n.text}
        </p>
      ))}
    </div>
  );
}
