// Components over the hooks (docs/12 §3.2). Styles are CSS variables
// (theme.css, light and dark, from tokens.json); every part can be
// replaced through `components`, every text through `strings`. The AI
// mark is drawn by the list around each message, outside what can be
// replaced (D36, EU AI Act art. 50): a replacement Message cannot drop it.

import { createContext, Fragment, useContext, useEffect, useLayoutEffect, useMemo, useRef, useState, type ComponentType, type CSSProperties, type FormEvent, type KeyboardEvent, type ReactNode } from 'react';
import type { Message } from '@agentimhq/client-web';
import { AgentIMProvider, useClient, useConnection, useConversations, useMessages, useSend, useTyping, type Client } from './hooks.js';
import { defaultStrings, type Strings } from './strings.js';
import { avatarColor, formatShort, formatTime, initials, interleave, layout, unreadIndex, type Notice, type When } from './format.js';
import { Alert, ArrowDown, ArrowUp, Check, Link, Search, Sparkle, Stop, ThumbDown, ThumbUp } from './icons.js';

export { defaultStrings, type Strings };

/** The avatar palette's size (tokens.json `avatar`). */
const AVATARS = 8;

export interface MessageProps {
  message: Message;
  mine: boolean;
  /** First and last of a run from one sender (the bubble's corners, the time). */
  first: boolean;
  last: boolean;
  /** The time to show under the message ("2:41 PM"), on the last of a run. */
  time?: string;
  strings: Strings;
  onRetry(): void;
  onDiscard(): void;
  onFeedback(rating: 'up' | 'down'): void;
  onRespond(blockId: string, r: { action?: string; values?: Record<string, unknown> }): void;
  /** Set when I may edit this message (mine, sent, plain text). */
  onEdit?(text: string): void;
  /** Set when I may delete it for everyone (mine, sent). */
  onRecall?(): void;
  /** Set while an agent's reply streams: interrupt it. */
  onStop?(): void;
}

export interface ConversationItemProps {
  conversation: any;
  selected: boolean;
  onSelect(): void;
  strings: Strings;
  /** "2:41 PM", "Yesterday", "Mon": when the last message was sent. */
  time?: string;
  me?: string;
}

export interface Components {
  Message: ComponentType<MessageProps>;
  ConversationItem: ComponentType<ConversationItemProps>;
  Avatar: ComponentType<{ id?: string; name?: string; kind?: string; size?: number }>;
  Empty: ComponentType<{ text: string; hint?: string }>;
}

interface Ui {
  strings: Strings;
  components: Components;
  me?: string;
  when: () => When;
}

const UiCtx = createContext<Ui | null>(null);

function useUi(): Ui {
  return useContext(UiCtx) ?? { strings: defaultStrings, components: defaultComponents, when: () => ({}) };
}

/**
 * The client, plus the texts, the replaceable parts, who "I" am (for my
 * own messages) and how dates are shown (the user's own locale and time
 * zone unless given).
 */
export function AgentIM({
  client,
  me,
  strings,
  components,
  locale,
  timeZone,
  now,
  children,
}: {
  client: Client;
  me?: string;
  strings?: Partial<Strings>;
  components?: Partial<Components>;
  locale?: string;
  timeZone?: string;
  /** The clock (tests and screenshots). */
  now?: () => number;
  children?: ReactNode;
}) {
  const ui: Ui = {
    me,
    strings: { ...defaultStrings, ...strings },
    components: { ...defaultComponents, ...components },
    when: () => ({ locale, timeZone, now: now?.() }),
  };
  return (
    <AgentIMProvider client={client}>
      <UiCtx.Provider value={ui}>{children}</UiCtx.Provider>
    </AgentIMProvider>
  );
}

/** Initials on a colour that stays the same for the same id; agents on the AI colour. */
function DefaultAvatar({ id, name, kind, size = 32 }: { id?: string; name?: string; kind?: string; size?: number }) {
  const style = { '--aim-av': `var(--aim-avatar-${avatarColor(id ?? name, AVATARS)})`, width: size, height: size, fontSize: Math.round(size * 0.4) } as CSSProperties;
  return (
    <span className="aim-avatar" data-kind={kind} style={style} aria-hidden="true">
      {initials(name)}
    </span>
  );
}

export function AIBadge() {
  const { strings } = useUi();
  return (
    <span className="aim-ai" title={strings.aiTitle} role="img" aria-label={strings.aiTitle}>
      <Sparkle />
      <span aria-hidden="true">{strings.ai}</span>
    </span>
  );
}

