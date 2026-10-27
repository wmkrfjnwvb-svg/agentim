// What does not need the DOM (docs/12 §3): the hooks, the texts and the
// layout and date rules. React Native's components are built on this too.
export { AgentIMProvider, useClient, useObservable, useConnection, useConversations, useMessages, useSend, useTyping, type Client, type MessagesState } from './hooks.js';
export { defaultStrings, type Strings } from './strings.js';
export { avatarColor, ended, formatDay, formatShort, formatTime, initials, interleave, layout, unreadIndex, type Ended, type Item, type Notice, type Row, type When } from './format.js';
