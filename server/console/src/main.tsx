// The AgentIM console (docs/12 §4). Pages are shown by role: the app area
// to owner/developer (moderator: reports and conversations), the
// deployment area to owner/operator.

import { StrictMode, useEffect, useState, type ReactNode } from 'react';
import { createRoot } from 'react-dom/client';
import { api, can, setCsrf, whenLoggedOut, type Me, type Role } from './api';
import { Account, Login, Setup } from './auth';
import { Agents, Audit, ClientSettings, Conversations, Keys, Overview, Reports, Users, Webhooks } from './app';
import { Admins, Cluster, Status } from './deploy';
import './style.css';

interface Route {
  path: string;
  label: string;
  area: 'App' | 'Deployment' | '';
  allowed: (r: Role) => boolean;
  page: (me: Me, refresh: () => void) => ReactNode;
}

const ROUTES: Route[] = [
  { path: 'overview', label: 'Overview', area: 'App', allowed: can.appRead, page: () => <Overview /> },
  { path: 'users', label: 'People', area: 'App', allowed: can.appRead, page: (me) => <Users me={me} /> },
  { path: 'conversations', label: 'Conversations', area: 'App', allowed: can.appRead, page: () => <Conversations /> },
  { path: 'agents', label: 'Agents', area: 'App', allowed: can.app, page: () => <Agents /> },
  { path: 'reports', label: 'Reports', area: 'App', allowed: can.appRead, page: () => <Reports /> },
  { path: 'keys', label: 'Keys', area: 'App', allowed: can.app, page: () => <Keys /> },
  { path: 'webhooks', label: 'Webhooks', area: 'App', allowed: can.app, page: () => <Webhooks /> },
  { path: 'audit', label: 'Audit log', area: 'App', allowed: can.app, page: () => <Audit /> },
  { path: 'settings', label: 'Client permissions', area: 'App', allowed: can.app, page: () => <ClientSettings /> },
  { path: 'status', label: 'Status', area: 'Deployment', allowed: can.deploy, page: () => <Status /> },
  { path: 'cluster', label: 'Cluster', area: 'Deployment', allowed: can.deploy, page: () => <Cluster /> },
  { path: 'admins', label: 'Admins', area: 'Deployment', allowed: can.owner, page: () => <Admins /> },
  { path: 'account', label: 'My account', area: '', allowed: () => true, page: (me, refresh) => <Account me={me} refresh={refresh} /> },
];

function current(): string {
  return location.pathname.replace(/^\/console\/?/, '').split('/')[0];
}

function App() {
  const [me, setMe] = useState<Me | null | undefined>(undefined);
  const [path, setPath] = useState(current());
  const refresh = () =>
    api<Me>('GET', '/me').then(
      (m) => {
        setCsrf(m.csrf);
        setMe(m);
      },
      () => setMe(null),
    );
  useEffect(() => {
    whenLoggedOut(() => setMe(null));
    const pop = () => setPath(current());
    addEventListener('popstate', pop);
    if (path !== 'setup') refresh();
    return () => removeEventListener('popstate', pop);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  const go = (p: string) => {
    history.pushState(null, '', `/console/${p}`);
    setPath(p);
  };

  if (path === 'setup') {
    const token = new URLSearchParams(location.search).get('token') ?? '';
    return (
      <Setup
        token={token}
        onDone={(m) => {
          setMe(m);
          go('overview');
        }}
      />
    );
  }
  if (me === undefined) return <div className="center muted">Loading…</div>;
  if (me === null) return <Login onDone={() => refresh()} />;
  const mine = ROUTES.filter((r) => r.allowed(me.admin.role));
  const route = mine.find((r) => r.path === path) ?? mine[0];
  return (
    <div className="shell">
      <nav>
        <div className="brand">AgentIM</div>
        {(['App', 'Deployment', ''] as const).map((area) => {
          const items = mine.filter((r) => r.area === area);
          if (!items.length) return null;
          return (
            <div key={area || 'me'} className="group">
              {area && <div className="area">{area}</div>}
              {items.map((r) => (
                <a
                  key={r.path}
                  href={`/console/${r.path}`}
                  className={r === route ? 'active' : ''}
                  onClick={(e) => {
                    e.preventDefault();
                    go(r.path);
                  }}
                >
                  {r.label}
                </a>
              ))}
            </div>
          );
        })}
        <div className="who">
          {me.admin.name} · {me.admin.role}
          <button
            className="link"
            onClick={async () => {
              await api('POST', '/logout').catch(() => {});
              setCsrf(null);
              setMe(null);
            }}
          >
            Log out
          </button>
        </div>
      </nav>
      <main key={route.path}>{route.page(me, refresh)}</main>
    </div>
  );
}

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
