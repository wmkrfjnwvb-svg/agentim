// What a UI holds (docs/12 §2): errors, observable values and a
// conversation's message view. No core, storage or network here, so other
// runtimes (React Native, `@agentimhq/client-web/view`) can use it without
// the wasm loader.

/** What a view needs from its client. */
export interface Commands {
  command(cmd: Record<string, unknown>): Promise<any>;
}

export class TokenRefused extends Error {}

export class AgentIMError extends Error {
  code: string;
  retryable: boolean;
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
  get = (): T => this.v;
  subscribe = (fn: (v: T) => void): (() => void) => {
    this.fns.add(fn);
    return () => this.fns.delete(fn);
  };
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

export interface Connection {
  state: 'closed' | 'connecting' | 'connected' | 'disconnected' | 'suspended' | 'failed';
  reason?: string | null;
  retryAt?: number | null;
}

export interface Message {
  key: string;
  msg_id: string;
  client_msg_id?: string;
  conv_id: string;
  content: string;
  status: 'sent' | 'sending' | 'uploading' | 'failed';
  order: number;
  [k: string]: any;
}

/** One conversation's messages as shown: snapshot, then changes. */
export class Messages extends Observable<Message[]> {
  hasOlder = false;
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
