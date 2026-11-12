// Threads, reactions and pinned messages (docs/en/design/messaging.md): the
// reactions under a message, the "N replies" link of a thread's root, a
// thread panel with its own composer, and the pinned bar of a conversation.
// Everything goes through `client.call`, so any client (or a test fake) works.

import { useCallback, useEffect, useState, type ReactNode } from 'react';
import type { Message } from '@agentimhq/client-web';
import { useClient } from './hooks.js';
import { Pin } from './icons.js';
import type { Strings } from './strings.js';

/** The reactions offered by the add button; apps pass their own. */
export const QUICK_REACTIONS = ['👍', '❤️', '😂', '🎉', '👀', '✅'];

export interface Reaction {
  key: string;
  count: number;
  me: boolean;
  users?: string[];
}

/** A message's reactions as chips (mine highlighted), and a button to add one. */
export function Reactions({
  message,
  strings,
  onReact,
  quick = QUICK_REACTIONS,
}: {
  message: Message;
  strings: Strings;
  onReact?(key: string, on: boolean): void;
  quick?: string[];
}) {
  const list: Reaction[] = ((message as any).reactions ?? []) as Reaction[];
  const [open, setOpen] = useState(false);
  if (!list.length && !onReact) return null;
  return (
    <div className="aim-reactions" data-empty={list.length ? undefined : true}>
      {list.map((r) => (
        <button
          key={r.key}
          type="button"
          className="aim-reaction"
          aria-pressed={r.me}
          aria-label={strings.reactedBy(r.key, r.count, r.me)}
          disabled={!onReact}
          onClick={() => onReact?.(r.key, !r.me)}
        >
          <span aria-hidden="true">{r.key}</span>
          <span className="aim-reaction-count">{r.count}</span>
        </button>
      ))}
      {onReact ? (
        <span className="aim-react">
          <button type="button" className="aim-react-add" aria-label={strings.react} title={strings.react} aria-expanded={open} onClick={() => setOpen(!open)}>
            <span aria-hidden="true">☺</span>
          </button>
          {open ? (
            <span className="aim-react-pick" role="menu">
              {quick.map((k) => (
                <button
                  key={k}
                  type="button"
                  role="menuitem"
                  onClick={() => {
                    setOpen(false);
                    const mine = list.find((r) => r.key === k)?.me ?? false;
                    onReact(k, !mine);
                  }}
                >
                  {k}
                </button>
              ))}
            </span>
          ) : null}
        </span>
      ) : null}
    </div>
  );
}

/** The "N replies" link under a thread's root. */
export function ThreadSummary({ message, strings, onOpen }: { message: Message; strings: Strings; onOpen?(): void }) {
  const t = (message as any).thread;
  const n: number = t?.replies ?? 0;
  if (!n && !onOpen) return null;
  return (
    <button type="button" className="aim-thread-link" data-unread={t?.unread ? true : undefined} onClick={onOpen} disabled={!onOpen}>
      {n ? strings.replies(n) : strings.replyInThread}
    </button>
  );
}

/** Whether a message is a reply shown only in its thread (not in the conversation). */
export function onlyInThread(m: Message): boolean {
  const x = m as any;
  return typeof x.thread_id === 'string' && x.thread_shown !== true;
}

/**
 * A thread: its root and replies (live), and a composer that answers in it.
 * `render` draws one message (default: sender and text).
 */
