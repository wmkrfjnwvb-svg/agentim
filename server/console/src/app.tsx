// The app area (/admin/v1/app): what the developer of the app manages.

import { useState } from 'react';
import { api, apiRaw, can, enc, qs, type Me } from './api';
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
      <div className="card next">
        <h2>Get started</h2>
        <ol className="steps">
          <li>
            <a href="/console/keys">Create a signing key and an API key</a>
            <span className="muted">Your backend signs users in and calls the open API.</span>
          </li>
          <li>
            <a href="/console/agents">Create an agent</a>
            <span className="muted">Its key connects any framework over MCP or the agent SDKs.</span>
          </li>
          <li>
            <a href="/console/settings">Choose what your users may do</a>
            <span className="muted">Groups, direct messages, adding people, their own agents.</span>
          </li>
          <li>
            <a href="https://agentim.co/docs/get-started/quickstart/" rel="noreferrer">Follow the quick start</a>
            <span className="muted">A React chat with a streaming agent in five minutes.</span>
          </li>
        </ol>
      </div>
    </Page>
  );
}

const SWITCHES = ['create_groups', 'create_direct', 'add_members', 'find_users', 'create_agents', 'direct_to_non_friends'] as const;

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

export function Conversations({ canTrace = false }: { canTrace?: boolean }) {
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
      {open && <ConversationDetail id={open} canTrace={canTrace} onClose={() => setOpen(null)} />}
    </Page>
  );
}

