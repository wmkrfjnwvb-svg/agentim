# @agentim/react

React hooks and components for AgentIM (docs/12 §3), built on `@agentim/client-web`.

```tsx
import { AgentIMClient } from '@agentim/client-web';
import { AgentIM, Chat } from '@agentim/react';
import '@agentim/react/theme.css';

const client = await AgentIMClient.create({ url, deviceId, tokenProvider });
client.connect();

<AgentIM client={client} me={myUserId}>
  <Chat convId={convId} />   {/* status, messages, typing, composer */}
</AgentIM>
```

- **Hooks**, for your own UI: `useConnection`, `useConversations`, `useMessages(convId)`
  (`messages`, `hasOlder`, `loadOlder`), `useSend`, `useTyping` (who is typing, and
  `notify()`). They read the client with `useSyncExternalStore`.
- **Components**: `Chat`, `ConversationList`, `MessageList`, `Composer`, `TypingIndicator`
  and `ConnectionBanner`. Messages show their states: sending, failed (retry or delete),
  streaming, stopped, recalled and edited. Also cards (buttons and forms), sources, and
  feedback on agent messages.
- **The AI mark cannot be turned off** (D36, EU AI Act art. 50). `MessageList` draws it next to
  every agent message, outside the replaceable message component. A replacement keeps it.
- **Replace parts**: `components={{ Message, ConversationItem, Empty }}`. **Replace texts**:
  `strings={{ … }}`. Every text is there, for any language. Text uses `dir="auto"`, and the
  layout follows `dir="rtl"`.
- **Theme**: CSS variables only (`--aim-*`), with light and dark built in. Set
  `data-theme="light"` or `"dark"` on `<html>` to force one.
- Long histories: rows off screen are skipped with `content-visibility: auto`. The list keeps
  its place when older pages load, and follows new messages only when at the bottom.

## Tests

```bash
npm test          # Vitest + Testing Library: every component state (fake client)
npm run e2e       # Chromium against a real agentim and a real agent (Node agent SDK)
```

`example/` is the customer-service window that the e2e test drives.
