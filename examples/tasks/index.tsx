// The task room: the channel, who claimed what, and the task's state.
import { Chat } from '@agentimhq/react';
import { mount, usePoll } from '../lib/session.js';

mount('pat', (s, client) => (
  <>
    <header className="ex-top">
      <strong>Task room</strong>
      <span className="ex-muted">Signed in as {s.name}</span>
    </header>
    <main className="ex-layout">
      <div className="ex-chat">
        <Chat convId={s.conv} />
      </div>
      <Status s={s} client={client} />
    </main>
  </>
));

function Status({ s, client }: { s: any; client: any }) {
  const room = usePoll(() => fetch('/api/room').then((r) => r.json()), 800) as any;
  const st = room?.state ?? 'submitted';
  const claims: string[] = room?.claims ?? [];
  return (
    <aside className="ex-side">
      <div className="ex-card" data-testid="task">
        <h3>Task</h3>
        <p>
          <span className="ex-badge" data-state={st}>
            {st}
          </span>
        </p>
        <div className="ex-row">
          <button className="ex-btn" disabled={['completed', 'failed', 'canceled'].includes(st)} onClick={() => client.call('update_task', { conv_id: s.conv, state: 'canceled', note: 'Stopped by Pat' })}>
            Cancel the task
          </button>
        </div>
      </div>
      <div className="ex-card" data-testid="claims">
        <h3>Who took each step</h3>
        {claims.length === 0 ? <p className="ex-muted">Steps the lead posts are claimed here.</p> : null}
        <ul className="ex-list">
          {claims.map((who, i) => (
            <li key={i}>{who} claimed step {i + 1}</li>
          ))}
        </ul>
      </div>
    </aside>
  );
}
