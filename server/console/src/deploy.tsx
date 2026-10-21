// The deployment area (/admin/v1/deploy): what the deployer looks after.

import { useState } from 'react';
import { api, ApiError, enc } from './api';
import { Err, Json, Page, Secret, time, useAction, useLoad } from './ui';

export function Status() {
  const [v, err, reload] = useLoad(() => api('GET', '/deploy/status'), []);
  return (
    <Page title="Status" actions={<button onClick={reload}>Refresh</button>}>
      <Err text={err} />
      {v && (
        <>
          <div className="tiles">
            <div className="tile">
              <div className="n">{v.version}</div>
              <div className="muted">version</div>
            </div>
            <div className="tile">
              <div className="n">{v.connections}</div>
              <div className="muted">people connected</div>
            </div>
            <div className="tile">
              <div className="n">{v.clustered ? 'yes' : 'no'}</div>
              <div className="muted">cluster</div>
            </div>
          </div>
          <h2>Databases</h2>
          <table>
            <thead>
              <tr>
                <th>Name</th>
                <th>Kind</th>
                <th>Replicas</th>
                <th>Encrypted</th>
                <th>Writer</th>
              </tr>
            </thead>
            <tbody>
              {v.database_details?.map((d: any) => (
                <tr key={d.name}>
                  <td>
                    {d.name} {d.name === v.control && <span className="badge">control</span>}
                  </td>
                  <td>{d.dialect}</td>
                  <td>{d.mode}</td>
                  <td>{d.encrypted ? 'yes' : 'no'}</td>
                  <td className="muted small">{JSON.stringify(d.writer)}</td>
                </tr>
              ))}
            </tbody>
          </table>
          <details>
            <summary>Everything</summary>
            <Json value={v} />
          </details>
        </>
      )}
    </Page>
  );
}

export function Cluster() {
  const [v, err, reload] = useLoad(
    () =>
      api('GET', '/deploy/cluster').catch((e) => {
        if (e instanceof ApiError && e.code === 'bad_request') return { single: e.message };
        throw e;
      }),
    [],
  );
  const a = useAction();
  return (
    <Page title="Cluster" actions={<button onClick={reload}>Refresh</button>}>
      <Err text={err ?? a.err} />
      {v?.single && <p className="muted">{v.single}.</p>}
      {v && !v.single && (
        <>
          <table>
            <thead>
              <tr>
                <th>Node</th>
                <th>State</th>
                <th>Heartbeat</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {(v.nodes ?? []).map((n: any) => (
                <tr key={n.node_id ?? n.id}>
                  <td>{n.node_id ?? n.id}</td>
                  <td>{n.state}</td>
                  <td>{time(n.heartbeat_at ?? n.last_heartbeat)}</td>
                  <td>
                    <button
                      onClick={() =>
                        confirm(`Take ${n.node_id ?? n.id} out of service?`) &&
                        a.run(async () => (await api('POST', `/deploy/cluster/nodes/${enc(n.node_id ?? n.id)}:drain`), reload()))
                      }
                    >
                      Drain
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          <details>
            <summary>Shards</summary>
            <Json value={v} />
          </details>
        </>
      )}
    </Page>
  );
}

export function Admins() {
  const [v, err, reload] = useLoad(() => api('GET', '/deploy/admins'), []);
  const [keys, err2, reloadKeys] = useLoad(() => api('GET', '/deploy/admin-keys'), []);
  const [name, setName] = useState('');
  const [role, setRole] = useState('developer');
  const [password, setPassword] = useState('');
  const [kname, setKname] = useState('');
  const [krole, setKrole] = useState('operator');
  const [secret, setSecret] = useState<string | null>(null);
  const a = useAction();
  const roles: string[] = v?.roles ?? ['owner', 'developer', 'operator', 'moderator'];
  const patch = (n: string, body: unknown) => a.run(async () => (await api('PATCH', `/deploy/admins/${enc(n)}`, body), reload()));
  return (
    <Page title="Admins">
      <Err text={err ?? err2 ?? a.err} />
      <div className="card">
        <h2>Console accounts</h2>
        <form
          className="toolbar"
          onSubmit={(e) => {
            e.preventDefault();
            a.run(async () => {
              await api('POST', '/deploy/admins', { name, role, password });
              setName('');
              setPassword('');
              reload();
            });
          }}
        >
          <input placeholder="Name" value={name} onChange={(e) => setName(e.target.value)} required />
          <select value={role} onChange={(e) => setRole(e.target.value)}>
            {roles.map((r) => (
              <option key={r}>{r}</option>
            ))}
          </select>
          <input type="password" placeholder="Initial password" value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="new-password" required />
          <button disabled={a.busy}>Add admin</button>
        </form>
        <table>
          <tbody>
            {v?.admins.map((x: any) => (
              <tr key={x.name}>
                <td>{x.name}</td>
                <td>
                  <select aria-label={`role of ${x.name}`} value={x.role} onChange={(e) => patch(x.name, { role: e.target.value })}>
                    {roles.map((r) => (
                      <option key={r}>{r}</option>
                    ))}
                  </select>
                </td>
                <td>{x.disabled ? <span className="badge bad">disabled</span> : ''}</td>
                <td className="row-actions">
                  <button onClick={() => patch(x.name, { disabled: !x.disabled })}>{x.disabled ? 'Enable' : 'Disable'}</button>
                  <button
                    onClick={() => {
                      const pw = prompt(`New password for ${x.name}`);
                      if (pw) a.run(async () => api('POST', `/deploy/admins/${enc(x.name)}:reset-password`, { password: pw }));
                    }}
                  >
                    Reset password
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      <div className="card">
        <h2>Admin keys</h2>
        <p className="muted">
          For <code>agentim admin --url … --key …</code> and scripts.
        </p>
        {secret && <Secret label="Admin key" value={secret} />}
        <form
          className="toolbar"
          onSubmit={(e) => {
            e.preventDefault();
            a.run(async () => {
              const k = await api('POST', '/deploy/admin-keys', { name: kname, role: krole });
              setSecret(k.key);
              setKname('');
              reloadKeys();
            });
          }}
        >
          <input placeholder="Name" value={kname} onChange={(e) => setKname(e.target.value)} required />
          <select value={krole} onChange={(e) => setKrole(e.target.value)}>
            {roles.map((r) => (
              <option key={r}>{r}</option>
            ))}
          </select>
          <button disabled={a.busy}>Create admin key</button>
        </form>
        <table>
          <tbody>
            {keys?.keys.map((k: any) => (
              <tr key={k.key_id}>
                <td>
                  <code>{k.key_id}</code>
                </td>
                <td>{k.name}</td>
                <td>{k.role}</td>
                <td>{k.revoked ? <span className="badge bad">revoked</span> : <button onClick={() => a.run(async () => (await api('DELETE', `/deploy/admin-keys/${enc(k.key_id)}`), reloadKeys()))}>Revoke</button>}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </Page>
  );
}