function ConversationDetail({ id, canTrace, onClose }: { id: string; canTrace: boolean; onClose: () => void }) {
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
                      {canTrace && m.msg?.msg_id && (
                        <>
                          {' '}
                          · <a href={`/console/trace?id=${enc(m.msg.msg_id)}`}>trace</a>
                        </>
                      )}
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
  const [roles] = useLoad(() => api('GET', '/app/roles'), []);
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
            <th>Role</th>
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
                <select
                  aria-label={`Role of ${u.id}`}
                  value={u.role ?? ''}
                  disabled={a.busy}
                  onChange={(e) => act(`/app/agents/${enc(u.id)}/role`, 'PUT', { role: e.target.value || null })}
                >
                  <option value="">No role</option>
                  {roles?.roles.map((r: any) => (
                    <option key={r.name} value={r.name}>
                      {r.name}
                    </option>
                  ))}
                </select>
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

const ROLE_TEMPLATE = `---
name: my-role
description: What this agent is for, in one sentence
tags: [support]
---
You are a helpful assistant for our customers.
Answer briefly and ask before doing anything irreversible.
`;

/** Agent roles (docs/en/design/agents.md): Markdown files, same format as Claude Code's agents/*.md. */
export function Roles() {
  const [v, err, reload] = useLoad(() => api('GET', '/app/roles'), []);
  const [name, setName] = useState('');
  const [md, setMd] = useState(ROLE_TEMPLATE);
  const [editing, setEditing] = useState<string | null>(null);
  const a = useAction();
  const open = (n: string) =>
    a.run(async () => {
      const r = await api('GET', `/app/roles/${enc(n)}`);
      setName(n);
      setMd(r.markdown);
      setEditing(n);
    });
  return (
    <Page title="Roles">
      <p className="muted">
        A role is a Markdown file: a header (<code>name</code>, <code>description</code>, <code>tags</code>, <code>tools</code>, <code>model</code>) and the
        instructions. Agents get it at login and whenever it changes. Claude Code subagent files work as they are.
      </p>
      <Err text={err ?? a.err} />
      <form
        className="card"
        onSubmit={(e) => {
          e.preventDefault();
          a.run(async () => {
            await api('PUT', `/app/roles/${enc(name)}`, { markdown: md });
            setEditing(name);
            reload();
          });
        }}
      >
        <div className="toolbar">
          <input placeholder="Role name (a-z 0-9 -)" value={name} onChange={(e) => setName(e.target.value)} required disabled={!!editing} />
          <button disabled={a.busy}>{editing ? 'Save' : 'Create role'}</button>
          {editing && (
            <button type="button" onClick={() => { setEditing(null); setName(''); setMd(ROLE_TEMPLATE); }}>
              New
            </button>
          )}
        </div>
        <textarea aria-label="Role file" rows={14} value={md} onChange={(e) => setMd(e.target.value)} spellCheck={false} style={{ fontFamily: 'ui-monospace, monospace', width: '100%' }} />
      </form>
      <table>
        <thead>
          <tr>
            <th>Name</th>
            <th>Description</th>
            <th>Version</th>
            <th>Updated</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          {v?.roles.map((r: any) => (
            <tr key={r.name}>
              <td>{r.name}</td>
              <td>{r.description}</td>
              <td>{r.version}</td>
              <td className="muted">{time(r.updated_at)}</td>
              <td className="row-actions">
                <button onClick={() => open(r.name)}>Edit</button>
                <button className="danger" onClick={() => confirm(`Delete role ${r.name}?`) && a.run(async () => { await api('DELETE', `/app/roles/${enc(r.name)}`); reload(); })}>
                  Delete
                </button>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </Page>
  );
}

const PROVIDERS: [string, string][] = [
  ['anthropic', 'Claude (Anthropic)'],
  ['openai', 'OpenAI (incl. Codex)'],
  ['gemini', 'Google Gemini'],
  ['deepseek', 'DeepSeek'],
  ['compatible', 'Other (OpenAI-compatible)'],
];

/** Model services (docs/en/design/assistants.md): the deployment's built-in ones and the app's own keys. */
export function ModelServices() {
  const [v, err, reload] = useLoad(() => api('GET', '/app/model-services'), []);
  const [form, setForm] = useState({ name: '', provider: 'anthropic', base_url: '', api_key: '', daily_tokens: '' });
  const [models, setModels] = useState<any>(null);
  const a = useAction();
  const set = (k: string) => (e: any) => setForm({ ...form, [k]: e.target.value });
  const remove = (name: string) => {
    if (window.confirm(`Delete ${name}?`)) a.run(async () => { await api('DELETE', `/app/model-services/${enc(name)}`); reload(); });
  };
  return (
    <Page title="AI models">
      <p className="muted">
        Model services power built-in agents. Built-in services use the deployment's keys; add your own key for Claude, OpenAI, Gemini,
        DeepSeek or any OpenAI-compatible service. The key is checked with the provider, stored encrypted, and never shown again.
      </p>
      <Err text={err ?? a.err} />
      <form
        className="card"
        onSubmit={(e) => {
          e.preventDefault();
          a.run(async () => {
            const body: any = { provider: form.provider };
            if (form.base_url) body.base_url = form.base_url;
            if (form.api_key) body.api_key = form.api_key;
            if (form.daily_tokens) body.daily_tokens = Number(form.daily_tokens);
            await api('PUT', `/app/model-services/${enc(form.name)}`, body);
            setForm({ ...form, api_key: '' });
            reload();
          });
        }}
      >
        <div className="toolbar">
          <input placeholder="Name (a-z 0-9 -)" value={form.name} onChange={set('name')} required />
          <select aria-label="Provider" value={form.provider} onChange={set('provider')}>
            {PROVIDERS.map(([k, l]) => <option key={k} value={k}>{l}</option>)}
          </select>
          <input placeholder={form.provider === 'compatible' ? 'Base URL (required)' : 'Base URL (optional)'} value={form.base_url} onChange={set('base_url')} />
          <input type="password" autoComplete="off" placeholder="API key (empty keeps the saved one)" value={form.api_key} onChange={set('api_key')} />
          <input type="number" min={0} placeholder="Tokens per day (0: no limit)" value={form.daily_tokens} onChange={set('daily_tokens')} />
          <button disabled={a.busy}>{a.busy ? 'Checking…' : 'Save'}</button>
        </div>
        {form.provider === 'deepseek' && <p className="muted small">DeepSeek processes data in China; tell your users if they are in the EU.</p>}
      </form>
      <table>
        <thead>
          <tr>
            <th>Name</th>
            <th>Provider</th>
            <th>Key</th>
            <th>Tokens / day</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          {v?.services.map((s: any) => (
            <tr key={s.name}>
              <td>{s.name} {s.builtin && <span className="badge">built-in</span>}</td>
              <td>{s.provider}</td>
              <td className="muted">{s.builtin ? (s.ready ? 'deployment key' : 'not configured') : s.key_hint}</td>
              <td>{s.daily_tokens ? s.daily_tokens.toLocaleString() : '—'}</td>
              <td className="row-actions">
                <button onClick={() => a.run(async () => setModels(await api('GET', `/app/model-services/${enc(s.name)}/models`)))}>Models</button>
                {!s.builtin && (
                  <>
                    <button onClick={() => setForm({ name: s.name, provider: s.provider, base_url: s.base_url, api_key: '', daily_tokens: String(s.daily_tokens || '') })}>Edit</button>
                    <button className="danger" onClick={() => remove(s.name)}>Delete</button>
                  </>
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      {models && (
        <div className="card">
          <h3>{models.service}: {models.models.length} models</h3>
          {models.builtin && <p className="muted small">With a built-in key only models with a known price can be used.</p>}
          <table>
            <thead>
              <tr>
                <th>Model</th>
                <th>Input / output per 1M tokens</th>
                <th>Usable</th>
              </tr>
            </thead>
            <tbody>
              {models.models.map((m: any) => (
                <tr key={m.id}>
                  <td><code>{m.id}</code></td>
                  <td>{m.price ? `$${m.price.input} / $${m.price.output}` : '—'}</td>
                  <td>{m.usable ? 'yes' : 'no price'}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </Page>
  );
}

/** Built-in agents (docs/en/design/assistants.md): enable a role from the catalog; the server answers for it. */
export function Assistants() {
  const [cat, err1, reloadCat] = useLoad(() => api('GET', '/app/assistants/catalog'), []);
  const [list, err2, reloadList] = useLoad(() => api('GET', '/app/assistants'), []);
  const [services] = useLoad(() => api('GET', '/app/model-services'), []);
  const since = Date.now() - 30 * 86400000;
  const [usage] = useLoad(() => api('GET', `/app/assistants/usage${qs({ since, limit: 10000 })}`), []);
  const [form, setForm] = useState({ slug: '', owner: '', service: '', model: '' });
  const [models, setModels] = useState<any[]>([]);
  const [know, setKnow] = useState<string | null>(null);
  const a = useAction();
  const reload = () => { reloadCat(); reloadList(); };
  const pickService = (name: string) => {
    setForm((f) => ({ ...f, service: name, model: '' }));
    setModels([]);
    if (name) a.run(async () => setModels((await api('GET', `/app/model-services/${enc(name)}/models`)).models.filter((m: any) => m.usable)));
  };
  const off = (name: string) => {
    if (window.confirm(`Stop the server answering for ${name}? The agent and its conversations stay.`)) a.run(async () => { await api('DELETE', `/app/agents/${enc(name)}/assistant`); reload(); });
  };
  const totals = new Map<string, { calls: number; tokens: number; nanos: number }>();
  for (const u of usage?.usage ?? []) {
    const t = totals.get(u.model) ?? { calls: 0, tokens: 0, nanos: 0 };
    t.calls += 1;
    t.tokens += u.input + u.output + u.cache_read + u.cache_write;
    t.nanos += u.cost_nanos;
    totals.set(u.model, t);
  }
  return (
    <Page title="Built-in agents">
      <p className="muted">
        Agents the server runs for you: pick a role, a model service and a model, and the agent answers in chats where it is
        mentioned or messaged directly. Its instructions are its role (edit them under Agent roles).
      </p>
      <Err text={err1 ?? err2 ?? a.err} />
      <div className="grid">
        {cat?.catalog.map((c: any) => (
          <div className="card" key={c.slug}>
            <h3>{c.slug} {c.enabled && <span className="badge">on</span>}</h3>
            <p className="muted small">{c.description}</p>
            {!c.enabled && <button onClick={() => setForm({ ...form, slug: c.slug })}>Enable…</button>}
          </div>
        ))}
      </div>
      {form.slug && (
        <form
          className="card"
          onSubmit={(e) => {
            e.preventDefault();
            a.run(async () => {
              await api('POST', `/app/assistants/catalog/${enc(form.slug)}`, { owner: form.owner, service: form.service, model: form.model });
              setForm({ slug: '', owner: '', service: '', model: '' });
              reload();
            });
          }}
        >
          <h3>Enable {form.slug}</h3>
          <div className="toolbar">
            <input placeholder="Maintainer (a user's id)" value={form.owner} onChange={(e) => setForm({ ...form, owner: e.target.value })} required />
            <select aria-label="Model service" value={form.service} onChange={(e) => pickService(e.target.value)} required>
              <option value="">Model service…</option>
              {services?.services.map((s: any) => <option key={s.name} value={s.name}>{s.name} ({s.provider})</option>)}
            </select>
            <select aria-label="Model" value={form.model} onChange={(e) => setForm({ ...form, model: e.target.value })} required>
              <option value="">Model…</option>
              {models.map((m: any) => <option key={m.id} value={m.id}>{m.id}{m.price ? ` ($${m.price.input}/$${m.price.output})` : ''}</option>)}
            </select>
            <button disabled={a.busy}>Enable</button>
            <button type="button" onClick={() => setForm({ slug: '', owner: '', service: '', model: '' })}>Cancel</button>
          </div>
        </form>
      )}
      <h2>Running</h2>
      <table>
        <thead>
          <tr>
            <th>Agent</th>
            <th>Service</th>
            <th>Model</th>
            <th>Replies / min, / day</th>
            <th>Behaviour</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          {list?.assistants.map((x: any) => (
            <tr key={x.agent_id}>
              <td>{x.name}</td>
              <td>{x.service}</td>
              <td><code>{x.model}</code></td>
              <td>{x.settings.replies_per_minute} / {x.settings.replies_per_day}</td>
              <td>
                <div className="switches">
                  {([['handoff', 'Hand off to a person'], ['steps', 'Show steps'], ['forms', 'Ask with forms'], ['cards', 'Show data as cards']] as const).map(([k, label]) => (
                    <label key={k}>
                      <input
                        type="checkbox"
                        checked={!!x.settings[k]}
                        onChange={(e) => a.run(async () => { await api('PUT', `/app/agents/${enc(x.name)}/assistant`, { service: x.service, model: x.model, settings: { ...x.settings, [k]: e.target.checked } }); reloadList(); })}
                      />{' '}
                      {label}
                    </label>
                  ))}
                </div>
              </td>
              <td className="row-actions"><button onClick={() => setKnow(x.name)}>Knowledge</button><button className="danger" onClick={() => off(x.name)}>Turn off</button></td>
            </tr>
          ))}
        </tbody>
      </table>
      {know && <Knowledge agent={know} />}
      <h2>Usage, last 30 days</h2>
      <table>
        <thead>
          <tr>
            <th>Model</th>
            <th>Calls</th>
            <th>Tokens</th>
            <th>Cost (built-in keys)</th>
          </tr>
        </thead>
        <tbody>
          {[...totals.entries()].map(([m, t]) => (
            <tr key={m}>
              <td><code>{m}</code></td>
              <td>{t.calls.toLocaleString()}</td>
              <td>{t.tokens.toLocaleString()}</td>
              <td>${(t.nanos / 1e9).toFixed(4)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </Page>
  );
}

const SKILL_TEMPLATE = `---
name: refunds
description: How refunds work and who approves them
---
Explain the refund rules. Refunds over $100 need a manager.
`;

/** Skills (docs/en/design/assistants.md): SKILL.md files; a role names the skills it uses in its header (skills: [name]). */
export function Skills() {
  const [v, err, reload] = useLoad(() => api('GET', '/app/skills'), []);
  const [name, setName] = useState('');
  const [md, setMd] = useState(SKILL_TEMPLATE);
  const [open, setOpen] = useState<any>(null);
  const [file, setFile] = useState({ path: '', body: '' });
  const a = useAction();
  const load = (n: string) => a.run(async () => { const s = await api('GET', `/app/skills/${enc(n)}`); setOpen(s); setName(n); setMd(s.markdown); });
  const remove = (n: string) => {
    if (window.confirm(`Delete skill ${n}?`)) a.run(async () => { await api('DELETE', `/app/skills/${enc(n)}`); setOpen(null); reload(); });
  };
  return (
    <Page title="Skills">
      <p className="muted">
        Skills use the Agent Skills format: a <code>SKILL.md</code> with <code>name</code> and <code>description</code>, instructions, and
        reference files. Add <code>skills: [name]</code> to a role's header to give it to that role's agents. Scripts are not run.
      </p>
      <Err text={err ?? a.err} />
      <form className="card" onSubmit={(e) => { e.preventDefault(); a.run(async () => { await api('PUT', `/app/skills/${enc(name)}`, { markdown: md }); reload(); load(name); }); }}>
        <div className="toolbar">
          <input placeholder="Skill name (a-z 0-9 -)" value={name} onChange={(e) => setName(e.target.value)} required />
          <button disabled={a.busy}>Save</button>
        </div>
        <textarea aria-label="SKILL.md" rows={10} value={md} onChange={(e) => setMd(e.target.value)} spellCheck={false} style={{ fontFamily: 'ui-monospace, monospace', width: '100%' }} />
      </form>
      {open && (
        <div className="card">
          <h3>{open.name}: files</h3>
          <ul>{open.files.map((p: string) => <li key={p}><code>{p}</code> <button className="danger" onClick={() => a.run(async () => { await api('DELETE', `/app/skills/${enc(open.name)}/files/${p}`); load(open.name); })}>Delete</button></li>)}</ul>
          <form className="toolbar" onSubmit={(e) => { e.preventDefault(); a.run(async () => { await apiRaw('PUT', `/app/skills/${enc(open.name)}/files/${file.path}`, file.body); setFile({ path: '', body: '' }); load(open.name); }); }}>
            <input placeholder="Path, e.g. reference.md" value={file.path} onChange={(e) => setFile({ ...file, path: e.target.value })} required />
            <input placeholder="Text" value={file.body} onChange={(e) => setFile({ ...file, body: e.target.value })} required />
            <button>Add file</button>
          </form>
        </div>
      )}
      <table>
        <thead><tr><th>Name</th><th>Description</th><th>Version</th><th></th></tr></thead>
        <tbody>
          {v?.skills.map((s: any) => (
            <tr key={s.name}>
              <td>{s.name}</td><td>{s.description}</td><td>{s.version}</td>
              <td className="row-actions"><button onClick={() => load(s.name)}>Edit</button><button className="danger" onClick={() => remove(s.name)}>Delete</button></td>
            </tr>
          ))}
        </tbody>
      </table>
    </Page>
  );
}

/** Connect (docs/en/design/assistants.md): remote MCP servers and HTTP APIs (OpenAPI). A role names them: mcp: [name], apis: [name]. */
export function Connections() {
  const [v, err, reload] = useLoad(() => api('GET', '/app/connections'), []);
  const [form, setForm] = useState({ name: '', kind: 'mcp', url: '', auth_header: 'Authorization', secret: '', spec: '', writes: 'read_only' });
  const [tools, setTools] = useState<any>(null);
  const [rules, setRules] = useState<any>(null);
  const a = useAction();
  const set = (k: string) => (e: any) => setForm({ ...form, [k]: e.target.value });
  const remove = (n: string) => {
    if (window.confirm(`Delete connection ${n}?`)) a.run(async () => { await api('DELETE', `/app/connections/${enc(n)}`); reload(); });
  };
  return (
    <Page title="Connections">
      <p className="muted">
        Connect agents to your systems: a remote MCP server, or an HTTP API described by an OpenAPI 3 document (JSON). Add{' '}
        <code>mcp: [name]</code> or <code>apis: [name]</code> to a role's header. Only read-only tools are used unless you allow writes; with “Ask first” each call that changes something waits for a person to approve it in the chat. The
        credential is stored encrypted and sent only to this address.
      </p>
      <Err text={err ?? a.err} />
      <form
        className="card"
        onSubmit={(e) => {
          e.preventDefault();
          a.run(async () => {
            const body: any = { kind: form.kind, url: form.url, writes: form.writes };
            if (form.auth_header) body.auth_header = form.auth_header;
            if (form.secret) body.secret = form.secret;
            if (form.kind === 'api' && form.spec) body.spec = JSON.parse(form.spec);
            await api('PUT', `/app/connections/${enc(form.name)}`, body);
            setForm({ ...form, secret: '' });
            reload();
          });
        }}
      >
        <div className="toolbar">
          <input placeholder="Name (a-z 0-9 -)" value={form.name} onChange={set('name')} required />
          <select aria-label="Kind" value={form.kind} onChange={set('kind')}><option value="mcp">MCP server</option><option value="api">HTTP API (OpenAPI)</option></select>
          <input placeholder={form.kind === 'mcp' ? 'MCP endpoint URL' : 'API base URL'} value={form.url} onChange={set('url')} required />
          <input placeholder="Header (e.g. Authorization)" value={form.auth_header} onChange={set('auth_header')} />
          <input type="password" autoComplete="off" placeholder="Credential (empty keeps the saved one)" value={form.secret} onChange={set('secret')} />
          <select aria-label="Writes" value={form.writes} onChange={set('writes')}><option value="read_only">Read-only tools</option><option value="confirm">Ask first for writes</option><option value="allow">Allow writes</option></select>
          <button disabled={a.busy}>{a.busy ? 'Checking…' : 'Save'}</button>
        </div>
        {form.kind === 'api' && <textarea aria-label="OpenAPI document" rows={8} placeholder='{"openapi": "3.0.3", "paths": {...}}' value={form.spec} onChange={set('spec')} style={{ fontFamily: 'ui-monospace, monospace', width: '100%' }} />}
      </form>
      <table>
        <thead><tr><th>Name</th><th>Kind</th><th>URL</th><th>Writes</th><th></th></tr></thead>
        <tbody>
          {v?.connections.map((c: any) => (
            <tr key={c.name}>
              <td>{c.name}</td><td>{c.kind}</td><td className="muted">{c.url}</td><td>{c.writes === 'allow' ? 'allowed' : c.writes === 'confirm' ? 'ask first' : 'read-only'}</td>
              <td className="row-actions">
                <button onClick={() => a.run(async () => setTools(await api('GET', `/app/connections/${enc(c.name)}/tools`)))}>Tools</button>
                <button onClick={() => setRules(c)}>Approval rules{c.approvals?.length ? ` (${c.approvals.length})` : ''}</button>
                <button className="danger" onClick={() => remove(c.name)}>Delete</button>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      {rules && <ApprovalRules conn={rules} onDone={() => { setRules(null); reload(); }} />}
      {tools && (
        <div className="card">
          <h3>{tools.name}: {tools.tools.length} tools</h3>
          <table>
            <thead><tr><th>Tool</th><th>Description</th><th>Used</th></tr></thead>
            <tbody>{tools.tools.map((t: any) => <tr key={t.name}><td><code>{t.name}</code></td><td>{t.description}</td><td>{!t.usable ? 'writes not allowed' : t.confirm ? 'after approval' : 'yes'}</td></tr>)}</tbody>
          </table>
        </div>
      )}
    </Page>
  );
}

/**
 * Who approves which operation of a connection (docs/en/design/assistants.md): rules in
 * order, the first that matches decides. Each says an operation, optionally
 * only above an amount, and who approves: the person asking, named people,
 * or nobody (it runs at once).
 */
function ApprovalRules({ conn, onDone }: { conn: any; onDone(): void }) {
  const [tools] = useLoad(() => api('GET', `/app/connections/${enc(conn.name)}/tools`), [conn.name]);
  const [people] = useLoad(() => api('GET', '/app/users?kind=human&limit=200').catch(() => ({ users: [] })), []);
  const nameOf = (id: string) => people?.users?.find((u: any) => u.id === id)?.name ?? id;
  const [list, setList] = useState<any[]>(() =>
    (conn.approvals ?? []).map((r: any) => ({
      tool: r.tool,
      who: r.confirm === false ? 'none' : r.approvers ? 'named' : 'asker',
      amount_field: r.amount_field ?? '',
      over: r.over ?? '',
      approvers: (r.approvers ?? []).map(nameOf).join(', '),
    })),
  );
  const a = useAction();
  const ops: any[] = (tools?.tools ?? []).filter((t: any) => !t.read_only);
  const set = (i: number, k: string, v: any) => setList(list.map((r, j) => (j === i ? { ...r, [k]: v } : r)));
  const move = (i: number, d: number) => {
    const j = i + d;
    if (j < 0 || j >= list.length) return;
    const n = [...list];
    [n[i], n[j]] = [n[j], n[i]];
    setList(n);
  };
  const save = () =>
    a.run(async () => {
      const approvals = list.map((r) => {
        const out: any = { tool: r.tool };
        if (r.who === 'none') out.confirm = false;
        else {
          if (r.amount_field) {
            out.amount_field = r.amount_field;
            out.over = Number(r.over);
          }
          if (r.who === 'named') out.approvers = String(r.approvers).split(',').map((x) => x.trim()).filter(Boolean);
        }
        return out;
      });
      const body: any = { kind: conn.kind, url: conn.url, writes: conn.writes, approvals };
      if (conn.auth_header) body.auth_header = conn.auth_header;
      await api('PUT', `/app/connections/${enc(conn.name)}`, body);
      onDone();
    });
  return (
    <div className="card">
      <h3>Approval rules: {conn.name}</h3>
      <p className="muted">
        The first rule that matches a call decides. A rule with an amount applies only above it (a call without the amount counts as above). Calls no rule matches follow the connection's
        setting ({conn.writes === 'confirm' ? 'ask first' : conn.writes === 'allow' ? 'allowed' : 'read-only'}). Named approvers decide only when they are in the conversation; otherwise the agent's owner does.
      </p>
      <Err text={a.err} />
      {list.length === 0 ? <p className="muted">No rules yet.</p> : null}
      {list.map((r, i) => {
        const params: any[] = ops.find((t) => t.name === r.tool)?.params ?? [];
        const numbers = params.filter((p) => p.type === 'number' || p.type === 'integer');
        return (
          <div key={i} className="toolbar rule-row">
            <span className="muted">{i + 1}.</span>
            <select aria-label="Operation" value={r.tool} onChange={(e) => set(i, 'tool', e.target.value)} required>
              <option value="">Operation…</option>
              {ops.map((t) => (
                <option key={t.name} value={t.name}>
                  {t.name}
                </option>
              ))}
            </select>
            <select aria-label="Who approves" value={r.who} onChange={(e) => set(i, 'who', e.target.value)}>
              <option value="asker">The person asking approves</option>
              <option value="named">These people approve</option>
              <option value="none">Runs without asking</option>
            </select>
            {r.who !== 'none' ? (
              <>
                <select aria-label="Amount" value={r.amount_field} onChange={(e) => set(i, 'amount_field', e.target.value)}>
                  <option value="">Any amount</option>
                  {numbers.map((p) => (
                    <option key={p.name} value={p.name}>
                      Only when {p.name} is over…
                    </option>
                  ))}
                </select>
                {r.amount_field ? <input aria-label="Over" type="number" min={0} step="any" placeholder="e.g. 30" value={r.over} onChange={(e) => set(i, 'over', e.target.value)} required style={{ width: 110 }} /> : null}
                {r.who === 'named' ? <input aria-label="Approvers" placeholder="Names, comma separated" value={r.approvers} onChange={(e) => set(i, 'approvers', e.target.value)} required /> : null}
              </>
            ) : null}
            <button type="button" aria-label="Move up" onClick={() => move(i, -1)} disabled={i === 0}>↑</button>
            <button type="button" aria-label="Move down" onClick={() => move(i, 1)} disabled={i === list.length - 1}>↓</button>
            <button type="button" className="danger" onClick={() => setList(list.filter((_, j) => j !== i))}>Remove</button>
          </div>
        );
      })}
      <div className="toolbar">
        <button type="button" onClick={() => setList([...list, { tool: ops[0]?.name ?? '', who: 'asker', amount_field: '', over: '', approvers: '' }])} disabled={list.length >= 20}>
          Add rule
        </button>
        <button type="button" className="primary" disabled={a.busy} onClick={save}>
          {a.busy ? 'Saving…' : 'Save rules'}
        </button>
        <button type="button" onClick={onDone}>Cancel</button>
      </div>
    </div>
  );
}

/** An agent's knowledge files (docs/en/design/assistants.md). */
export function Knowledge({ agent }: { agent: string }) {
  const [v, err, reload] = useLoad(() => api('GET', `/app/agents/${enc(agent)}/knowledge`), [agent]);
  const a = useAction();
  const upload = (f: File) => a.run(async () => { await apiRaw('PUT', `/app/agents/${enc(agent)}/knowledge/${encodeURIComponent(f.name)}`, f); reload(); });
  return (
    <div className="card">
      <h3>Knowledge of {agent}</h3>
      <p className="muted small">Markdown, text, CSV, JSON or PDF, up to 5 MB each and 50 files. The agent answers from the best-matching parts.</p>
      <Err text={err ?? a.err} />
      <input type="file" accept=".md,.markdown,.txt,.csv,.json,.pdf" onChange={(e) => e.target.files?.[0] && upload(e.target.files[0])} />
      <ul>
        {v?.files.map((f: any) => (
          <li key={f.name}>
            {f.name} <span className="muted small">{f.chars.toLocaleString()} characters</span>{' '}
            <button className="danger" onClick={() => a.run(async () => { await api('DELETE', `/app/agents/${enc(agent)}/knowledge/${encodeURIComponent(f.name)}`); reload(); })}>Delete</button>
          </li>
        ))}
      </ul>
    </div>
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
