// Every text the components show (docs/en/design/developer-experience.md): English, sentence case;
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
  attach: 'Attach a file',
  image: 'Photo',
  viewOriginal: 'View original',
  download: 'Download',
  close: 'Close',
  uploading: (pct: number) => `Uploading… ${pct}%`,
  fileUnavailable: 'File no longer available',
  dropFiles: 'Drop files to send',
  fileTypes: { pdf: 'PDF', doc: 'Document', sheet: 'Spreadsheet', slides: 'Presentation', archive: 'Archive', audio: 'Audio', video: 'Video', code: 'Code', image: 'Image', other: 'File' } as Record<string, string>,
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
  approvalTitle: 'Approval needed',
  approvalFor: 'The assistant wants to',
  approve: 'Approve',
  decline: 'Decline',
  approved: 'Approved',
  declined: 'Declined',
  approvalExpired: 'Expired',
  approvalCancelled: 'Cancelled',
  approvalAmount: 'Amount',
  /** Cards (docs/en/design/chat-components.md). */
  cardWaiting: 'Waiting for an answer',
  cardAnswered: 'Answered',
  answeredBy: (name: string) => `Sent by ${name}`,
  fieldRequired: 'Required',
  fieldInvalid: 'Check this value',
  emptyValue: '—',
  /** Steps (docs/en/design/chat-components.md). */
  stepsTitle: 'Steps',
  stepsSummary: (n: number, seconds?: number) => `${n} ${n === 1 ? 'step' : 'steps'}${seconds ? ` · ${seconds < 60 ? `${seconds}s` : `${Math.round(seconds / 60)} min`}` : ''}`,
  stepStates: { pending: 'Not started', running: 'In progress', waiting: 'Waiting', done: 'Done', failed: 'Failed', skipped: 'Skipped' } as Record<string, string>,
  showSteps: 'Show steps',
  hideSteps: 'Hide steps',
  /** The approval inbox (docs/en/design/chat-components.md). */
  inbox: 'Inbox',
  inboxTabs: { all: 'All', request: 'Approvals', draft: 'Drafts', handoff: 'Hand-offs', question: 'Questions', done: 'Done' } as Record<string, string>,
  inboxEmpty: 'You’re all caught up',
  inboxEmptyHint: 'Approvals, drafts and hand-offs that need you show up here.',
  inboxSelect: 'Select an item to see the details',
  inboxSearch: 'Search the inbox',
  inboxWaiting: (ago: string) => `Waiting ${ago}`,
  inboxExpiresSoon: 'Expires soon',
  inboxFor: (name: string) => `for ${name}`,
  inboxRule: 'Why it needs approval',
  inboxTriggeredBy: 'Asked in the conversation',
  inboxDeclineReason: 'Reason (optional)',
  inboxDraftFrom: (agent: string) => `${agent} suggests replying`,
  inboxHandoffFrom: (agent: string) => `${agent} handed this conversation to you`,
  inboxOpen: 'Open conversation',
  inboxHandBack: 'Hand back',
  inboxSend: 'Send',
  inboxSendEdited: 'Send edited',
  inboxDiscard: 'Discard',
  inboxAnswer: 'Answer',
  inboxBack: 'Back',
  inboxDecidedBy: (name: string, state: string) => `${state} by ${name}`,
  inboxYou: 'you',
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