export function ConnectionBanner() {
  const c = useConnection();
  const { strings } = useUi();
  const text = strings.connection[c.state] ?? c.state;
  if (!text) return null;
  return (
    <div className="aim-banner" role="status" data-state={c.state}>
      <span className="aim-dot" aria-hidden="true" />
      {text}
    </div>
  );
}

function preview(c: any, strings: Strings, me?: string): string {
  const m = c.last_message;
  if (!m) return '';
  const text = m.state === 'recalled' ? strings.recalled : m.content || '';
  return m.sender_id && m.sender_id === me ? `${strings.you}: ${text}` : text;
}

function DefaultConversationItem({ conversation: c, selected, onSelect, strings, time, me }: ConversationItemProps) {
  const { components } = useUi();
  const last = preview(c, strings, me);
  return (
    <button type="button" className="aim-conv" aria-current={selected || undefined} data-unread={c.unread > 0 || undefined} onClick={onSelect}>
      <components.Avatar id={c.conv_id} name={c.title || c.conv_id} size={40} />
      <span className="aim-conv-body">
        <span className="aim-conv-top">
          <span className="aim-conv-title" dir="auto">
            {c.title || c.conv_id}
          </span>
          {time ? <span className="aim-conv-time">{time}</span> : null}
        </span>
        <span className="aim-conv-bottom">
          <span className="aim-conv-last" dir="auto">
            {last}
          </span>
          {c.unread > 0 ? <span className="aim-unread">{c.unread > 99 ? '99+' : c.unread}</span> : null}
        </span>
      </span>
    </button>
  );
}

/** The conversations, newest first, with a search box that filters by title and last message. */
export function ConversationList({ selected, onSelect, search = true }: { selected?: string; onSelect(convId: string): void; search?: boolean }) {
  const list = useConversations();
  const { strings, components, me, when } = useUi();
  const [q, setQ] = useState('');
  const w = when();
  const shown = useMemo(() => {
    const needle = q.trim().toLocaleLowerCase();
    if (!needle) return list;
    return list.filter((c) => `${c.title ?? ''}\n${c.last_message?.content ?? ''}`.toLocaleLowerCase().includes(needle));
  }, [list, q]);
  return (
    <div className="aim-convs-wrap">
      {search && list.length ? (
        <label className="aim-search">
          <Search />
          <input type="search" value={q} placeholder={strings.search} aria-label={strings.search} onChange={(e) => setQ(e.target.value)} />
        </label>
      ) : null}
      {!list.length ? (
        <components.Empty text={strings.noConversations} />
      ) : !shown.length ? (
        <components.Empty text={strings.noMatches} />
      ) : (
        <nav className="aim-convs" aria-label={strings.conversations}>
          {shown.map((c) => (
            <components.ConversationItem
              key={c.conv_id}
              conversation={c}
              selected={c.conv_id === selected}
              onSelect={() => onSelect(c.conv_id)}
              strings={strings}
              me={me}
              time={c.last_message?.created_at ? formatShort(c.last_message.created_at, w) : undefined}
            />
          ))}
        </nav>
      )}
    </div>
  );
}

function Card({ m, mine, strings, onRespond }: { m: Message; mine: boolean; strings: Strings; onRespond: MessageProps['onRespond'] }) {
  const p = m.payload ?? {};
  // A block answered here, or my own card: that block is done.
  const [answered, setAnswered] = useState<ReadonlySet<string>>(new Set());
  const done = (id: string) => mine || answered.has(id);
  const answer: MessageProps['onRespond'] = (b, r) => {
    setAnswered((a) => new Set(a).add(b));
    onRespond(b, r);
  };
  return (
    <div className="aim-card">
      {p.title ? (
        <div className="aim-card-title" dir="auto">
          {p.title}
        </div>
      ) : null}
      {(p.blocks ?? []).map((b: any, i: number) => {
        if (b.type === 'text')
          return (
            <p key={i} className="aim-card-text" dir="auto">
              {b.text}
            </p>
          );
        if (b.type === 'buttons')
          return (
            <div key={b.id} className="aim-card-buttons">
              {(b.options ?? []).map((x: any) => (
                <button key={x.id} type="button" data-style={x.style} disabled={done(b.id)} onClick={() => answer(b.id, { action: x.id })}>
                  {x.label}
                </button>
              ))}
            </div>
          );
        if (b.type === 'form') return <CardForm key={b.id} block={b} strings={strings} done={done(b.id)} onRespond={answer} />;
        return null;
      })}
    </div>
  );
}

