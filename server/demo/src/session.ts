// Signing in to the demo: the demo server hands out a client token for
// a demo person by name (a real app gets it from its own backend), and
// the browser SDK keeps one device id per person.

import { AgentIMClient } from '@agentimhq/client-web';
import wasm from '@agentimhq/client-web/agentim_core.wasm?url';

export interface Me {
  id: string;
  name: string;
}

export interface Session {
  me: Me;
  client: AgentIMClient;
}

const KEY = 'agentim.demo.person';

async function token(name: string): Promise<{ user: Me; token: string }> {
  const r = await fetch('/api/login', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ name }) });
  if (!r.ok) throw new Error(r.status === 404 ? `There is no demo person called ${name}.` : `Sign-in failed (${r.status}).`);
  return r.json();
}

function deviceId(name: string): string {
  const key = `agentim.demo.device.${name}`;
  try {
    let id = localStorage.getItem(key);
    if (!id) {
      id = `web-${crypto.randomUUID().slice(0, 18)}`;
      localStorage.setItem(key, id);
    }
    return id;
  } catch {
    return `web-${crypto.randomUUID().slice(0, 18)}`;
  }
}

export async function signIn(name: string): Promise<Session> {
  const first = await token(name);
  let fresh: string | undefined = first.token;
  const client = await AgentIMClient.create({
    url: location.origin,
    deviceId: deviceId(name),
    deviceName: 'Web demo',
    platform: 'web',
    clientName: 'agentim-demo',
    wasm,
    tokenProvider: async () => {
      // The first token, then a new one each time the SDK asks.
      const t = fresh ?? (await token(name)).token;
      fresh = undefined;
      return t;
    },
  });
  await client.connect();
  try {
    sessionStorage.setItem(KEY, name);
  } catch {}
  return { me: first.user, client };
}

/** Who was signed in in this tab before a reload. */
export function remembered(): string | null {
  try {
    return sessionStorage.getItem(KEY);
  } catch {
    return null;
  }
}

export async function signOut(s: Session) {
  try {
    sessionStorage.removeItem(KEY);
  } catch {}
  await s.client.dispose();
}
