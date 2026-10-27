// Every text the components show (docs/12 §3.1): English, sentence case;
// replace any of them through `strings`. Shared with React Native.

export const defaultStrings = {
  ai: 'AI',
  aiTitle: 'AI agent',
  you: 'You',
  sending: 'Sending…',
  sent: 'Sent',
  failed: 'Not delivered',
  retry: 'Retry',
  edit: 'Edit',
  editing: 'Edit message',
  save: 'Save',
  cancel: 'Cancel',
  stop: 'Stop',
  discard: 'Delete',
  recalled: 'This message was deleted',
  streaming: 'Writing…',
  cancelled: 'Stopped',
  streamFailed: 'Couldn’t finish',
  edited: 'Edited',
  loadOlder: 'Load earlier messages',
  loading: 'Loading…',
  start: 'This is the beginning of the conversation',
  retention: 'Earlier messages are no longer available',
  empty: 'No messages yet',
  emptyHint: 'Say hello to get things started.',
  noConversations: 'No conversations yet',
  noMatches: 'No matches',
  search: 'Search',
  placeholder: 'Message',
  send: 'Send',
  newMessages: 'New messages',
  jumpToLatest: (n: number) => (n ? `${n} new ${n === 1 ? 'message' : 'messages'}` : 'Jump to latest'),
  typing: (names: string[]) =>
    names.length === 1 ? `${names[0]} is typing…` : names.length === 2 ? `${names[0]} and ${names[1]} are typing…` : 'Several people are typing…',
  helpful: 'Helpful',
  notHelpful: 'Not helpful',
  submit: 'Submit',
  sources: 'Sources',
  connection: {
    closed: 'Offline',
    connecting: 'Connecting…',
    connected: '',
    disconnected: 'Reconnecting…',
    suspended: 'Waiting for network…',
    failed: 'Can’t connect',
  } as Record<string, string>,
  messages: 'Messages',
  conversations: 'Conversations',
  members: 'Members',
  roles: { owner: 'Owner', admin: 'Admin', member: '' } as Record<string, string>,
  handedOff: 'You’re being connected to a person',
  handedBack: 'The assistant is back',
  /** The line in the chat once this person is no longer in it (`by`: who did it, when known). */
  ended: (reason: string, by?: string) =>
    reason === 'removed'
      ? by
        ? `${by} removed you`
        : 'You were removed from this conversation'
      : reason === 'dissolved'
        ? by
          ? `${by} closed this conversation`
          : 'This conversation was closed'
        : 'You left this conversation',
  /** In place of the composer. */
  endedComposer: (reason: string) =>
    reason === 'dissolved'
      ? 'This conversation is closed. You can still read its history.'
      : 'You can’t send messages because you’re no longer a member.',
};

export type Strings = typeof defaultStrings;
