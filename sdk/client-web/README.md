# @agentim/client-web

The AgentIM client SDK for browsers and Node (docs/12 §2): the Rust core,
compiled to WebAssembly, keeps the connection, sync without gaps, the
offline queue (stored first, sent once and in order), an encrypted cache in
IndexedDB and paging back through history. Tabs of the same account share
one connection.

```js
import { AgentIMClient, TokenRefused } from '@agentim/client-web';

const client = await AgentIMClient.create({
  url: 'https://chat.example.com',
  deviceId: savedDeviceId,                  // the same every run on this device
  tokenProvider: () => backend.chatToken(), // a client token your backend signs; throw TokenRefused to stop
});
client.connection.subscribe((c) => render(c.state));
client.connect();

const view = client.messages(convId);       // shown at once from the cache, kept in order
view.subscribe((list) => renderMessages(list));
await client.send(convId, 'hello');         // shown when this returns; sent once, also after a reload
```

React components are in `@agentim/react`. Errors are `AgentIMError` with
the server's codes (`forbidden`, `not_found`, `conflict`, `bad_request`,
`rate_limited`, ...). Licensed under Apache-2.0.