function CardForm({ block, strings, done, onRespond }: { block: any; strings: Strings; done: boolean; onRespond: MessageProps['onRespond'] }) {
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
        <label key={f.id} data-type={f.type}>
          <span>{f.label}</span>
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
      <button type="submit" data-style="primary" disabled={done}>
        {block.submit ?? strings.submit}
      </button>
    </form>
  );
}

function EditBox({ text, strings, onSave, onCancel }: { text: string; strings: Strings; onSave(t: string): void; onCancel(): void }) {
  const [value, setValue] = useState(text);
  const save = () => {
    const t = value.trim();
    if (t && t !== text) onSave(t);
    else onCancel();
  };
  return (
    <div className="aim-edit">
      <textarea
        autoFocus
        aria-label={strings.editing}
        dir="auto"
        value={value}
        onChange={(e) => setValue(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Escape') onCancel();
          if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
            e.preventDefault();
            save();
          }
        }}
      />
      <div className="aim-edit-buttons">
        <button type="button" onClick={onCancel}>
          {strings.cancel}
        </button>
        <button type="button" data-style="primary" onClick={save} disabled={!value.trim()}>
          {strings.save}
        </button>
      </div>
    </div>
  );
}

function DefaultMessage({ message: m, mine, first, last, time, strings, onRetry, onDiscard, onFeedback, onRespond, onEdit, onRecall, onStop }: MessageProps) {
  const [editing, setEditing] = useState(false);
  if (m.state === 'recalled')
    return (
      <div className="aim-bubble aim-recalled" data-mine={mine || undefined}>
        {strings.recalled}
      </div>
    );
  const streaming = m.state === 'streaming';
  const text = streaming ? (m.streamed ?? m.content) : m.content;
  const sources: any[] = m.envelope?.sources ?? [];
  const failed = m.status === 'failed';
  // Versions start at 1; streams also move them, so only a committed message counts as edited.
  const edited = m.state === 'committed' && m.version > 1;
  const notes = [streaming && strings.streaming, m.state === 'cancelled' && strings.cancelled, m.state === 'failed' && strings.streamFailed, edited && strings.edited].filter(Boolean);
  const feedback = m.sender_type === 'agent' && !mine && !streaming && m.status === 'sent';
  const actions = !!(onEdit || onRecall || onStop);
  if (editing && onEdit)
    return (
      <EditBox
        text={m.content}
        strings={strings}
        onCancel={() => setEditing(false)}
        onSave={(t) => {
          setEditing(false);
          onEdit(t);
        }}
      />
    );
  return (
    <>
      <div className="aim-bubble" data-state={m.state} data-mine={mine || undefined} data-first={first || undefined} data-last={last || undefined} data-failed={failed || undefined} data-sending={m.status === 'sending' || undefined} aria-busy={streaming || undefined}>
        {m.kind === 'agentim.card' ? (
          <Card m={m} mine={mine} strings={strings} onRespond={onRespond} />
        ) : (
          <div className="aim-text" dir="auto">
            {text}
            {streaming ? <span className="aim-cursor" aria-hidden="true" /> : null}
          </div>
        )}
        {sources.length ? (
          <div className="aim-sources">
            <span className="aim-sources-label">{strings.sources}</span>
            <ul aria-label={strings.sources}>
              {sources.map((s: any) => (
                <li key={s.url}>
                  <a href={s.url} target="_blank" rel="noreferrer noopener">
                    {s.title}
                    <Link />
                  </a>
                </li>
              ))}
            </ul>
          </div>
        ) : null}
      </div>
      {last || failed || m.status === 'sending' || notes.length || feedback || actions ? (
        <div className="aim-meta" data-mine={mine || undefined} data-float={(!last && !failed && m.status !== 'sending' && !notes.length && !onStop) || undefined}>
          {notes.map((n) => (
            <span key={n as string}>{n}</span>
          ))}
          {last && time && !failed ? <span className="aim-time">{time}</span> : null}
          {mine && m.status === 'sending' ? <span>{strings.sending}</span> : null}
          {mine && last && m.status === 'sent' ? (
            <span className="aim-sent" title={strings.sent}>
              <Check />
              <span className="aim-sr">{strings.sent}</span>
            </span>
          ) : null}
          {failed ? (
            <span className="aim-failed" role="alert">
              <Alert />
              {strings.failed}
              <button type="button" onClick={onRetry}>
                {strings.retry}
              </button>
              <button type="button" onClick={onDiscard}>
                {strings.discard}
              </button>
            </span>
          ) : null}
          {onStop ? (
            <button type="button" className="aim-stop" onClick={onStop}>
              <Stop />
              {strings.stop}
            </button>
          ) : null}
          {onEdit || onRecall ? (
            <span className="aim-actions">
              {onEdit ? (
                <button type="button" onClick={() => setEditing(true)}>
                  {strings.edit}
                </button>
              ) : null}
              {onRecall ? (
                <button type="button" onClick={onRecall}>
                  {strings.discard}
                </button>
              ) : null}
            </span>
          ) : null}
          {feedback ? (
            <span className="aim-feedback">
              <button type="button" aria-label={strings.helpful} title={strings.helpful} onClick={() => onFeedback('up')}>
                <ThumbUp />
              </button>
              <button type="button" aria-label={strings.notHelpful} title={strings.notHelpful} onClick={() => onFeedback('down')}>
                <ThumbDown />
              </button>
            </span>
          ) : null}
        </div>
      ) : null}
    </>
  );
}

