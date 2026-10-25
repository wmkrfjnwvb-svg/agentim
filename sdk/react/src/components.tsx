// Components over the hooks (docs/12 §3). Styles are CSS variables
// (theme.css, light and dark); every part can be replaced through
// `components`, and every text through `strings`. The AI mark is drawn by
// the list around each message, outside what can be replaced (D36, EU AI
// Act art. 50): a replacement Message cannot drop it.

import { createContext, useContext, useEffect, useLayoutEffect, useRef, useState, type ComponentType, type FormEvent, type KeyboardEvent, type ReactNode } from 'react';
import type { Message } from '@agentimhq/client-web';
import { AgentIMProvider, useClient, useConnection, useConversations, useMessages, useSend, useTyping, type Client } from './hooks.js';

export const defaultStrings = {
  ai: 'AI',
  aiTitle: 'Written by an AI agent',
  sending: 'Sending…',
  failed: 'Not sent',
  retry: 'Retry',
  discard: 'Delete',
  recalled: 'This message was recalled',
  streaming: 'Writing…',
  cancelled: 'Stopped',
  streamFailed: 'Could not finish',
  edited: 'edited',
  loadOlder: 'Load earlier messages',
  start: 'This is the start of the conversation',
  retention: 'Earlier messages are no longer kept',
  empty: 'No messages yet',
  noConversations: 'No conversations yet',
  placeholder: 'Write a message',
  send: 'Send',
  typing: (names: string[]) => (names.length === 1 ? `${names[0]} is typing…` : `${names.length} people are typing…`),
  helpful: 'Helpful',
  notHelpful: 'Not helpful',
  submit: 'Submit',
  sources: 'Sources',
  connection: {
    closed: 'Offline',
    connecting: 'Connecting…',
    connected: '',
    disconnected: 'Reconnecting…',
    suspended: 'Waiting for the network…',
    failed: 'Cannot connect',
  } as Record<string, string>,
  messages: 'Messages',
  conversations: 'Conversations',
  members: 'Members',
  roles: { owner: 'Owner', admin: 'Admin', member: '' } as Record<string, string>,
  handedOff: 'The assistant handed this conversation to a person',
  handedBack: 'The assistant is back',
};
export type Strings = typeof defaultStrings;

export interface MessageProps {
  message: Message;
  mine: boolean;
  strings: Strings;
  onRetry(): void;
  onDiscard(): void;
  onFeedback(rating: 'up' | 'down'): void;
  onRespond(blockId: string, r: { action?: string; values?: Record<string, unknown> }): void;
}

export interface Components {
  Message: ComponentType<MessageProps>;
  ConversationItem: ComponentType<{ conversation: any; selected: boolean; onSelect(): void; strings: Strings }>;
  Empty: ComponentType<{ text: string }>;
}

interface Ui {
  strings: Strings;
  components: Components;
  me?: string;
}

const UiCtx = createContext<Ui | null>(null);

function useUi(): Ui {
  return useContext(UiCtx) ?? { strings: defaultStrings, components: defaultComponents };
}

/** The client, plus the texts, the replaceable parts and who "I" am (for my own messages). */
export function AgentIM({
  client,
  me,
  strings,
  components,
  children,
}: {
  client: Client;
  me?: string;
  strings?: Partial<Strings>;
  components?: Partial<Components>;
  children?: ReactNode;
}) {
  const ui: Ui = { me, strings: { ...defaultStrings, ...strings }, components: { ...defaultComponents, ...components } };
  return (
    <AgentIMProvider client={client}>
      <UiCtx.Provider value={ui}>{children}</UiCtx.Provider>
    </AgentIMProvider>
  );
}

export function ConnectionBanner() {
  const c = useConnection();
  const { strings } = useUi();
  const text = strings.connection[c.state] ?? c.state;
  if (!text) return null;
  return (
    <div className="aim-banner" role="status" data-state={c.state}>
      {text}
    </div>
  );
}

