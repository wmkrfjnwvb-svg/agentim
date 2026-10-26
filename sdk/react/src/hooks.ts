// Hooks over @agentimhq/client-web (docs/12 §3): the client's observables
// read with useSyncExternalStore (the snapshot is the same object until
// it changes), its methods wrapped for components.

import { createContext, createElement, useCallback, useContext, useEffect, useMemo, useRef, useState, useSyncExternalStore, type ReactNode } from 'react';
import type { AgentIMClient, Connection, Message } from '@agentimhq/client-web';

/** What the hooks need from a client: `AgentIMClient`, or a fake in tests. */
export type Client = Pick<AgentIMClient, 'connection' | 'conversations' | 'messages' | 'send' | 'edit' | 'recall' | 'retry' | 'discard' | 'feedback' | 'respond' | 'read' | 'call' | 'on'> &
  Partial<Pick<AgentIMClient, 'focus'>>;

interface Readable<T> {
  get(): T;
  subscribe(fn: (v: T) => void): () => void;
}

const Ctx = createContext<Client | null>(null);

export function AgentIMProvider({ client, children }: { client: Client; children?: ReactNode }) {
  return createElement(Ctx.Provider, { value: client }, children);
}

export function useClient(): Client {
  const c = useContext(Ctx);
  if (!c) throw new Error('useClient: wrap the tree in <AgentIMProvider client={…}>');
  return c;
}

export function useObservable<T>(o: Readable<T>): T {
  return useSyncExternalStore(o.subscribe, o.get, o.get);
}

export function useConnection(): Connection {
  return useObservable(useClient().connection);
}

export function useConversations(): any[] {
  return useObservable(useClient().conversations);
}

export interface MessagesState {
  messages: Message[];
  hasOlder: boolean;
  /** `start` when the beginning is reached, `retention` when older ones are gone. */
  olderEnd?: string;
  loadingOlder: boolean;
  loadOlder(): Promise<void>;
  error?: { code: string; message: string };
}

/** One conversation's messages, oldest first, with paging back. */
export function useMessages(convId: string): MessagesState {
  const client = useClient();
  const view = useMemo(() => client.messages(convId), [client, convId]);
  const messages = useObservable(view);
  const [loadingOlder, setLoading] = useState(false);
  const [error, setError] = useState<{ code: string; message: string }>();
  const loadOlder = useCallback(async () => {
    setLoading(true);
    setError(undefined);
    try {
      await view.loadOlder();
    } catch (e: any) {
      setError({ code: e?.code ?? 'internal', message: e?.message ?? String(e) });
    } finally {
      setLoading(false);
    }
  }, [view]);
  return { messages, hasOlder: view.hasOlder, olderEnd: view.olderEnd, loadingOlder, loadOlder, error };
}

/** Sends to a conversation; the message shows at once and is sent when it can be. */
export function useSend(convId: string) {
  const client = useClient();
  const [error, setError] = useState<{ code: string; message: string }>();
  const send = useCallback(
    async (text: string, opts?: { replyTo?: string }) => {
      setError(undefined);
      try {
        return await client.send(convId, text, opts);
      } catch (e: any) {
        setError({ code: e?.code ?? 'internal', message: e?.message ?? String(e) });
        return undefined;
      }
    },
    [client, convId],
  );
  return { send, error };
}

const TYPING_SHOWN_MS = 6000;
const TYPING_EVERY_MS = 3000;

/** Who is typing in the conversation (user ids), and `notify()` to say that I am. */
export function useTyping(convId: string) {
  const client = useClient();
  const [typing, setTyping] = useState<string[]>([]);
  const until = useRef(new Map<string, number>());
  const last = useRef(0);
  useEffect(() => {
    until.current.clear();
    setTyping([]);
    const publish = () => {
      const now = Date.now();
      for (const [k, t] of until.current) if (t <= now) until.current.delete(k);
      setTyping([...until.current.keys()]);
    };
    const off = client.on('signal', (e) => {
      if (e.conv_id !== convId || e.kind !== 'typing' || !e.from) return;
      until.current.set(e.from, Date.now() + TYPING_SHOWN_MS);
      publish();
    });
    const timer = setInterval(publish, 1000);
    return () => {
      off();
      clearInterval(timer);
    };
  }, [client, convId]);
  const notify = useCallback(() => {
    const now = Date.now();
    if (now - last.current < TYPING_EVERY_MS) return;
    last.current = now;
    // Best effort: typing is not worth an error.
    client.call('signal', { conv_id: convId, kind: 'typing' }).catch(() => {});
  }, [client, convId]);
  return { typing, notify };
}
