// Setting the console up, logging in, and one's own account.

import { useState } from 'react';
import { api, setCsrf, ApiError, type Me } from './api';
import { Err, Page, Secret, useAction } from './ui';

export function Setup({ token, onDone }: { token: string; onDone: (m: Me) => void }) {
  const [name, setName] = useState('owner');
  const [password, setPassword] = useState('');
  const [again, setAgain] = useState('');
  const a = useAction();
  return (
    <div className="center">
      <form
        className="card narrow"
        onSubmit={(e) => {
          e.preventDefault();
          if (password !== again) return a.setErr('the passwords differ');
          a.run(async () => {
            const m = await api<Me>('POST', '/setup', { token, name, password });
            setCsrf(m.csrf);
            history.replaceState(null, '', '/console/');
            onDone(m);
          });
        }}
      >
        <h1>Set up the console</h1>
        <p className="muted">Create the first owner. This link works once, from the machine the server runs on.</p>
        <label>
          Name
          <input name="name" value={name} onChange={(e) => setName(e.target.value)} autoComplete="username" required />
        </label>
        <label>
          Password (10–128 characters)
          <input name="password" type="password" value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="new-password" required />
        </label>
        <label>
          Password again
          <input name="again" type="password" value={again} onChange={(e) => setAgain(e.target.value)} autoComplete="new-password" required />
        </label>
        <Err text={a.err} />
        <button type="submit" disabled={a.busy}>
          Create owner
        </button>
      </form>
    </div>
  );
}

export function Login({ onDone }: { onDone: (m: Me) => void }) {
  const [name, setName] = useState('');
  const [password, setPassword] = useState('');
  const [totp, setTotp] = useState('');
  const [needTotp, setNeedTotp] = useState(false);
  const a = useAction();
  return (
    <div className="center">
      <form
        className="card narrow"
        onSubmit={(e) => {
          e.preventDefault();
          a.run(async () => {
            try {
              const m = await api<Me>('POST', '/login', { name, password, totp: needTotp ? totp : undefined });
              setCsrf(m.csrf);
              onDone(m);
            } catch (e) {
              if (e instanceof ApiError && e.code === 'totp_required') {
                setNeedTotp(true);
                return;
              }
              throw e;
            }
          });
        }}
      >
        <h1>AgentIM console</h1>
        <label>
          Name
          <input name="name" value={name} onChange={(e) => setName(e.target.value)} autoComplete="username" required />
        </label>
        <label>
          Password
          <input name="password" type="password" value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="current-password" required />
        </label>
        {needTotp && (
          <label>
            Code from your authenticator app
            <input name="totp" value={totp} onChange={(e) => setTotp(e.target.value)} inputMode="numeric" autoComplete="one-time-code" autoFocus required />
          </label>
        )}
        <Err text={a.err} />
        <button type="submit" disabled={a.busy}>
          Log in
        </button>
      </form>
    </div>
  );
}

export function Account({ me, refresh }: { me: Me; refresh: () => void }) {
  const [current, setCurrent] = useState('');
  const [next, setNext] = useState('');
  const [setup, setSetup] = useState<{ secret: string; uri: string } | null>(null);
  const [code, setCode] = useState('');
  const pw = useAction();
  const tf = useAction();
  return (
    <Page title="My account">
      <div className="card">
        <p>
          {me.admin.name} · <span className="badge">{me.admin.role}</span>
        </p>
      </div>
      <form
        className="card"
        onSubmit={(e) => {
          e.preventDefault();
          pw.run(async () => {
            await api('POST', '/me/password', { current, new: next });
            refresh();
          });
        }}
      >
        <h2>Password</h2>
        <p className="muted">Changing it logs you out everywhere.</p>
        <label>
          Current password
          <input type="password" value={current} onChange={(e) => setCurrent(e.target.value)} autoComplete="current-password" />
        </label>
        <label>
          New password
          <input type="password" value={next} onChange={(e) => setNext(e.target.value)} autoComplete="new-password" />
        </label>
        <Err text={pw.err} />
        <button disabled={pw.busy}>Change password</button>
      </form>
      <div className="card">
        <h2>Two-factor login</h2>
        <p>{me.totp ? 'On.' : 'Off.'}</p>
        {!me.totp && !setup && (
          <button onClick={() => tf.run(async () => setSetup(await api('POST', '/me/totp:setup')))}>Set up</button>
        )}
        {setup && <Secret label="Secret for your authenticator app" value={setup.secret} />}
        {(setup || me.totp) && (
          <form
            onSubmit={(e) => {
              e.preventDefault();
              tf.run(async () => {
                await api('POST', me.totp ? '/me/totp:disable' : '/me/totp:enable', { code });
                setSetup(null);
                setCode('');
                refresh();
              });
            }}
          >
            <label>
              Code
              <input value={code} onChange={(e) => setCode(e.target.value)} inputMode="numeric" />
            </label>
            <button>{me.totp ? 'Turn off' : 'Turn on'}</button>
          </form>
        )}
        <Err text={tf.err} />
      </div>
    </Page>
  );
}