function DefaultEmpty({ text, hint }: { text: string; hint?: string }) {
  return (
    <div className="aim-empty">
      <div className="aim-empty-text">{text}</div>
      {hint ? <div className="aim-empty-hint">{hint}</div> : null}
    </div>
  );
}

export const defaultComponents: Components = { Message: DefaultMessage, ConversationItem: DefaultConversationItem, Avatar: DefaultAvatar, Empty: DefaultEmpty };

function reducedMotion(): boolean {
  return typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches;
}

/**
 * The messages, oldest first, in runs per sender with day separators and
 * "New messages" where the unread ones started; scrolled to the newest,
 * keeps its place when older ones load, and offers a way back down.
 */
export function MessageList({ convId, notices = [] }: { convId: string; notices?: Notice<ReactNode>[] }) {
  const client = useClient();
  const { strings, components, me, when } = useUi();
  const { messages, hasOlder, olderEnd, loadingOlder, loadOlder } = useMessages(convId);
  const conv = useConversations().find((c) => c.conv_id === convId);
  const box = useRef<HTMLDivElement>(null);
  const top = useRef<HTMLDivElement>(null);
  const flow = useRef<HTMLDivElement>(null);
  const [atBottom, setAtBottom] = useState(true);
  const bottom = useRef(true);
  const seen = useRef(0);
  const lastKey = useRef<string | undefined>(undefined);
  const prevHeight = useRef(0);
  const firstKey = useRef<string | undefined>(undefined);
  // "New messages": counted when the conversation opens, placed when its messages are there.
  const divider = useRef<{ conv: string; count: number; key?: string }>({ conv: '', count: 0 });
  if (divider.current.conv !== convId) divider.current = { conv: convId, count: conv?.unread ?? 0 };
  if (divider.current.count > 0 && divider.current.key === undefined && messages.length) {
    const i = unreadIndex(messages, divider.current.count, me);
    divider.current.key = i === undefined ? '' : messages[i].key;
  }
  const unreadFrom = divider.current.key ? messages.findIndex((m) => m.key === divider.current.key) : -1;
  const w = when();
  const items = interleave(layout(messages, w, unreadFrom >= 0 ? unreadFrom : undefined), notices);
  const isMine = (m: Message) => m.sender_id === me || m.key.startsWith('local:');

  useLayoutEffect(() => {
    bottom.current = true;
    setAtBottom(true);
    seen.current = 0;
  }, [convId]);

  // Follow new messages when at the bottom, or when I sent one; keep the
  // place when older ones are added above.
  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    const newest = messages.at(-1);
    const olderAdded = firstKey.current !== undefined && messages[0]?.key !== firstKey.current && newest?.key === lastKey.current;
    if (olderAdded && !bottom.current) el.scrollTop += el.scrollHeight - prevHeight.current;
    else if (bottom.current || (newest && newest.key !== lastKey.current && isMine(newest))) {
      el.scrollTop = el.scrollHeight;
      bottom.current = true;
      setAtBottom(true);
    }
    if (bottom.current) seen.current = messages.length;
    lastKey.current = newest?.key;
    firstKey.current = messages[0]?.key;
    prevHeight.current = el.scrollHeight;
  }, [messages]);

  // Rows settle their height after layout (content-visibility, streaming
  // text, cards): stay at the bottom while it grows.
  useEffect(() => {
    const el = box.current;
    const inner = flow.current;
    if (!el || !inner || typeof ResizeObserver === 'undefined') return;
    const ro = new ResizeObserver(() => {
      if (bottom.current) el.scrollTop = el.scrollHeight;
      prevHeight.current = el.scrollHeight;
    });
    ro.observe(inner);
    return () => ro.disconnect();
  }, []);

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

  // The open conversation is the focused one: its streamed replies grow live.
  useEffect(() => {
    const focus = (c: string | null) => void client.focus?.(c).catch(() => {});
    focus(convId);
    return () => focus(null);
  }, [client, convId]);

  useEffect(() => {
    const last = messages.at(-1);
    if (last?.created_seq) client.read(convId, last.created_seq).catch(() => {});
  }, [client, convId, messages]);

  const below = atBottom ? 0 : messages.slice(seen.current).filter((m) => !isMine(m)).length;
  const jump = () => {
    const el = box.current;
    if (!el) return;
    if (typeof el.scrollTo === 'function') el.scrollTo({ top: el.scrollHeight, behavior: reducedMotion() ? 'auto' : 'smooth' });
    else el.scrollTop = el.scrollHeight;
  };

  return (
    <div className="aim-list">
      <div
        ref={box}
        className="aim-messages"
        role="log"
        aria-label={strings.messages}
        aria-live="polite"
        onScroll={(e) => {
          const el = e.currentTarget;
          const at = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
          bottom.current = at;
          if (at) seen.current = messages.length;
          if (at !== atBottom) setAtBottom(at);
          prevHeight.current = el.scrollHeight;
        }}
      >
        <div ref={top} className="aim-top">
          {hasOlder ? (
            <button type="button" onClick={() => void loadOlder()} disabled={loadingOlder}>
              {loadingOlder ? strings.loading : strings.loadOlder}
            </button>
          ) : olderEnd === 'retention' ? (
            strings.retention
          ) : messages.length ? (
            strings.start
          ) : null}
        </div>
        {!messages.length ? <components.Empty text={strings.empty} hint={strings.emptyHint} /> : null}
        <div ref={flow} className="aim-flow">
        {items.map((it) => {
          if (it.notice)
            return (
              <Fragment key={`n:${it.notice.key}`}>
                {it.day ? (
                  <div className="aim-day" role="separator" aria-label={it.day}>
                    <span>{it.day}</span>
                  </div>
                ) : null}
                <div className="aim-notice" data-tone={it.notice.tone} role="note">
                  {it.notice.content}
                </div>
              </Fragment>
            );
          const { message: m, day, unread, first, last } = it.row;
          const mine = isMine(m);
          const sent = mine && m.status === 'sent' && m.msg_id && !m.key.startsWith('local:');
          const target = { msgId: m.msg_id };
          const agent = m.sender_type === 'agent';
          return (
            <Fragment key={m.key}>
              {day ? (
                <div className="aim-day" role="separator" aria-label={day}>
                  <span>{day}</span>
                </div>
              ) : null}
              {unread ? (
                <div className="aim-new" role="separator" aria-label={strings.newMessages}>
                  <span>{strings.newMessages}</span>
                </div>
              ) : null}
              <div className="aim-row" data-mine={mine || undefined} data-first={first || undefined} data-last={last || undefined} data-sender-type={m.sender_type}>
                {!mine ? <div className="aim-gutter">{first ? <components.Avatar id={m.sender_id} name={m.sender_name} kind={m.sender_type} /> : null}</div> : null}
                <div className="aim-col">
                  {first && !mine ? (
                    <div className="aim-sender">
                      <span dir="auto">{m.sender_name}</span>
                      {agent ? <AIBadge /> : null}
                    </div>
                  ) : agent ? (
                    <span className="aim-sr">{strings.aiTitle}</span>
                  ) : null}
                  <components.Message
                    message={m}
                    mine={mine}
                    first={first}
                    last={last}
                    time={last && m.created_at ? formatTime(m.created_at, w) : undefined}
                    strings={strings}
                    onRetry={() => m.client_msg_id && void client.retry(convId, m.client_msg_id)}
                    onDiscard={() => m.client_msg_id && void client.discard(convId, m.client_msg_id)}
                    onFeedback={(r) => void client.feedback(convId, m.msg_id, r).catch(() => {})}
                    onRespond={(b, r) => void client.respond(convId, m.msg_id, b, r).catch(() => {})}
                    onEdit={sent && m.state === 'committed' && (m.kind ?? 'text') === 'text' ? (t) => void client.edit(convId, target, t).catch(() => {}) : undefined}
                    onRecall={sent && m.state !== 'streaming' ? () => void client.recall(convId, target).catch(() => {}) : undefined}
                    onStop={m.state === 'streaming' && agent ? () => void client.call('stream_cancel', { msg_id: m.msg_id }).catch(() => {}) : undefined}
                  />
                </div>
              </div>
            </Fragment>
          );
        })}
        </div>
      </div>
      {!atBottom ? (
        <button type="button" className="aim-jump" onClick={jump} data-new={below || undefined}>
          <ArrowDown />
          {strings.jumpToLatest(below)}
        </button>
      ) : null}
    </div>
  );
}