function DefaultConversationItem({ conversation: c, selected, onSelect }: { conversation: any; selected: boolean; onSelect(): void; strings: Strings }) {
  return (
    <button type="button" className="aim-conv" aria-current={selected || undefined} onClick={onSelect}>
      <span className="aim-conv-title" dir="auto">
        {c.title || c.conv_id}
      </span>
      {c.last_message?.content ? (
        <span className="aim-conv-last" dir="auto">
          {c.last_message.content}
        </span>
      ) : null}
      {c.unread > 0 ? <span className="aim-unread">{c.unread > 99 ? '99+' : c.unread}</span> : null}
    </button>
  );
}

export function ConversationList({ selected, onSelect }: { selected?: string; onSelect(convId: string): void }) {
  const list = useConversations();
  const { strings, components } = useUi();
  if (!list.length) return <components.Empty text={strings.noConversations} />;
  return (
    <nav className="aim-convs" aria-label={strings.conversations}>
      {list.map((c) => (
        <components.ConversationItem key={c.conv_id} conversation={c} selected={c.conv_id === selected} onSelect={() => onSelect(c.conv_id)} strings={strings} />
      ))}
    </nav>
  );
}

function Card({ m, strings, onRespond }: { m: Message; strings: Strings; onRespond: MessageProps['onRespond'] }) {
  const p = m.payload ?? {};
  return (
    <div className="aim-card">
      {p.title ? <div className="aim-card-title" dir="auto">
          {p.title}
        </div> : null}
      {(p.blocks ?? []).map((b: any) => {
        if (b.type === 'text')
          return (
            <p key={b.text} dir="auto">
              {b.text}
            </p>
          );
        if (b.type === 'buttons')
          return (
            <div key={b.id} className="aim-card-buttons">
              {(b.options ?? []).map((x: any) => (
                <button key={x.id} type="button" data-style={x.style} onClick={() => onRespond(b.id, { action: x.id })}>
                  {x.label}
                </button>
              ))}
            </div>
          );
        if (b.type === 'form') return <CardForm key={b.id} block={b} strings={strings} onRespond={onRespond} />;
        return null;
      })}
    </div>
  );
}

function CardForm({ block, strings, onRespond }: { block: any; strings: Strings; onRespond: MessageProps['onRespond'] }) {
  const submit = (e: FormEvent<HTMLFormElement>) => {
    e.preventDefault();
    const values: Record<string, unknown> = {};
    for (const f of block.fields ?? []) {
      const el = e.currentTarget.elements.namedItem(f.id) as HTMLInputElement | null;
      if (!el) continue;
      values[f.id] = f.type === 'checkbox' ? el.checked : f.type === 'number' ? Number(el.value) : el.value;
    }
    onRespond(block.id, { values });
  };
  return (
    <form className="aim-card-form" onSubmit={submit}>
      {(block.fields ?? []).map((f: any) => (
        <label key={f.id}>
          {f.label}
          {f.type === 'select' ? (
            <select name={f.id} required={f.required}>
              {(f.options ?? []).map((o: string) => (
                <option key={o} value={o}>
                  {o}
                </option>
              ))}
            </select>
          ) : (
            <input name={f.id} type={f.type === 'checkbox' ? 'checkbox' : f.type === 'number' ? 'number' : 'text'} required={f.required} maxLength={f.max} />
          )}
        </label>
      ))}
      <button type="submit">{block.submit ?? strings.submit}</button>
    </form>
  );
}

