// A message's trace (docs/14 §4.3): what happened to one message, from the
// data the server keeps. Metadata only; every look is audited.

import { useEffect, useState } from 'react';
import { api, enc } from './api';
import { Err, Page, time, useAction } from './ui';

const STATE_BADGE: Record<string, string> = {
  delivered: 'ok',
  pending: '',
  dropped: 'bad',
  delivered_or_withdrawn: '',
};

export function MessageTrace() {
  const [id, setId] = useState(() => new URLSearchParams(location.search).get('id') ?? '');
  const [t, setT] = useState<any>(null);
  const a = useAction();
  const load = (msg: string) =>
    a.run(async () => {
      setT(null);
      setT(await api('GET', `/messages/${enc(msg.trim())}/trace`));
    });
  useEffect(() => {
    if (id) load(id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  const m = t?.message;
  return (
    <Page title="Message trace" actions={t && <button onClick={() => load(id)}>Refresh</button>}>
      <div className="card">
        <p className="muted">
          Why a message did or did not arrive: who could see it, which devices have it, what agents and webhooks did, and what happened to it since. Content is never shown; each look is
          recorded in the audit log.
        </p>
        <form
          className="toolbar"
          onSubmit={(e) => {
            e.preventDefault();
            history.replaceState(null, '', `/console/trace?id=${enc(id.trim())}`);
            load(id);
          }}
        >
          <input aria-label="Message id" placeholder="m_…" value={id} onChange={(e) => setId(e.target.value)} required size={40} />
          <button disabled={a.busy}>Trace</button>
        </form>
      </div>
      <Err text={a.err} />
      {m && (
        <>
          <div className="card">
            <h2>Message</h2>
            <dl className="facts">
              <dt>Conversation</dt>
              <dd>
                <code>{m.conv_id}</code> · seq {m.seq} · database {m.database}
              </dd>
              <dt>Sender</dt>
              <dd>
                {m.sender.name} <span className="badge">{m.sender.type}</span>
                {m.on_behalf_of && <> for {m.on_behalf_of}</>}
              </dd>
              <dt>State</dt>
              <dd>
                <span className={`badge ${m.state === 'recalled' ? 'bad' : 'ok'}`}>{m.state}</span> version {m.version}
                {m.state_reason && <> · {m.state_reason}</>}
              </dd>
              <dt>Sent</dt>
              <dd>
                {time(m.created_at)}
                {m.client_msg_id && (
                  <>
                    {' '}
                    · client id <code>{m.client_msg_id}</code>
                  </>
                )}
              </dd>
              {m.causation_id && (
                <>
                  <dt>Caused by</dt>
                  <dd>
                    <a href={`/console/trace?id=${enc(m.causation_id)}`}>
                      <code>{m.causation_id}</code>
                    </a>
                  </dd>
                </>
              )}
              <dt>Trace</dt>
              <dd>{t.trace.trace_id ? t.trace.url ? <a href={t.trace.url} target="_blank" rel="noreferrer"><code>{t.trace.trace_id}</code></a> : <code>{t.trace.trace_id}</code> : <span className="muted">none (sent before tracing)</span>}</dd>
              {m.expires_at && (
                <>
                  <dt>Deleted after</dt>
                  <dd>{time(m.expires_at)}</dd>
                </>
              )}
            </dl>
          </div>
          <div className="card">
            <h2>Delivery</h2>
            <p>
              {t.audience.people} people and {t.audience.agents} agents could see it
              {t.audience.removed_since > 0 && <> ({t.audience.removed_since} removed since)</>}. {t.delivery.delivered} of {t.delivery.devices} devices have it.
              {t.audience.side_events > 0 && <> {t.audience.side_events} side-channel events are anchored at it.</>}
            </p>
            {t.delivery.pending > 0 && (
              <table>
                <thead>
                  <tr>
                    <th>Not yet on</th>
                    <th>Device</th>
                    <th>Up to seq</th>
                    <th>Last seen</th>
                  </tr>
                </thead>
                <tbody>
                  {t.delivery.pending_devices.map((d: any) => (
                    <tr key={d.user + d.device_id}>
                      <td>{d.user}</td>
                      <td>
                        <code>{d.device_id}</code> {d.platform && <span className="badge">{d.platform}</span>}
                      </td>
                      <td>{d.acked_seq ?? <span className="muted">never synced</span>}</td>
                      <td>{time(d.last_seen ?? d.acked_at)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </div>
          {(t.agents.length > 0 || t.replies.length > 0 || t.drafts.length > 0) && (
            <div className="card">
              <h2>Agents</h2>
              <table>
                <thead>
                  <tr>
                    <th>Agent</th>
                    <th>Trigger</th>
                    <th>Devices</th>
                    <th>Replies</th>
                  </tr>
                </thead>
                <tbody>
                  {t.agents.map((g: any) => (
                    <tr key={g.agent}>
                      <td>
                        {g.agent} {g.stopped && <span className="badge bad">stopped</span>}
                      </td>
                      <td>{g.trigger}</td>
                      <td>
                        {g.devices_delivered} have it, {g.devices_pending} not yet
                      </td>
                      <td>
                        {g.replies.map((r: string) => (
                          <a key={r} href={`/console/trace?id=${enc(r)}`}>
                            <code>{r}</code>{' '}
                          </a>
                        ))}
                        {g.drafts.length > 0 && <span className="badge">{g.drafts.length} draft(s) waiting</span>}
                        {g.replies.length === 0 && g.drafts.length === 0 && <span className="muted">—</span>}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
          {t.webhooks.length > 0 && (
            <div className="card">
              <h2>Webhooks</h2>
              <table>
                <thead>
                  <tr>
                    <th>Webhook</th>
                    <th>Event</th>
                    <th>State</th>
                  </tr>
                </thead>
                <tbody>
                  {t.webhooks.map((w: any) => (
                    <tr key={w.webhook + w.seq}>
                      <td>{w.webhook}</td>
                      <td>
                        {w.type} <span className="muted">#{w.seq}</span>
                      </td>
                      <td>
                        <span className={`badge ${STATE_BADGE[w.state] ?? ''}`}>{w.state.replaceAll('_', ' ')}</span>
                        {w.queued_ahead != null && <span className="muted"> · {w.queued_ahead} ahead</span>}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
          <div className="card">
            <h2>History</h2>
            <table>
              <tbody>
                {t.events.map((e: any) => (
                  <tr key={e.seq}>
                    <td className="muted">#{e.seq}</td>
                    <td>{e.type}</td>
                    <td>{e.actor ?? ''}</td>
                    <td>{time(e.at)}</td>
                  </tr>
                ))}
                {t.reports.map((r: any) => (
                  <tr key={r.id}>
                    <td className="muted">report</td>
                    <td>{r.category}</td>
                    <td>
                      <span className="badge">{r.state}</span>
                    </td>
                    <td>
                      <code>{r.id}</code>
                    </td>
                  </tr>
                ))}
                {t.files.map((f: any) => (
                  <tr key={f.file_id}>
                    <td className="muted">file</td>
                    <td>{f.kind}</td>
                    <td>
                      <span className="badge">{f.state}</span>
                    </td>
                    <td>
                      <code>{f.file_id}</code>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </>
      )}
    </Page>
  );
}
