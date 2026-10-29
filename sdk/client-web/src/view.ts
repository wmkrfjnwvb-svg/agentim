// What a UI holds (docs/12 §2): errors, observable values and a
// conversation's message view. No core, storage or network here, so other
// runtimes (React Native, `@agentimhq/client-web/view`) can use it without
// the wasm loader.

/** What a view needs from its client. */
export interface Commands {
  /** Runs one core command and resolves with its answer; the typed methods are built on this. */
  command(cmd: Record<string, unknown>): Promise<any>;
}

/**
 * Thrown by a token provider when your backend refuses the user: the client stops instead of
 * retrying.
 */
export class TokenRefused extends Error {}

/**
 * A refused or failed command: `code` is the error code, `retryable` whether trying again may
 * work.
 */
export class AgentIMError extends Error {
  /**
   * The error code: the server's (`forbidden`, `not_found`, `conflict`, `bad_request`, …) or the
   * client's (`disconnected`, `timeout`, `closed`, `unauthenticated`).
   */
  code: string;
  /** Whether the same request may work later. */
  retryable: boolean;
  /** How long the server asked to wait before trying again. */
  retryAfterMs?: number;
  constructor(e: { code?: string; message?: string; retryable?: boolean; retry_after_ms?: number }) {
    super(e.message ?? e.code ?? 'error');
    this.code = e.code ?? 'internal';
    this.retryable = !!e.retryable;
    this.retryAfterMs = e.retry_after_ms;
  }
}

/** A value the UI follows: `get()` returns the same object until it changes. */
export class Observable<T> {
  private fns = new Set<(v: T) => void>();
  constructor(private v: T) {}
  /** The current value (the same object until it changes, for React's useSyncExternalStore). */
  get = (): T => this.v;
  /** Calls `fn` on every change; returns the function that stops it. */
  subscribe = (fn: (v: T) => void): (() => void) => {
    this.fns.add(fn);
    return () => this.fns.delete(fn);
  };
  /** Replaces the value and tells the subscribers. */
  set(v: T) {
    this.v = v;
    for (const f of [...this.fns]) {
      try {
        f(v);
      } catch {
        // A listener's error is the listener's.
      }
    }
  }
}

/** The connection state and why. */
export interface Connection {
  /**
   * `closed` (not started or closed), `connecting`, `connected`, `disconnected` (waiting to
   * reconnect), `suspended` (offline, or down for a long time: retries less often) or `failed`
   * (stopped: the token was refused or the account deactivated).
   */
  state: 'closed' | 'connecting' | 'connected' | 'disconnected' | 'suspended' | 'failed';
  /** Why it is not connected (an error code), when it is not. */
  reason?: string | null;
  /** When the next reconnect attempt is due (Unix ms), while waiting to reconnect. */
  retryAt?: number | null;
}

/** One message as shown in a list. */
export interface Message {
  /** The list identity: `local:<client_msg_id>` for a message sent from this device until the server's copy replaces it (in one change), then its `msg_id`. */
  key: string;
  /** The server's id (empty while it is still queued). */
  msg_id: string;
  /** The id this device gave the message, for retries. */
  client_msg_id?: string;
  /** The conversation it belongs to. */
  conv_id: string;
  /** The text (for a streaming reply, what has arrived so far). */
  content: string;
  /**
   * `sent`, `sending` (queued or on its way), `uploading` (its file is uploading) or `failed`
   * (refused; retry or discard it).
   */
  status: 'sent' | 'sending' | 'uploading' | 'failed';
  /** The sort position in the list. */
  order: number;
  [k: string]: any;
}

/** One conversation's messages as shown: snapshot, then changes. */
export class Messages extends Observable<Message[]> {
  /** Whether older messages can still be loaded. */
  hasOlder = false;
  /**
   * Why nothing older can be loaded: `start` (the beginning) or `retention` (deleted by the
   * retention period).
   */
  olderEnd?: string;
  private byKey = new Map<string, Message>();
  constructor(
    readonly convId: string,
    private client: Commands,
  ) {
    super([]);
  }
  /** @internal */
  reset(list: Message[], hasOlder?: boolean, olderEnd?: string) {
    this.byKey = new Map(list.map((m) => [m.key, m]));
    if (hasOlder !== undefined) this.hasOlder = hasOlder;
    if (olderEnd !== undefined) this.olderEnd = olderEnd;
    this.publish();
  }
  /** @internal */
  apply(changes: any[]) {
    this.applyEvent({ changes });
  }
  /** @internal A `messages` event: with `reset` the view is replaced by
   * the changes in one step, so no one sees it empty in between. */
  applyEvent(e: { reset?: boolean; changes?: any[] }) {
    if (e.reset) this.byKey.clear();
    for (const c of e.changes ?? []) {
      if (c.op === 'upsert') this.byKey.set(c.message.key, c.message);
      else this.byKey.delete(c.key);
    }
    this.publish();
  }
  private publish() {
    this.set([...this.byKey.values()].sort((a, b) => a.order - b.order));
  }
  /** The page before the oldest shown: the cache first, then the server. */
  async loadOlder(): Promise<boolean> {
    const r = await this.client.command({ cmd: 'load_older', conv_id: this.convId });
    this.hasOlder = r.has_older;
    this.olderEnd = r.older_end ?? undefined;
    return r.has_older;
  }
  /** A page around a message (it stays loaded). */
  jumpTo(seq: number) {
    return this.client.command({ cmd: 'jump_to', conv_id: this.convId, seq });
  }
}
