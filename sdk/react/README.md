# @agentimhq/react

React hooks and components for AgentIM (docs/12 §3), built on `@agentimhq/client-web`.

```tsx
import { AgentIMClient } from '@agentimhq/client-web';
import { AgentIM, Chat } from '@agentimhq/react';
import '@agentimhq/react/theme.css';

const client = await AgentIMClient.create({ url, deviceId, tokenProvider });
client.connect();

<AgentIM client={client} me={myUserId}>
  <Chat convId={convId} title="Acme Support" />   {/* header, status, messages, typing, composer */}
</AgentIM>
```

- **Hooks**, for your own UI: `useConnection`, `useConversations`, `useMessages(convId)`
  (`messages`, `hasOlder`, `loadOlder`), `useSend`, `useTyping` (who is typing, and
  `notify()`). They read the client with `useSyncExternalStore`. `@agentimhq/react/hooks` has
  them without any DOM code, plus the texts and the layout and date helpers (`layout`,
  `formatTime`, `formatDay`, `formatShort`, `initials`): React Native uses the same ones.
- **Components**: `Chat`, `ConversationList` (avatars, last message, time, unread count, search),
  `MessageList`, `Composer`, `TypingIndicator`, `MemberList`, `HandoffNotice`, `ConnectionBanner`
  and `AIBadge`.
- **Message list**: one sender's messages less than five minutes apart form a run, with the name
  and avatar once and the time under the last one. Day separators ("Today", "Yesterday",
  "Monday", "Mar 3") and a "New messages" line where the unread ones start. Scrolled up, a
  button counts what arrived and brings you back down; sending a message always does. Messages
  show their states: sending, sent, not delivered (retry or delete), streaming, stopped,
  deleted and edited. Also cards (buttons and forms), sources, and feedback on agent messages.
- **Dates and times** use `Intl` in the user's own locale and time zone. Pass `locale`,
  `timeZone` or a clock (`now`) to `<AgentIM>` to fix them.
- **The AI mark cannot be turned off** (D36, EU AI Act art. 50). `MessageList` draws it next to
  every agent's name, outside the replaceable message component. A replacement keeps it.
- **Replace parts**: `components={{ Message, ConversationItem, Avatar, Empty }}`. **Replace
  texts**: `strings={{ … }}`. Every text is there, for any language. Text uses `dir="auto"`,
  and the layout follows `dir="rtl"`.
- **Theme**: CSS variables only (`--aim-*`), light and dark built in, generated from
  `tokens.json` (the React Native package uses the same file). Set `data-theme="light"` or
  `"dark"` on `<html>` to force one. System fonts only: nothing is downloaded from a font
  service. Every text and control colour meets WCAG 2.1 AA contrast, checked in the tests.
- Long histories: rows off screen are skipped with `content-visibility: auto`. The list keeps
  its place when older pages load.

## Tests

```bash
npm test          # tokens up to date and contrast; Vitest + Testing Library: every component state (fake client)
npm run e2e       # Chromium against a real agentim and a real agent (Node agent SDK)
npm run tokens    # after changing tokens.json or src/styles.css: regenerate theme.css
```

`example/` has the customer-service window that the e2e test drives, and `gallery.html`: every
component in fixed states with a fake client (`?theme=dark`, `?dir=rtl`, `?view=support`).