export function ThreadPanel({
  convId,
  threadId,
  strings,
  onClose,
  render,
}: {
  convId: string;
  threadId: string;
  strings: Strings;
  onClose?(): void;
  render?(m: any): ReactNode;
}) {
  const client = useClient();
  const [root, setRoot] = useState<any>(null);
  const [replies, setReplies] = useState<any[]>([]);
  const [text, setText] = useState('');
  const [alsoInConv, setAlso] = useState(false);
  const load = useCallback(async () => {
    try {
      const t: any = await client.call('pull_thread', { conv_id: convId, thread_id: threadId, limit: 200 });
      setRoot(t.root ?? null);
      setReplies(t.replies ?? []);
      const last = t.replies?.[t.replies.length - 1]?.created_seq;
      if (last) void client.call('read_thread', { conv_id: convId, thread_id: threadId, seq: last }).catch(() => {});
    } catch {
      /* shown as empty */
    }
  }, [client, convId, threadId]);
  useEffect(() => {
    void load();
    // New replies, edits and recalls of this thread reload it.
    return client.on('event', (f: any) => {
      const e = f?.event ?? f;
      if (e?.conv_id === convId && (e.msg?.thread_id === threadId || e.msg?.msg_id === threadId)) void load();
    });
  }, [client, convId, threadId, load]);
  const draw = render ?? ((m: any) => (
    <div className="aim-thread-msg" data-state={m.state}>
      <span className="aim-thread-sender" dir="auto">{m.sender_name}</span>
      <span className="aim-text" dir="auto">{m.state === 'recalled' ? strings.recalled : m.content}</span>
    </div>
  ));
  const send = async () => {
    const t = text.trim();
    if (!t) return;
    setText('');
    await (client as any).send(convId, t, { threadId, alsoInConv: alsoInConv || undefined });
    void load();
  };
  return (
    <section className="aim-root aim-thread" aria-label={strings.thread}>
      <header className="aim-thread-header">
        <span>{strings.thread}</span>
        {onClose ? (
          <button type="button" aria-label={strings.close} onClick={onClose}>
            ×
          </button>
        ) : null}
      </header>
      <div className="aim-thread-body">
        {root ? <div className="aim-thread-root">{draw(root)}</div> : null}
        {replies.length ? <div className="aim-thread-count">{strings.replies(replies.length)}</div> : null}
        {replies.map((m) => (
          <div key={m.msg_id}>{draw(m)}</div>
        ))}
      </div>
      <form
        className="aim-thread-composer"
        onSubmit={(e) => {
          e.preventDefault();
          void send();
        }}
      >
        <input aria-label={strings.threadPlaceholder} placeholder={strings.threadPlaceholder} value={text} onChange={(e) => setText(e.target.value)} />
        <label className="aim-thread-also">
          <input type="checkbox" checked={alsoInConv} onChange={(e) => setAlso(e.target.checked)} />
          {strings.alsoInConversation}
        </label>
        <button type="submit" disabled={!text.trim()}>
          {strings.send}
        </button>
      </form>
    </section>
  );
}

/** The conversation's pinned messages: the latest one, and how many. A click hands the pins to `onOpen` (in `Chat`, it jumps to the latest one). */
export function PinnedBar({ convId, strings, onOpen }: { convId: string; strings: Strings; onOpen?(pins: any[]): void }) {
  const client = useClient();
  const [pins, setPins] = useState<any[]>([]);
  const [count, setCount] = useState(0);
  const load = useCallback(async () => {
    try {
      const r: any = await client.call('list_pins', { conv_id: convId });
      setPins(r.pins ?? []);
      setCount(r.count ?? 0);
    } catch {
      /* nothing shown */
    }
  }, [client, convId]);
  useEffect(() => {
    void load();
    return client.on('event', (f: any) => {
      const e = f?.event ?? f;
      if (e?.conv_id === convId && (e.type === 'msg.pinned' || e.type === 'msg.unpinned' || e.type === 'msg.recalled')) void load();
    });
  }, [client, convId, load]);
  if (!pins.length) return null;
  const latest = pins[0];
  return (
    <button type="button" className="aim-pinned" onClick={() => onOpen?.(pins)} aria-label={strings.pinnedCount(count)}>
      <span className="aim-pinned-icon">
        <Pin />
      </span>
      <span className="aim-pinned-body">
        <span className="aim-pinned-label">
          {strings.pinned}
          {count > 1 ? <span className="aim-pinned-count">{count}</span> : null}
        </span>
        <span className="aim-pinned-text" dir="auto">
          {latest.msg?.content}
        </span>
      </span>
    </button>
  );
}
