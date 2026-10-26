# @agentimhq/react-native-ui

React Native components for AgentIM (docs/12 §3.3), on `@agentimhq/client-react-native`. Same
names, props, texts and look as the web components (`@agentimhq/react`), drawn with native views.

```tsx
import { KeyboardAvoidingView, Platform, SafeAreaView } from 'react-native';
import { AgentIMClient } from '@agentimhq/client-react-native';
import { AgentIM, Chat } from '@agentimhq/react-native-ui';

const client = AgentIMClient.create({ url, deviceId, tokenProvider });
client.connect();

<SafeAreaView style={{ flex: 1 }}>
  {/* Keyboard handling is the app's: the components do not wrap themselves. */}
  <KeyboardAvoidingView style={{ flex: 1 }} behavior={Platform.OS === 'ios' ? 'padding' : undefined}>
    <AgentIM client={client} me={myUserId}>
      <Chat convId={convId} title="Acme Support" />
    </AgentIM>
  </KeyboardAvoidingView>
</SafeAreaView>
```

- **Components**: `Chat`, `ConversationList` (avatars, last message, time, unread count, search),
  `MessageList`, `Composer`, `TypingIndicator`, `MemberList`, `HandoffNotice`,
  `ConnectionBanner` and `AIBadge`.
- **Message list**: an inverted `FlatList` (newest at the bottom; scrolling up loads earlier
  messages). Runs per sender with the name and avatar once, day separators, a "New messages"
  line, timeline `notices`, a button back to the latest that counts new messages. Long-press my
  sent message to edit or delete it; a streaming agent reply shows Stop. Cards with buttons and
  forms (text, number, switch, choice chips); each block can be answered once.
- **The AI mark cannot be turned off** (D36, EU AI Act art. 50): the list draws it next to every
  agent's name, outside the replaceable message component.
- **Theme**: light and dark follow the system (`useColorScheme`), or pass `scheme="light"` or
  `"dark"`; `colors={{ … }}` overrides any token. The tokens come from the same `tokens.json`
  as the web theme (`light`, `dark` are exported), and every text and control colour meets
  WCAG 2.1 AA contrast.
- **Accessibility**: every control has a role and a label; touch targets are at least 44 pt
  (Apple HIG); text follows the system font size.
- **Replace parts**: `components={{ Message, ConversationItem, Avatar, Empty }}`. **Replace
  texts**: `strings={{ … }}`. Dates and times use `Intl` in the user's locale and time zone;
  pass `locale`, `timeZone` or `now` to fix them.
- The hooks (`useConversations`, `useMessages`, `useSend`, `useTyping`, …) and the layout
  helpers are re-exported from `@agentimhq/react/hooks`, shared with the web.

## Tests

```bash
npm test    # shared tokens up to date; Jest + React Native Testing Library: every component state
```

The React Native test app (`sdk/client-react-native/test-app.sh`) renders `Chat` on Android and
iOS against a real server and checks that the message is drawn.
