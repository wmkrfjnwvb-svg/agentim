// The app test-app.sh builds (docs/12 §2.12, §3.5): the packages as an app
// installs them, then two people talk through the native core against a
// real server, and the receiver's screen shows it with the components.
// The result is logged as "AGENTIM_SMOKE ok" or "AGENTIM_SMOKE FAIL ...".
import React, { useEffect, useState } from 'react';
import { SafeAreaView, Text } from 'react-native';
import { AgentIMClient } from '@agentimhq/client-react-native';
import { AgentIM, Chat, defaultComponents, type MessageProps } from '@agentimhq/react-native-ui';

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

const TEXT = 'hello from React Native 👋';
// Set when the list draws the message with the default bubble.
let drawn: () => void = () => {};
function Seen(p: MessageProps) {
  useEffect(() => {
    if (p.message.content === TEXT) drawn();
  }, [p.message.content]);
  return <defaultComponents.Message {...p} />;
}

async function run(show: (c: AgentIMClient, me: string, conv: string) => void): Promise<string> {
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
  let onScreen = false;
  drawn = () => (onScreen = true);
  show(b, bob.user.id, conv);
  await a.send(conv, TEXT);
  const bv = b.messages(conv);
  const got = await until(() => bv.get().some((m: any) => m.content === TEXT && m.status === 'sent'));
  const shown = got && (await until(() => onScreen));
  a.dispose();
  return !got ? 'the other side did not get it' : shown ? 'ok' : 'the components did not show it';
}

export default function App() {
  const [result, setResult] = useState('running');
  const [chat, setChat] = useState<{ client: AgentIMClient; me: string; conv: string } | null>(null);
  useEffect(() => {
    run((client, me, conv) => setChat({ client, me, conv }))
      .catch((e) => `error ${e?.code ?? ''} ${e?.message ?? e}`)
      .then((r) => {
        // console.error: release builds log errors only.
        console.error(`AGENTIM_SMOKE ${r === 'ok' ? 'ok' : `FAIL ${r}`}`);
        setResult(r);
      });
  }, []);
  return (
    <SafeAreaView style={{ flex: 1 }}>
      <Text>{result}</Text>
      {chat ? (
        <AgentIM client={chat.client as any} me={chat.me} components={{ Message: Seen }}>
          <Chat convId={chat.conv} title="rn" />
        </AgentIM>
      ) : null}
    </SafeAreaView>
  );
}
