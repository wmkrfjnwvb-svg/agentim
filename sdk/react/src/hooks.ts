// Hooks over @agentimhq/client-web (docs/12 §3): the client's observables
// read with useSyncExternalStore (the snapshot is the same object until
// it changes), its methods wrapped for components.

import { createContext, createElement, useCallback, useContext, useEffect, useMemo, useRef, useState, useSyncExternalStore, type ReactNode } from 'react';
import type { AgentIMClient, Connection, Message } from '@agentimhq/client-web';

/** What the hooks need from a client: `AgentIMClient`, or a fake in tests. */
export type Client = Pick<AgentIMClient, 'connection' | 'conversations' | 'messages' | 'send' | 'edit' | 'recall' | 'retry' | 'discard' | 'feedback' | 'respond' | 'read' | 'call' | 'on'> &
  Partial<Pick<AgentIMClient, 'focus' | 'sendFile' | 'fileUrl' | 'localPreview'>>;

interface Readable<T> {
  get(): T;
  subscribe(fn: (v: T) => void): () => void;
}

const Ctx = createContext<Client | null>(null);

/** Gives the hooks their client (the components use `AgentIM`, which includes it). */
export function AgentIMProvider({ client, children }: { client: Client; children?: ReactNode }) {
  return createElement(Ctx.Provider, { value: client }, children);
}

/** The client given to the provider. */
export function useClient(): Client {
  const c = useContext(Ctx);
  if (!c) throw new Error('useClient: wrap the tree in <AgentIMProvider client={…}>');
  return c;
}

/** The current value of an observable, re-rendering on change. */
export function useObservable<T>(o: Readable<T>): T {
  return useSyncExternalStore(o.subscribe, o.get, o.get);
}

/** The connection state, re-rendering on change. */
export function useConnection(): Connection {
  return useObservable(useClient().connection);
}

/** The conversation list, re-rendering on change. */
export function useConversations(): any[] {
  return useObservable(useClient().conversations);
}

/** What `useMessages` returns. */
export interface MessagesState {
  /** The messages, oldest first. */
  messages: Message[];
  /** Whether older messages can still be loaded. */
  hasOlder: boolean;
  /** `start` when the beginning is reached, `retention` when older ones are gone. */
  olderEnd?: string;
  /** Whether older messages are loading now. */
  loadingOlder: boolean;
  /** Loads the next page of older messages. */
  loadOlder(): Promise<void>;
  /** Why loading older messages failed, if it did. */
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

/** Thumbnails fetched once per file and kept for the page (their content never changes). */
const thumbs = new Map<string, Promise<string>>();
const THUMBS_KEPT = 300;

/**
 * The address to show a file's thumbnail (`thumb`; a small image is its
 * own) or to read it: a local copy while this tab sends it, then the
 * server's. `error` when it is gone (expired, deleted).
 */
export function useFileUrl(convId: string, payload: any, variant: 'thumb' | 'original' = 'thumb', opts: { blob?: boolean } = {}): { url?: string; error?: string } {
  const client = useClient();
  const local = client.localPreview?.(variant === 'thumb' ? (payload?.local_thumb ?? payload?.local_source) : payload?.local_source);
  const fileId: string | undefined = payload?.file_id;
  const [state, setState] = useState<{ key?: string; url?: string; error?: string }>({});
  const key = fileId ? `${fileId}/${variant}` : undefined;
  useEffect(() => {
    if (!key || !fileId || !client.fileUrl) return;
    let live = true;
    // Only local copies are kept here: a signed address expires (the client
    // core reuses it while it is valid).
    const keep = variant === 'thumb' && opts.blob !== false;
    let p = keep ? thumbs.get(key) : undefined;
    if (!p) {
      p = client.fileUrl(convId, fileId, variant).then(async (r) => {
        // React Native has no Blob URLs (`blob: false`): its image cache keeps the picture.
        if (variant !== 'thumb' || opts.blob === false || typeof fetch !== 'function' || typeof URL.createObjectURL !== 'function') return r.url;
        // Kept as a local copy: the signed address changes, the picture does not.
        const b = await fetch(r.url).then((x) => (x.ok ? x.blob() : Promise.reject(new Error(String(x.status)))));
        return URL.createObjectURL(b);
      });
      if (keep) {
        thumbs.set(key, p);
        if (thumbs.size > THUMBS_KEPT) thumbs.delete(thumbs.keys().next().value!);
        p.catch(() => thumbs.delete(key));
      }
    }
    p.then(
      (url) => live && setState({ key, url }),
      (e) => live && setState({ key, error: e?.code ?? 'unavailable' }),
    );
    return () => {
      live = false;
    };
  }, [client, convId, fileId, key, variant, opts.blob]);
  if (state.key === key && (state.url || state.error)) return { url: state.url ?? local, error: state.url ? undefined : state.error };
  return { url: local };
}