function DefaultMessage({ message: m, mine, strings, onRetry, onDiscard, onFeedback, onRespond }: MessageProps) {
  if (m.state === 'recalled') return <div className="aim-recalled">{strings.recalled}</div>;
  const streaming = m.state === 'streaming';
  const text = streaming ? (m.streamed ?? m.content) : m.content;
  const sources: any[] = m.envelope?.sources ?? [];
  return (
    <div className="aim-bubble" data-mine={mine || undefined} aria-busy={streaming || undefined}>
      {!mine ? (
        <div className="aim-sender" dir="auto">
          {m.sender_name}
        </div>
      ) : null}
      {m.kind === 'agentim.card' ? (
        <Card m={m} strings={strings} onRespond={onRespond} />
      ) : (
        <div className="aim-text" dir="auto">
          {text}
        </div>
      )}
      {sources.length ? (
        <ul className="aim-sources" aria-label={strings.sources}>
          {sources.map((s: any) => (
            <li key={s.url}>
              <a href={s.url} target="_blank" rel="noreferrer noopener">
                {s.title}
              </a>
            </li>
          ))}
        </ul>
      ) : null}
      <div className="aim-meta">
        {streaming ? <span>{strings.streaming}</span> : null}
        {m.state === 'cancelled' ? <span>{strings.cancelled}</span> : null}
        {m.state === 'failed' ? <span>{strings.streamFailed}</span> : null}
        {/* Versions start at 1; streams also move them, so only a committed message counts as edited. */}
        {m.state === 'committed' && m.version > 1 ? <span>{strings.edited}</span> : null}
        {m.status === 'sending' ? <span>{strings.sending}</span> : null}
        {m.status === 'failed' ? (
          <span className="aim-failed" role="alert">
            {strings.failed}
            <button type="button" onClick={onRetry}>
              {strings.retry}
            </button>
            <button type="button" onClick={onDiscard}>
              {strings.discard}
            </button>
          </span>
        ) : null}
        {m.sender_type === 'agent' && !streaming && m.status === 'sent' ? (
          <span className="aim-feedback">
            <button type="button" aria-label={strings.helpful} onClick={() => onFeedback('up')}>
              👍
            </button>
            <button type="button" aria-label={strings.notHelpful} onClick={() => onFeedback('down')}>
              👎
            </button>
          </span>
        ) : null}
      </div>
    </div>
  );
}

function DefaultEmpty({ text }: { text: string }) {
  return <div className="aim-empty">{text}</div>;
}

export const defaultComponents: Components = { Message: DefaultMessage, ConversationItem: DefaultConversationItem, Empty: DefaultEmpty };

/** The messages, oldest first; scrolled to the newest, keeps its place when older ones load. */
export function MessageList({ convId }: { convId: string }) {
  const client = useClient();
  const { strings, components, me } = useUi();
  const { messages, hasOlder, olderEnd, loadingOlder, loadOlder } = useMessages(convId);
  const box = useRef<HTMLDivElement>(null);
  const atBottom = useRef(true);
  const top = useRef<HTMLDivElement>(null);

  // Follow new messages only when already at the bottom.
  useLayoutEffect(() => {
    const el = box.current;
    if (el && atBottom.current) el.scrollTop = el.scrollHeight;
  }, [messages]);

  // Scrolling to the top loads the page before.
  useEffect(() => {
    const t = top.current;
    if (!t || typeof IntersectionObserver === 'undefined') return;
    const io = new IntersectionObserver((es) => {
      if (es.some((e) => e.isIntersecting) && hasOlder && !loadingOlder) void loadOlder();
    });
    io.observe(t);
    return () => io.disconnect();
  }, [hasOlder, loadingOlder, loadOlder]);

  useEffect(() => {
    const last = messages.at(-1);
    if (last?.created_seq) client.read(convId, last.created_seq).catch(() => {});
  }, [client, convId, messages]);

  return (
    <div
      ref={box}
      className="aim-messages"
      role="log"
      aria-label={strings.messages}
      aria-live="polite"
      onScroll={(e) => {
        const el = e.currentTarget;
        atBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
      }}
    >
      <div ref={top} className="aim-top">
        {hasOlder ? (
          <button type="button" onClick={() => void loadOlder()} disabled={loadingOlder}>
            {strings.loadOlder}
          </button>
        ) : olderEnd === 'retention' ? (
          strings.retention
        ) : messages.length ? (
          strings.start
        ) : null}
      </div>
      {!messages.length ? <components.Empty text={strings.empty} /> : null}
      {messages.map((m) => {
        const mine = m.sender_id === me || m.key.startsWith('local:');
        return (
          <div key={m.key} className="aim-row" data-mine={mine || undefined} data-sender-type={m.sender_type}>
            {m.sender_type === 'agent' ? (
              <span className="aim-ai" title={strings.aiTitle} aria-label={strings.aiTitle}>
                {strings.ai}
              </span>
            ) : null}
            <components.Message
              message={m}
              mine={mine}
              strings={strings}
              onRetry={() => m.client_msg_id && void client.retry(convId, m.client_msg_id)}
              onDiscard={() => m.client_msg_id && void client.discard(convId, m.client_msg_id)}
              onFeedback={(r) => void client.feedback(convId, m.msg_id, r).catch(() => {})}
              onRespond={(b, r) => void client.respond(convId, m.msg_id, b, r).catch(() => {})}
            />
          </div>
        );
      })}
    </div>
  );
}

