// The customer-service window (docs/12 §3): one conversation with a support
// agent, built from the components. Open with ?url=…&token=…&conv=…&me=…
// (a real app gets the token from its own backend).

import { createRoot } from 'react-dom/client';
import { AgentIMClient } from '@agentimhq/client-web';
import { AgentIM, Chat } from '../src/index.js';

const q = new URLSearchParams(location.search);
const client = await AgentIMClient.create({
  url: q.get('url')!,
  deviceId: q.get('device') ?? 'support-widget',
  tokenProvider: async () => q.get('token')!,
});
await client.connect();
(window as any).client = client;

createRoot(document.getElementById('root')!).render(
  <AgentIM client={client} me={q.get('me') ?? undefined}>
    <Chat convId={q.get('conv')!} />
  </AgentIM>,
);
