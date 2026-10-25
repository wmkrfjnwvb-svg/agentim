// The app test-app.sh builds (docs/12 §2.12): the package as an app installs
// it, then two people talk through the native core against a real server.
// The result is logged as "AGENTIM_SMOKE ok" or "AGENTIM_SMOKE FAIL ...".
import React, { useEffect, useState } from 'react';
import { Text } from 'react-native';
import { AgentIMClient } from '@agentimhq/client-react-native';

const URL = 'http://127.0.0.1:__PORT__';

async function person(name: string) {
  const r = await fetch(`${URL}/api/dev/users`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', Origin: URL },
    body: JSON.stringify({ name }),
  });
  if (!r.ok) throw new Error(`POST /api/dev/users: HTTP ${r.status} ${await r.text()}`);
  return r.json();
}

async function until(f: () => boolean) {
  for (let i = 0; i < 400 && !f(); i++) await new Promise((r) => setTimeout(r, 50));
  return f();
}

async function run(): Promise<string> {
  if (!AgentIMClient.version()) return 'no version';
  // Unique names: demo mode already has alice and bob.
  const tag = Date.now();
  const [alice, bob] = [await person(`rn-a${tag}`), await person(`rn-b${tag}`)];
  const make = (p: any, device: string) =>
    AgentIMClient.create({ url: URL, deviceId: device, tokenProvider: async () => p.token });
  const a = make(alice, 'alice-rn');
  const b = make(bob, 'bob-rn');
  await Promise.all([a.connect(), b.connect()]);
  if (!(await until(() => a.connection.get().state === 'connected' && b.connection.get().state === 'connected')))
    return `not connected (${a.connection.get().state}, ${b.connection.get().state})`;
  const conv = (await a.call('create_conv', { kind: 'group', title: 'rn', members: [bob.user.id] })).conv_id;
  await a.send(conv, 'hello from React Native 👋');
  const bv = b.messages(conv);
  const got = await until(() => bv.get().some((m: any) => m.content === 'hello from React Native 👋' && m.status === 'sent'));
  a.dispose();
  b.dispose();
  return got ? 'ok' : 'the other side did not get it';
}

export default function App() {
  const [result, setResult] = useState('running');
  useEffect(() => {
    run()
      .catch((e) => `error ${e?.code ?? ''} ${e?.message ?? e}`)
      .then((r) => {
        // console.error: release builds log errors only.
        console.error(`AGENTIM_SMOKE ${r === 'ok' ? 'ok' : `FAIL ${r}`}`);
        setResult(r);
      });
  }, []);
  return <Text>{result}</Text>;
}
