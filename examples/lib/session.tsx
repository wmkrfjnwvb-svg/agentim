// A page's session: it asks "your backend" (the example's server) for a token
// for the person in `?as=` and the conversation to open, then connects.

import { useEffect, useState, type ReactNode } from 'react';
import { createRoot } from 'react-dom/client';
import { AgentIMClient } from '@agentimhq/client-web';
import { AgentIM } from '@agentimhq/react';

export type Session = { url: string; token: string; conv: string; me: string; name: string; [k: string]: unknown };

/** Renders `app` once connected as the person the page is for. */
export async function mount(defaultAs: string, app: (s: Session, client: AgentIMClient) => ReactNode) {
  const q = new URLSearchParams(location.search);
  const who = q.get('as') ?? defaultAs;
  const s: Session = await fetch(`/api/session?as=${encodeURIComponent(who)}`).then((r) => r.json());
  const client = await AgentIMClient.create({
    url: s.url,
    deviceId: `example-${who}`,
    store: 'memory',
    tokenProvider: async () => (await fetch(`/api/session?as=${encodeURIComponent(who)}`).then((r) => r.json())).token,
  });
  await client.connect();
  (window as any).client = client;
  createRoot(document.getElementById('root')!).render(
    <AgentIM client={client} me={s.me}>
      {app(s, client)}
    </AgentIM>,
  );
}

/** Calls `f` now and every `ms` while mounted. */
export function usePoll<T>(f: () => Promise<T>, ms = 1000): T | undefined {
  const [v, setV] = useState<T>();
  useEffect(() => {
    let on = true;
    const tick = () => f().then((x) => on && setV(x)).catch(() => {});
    tick();
    const t = setInterval(tick, ms);
    return () => {
      on = false;
      clearInterval(t);
    };
  }, []);
  return v;
}