/** Who is typing, by name (from the members), with the three dots. */
export function TypingIndicator({ convId, names }: { convId: string; names?: (ids: string[]) => string[] }) {
  const { typing } = useTyping(convId);
  const { strings } = useUi();
  const members: any[] = useConversations().find((c) => c.conv_id === convId)?.members ?? [];
  if (!typing.length) return <div className="aim-typing" aria-hidden="true" />;
  const shown = names ? names(typing) : typing.map((id) => members.find((m) => m.user_id === id)?.name ?? id);
  return (
    <div className="aim-typing" role="status">
      <span className="aim-dots" aria-hidden="true">
        <i />
        <i />
        <i />
      </span>
      {strings.typing(shown)}
    </div>
  );
}

const MAX_COMPOSER_PX = 160;

export function Composer({ convId }: { convId: string }) {
  const { send } = useSend(convId);
  const { notify } = useTyping(convId);
  const { strings } = useUi();
  const [text, setText] = useState('');
  const area = useRef<HTMLTextAreaElement>(null);
  // Grows with the text, up to a limit, then scrolls.
  useLayoutEffect(() => {
    const el = area.current;
    if (!el) return;
    el.style.height = 'auto';
    el.style.height = `${Math.min(el.scrollHeight, MAX_COMPOSER_PX)}px`;
  }, [text]);
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
      <div className="aim-composer-box">
        <textarea
          ref={area}
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
        <button type="submit" className="aim-send" aria-label={strings.send} title={strings.send} disabled={!text.trim()}>
          <ArrowUp />
        </button>
      </div>
    </form>
  );
}