export function TypingIndicator({ convId, names }: { convId: string; names?: (ids: string[]) => string[] }) {
  const { typing } = useTyping(convId);
  const { strings } = useUi();
  if (!typing.length) return <div className="aim-typing" aria-hidden="true" />;
  return (
    <div className="aim-typing" role="status">
      {strings.typing(names ? names(typing) : typing)}
    </div>
  );
}

export function Composer({ convId }: { convId: string }) {
  const { send } = useSend(convId);
  const { notify } = useTyping(convId);
  const { strings } = useUi();
  const [text, setText] = useState('');
  const submit = () => {
    const t = text.trim();
    if (!t) return;
    setText('');
    void send(t);
  };
  const onKey = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      submit();
    }
  };
  return (
    <form
      className="aim-composer"
      onSubmit={(e) => {
        e.preventDefault();
        submit();
      }}
    >
      <textarea
        aria-label={strings.placeholder}
        placeholder={strings.placeholder}
        dir="auto"
        rows={1}
        value={text}
        onChange={(e) => {
          setText(e.target.value);
          if (e.target.value) notify();
        }}
        onKeyDown={onKey}
      />
      <button type="submit" disabled={!text.trim()}>
        {strings.send}
      </button>
    </form>
  );
}

/** The conversation's members; agents carry the AI mark. */
export function MemberList({ convId }: { convId: string }) {
  const conv = useConversations().find((c) => c.conv_id === convId);
  const { strings } = useUi();
  const members: any[] = conv?.members ?? [];
  return (
    <ul className="aim-members" aria-label={strings.members}>
      {members.map((m) => (
        <li key={m.user_id} data-kind={m.kind}>
          <span dir="auto">{m.name}</span>
          {m.kind === 'agent' ? (
            <span className="aim-ai" title={strings.aiTitle} aria-label={strings.aiTitle}>
              {strings.ai}
            </span>
          ) : null}
          {strings.roles[m.role] ? <span className="aim-role">{strings.roles[m.role]}</span> : null}
        </li>
      ))}
    </ul>
  );
}

/** Says when an agent hands the conversation to a person, and when it is back (docs/03 §3.6). */
export function HandoffNotice({ convId }: { convId: string }) {
  const client = useClient();
  const { strings } = useUi();
  const [notice, setNotice] = useState<{ text: string; note?: string } | null>(null);
  useEffect(() => {
    setNotice(null);
    return client.on('event', (e) => {
      const ev = e.event ?? {};
      if (ev.conv_id !== convId || ev.type !== 'agent.state') return;
      const d = ev.data ?? {};
      if (d.state === 'taken_over' && d.handoff) setNotice({ text: strings.handedOff, note: d.note });
      else if (d.state === 'active' && d.handback) setNotice({ text: strings.handedBack, note: d.note });
    });
  }, [client, convId, strings]);
  if (!notice) return null;
  return (
    <div className="aim-handoff" role="status">
      <span>{notice.text}</span>
      {notice.note ? (
        <span className="aim-handoff-note" dir="auto">
          {notice.note}
        </span>
      ) : null}
    </div>
  );
}

/** A whole chat window (the customer-service widget): status, messages, typing, composer. */
export function Chat({ convId }: { convId: string }) {
  return (
    <section className="aim-chat">
      <ConnectionBanner />
      <HandoffNotice convId={convId} />
      <MessageList convId={convId} />
      <TypingIndicator convId={convId} />
      <Composer convId={convId} />
    </section>
  );
}
