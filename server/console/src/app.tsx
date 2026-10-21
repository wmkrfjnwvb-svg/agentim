// The app area (/admin/v1/app): what the developer of the app manages.

import { useState } from 'react';
import { api, can, enc, qs, type Me } from './api';
import { Err, Json, Page, Secret, time, useAction, useLoad } from './ui';

export function Overview() {
  const [v, err] = useLoad(() => api('GET', '/app/overview'), []);
  return (
    <Page title="Overview">
      <Err text={err} />
      {v && (
        <div className="tiles">
          {(
            [
              ['People', v.people],
              ['Agents', v.agents],
              ['Conversations', v.conversations],
              ['Online', v.online],
              ['Open reports', v.open_reports],
              ['API keys', v.api_keys],
              ['Signing keys', v.signing_keys],
            ] as [string, number][]
          ).map(([k, n]) => (
            <div className="tile" key={k}>
              <div className="n">{n}</div>
              <div className="muted">{k}</div>
            </div>
          ))}
        </div>
      )}
    </Page>
  );
}

const SWITCHES = ['create_groups', 'create_direct', 'add_members', 'find_users', 'create_agents'] as const;

export function Users({ me }: { me: Me }) {
  const [q, setQ] = useState('');
  const [kind, setKind] = useState('');
  const [after, setAfter] = useState<string[]>([]);
  const [open, setOpen] = useState<string | null>(null);
  const cur = after[after.length - 1];
  const [v, err] = useLoad(() => api('GET', `/app/users${qs({ q, kind, after: cur, limit: 50 })}`), [q, kind, cur]);
  return (
    <Page title="People and agents">
      <div className="toolbar">
        <input placeholder="Search by id" value={q} onChange={(e) => (setQ(e.target.value), setAfter([]))} />
        <select value={kind} onChange={(e) => (setKind(e.target.value), setAfter([]))}>
          <option value="">Everyone</option>
          <option value="human">People</option>
          <option value="agent">Agents</option>
        </select>
      </div>
      <Err text={err} />
      <table>
        <thead>
          <tr>
            <th>Id</th>
            <th>Kind</th>
            <th>Owner</th>
            <th>State</th>
          </tr>
        </thead>
        <tbody>
          {v?.users.map((u: any) => (
            <tr key={u.id} className="click" onClick={() => setOpen(u.id)}>
              <td>{u.id}</td>
              <td>{u.kind}</td>
              <td>{u.owner ?? ''}</td>
              <td>
                {u.online && <span className="badge ok">online</span>}
                {u.deactivated && <span className="badge bad">deactivated</span>}
                {u.stopped && <span className="badge bad">stopped</span>}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      <div className="pager">
        <button disabled={!after.length} onClick={() => setAfter(after.slice(0, -1))}>
          Previous
        </button>
        <button disabled={!v?.has_more} onClick={() => setAfter([...after, v.users[v.users.length - 1].id])}>
          Next
        </button>
      </div>
      {open && <UserDetail id={open} me={me} onClose={() => setOpen(null)} />}
    </Page>
  );
}

function UserDetail({ id, me, onClose }: { id: string; me: Me; onClose: () => void }) {
  const [u, err, reload] = useLoad(() => api('GET', `/app/users/${enc(id)}`), [id]);
  const a = useAction();
  const write = can.app(me.admin.role);
  const setPerm = (k: string, v: boolean | number | null) =>
    a.run(async () => {
      await api('PATCH', `/app/users/${enc(id)}`, { permissions: { [k]: v } });
      reload();
    });
  return (
    <div className="drawer" role="dialog" aria-label={`user ${id}`}>
      <header className="page-head">
        <h2>{id}</h2>
        <button onClick={onClose}>Close</button>
      </header>
      <Err text={err ?? a.err} />
      {u && (
        <>
          <p>
            {u.kind} {u.owner && <>· owned by {u.owner}</>} {u.deactivated && <span className="badge bad">deactivated</span>}
          </p>
          {write && u.kind === 'human' && (
            <button
              onClick={() =>
                a.run(async () => {
                  await api('POST', `/app/users/${enc(id)}:${u.deactivated ? 'reactivate' : 'deactivate'}`);
                  reload();
                })
              }
            >
              {u.deactivated ? 'Reactivate' : 'Deactivate'}
            </button>
          )}
          {u.permissions && (
            <>
              <h3>What they may do in their app</h3>
              <table className="perms">
                <tbody>
                  {SWITCHES.map((k) => {
                    const o = u.permission_overrides?.[k];
                    return (
                      <tr key={k}>
                        <td>{k.replace(/_/g, ' ')}</td>
                        <td>{u.permissions[k] ? 'yes' : 'no'}</td>
                        <td className="muted">{o === undefined ? 'app default' : 'set for this person'}</td>
                        {write && (
                          <td>
                            <select
                              aria-label={k}
                              value={o === undefined ? '' : String(o)}
                              onChange={(e) => setPerm(k, e.target.value === '' ? null : e.target.value === 'true')}
                            >
                              <option value="">app default</option>
                              <option value="true">allow</option>
                              <option value="false">refuse</option>
                            </select>
                          </td>
                        )}
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </>
          )}
          <h3>Devices</h3>
          <table>
            <tbody>
              {u.devices?.map((d: any) => (
                <tr key={d.device_id}>
                  <td>{d.name ?? d.device_id}</td>
                  <td>{d.platform ?? ''}</td>
                  <td>{time(d.last_seen)}</td>
                  <td>
                    {write && (
                      <button
                        onClick={() =>
                          a.run(async () => {
                            await api('DELETE', `/app/users/${enc(id)}/devices/${enc(d.device_id)}`);
                            reload();
                          })
                        }
                      >
                        Log out
                      </button>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          {u.agents?.length > 0 && (
            <>
              <h3>Agents</h3>
              <ul>
                {u.agents.map((x: any) => (
                  <li key={x.id}>
                    {x.id} {x.team && '(team)'} {x.stopped && <span className="badge bad">stopped</span>}
                  </li>
                ))}
              </ul>
            </>
          )}
        </>
      )}
    </div>
  );
}

export function Conversations() {
  const [q, setQ] = useState('');
  const [after, setAfter] = useState<string[]>([]);
  const [open, setOpen] = useState<string | null>(null);
  const cur = after[after.length - 1];
  const [v, err] = useLoad(() => api('GET', `/app/conversations${qs({ q, after: cur, limit: 50 })}`), [q, cur]);
  return (
    <Page title="Conversations">
      <div className="toolbar">
        <input placeholder="Title or id" value={q} onChange={(e) => (setQ(e.target.value), setAfter([]))} />
      </div>
      <Err text={err} />
      <table>
        <thead>
          <tr>
            <th>Id</th>
            <th>Kind</th>
            <th>Title</th>
            <th>Members</th>
            <th>Messages</th>
          </tr>
        </thead>
        <tbody>
          {v?.conversations.map((c: any) => (
            <tr key={c.id} className="click" onClick={() => setOpen(c.id)}>
              <td>
                <code>{c.id}</code>
              </td>
              <td>{c.kind}</td>
              <td>{c.title}</td>
              <td>{c.members}</td>
              <td>{c.last_seq}</td>
            </tr>
          ))}
        </tbody>
      </table>
      <div className="pager">
        <button disabled={!after.length} onClick={() => setAfter(after.slice(0, -1))}>
          Previous
        </button>
        <button disabled={!v?.has_more} onClick={() => setAfter([...after, v.conversations[v.conversations.length - 1].id])}>
          Next
        </button>
      </div>
      {open && <ConversationDetail id={open} onClose={() => setOpen(null)} />}
    </Page>
  );
}

function ConversationDetail({ id, onClose }: { id: string; onClose: () => void }) {
  const [c, err] = useLoad(() => api('GET', `/app/conversations/${enc(id)}`), [id]);
  const [reason, setReason] = useState('');
  const [msgs, setMsgs] = useState<any[] | null>(null);
  const a = useAction();
  return (
    <div className="drawer" role="dialog" aria-label={`conversation ${id}`}>
      <header className="page-head">
        <h2>{c?.title || id}</h2>
        <button onClick={onClose}>Close</button>
      </header>
      <Err text={err ?? a.err} />
      {c && (
        <>
          <h3>Members</h3>
          <table>
            <tbody>
              {c.members.map((m: any) => (
                <tr key={m.user}>
                  <td>{m.user}</td>
                  <td>{m.kind}</td>
                  <td>{m.role}</td>
                </tr>
              ))}
            </tbody>
          </table>
          <h3>Content</h3>
          {msgs === null ? (
            <form
              onSubmit={(e) => {
                e.preventDefault();
                a.run(async () => {
                  const v = await api('GET', `/app/conversations/${enc(id)}/messages${qs({ reason, limit: 200 })}`);
                  setMsgs(v.events ?? []);
                });
              }}
            >
              <p className="muted">Reading messages is recorded in the audit log with your reason.</p>
              <label>
                Reason
                <input name="reason" value={reason} maxLength={500} onChange={(e) => setReason(e.target.value)} placeholder="e.g. report rep_…" required />
              </label>
              <button disabled={a.busy}>Read messages</button>
            </form>
          ) : (
            <div className="messages">
              {msgs.length === 0 && <p className="muted">No messages.</p>}
              {msgs.map((w: any, i: number) => {
                const m = w.event ?? w;
                return (
                  <div className="message" key={i}>
                    <span className="muted">
                      #{m.seq} {m.type} {m.msg?.sender_name ?? ''} · {time(m.created_at)}
                    </span>
                    <div>{text(m)}</div>
                  </div>
                );
              })}
            </div>
          )}
        </>
      )}
    </div>
  );
}

/// The readable part of a log event: a message's text, or its type.
function text(e: any): string {
  if (e.msg) return e.msg.state === 'recalled' ? '(recalled)' : e.msg.content;
  return Object.keys(e.data ?? {}).length ? JSON.stringify(e.data) : '';
}

export function Agents() {
  const [q, setQ] = useState('');
  const [v, err, reload] = useLoad(() => api('GET', `/app/agents${qs({ q, limit: 200 })}`), [q]);
  const [id, setId] = useState('');
  const [owner, setOwner] = useState('');
  const [team, setTeam] = useState(false);
  const [key, setKey] = useState<string | null>(null);
  const a = useAction();
  const act = (path: string, method = 'POST', body?: unknown) =>
    a.run(async () => {
      const r = await api(method, path, body);
      if (r?.key) setKey(r.key);
      reload();
    });
  return (
    <Page title="Agents">
      <form
        className="card toolbar"
        onSubmit={(e) => {
          e.preventDefault();
          act('/app/agents', 'POST', { id, owner, team });
        }}
      >
        <input placeholder="Agent id" value={id} onChange={(e) => setId(e.target.value)} required />
        <input placeholder="Owner (a person's id)" value={owner} onChange={(e) => setOwner(e.target.value)} required />
        <label className="inline">
          <input type="checkbox" checked={team} onChange={(e) => setTeam(e.target.checked)} /> team agent
        </label>
        <button disabled={a.busy}>Create agent</button>
      </form>
      {key && <Secret label="Agent key" value={key} />}
      <Err text={err ?? a.err} />
      <div className="toolbar">
        <input placeholder="Search" value={q} onChange={(e) => setQ(e.target.value)} />
      </div>
      <table>
        <thead>
          <tr>
            <th>Id</th>
            <th>Owner</th>
            <th>State</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          {v?.users.map((u: any) => (
            <tr key={u.id}>
              <td>{u.id}</td>
              <td>
                {u.owner} {u.team && '(team)'}
              </td>
              <td>
                {u.online && <span className="badge ok">online</span>}
                {u.stopped && <span className="badge bad">stopped</span>}
              </td>
              <td className="row-actions">
                {u.stopped ? (
                  <button onClick={() => act(`/app/agents/${enc(u.id)}:resume`)}>Resume</button>
                ) : (
                  <button
                    onClick={() => {
                      const reason = prompt('Why stop this agent?');
                      if (reason !== null) act(`/app/agents/${enc(u.id)}:stop`, 'POST', { reason });
                    }}
                  >
                    Stop
                  </button>
                )}
                <button onClick={() => act(`/app/agents/${enc(u.id)}/keys`, 'POST', { revoke_others: false })}>New key</button>
                <button
                  className="danger"
                  onClick={() => confirm(`Destroy ${u.id}? Its data is erased.`) && act(`/app/agents/${enc(u.id)}`, 'DELETE')}
                >
                  Destroy
                </button>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </Page>
  );
}

export function Reports() {
  const [state, setState] = useState('open');
  const [v, err, reload] = useLoad(() => api('GET', `/app/reports${qs({ state, limit: 100 })}`), [state]);
  const [open, setOpen] = useState<string | null>(null);
  return (
    <Page title="Reports">
      <div className="toolbar">
        <select value={state} onChange={(e) => setState(e.target.value)}>
          <option value="open">Open</option>
          <option value="resolved">Resolved</option>
        </select>
      </div>
      <Err text={err} />
      <table>
        <thead>
          <tr>
            <th>Reported</th>
            <th>Target</th>
            <th>Category</th>
            <th>State</th>
          </tr>
        </thead>
        <tbody>
          {v?.reports.map((r: any) => (
            <tr key={r.id} className="click" onClick={() => setOpen(r.id)}>
              <td>{time(r.created_at)}</td>
              <td>
                {r.target_kind} <code>{r.target_id}</code>
              </td>
              <td>{r.category}</td>
              <td>{r.state ?? (r.resolved_at ? 'resolved' : 'open')}</td>
            </tr>
          ))}
        </tbody>
      </table>
      {open && (
        <ReportDetail
          id={open}
          onClose={() => {
            setOpen(null);
            reload();
          }}
        />
      )}
    </Page>
  );
}

function ReportDetail({ id, onClose }: { id: string; onClose: () => void }) {
  const [r, err, reload] = useLoad(() => api('GET', `/app/reports/${enc(id)}`), [id]);
  const [action, setAction] = useState('none');
  const [reason, setReason] = useState('');
  const [days, setDays] = useState('');
  const a = useAction();
  return (
    <div className="drawer" role="dialog" aria-label={`report ${id}`}>
      <header className="page-head">
        <h2>Report</h2>
        <button onClick={onClose}>Close</button>
      </header>
      <Err text={err ?? a.err} />
      {r && <Json value={r} />}
      {r && !r.resolved_at && (
        <form
          onSubmit={(e) => {
            e.preventDefault();
            a.run(async () => {
              await api('POST', `/app/reports/${enc(id)}:resolve`, {
                action,
                reason,
                days: action === 'suspend_user' && days ? Number(days) : undefined,
              });
              reload();
            });
          }}
        >
          <label>
            Decision
            <select name="action" value={action} onChange={(e) => setAction(e.target.value)}>
              <option value="none">No action</option>
              <option value="remove_content">Remove the content</option>
              <option value="suspend_user">Suspend the user</option>
              <option value="ban_user">Ban the user</option>
            </select>
          </label>
          {action === 'suspend_user' && (
            <label>
              Days
              <input type="number" min={1} value={days} onChange={(e) => setDays(e.target.value)} />
            </label>
          )}
          <label>
            Statement of reasons (sent to the people concerned)
            <textarea name="reason" value={reason} onChange={(e) => setReason(e.target.value)} required />
          </label>
          <button disabled={a.busy}>Decide</button>
        </form>
      )}
    </div>
  );
}

export function Keys() {
  const [api_, err1, reload1] = useLoad(() => api('GET', '/app/api-keys'), []);
  const [sign, err2, reload2] = useLoad(() => api('GET', '/app/signing-keys'), []);
  const [name, setName] = useState('');
  const [scopes, setScopes] = useState<string[]>([]);
  const [sname, setSname] = useState('');
  const [secret, setSecret] = useState<{ label: string; value: string } | null>(null);
  const a = useAction();
  return (
    <Page title="Keys">
      <Err text={err1 ?? err2 ?? a.err} />
      {secret && <Secret label={secret.label} value={secret.value} />}
      <div className="card">
        <h2>Signing keys</h2>
        <p className="muted">Your backend signs client tokens (JWT, HS256) with these; the app logs people in with the token.</p>
        <form
          className="toolbar"
          onSubmit={(e) => {
            e.preventDefault();
            a.run(async () => {
              const v = await api('POST', '/app/signing-keys', { name: sname });
              setSecret({ label: `Signing secret of ${v.kid}`, value: v.secret });
              setSname('');
              reload2();
            });
          }}
        >
          <input placeholder="Name" value={sname} onChange={(e) => setSname(e.target.value)} required />
          <button disabled={a.busy}>Create signing key</button>
        </form>
        <table>
          <tbody>
            {sign?.keys.map((k: any) => (
              <tr key={k.kid}>
                <td>
                  <code>{k.kid}</code>
                </td>
                <td>{k.name}</td>
                <td>{time(k.created_at)}</td>
                <td>{k.revoked_at ? <span className="badge bad">revoked</span> : <button onClick={() => confirm(`Revoke ${k.kid}? Devices that logged in with it are disconnected.`) && a.run(async () => (await api('DELETE', `/app/signing-keys/${enc(k.kid)}`), reload2()))}>Revoke</button>}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      <div className="card">
        <h2>Open API keys</h2>
        <form
          onSubmit={(e) => {
            e.preventDefault();
            a.run(async () => {
              const v = await api('POST', '/app/api-keys', { name, scopes });
              setSecret({ label: `API key ${v.key_id}`, value: v.key });
              setName('');
              setScopes([]);
              reload1();
            });
          }}
        >
          <div className="toolbar">
            <input placeholder="Name" value={name} onChange={(e) => setName(e.target.value)} required />
            <button disabled={a.busy}>Create API key</button>
          </div>
          <div className="scopes">
            {api_?.scopes.map((s: string) => (
              <label key={s} className="inline">
                <input type="checkbox" checked={scopes.includes(s)} onChange={(e) => setScopes(e.target.checked ? [...scopes, s] : scopes.filter((x) => x !== s))} /> {s}
              </label>
            ))}
          </div>
        </form>
        <table>
          <tbody>
            {api_?.keys.map((k: any) => (
              <tr key={k.key_id}>
                <td>
                  <code>{k.key_id}</code>
                </td>
                <td>{k.name}</td>
                <td className="muted">{(k.scopes ?? []).join(' ')}</td>
                <td>{k.revoked ? <span className="badge bad">revoked</span> : <button onClick={() => confirm(`Revoke ${k.key_id}?`) && a.run(async () => (await api('DELETE', `/app/api-keys/${enc(k.key_id)}`), reload1()))}>Revoke</button>}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </Page>
  );
}

export function Webhooks() {
  const [v, err] = useLoad(() => api('GET', '/app/webhooks'), []);
  return (
    <Page title="Webhooks">
      <Err text={err} />
      <p className="muted">Configured in the server's configuration file; shown here read-only.</p>
      {v && (
        <>
          <p>Events waiting to be delivered: {v.queued}</p>
          <table>
            <tbody>
              {v.webhooks.map((w: any) => (
                <tr key={w.name}>
                  <td>{w.name}</td>
                  <td>
                    <code>{w.url}</code>
                  </td>
                  <td className="muted">{(w.events ?? []).join(' ')}</td>
                </tr>
              ))}
            </tbody>
          </table>
          {v.before_send && <p>Checked before sending: {v.before_send.url}</p>}
        </>
      )}
    </Page>
  );
}

export function Audit() {
  const [prefix, setPrefix] = useState('');
  const [before, setBefore] = useState<string[]>([]);
  const cur = before[before.length - 1];
  const [v, err] = useLoad(() => api('GET', `/app/audit${qs({ prefix, before: cur, limit: 50 })}`), [prefix, cur]);
  return (
    <Page title="Audit log">
      <div className="toolbar">
        <input placeholder="Action starts with, e.g. admin." value={prefix} onChange={(e) => (setPrefix(e.target.value), setBefore([]))} />
      </div>
      <Err text={err} />
      <table>
        <thead>
          <tr>
            <th>When</th>
            <th>Who</th>
            <th>Action</th>
            <th>Target</th>
            <th>Detail</th>
          </tr>
        </thead>
        <tbody>
          {v?.records.map((r: any) => (
            <tr key={r.id}>
              <td>{time(r.ts)}</td>
              <td>{r.actor}</td>
              <td>
                <code>{r.action}</code>
              </td>
              <td>{r.target ?? r.conv_id ?? ''}</td>
              <td className="muted small">{r.detail && JSON.stringify(r.detail)}</td>
            </tr>
          ))}
        </tbody>
      </table>
      <div className="pager">
        <button disabled={!before.length} onClick={() => setBefore(before.slice(0, -1))}>
          Newer
        </button>
        <button disabled={!v?.has_more} onClick={() => setBefore([...before, v.records[v.records.length - 1].id])}>
          Older
        </button>
      </div>
    </Page>
  );
}

export function ClientSettings() {
  const [v, err, reload] = useLoad(() => api('GET', '/app/settings/clients'), []);
  const [draft, setDraft] = useState<any>(null);
  const a = useAction();
  const cur = draft ?? v;
  return (
    <Page title="Client permissions">
      <p className="muted">What people may do in their app unless set for them. Changes apply at once on every node.</p>
      <Err text={err ?? a.err} />
      {cur && (
        <form
          className="card"
          onSubmit={(e) => {
            e.preventDefault();
            a.run(async () => {
              await api('PUT', '/app/settings/clients', cur);
              setDraft(null);
              reload();
            });
          }}
        >
          {SWITCHES.map((k) => (
            <label key={k} className="inline block">
              <input type="checkbox" name={k} checked={!!cur[k]} onChange={(e) => setDraft({ ...cur, [k]: e.target.checked })} /> {k.replace(/_/g, ' ')}
            </label>
          ))}
          {(['edit_minutes', 'recall_minutes'] as const).map((k) => (
            <label key={k}>
              {k.replace(/_/g, ' ')} (empty: always)
              <input
                type="number"
                min={0}
                name={k}
                value={cur[k] ?? ''}
                onChange={(e) => setDraft({ ...cur, [k]: e.target.value === '' ? null : Number(e.target.value) })}
              />
            </label>
          ))}
          <button disabled={a.busy || !draft}>Save</button>
        </form>
      )}
    </Page>
  );
}