/** The conversation's members; agents carry the AI mark. */
export function MemberList({ convId }: { convId: string }) {
  const conv = useConversations().find((c) => c.conv_id === convId);
  const { strings, components } = useUi();
  const members: any[] = conv?.members ?? [];
  return (
    <ul className="aim-members" aria-label={strings.members}>
      {members.map((m) => (
        <li key={m.user_id} data-kind={m.kind}>
          <components.Avatar id={m.user_id} name={m.name} kind={m.kind} size={28} />
          <span className="aim-member-name" dir="auto">
            {m.name}
          </span>
          {m.kind === 'agent' ? <AIBadge /> : null}
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
  const [notice, setNotice] = useState<{ text: string; note?: string; state: string } | null>(null);
  useEffect(() => {
    setNotice(null);
    return client.on('event', (e) => {
      const ev = e.event ?? {};
      if (ev.conv_id !== convId || ev.type !== 'agent.state') return;
      const d = ev.data ?? {};
      if (d.state === 'taken_over' && d.handoff) setNotice({ text: strings.handedOff, note: d.note, state: 'handoff' });
      else if (d.state === 'active' && d.handback) setNotice({ text: strings.handedBack, note: d.note, state: 'handback' });
    });
  }, [client, convId, strings]);
  if (!notice) return null;
  return (
    <div className="aim-handoff" role="status" data-state={notice.state}>
      <span className="aim-handoff-text">{notice.text}</span>
      {notice.note ? (
        <span className="aim-handoff-note" dir="auto">
          {notice.note}
        </span>
      ) : null}
    </div>
  );
}

/** A whole chat window (the customer-service widget): header, status, messages, typing, composer. */
export function Chat({ convId, title, subtitle }: { convId: string; title?: ReactNode; subtitle?: ReactNode }) {
  return (
    <section className="aim-root aim-chat">
      {title ? (
        <header className="aim-chat-header">
          <div className="aim-chat-title">{title}</div>
          {subtitle ? <div className="aim-chat-subtitle">{subtitle}</div> : null}
        </header>
      ) : null}
      <ConnectionBanner />
      <HandoffNotice convId={convId} />
      <MessageList convId={convId} />
      <TypingIndicator convId={convId} />
      <Composer convId={convId} />
    </section>
  );
}
