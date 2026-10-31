// The AgentIM console (docs/12 §4). Pages are shown by role: the app area
// to owner/developer (moderator: reports and conversations), the
// deployment area to owner/operator.

import { StrictMode, useEffect, useState, type ReactNode } from 'react';
import { createRoot } from 'react-dom/client';
import { api, can, setCsrf, whenLoggedOut, type Me, type Role } from './api';
import { Account, Login, Setup } from './auth';
import { Agents, Audit, ClientSettings, Conversations, Keys, Overview, Reports, Users, Webhooks } from './app';
import { Admins, Cluster, DyeRules, Status } from './deploy';
import { MessageTrace } from './trace';
import '@fontsource-variable/inter';
import logoLight from './assets/logo-light.svg?raw';
import logoDark from './assets/logo-dark.svg?raw';
import './style.css';

// Sidebar icons (24×24 strokes).
const ICONS: Record<string, string> = {
  overview: '<rect x="3" y="3" width="7" height="9" rx="1.5"/><rect x="14" y="3" width="7" height="5" rx="1.5"/><rect x="14" y="12" width="7" height="9" rx="1.5"/><rect x="3" y="16" width="7" height="5" rx="1.5"/>',
  users: '<circle cx="9" cy="8" r="3.5"/><path d="M2.5 20c.8-3.5 3.4-5.5 6.5-5.5s5.7 2 6.5 5.5"/><path d="M16 4.5a3.5 3.5 0 0 1 0 7M18 14.8c2 .8 3.2 2.6 3.5 5.2"/>',
  conversations: '<path d="M4 5h16v11H9l-5 4z"/>',
  agents: '<rect x="4" y="7" width="16" height="12" rx="3"/><path d="M12 3v4M9 12h.01M15 12h.01M9 16h6"/>',
  trace: '<circle cx="5" cy="6" r="2"/><circle cx="19" cy="18" r="2"/><path d="M7 6h7a3 3 0 0 1 0 6h-4a3 3 0 0 0 0 6h7"/>',
  reports: '<path d="M5 21V4M5 4h11l-2 4 2 4H5"/>',
  keys: '<circle cx="8" cy="15" r="4"/><path d="M11 12l9-9M17 6l3 3M15 8l2 2"/>',
  webhooks: '<path d="M10 14a4 4 0 1 1 2-7.5M14 10a4 4 0 1 1-2 7.5M8 18h8"/>',
  audit: '<path d="M6 3h9l4 4v14H6z"/><path d="M9 12h7M9 16h7M9 8h3"/>',
  settings: '<path d="M4 6h10M18 6h2M4 12h4M12 12h8M4 18h12M20 18h0"/><circle cx="16" cy="6" r="2"/><circle cx="10" cy="12" r="2"/><circle cx="18" cy="18" r="2"/>',
  status: '<path d="M3 12h4l3-7 4 14 3-7h4"/>',
  cluster: '<rect x="3" y="4" width="18" height="6" rx="1.5"/><rect x="3" y="14" width="18" height="6" rx="1.5"/><path d="M7 7h.01M7 17h.01"/>',
  admins: '<path d="M12 3l8 3v6c0 5-3.5 8-8 9-4.5-1-8-4-8-9V6z"/>',
  dye: '<path d="M12 3c3 4.5 6 7.6 6 11a6 6 0 0 1-12 0c0-3.4 3-6.5 6-11z"/><path d="M9.5 15a2.5 2.5 0 0 0 2.5 2.5"/>',
  account: '<circle cx="12" cy="8" r="4"/><path d="M4 21c1-4 4-6 8-6s7 2 8 6"/>',
};
const Icon = ({ name }: { name: string }) => (
  <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" dangerouslySetInnerHTML={{ __html: ICONS[name] ?? '' }} />
);


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
  { path: 'conversations', label: 'Conversations', area: 'App', allowed: can.appRead, page: (me) => <Conversations canTrace={can.trace(me.admin.role)} /> },
  { path: 'agents', label: 'Agents', area: 'App', allowed: can.app, page: () => <Agents /> },
  { path: 'trace', label: 'Message trace', area: 'App', allowed: can.trace, page: () => <MessageTrace /> },
  { path: 'reports', label: 'Reports', area: 'App', allowed: can.appRead, page: () => <Reports /> },
  { path: 'keys', label: 'Keys', area: 'App', allowed: can.app, page: () => <Keys /> },
  { path: 'webhooks', label: 'Webhooks', area: 'App', allowed: can.app, page: () => <Webhooks /> },
  { path: 'audit', label: 'Audit log', area: 'App', allowed: can.app, page: () => <Audit /> },
  { path: 'settings', label: 'Client permissions', area: 'App', allowed: can.app, page: () => <ClientSettings /> },
  { path: 'status', label: 'Status', area: 'Deployment', allowed: can.deploy, page: () => <Status /> },
  { path: 'cluster', label: 'Cluster', area: 'Deployment', allowed: can.deploy, page: () => <Cluster /> },
  { path: 'dye', label: 'Dye rules', area: 'Deployment', allowed: can.deploy, page: () => <DyeRules /> },
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
        <div className="brand" aria-label="AgentIM console">
          <span className="logo-light" dangerouslySetInnerHTML={{ __html: logoLight }} />
          <span className="logo-dark" dangerouslySetInnerHTML={{ __html: logoDark }} />
          <span className="brand-tag">Console</span>
        </div>
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
                  <Icon name={r.path} />
                  {r.label}
                </a>
              ))}
            </div>
          );
        })}
        <div className="who">
          <span className="avatar" aria-hidden="true">{me.admin.name.slice(0, 1).toUpperCase()}</span>
          <span className="who-text">
            <strong>{me.admin.name}</strong>
            <span className="muted small">{me.admin.role}</span>
          </span>
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
