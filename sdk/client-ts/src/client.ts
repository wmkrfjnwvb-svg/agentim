// AgentIM client (docs/12-developer-experience.md §2.1, rules in
// docs/05-operations.md §6 and docs/08-sync-and-client.md).
//
//   const client = new AgentIMClient({ url, tokenProvider: () => fetch('/my/token').then(r => r.text()) });
//   client.on('messages', ({ convId, messages }) => render(convId, messages));
//   await client.connect();
//   await client.send(convId, 'hello');
//
// Guarantees: per conversation, events are applied in seq order, each once;
// a send is retried with the same client_msg_id until the server answers
// (the server deduplicates), for at most `sendRetryMs`; a missed push is
// found by the gap it leaves or by the periodic position check.

import { MemoryStore, type Store } from './store/index.js';
import {
  AgentIMError,
  RETRYABLE,
  type ChatMessage,
  type ConnectionState,
  type Event,
  type Me,
  type Msg,
  type SideEvent,
} from './types.js';

type WebSocketCtor = new (url: string) => WebSocket;

export interface ClientOptions {
  /** Server address, e.g. `https://chat.example.com`. */
  url: string;
  /** Returns a user token from your backend (it calls the open API). Called again when the server refuses the current one. */
  tokenProvider: () => Promise<string>;
  /**
   * The local cache: `SqliteStore` on devices, `IndexedDBStore` in browsers
   * (docs/12 §2.1); in memory by default. One per account.
   */
  store?: Store;
  /**
   * The user this client is for (the id your backend issues tokens for).
   * Known up front, the cache opens before the connection (messages show
   * offline); otherwise it opens once the server says who it is.
   */
  userId?: string;
  /** Cache size limit in bytes of message content (default 256 MB, at least 32 MB). */
  maxCacheBytes?: number;
  /** Messages per page from the cache (default 50). */
  pageSize?: number;
  /** For runtimes without a global WebSocket. */
  WebSocket?: WebSocketCtor;
  clientName?: string;
  clientVersion?: string;
  /** How long a send is retried before it is marked failed (default 2 min). */
  sendRetryMs?: number;
  /** How often positions are compared with the server (default 30 s). */
  positionCheckMs?: number;
  log?: (m: string) => void;
}

export interface ClientEvents {
  connection: { state: ConnectionState; attempt?: number };
  /** A conversation's messages changed (new, edited, recalled, streamed, sent, failed). */
  messages: { convId: string; messages: ChatMessage[] };
  /** Every applied event, for apps that track more than messages. */
  event: Event;
  /** Approvals, decisions, whispers and other side-channel events. */
  side: SideEvent;
  /** Typing and other signals of members online. */
  signal: { convId: string; kind: string; from: string; data?: unknown };
  /** The device's position in a conversation was reset by the server: older messages may be missing. */
  reset: { convId: string; fromSeq: number };
  /** The local cache was rebuilt (another account, an unknown or damaged store) or emptied conversations to stay under its limit. */
  storage: { reset?: boolean; evicted?: string[] };
}

type Handler<T> = (v: T) => void;

interface Conv {
  convId: string;
  /** Highest seq applied and acknowledged. */
  lastSeq: number;
  openedAt: number;
  /** The loaded window: sent ones by seq, unsent ones last. */
  messages: ChatMessage[];
  /** Older messages may be in the cache (the first page was full). */
  hasOlder: boolean;
  /** Pushes that arrived ahead of a gap, applied once it is filled. */
  ahead: Map<number, Event>;
  /** Keys of messages changed (or gone) since the last write to the store. */
  dirty: Set<string>;
  gone: Set<string>;
}

const uid = () =>
  globalThis.crypto?.randomUUID?.() ?? `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;

export class AgentIMClient {
  me: Me | null = null;
  state: ConnectionState = 'closed';
  private o: Required<Omit<ClientOptions, 'store' | 'WebSocket' | 'log' | 'userId'>> & Pick<ClientOptions, 'log' | 'userId'>;
  private store: Store;
  private WS: WebSocketCtor;
  private ws: WebSocket | null = null;
  private rid = 0;
  private pending = new Map<number, { resolve: (v: any) => void; reject: (e: AgentIMError) => void }>();
  private handlers = new Map<keyof ClientEvents, Set<Handler<any>>>();
  private convs = new Map<string, Conv>();
  private stopped = true;
  private queue: Promise<unknown> = Promise.resolve();
  private connected: Promise<void> = Promise.resolve();
  private markConnected: () => void = () => {};
  /** Opened for one account; until then nothing is read or written. */
  private storeFor: string | null = null;
  private storeOpening: Promise<void> | null = null;
  private writes = 0;
  private typingAt = new Map<string, number>();
  private focused: string | null = null;
  /** Sends waiting for their event to be applied. */
  private waiters = new Map<string, () => void>();
  /** Timers of the client's own loops; `close()` ends them all. */
  private timers = new Map<ReturnType<typeof setTimeout>, () => void>();

  constructor(opts: ClientOptions) {
    this.o = {
      clientName: 'agentim-js',
      clientVersion: '0.1.0',
      sendRetryMs: 120_000,
      positionCheckMs: 30_000,
      maxCacheBytes: 256 * 1024 * 1024,
      pageSize: 50,
      ...opts,
    };
    this.o.maxCacheBytes = Math.max(this.o.maxCacheBytes, 32 * 1024 * 1024);
    this.store = opts.store ?? new MemoryStore();
    const ws = opts.WebSocket ?? (globalThis as any).WebSocket;
    if (!ws) throw new Error('no WebSocket: pass options.WebSocket');
    this.WS = ws;
  }

  // ------------------------------------------------------------ events

  on<K extends keyof ClientEvents>(k: K, fn: Handler<ClientEvents[K]>): () => void {
    let s = this.handlers.get(k);
    if (!s) this.handlers.set(k, (s = new Set()));
    s.add(fn);
    return () => s!.delete(fn);
  }

  private emit<K extends keyof ClientEvents>(k: K, v: ClientEvents[K]) {
    for (const f of this.handlers.get(k) ?? []) {
      try {
        f(v);
      } catch (e) {
        this.o.log?.(`handler for ${k} threw: ${e}`);
      }
    }
  }

  private setState(state: ConnectionState, attempt?: number) {
    this.state = state;
    this.emit('connection', { state, attempt });
  }

  // -------------------------------------------------------- connection

  /** Connects, syncs, and keeps the connection up until `close()`. */
  async connect(): Promise<void> {
    if (!this.stopped) return this.connected;
    this.stopped = false;
    this.connected = new Promise((r) => (this.markConnected = r));
    void this.keepAlive();
    void this.checkPositions();
    return this.connected;
  }

  /** Waits, unless the client closes first (so no timer outlives it). */
  private wait(ms: number): Promise<void> {
    return new Promise((resolve) => {
      if (this.stopped) return resolve();
      const t = setTimeout(() => {
        this.timers.delete(t);
        resolve();
      }, ms);
      this.timers.set(t, resolve);
    });
  }

  /** Drops the connection; the keep-alive loop opens a new one at once. */
  private reconnect(why: string) {
    this.o.log?.(`reconnecting: ${why}`);
    this.ws?.close(4000);
  }

  close() {
    this.stopped = true;
    for (const [t, resolve] of this.timers) {
      clearTimeout(t);
      resolve();
    }
    this.timers.clear();
    this.ws?.close();
    this.setState('closed');
  }

  /** Logs out locally: closes and wipes the cache (GDPR). */
  async logout() {
    this.close();
    this.convs.clear();
    await this.store.clear();
  }

  private async keepAlive() {
    let attempt = 0;
    let wait = 500;
    while (!this.stopped) {
      this.setState(attempt ? 'reconnecting' : 'connecting', attempt || undefined);
      try {
        const { closed } = await this.open();
        attempt = 0;
        wait = 500;
        this.markConnected();
        this.setState('connected');
        const code = await closed;
        // 1012: the node is going away; reconnect at once.
        if (code === 1012 || code === 4000) wait = 0;
      } catch (e) {
        this.o.log?.(`connect failed: ${e}`);
      }
      if (this.stopped) return;
      attempt++;
      // Backoff with jitter so many clients do not reconnect together.
      await this.wait(wait + Math.random() * wait);
      wait = Math.min(Math.max(wait, 250) * 2, 30_000);
    }
  }

  /** Opens one connection; `closed` resolves with its close code (wrapped: an async function would wait for it). */
  private async open(): Promise<{ closed: Promise<number> }> {
    const token = await this.o.tokenProvider();
    const base = this.o.url.replace(/^http/, 'ws').replace(/\/$/, '');
    const ws = new this.WS(`${base}/ws?token=${encodeURIComponent(token)}`);
    let welcomed = false;
    const closed = new Promise<number>((done) => {
      ws.addEventListener('close', (e: any) => {
        for (const [, p] of this.pending) p.reject(new AgentIMError('disconnected', 'connection closed'));
        this.pending.clear();
        if (this.ws === ws) this.ws = null;
        done(e?.code ?? 1006);
      });
    });
    await new Promise<void>((resolve, reject) => {
      ws.addEventListener('error', () => {
        if (!welcomed) reject(new AgentIMError('unauthorized', 'could not connect'));
      });
      void closed.then(() => {
        if (!welcomed) reject(new AgentIMError('disconnected', 'closed before welcome'));
      });
      ws.addEventListener('message', (m: any) => {
        let f: any;
        try {
          f = JSON.parse(String(m.data));
        } catch {
          return;
        }
        if (f.type === 'welcome') {
          welcomed = true;
          this.me = f.me;
          if (this.o.userId && f.me?.id && f.me.id !== this.o.userId) this.o.log?.(`options.userId ${this.o.userId} is not the token's user ${f.me.id}; using the token's`);
          this.ws = ws;
          resolve();
          return;
        }
        this.frame(f);
      });
    });
    await this.call('hello', { client: { name: this.o.clientName, version: this.o.clientVersion } });
    // The token's user decides whose cache this is.
    await this.openStore(this.me?.id ?? this.o.userId ?? 'anonymous');
    await this.serial(() => this.sync());
    if (this.focused) await this.call('focus', { conv_id: this.focused }).catch(() => {});
    void this.flushOutbox();
    return { closed };
  }

  /** Sends one request and waits for its answer. */
  call<T = any>(op: string, args: Record<string, unknown> = {}): Promise<T> {
    const ws = this.ws;
    if (!ws || ws.readyState !== 1) return Promise.reject(new AgentIMError('disconnected', 'not connected'));
    const rid = ++this.rid;
    return new Promise<T>((resolve, reject) => {
      const t = setTimeout(() => {
        if (this.pending.delete(rid)) reject(new AgentIMError('timeout', `${op} timed out`));
      }, 30_000);
      this.pending.set(rid, {
        resolve: (v) => {
          clearTimeout(t);
          resolve(v);
        },
        reject: (e) => {
          clearTimeout(t);
          reject(e);
        },
      });
      ws.send(JSON.stringify({ op, rid, ...args }));
    });
  }

  /** Sync rounds and pushes run one at a time, in arrival order. */
  private serial<T>(f: () => Promise<T>): Promise<T> {
    const run = this.queue.then(f);
    this.queue = run.catch((e) => this.o.log?.(`sync step failed: ${e}`));
    return run;
  }

  private frame(f: any) {
    switch (f.type) {
      case 'res': {
        const p = this.pending.get(f.rid);
        if (!p) return;
        this.pending.delete(f.rid);
        if (f.ok) p.resolve(f);
        else p.reject(new AgentIMError(f.error?.code ?? 'internal', f.error?.message ?? ''));
        return;
      }
      case 'event':
        void this.serial(() => this.pushed(f.event as Event));
        return;
      case 'side':
        this.emit('side', f.event as SideEvent);
        void this.call('ack', { acks: [{ conv_id: f.event.conv_id, side_seq: f.event.side_seq }] }).catch(() => {});
        return;
      case 'delta':
        void this.serial(async () => this.delta(f));
        return;
      case 'signal':
        this.emit('signal', { convId: f.conv_id, kind: f.kind, from: f.from, data: f.data });
        return;
      case 'resync':
        void this.serial(() => this.sync());
        return;
      default:
      // Unknown frames are skipped (a newer server).
    }
  }

  // ------------------------------------------------------------- cache

  /** Opens the cache for an account (wiping another account's data). */
  private openStore(account: string): Promise<void> {
    if (this.storeFor === account && this.storeOpening) return this.storeOpening;
    this.storeFor = account;
    this.storeOpening = (async () => {
      const { reset } = await this.store.open(account);
      if (reset) {
        this.convs.clear();
        this.emit('storage', { reset: true });
      }
    })();
    return this.storeOpening;
  }

  private async ready() {
    if (this.o.userId) await this.openStore(this.o.userId);
    else if (this.storeOpening) await this.storeOpening;
    else throw new AgentIMError('not_ready', 'pass options.userId to read the cache before connecting');
  }

  private async conv(convId: string): Promise<Conv> {
    let c = this.convs.get(convId);
    if (c) return c;
    await this.ready();
    const row = await this.store.loadConv(convId);
    const page = row ? await this.store.messages(convId, { limit: this.o.pageSize }) : [];
    // Another call may have made it while the store answered.
    c = this.convs.get(convId) ?? {
      convId,
      lastSeq: row?.lastSeq ?? 0,
      openedAt: row?.openedAt ?? 0,
      messages: page,
      hasOlder: page.filter((m) => m.status === 'sent').length >= this.o.pageSize,
      ahead: new Map(),
      dirty: new Set(),
      gone: new Set(),
    };
    this.convs.set(convId, c);
    return c;
  }

  /** Keeps the cache under its limit, now and then (never what is on screen or queued). */
  private async trim() {
    if (++this.writes % 100 !== 1) return;
    try {
      if ((await this.store.size()) <= this.o.maxCacheBytes) return;
      const protect = new Set<string>(this.focused ? [this.focused] : []);
      const evicted = await this.store.evict(Math.floor(this.o.maxCacheBytes * 0.9), protect);
      for (const id of evicted) {
        const c = this.convs.get(id);
        if (c && id !== this.focused) this.convs.delete(id);
      }
      if (evicted.length) this.emit('storage', { evicted });
    } catch (e) {
      this.o.log?.(`cache trim failed: ${e}`);
    }
  }

  // ----------------------------------------------------------- syncing

  /** Sync rounds until the server owes nothing; each batch is acknowledged after it is stored. */
  private async sync(): Promise<void> {
    for (let round = 0; round < 1000; round++) {
      const r = await this.call('sync', { limit: 200 });
      const acks: any[] = [];
      for (const s of r.convs ?? []) {
        const c = await this.conv(s.conv_id);
        if (s.reset) {
          // The server skipped ahead (a long absence): local history before it may have gaps.
          c.lastSeq = Math.max(0, s.reset.from_seq - 1);
          this.emit('reset', { convId: s.conv_id, fromSeq: s.reset.from_seq });
        }
        // Pushes still queued behind this sync may come before these
        // events: only what continues the log is applied, the rest waits.
        for (const item of s.events ?? []) this.place(c, item.event as Event);
        this.drain(c);
        for (const side of s.side_events ?? []) this.emit('side', side);
        await this.persist(c);
        if (s.upto !== undefined) acks.push({ conv_id: s.conv_id, seq: Math.min(s.upto, c.lastSeq), side_seq: s.side_upto ?? 0 });
      }
      if (acks.length) await this.call('ack', { acks });
      if (!r.more) return;
    }
  }

  private async pushed(ev: Event) {
    const c = await this.conv(ev.conv_id);
    if (ev.seq <= c.lastSeq) return; // seen already
    this.place(c, ev);
    this.drain(c);
    await this.persist(c);
    await this.call('ack', { acks: [{ conv_id: c.convId, seq: c.lastSeq }] }).catch(() => {});
    // A hole: what fills it may still be queued, so look again after it.
    if (c.ahead.size > 0) void this.serial(() => this.holeCheck(c));
  }

  /** Applies an event that continues the log; keeps one that comes after a hole. */
  private place(c: Conv, ev: Event) {
    if (ev.seq <= c.lastSeq) return;
    if (ev.seq === c.lastSeq + 1) this.apply(c, ev);
    else c.ahead.set(ev.seq, ev);
  }

  private drain(c: Conv) {
    while (c.ahead.has(c.lastSeq + 1)) {
      const next = c.ahead.get(c.lastSeq + 1)!;
      c.ahead.delete(next.seq);
      this.apply(c, next);
    }
    for (const s of [...c.ahead.keys()]) if (s <= c.lastSeq) c.ahead.delete(s);
  }

  /**
   * A hole nothing queued filled: sync; if the server still owes nothing
   * (it never resends a push on the same connection), a new connection
   * syncs from the acknowledged position.
   */
  private async holeCheck(c: Conv) {
    if (c.ahead.size === 0) return;
    await this.sync();
    this.drain(c);
    if (c.ahead.size > 0) this.reconnect('gap');
  }

  /** Applies one event in order; returns false when it was a duplicate. */
  private apply(c: Conv, ev: Event): boolean {
    if (ev.seq <= c.lastSeq) return false;
    c.lastSeq = ev.seq;
    if (ev.msg) {
      this.upsert(c, ev.msg);
      this.waiters.get(ev.msg.msg_id)?.();
    }
    this.emit('event', ev);
    if (ev.msg) this.emit('messages', { convId: c.convId, messages: this.messages(c.convId) });
    return true;
  }

  private upsert(c: Conv, msg: Msg) {
    const i = c.messages.findIndex(
      (m) => m.msg_id === msg.msg_id || (m.status !== 'sent' && msg.client_msg_id && m.client_msg_id === msg.client_msg_id && m.sender_id === msg.sender_id),
    );
    const old = i >= 0 ? c.messages[i] : undefined;
    // An older version (a late push) never replaces a newer one.
    if (old && old.status === 'sent' && old.version > msg.version) return;
    const next: ChatMessage = {
      ...msg,
      status: 'sent',
      streamed: msg.state === 'streaming' ? (old?.streamed ?? '') : undefined,
    };
    if (i >= 0) {
      c.messages.splice(i, 1);
      if (old!.msg_id !== next.msg_id) c.gone.add(old!.msg_id);
    }
    c.dirty.add(next.msg_id);
    // Sent messages by seq; unsent ones stay at the end.
    let at = c.messages.findIndex((m) => m.status !== 'sent' || m.created_seq > next.created_seq);
    if (at < 0) at = c.messages.length;
    c.messages.splice(at, 0, next);
  }

  private delta(f: { conv_id: string; msg_id: string; text: string; reset?: boolean }) {
    const c = this.convs.get(f.conv_id);
    const m = c?.messages.find((x) => x.msg_id === f.msg_id);
    if (!c || !m || m.state !== 'streaming') return;
    m.streamed = (f.reset ? '' : (m.streamed ?? '')) + f.text;
    this.emit('messages', { convId: c.convId, messages: this.messages(c.convId) });
  }

  /** Writes what changed: the position and the changed messages, together. */
  private async persist(c: Conv) {
    const put = c.messages.filter((m) => c.dirty.has(m.msg_id));
    const gone = [...c.gone].filter((k) => !put.some((m) => m.msg_id === k));
    c.dirty.clear();
    c.gone.clear();
    await this.store.write({
      convs: [{ convId: c.convId, lastSeq: c.lastSeq, openedAt: c.openedAt }],
      put,
      remove: gone.map((key) => ({ convId: c.convId, key })),
    });
    void this.trim();
  }

  // Now and then compare positions with the server: a lost last push
  // (nothing after it, no resync notice) is found and synced.
  private async checkPositions() {
    while (!this.stopped) {
      await this.wait(this.o.positionCheckMs);
      if (this.stopped || !this.ws || this.convs.size === 0) continue;
      const ids = [...this.convs.keys()].slice(0, 200);
      try {
        const r = await this.call('heads', { conv_ids: ids });
        const behind = () => ids.some((id) => (r.heads?.[id] ?? 0) > (this.convs.get(id)?.lastSeq ?? 0));
        if (behind()) {
          await this.serial(() => this.sync());
          if (behind()) this.reconnect('behind');
        }
      } catch {
        // Next round.
      }
    }
  }

  // ------------------------------------------------------------ reading

  /**
   * Opens a conversation: its newest page from the cache (also offline,
   * with options.userId), then whatever sync brings. Marks it as used
   * now (the cache keeps the conversations used most recently).
   */
  async load(convId: string): Promise<ChatMessage[]> {
    const c = await this.conv(convId);
    c.openedAt = Date.now();
    await this.persist(c);
    return this.messages(convId);
  }

  /**
   * The page before the oldest loaded message, from the cache. Returns
   * false when there is nothing older here (the server gives a device
   * nothing from before it started, docs/08).
   */
  async loadOlder(convId: string): Promise<boolean> {
    const c = await this.conv(convId);
    const oldest = c.messages.find((m) => m.status === 'sent');
    if (!c.hasOlder || !oldest) return false;
    const page = await this.store.messages(convId, { beforeSeq: oldest.created_seq, limit: this.o.pageSize });
    const known = new Set(c.messages.map((m) => m.msg_id));
    c.messages.unshift(...page.filter((m) => !known.has(m.msg_id)));
    c.hasOlder = page.length >= this.o.pageSize;
    this.emit('messages', { convId, messages: this.messages(convId) });
    return c.hasOlder;
  }

  /** Whether older messages may be loaded with `loadOlder`. */
  hasOlder(convId: string): boolean {
    return this.convs.get(convId)?.hasOlder ?? false;
  }

  messages(convId: string): ChatMessage[] {
    return (this.convs.get(convId)?.messages ?? []).map((m) => ({ ...m }));
  }

  /** The user's conversations with unread counts and last messages. */
  conversations(): Promise<any[]> {
    return this.call('list_convs').then((r) => r.convs ?? []);
  }

  // ------------------------------------------------------------ writing

  /**
   * Sends a message. It shows at once (`sending`), then `sent` with its
   * seq, or `failed` with the server's code. Retryable errors are retried
   * with the same client_msg_id, so it is never sent twice.
   */
  async send(
    convId: string,
    text: string,
    opts: { replyTo?: string; kind?: string; payload?: unknown; clientMsgId?: string } = {},
  ): Promise<ChatMessage> {
    const c = await this.conv(convId);
    const cm = opts.clientMsgId ?? uid();
    const now = Date.now();
    const local: ChatMessage = {
      msg_id: `local:${cm}`,
      client_msg_id: cm,
      conv_id: convId,
      created_seq: Number.MAX_SAFE_INTEGER,
      sender_id: this.me?.id ?? '',
      sender_name: this.me?.name ?? '',
      sender_type: 'human',
      kind: opts.kind ?? 'text',
      content: text,
      payload: opts.payload,
      version: 0,
      state: 'committed',
      reply_to: opts.replyTo ?? null,
      created_at: now,
      updated_at: now,
      status: 'sending',
    };
    if (!c.messages.some((m) => m.client_msg_id === cm)) {
      c.messages.push(local);
      c.dirty.add(local.msg_id);
    }
    this.emit('messages', { convId, messages: this.messages(convId) });
    await this.persist(c);
    return this.deliver(convId, local, now + this.o.sendRetryMs);
  }

  private async deliver(convId: string, local: ChatMessage, deadline: number): Promise<ChatMessage> {
    const args: Record<string, unknown> = { conv_id: convId, client_msg_id: local.client_msg_id, text: local.content };
    if (local.kind !== 'text') args.kind = local.kind;
    if (local.payload !== undefined) args.payload = local.payload;
    if (local.reply_to) args.reply_to = local.reply_to;
    let wait = 250;
    for (;;) {
      // Closed: it stays `sending` and goes out after the next connect.
      if (this.stopped) return this.find(convId, local.client_msg_id!) ?? { ...local };
      try {
        if (!this.ws) await Promise.race([this.connected, this.wait(wait)]);
        const r = await this.call('send', args);
        // The answer names the stored message: the local copy becomes it
        // (or goes, when the event already arrived), then the event fills it in.
        const applied = this.appliedOnce(r.msg_id);
        const pendingEvent = await this.serial(async () => {
          const c = await this.conv(convId);
          this.settle(c, local.client_msg_id!, r.msg_id, r.seq);
          await this.persist(c);
          return c.messages.some((m) => m.msg_id === r.msg_id && m.status !== 'sent');
        });
        // The event is on its way (pushed before the answer); a sync gets it
        // if it was lost.
        if (pendingEvent && !(await Promise.race([applied, this.wait(3000).then(() => false)]))) {
          await this.serial(() => this.sync());
        }
        this.waiters.delete(r.msg_id);
        return this.messages(convId).find((m) => m.msg_id === r.msg_id) ?? { ...local, status: 'sent' };
      } catch (e) {
        const err = e instanceof AgentIMError ? e : new AgentIMError('internal', String(e));
        if (!RETRYABLE.has(err.code) || Date.now() + wait > deadline) {
          return this.fail(convId, local.client_msg_id!, err);
        }
        await this.wait(wait);
        wait = Math.min(wait * 2, 5000);
      }
    }
  }

  private appliedOnce(msgId: string): Promise<boolean> {
    return new Promise((resolve) => this.waiters.set(msgId, () => resolve(true)));
  }

  private find(convId: string, cm: string) {
    return this.messages(convId).find((m) => m.client_msg_id === cm);
  }

  /** Links the local copy of a sent message to the stored one. */
  private settle(c: Conv, cm: string, msgId: string, seq: number) {
    const i = c.messages.findIndex((m) => m.client_msg_id === cm && m.status !== 'sent' && m.msg_id.startsWith('local:'));
    if (i < 0) return;
    c.gone.add(c.messages[i].msg_id);
    if (c.messages.some((m) => m.msg_id === msgId)) {
      c.messages.splice(i, 1);
    } else {
      c.messages[i].msg_id = msgId;
      c.messages[i].created_seq = seq;
      c.dirty.add(msgId);
    }
    this.emit('messages', { convId: c.convId, messages: this.messages(c.convId) });
  }

  private async fail(convId: string, cm: string, e: AgentIMError): Promise<ChatMessage> {
    const c = await this.conv(convId);
    const m = c.messages.find((x) => x.client_msg_id === cm && x.status !== 'sent');
    if (m) {
      m.status = 'failed';
      m.error = { code: e.code, message: e.detail };
      c.dirty.add(m.msg_id);
      await this.persist(c);
      this.emit('messages', { convId, messages: this.messages(convId) });
    }
    return { ...(m ?? ({} as ChatMessage)) };
  }

  /** Sends a failed message again (same client_msg_id: never twice). */
  async retry(convId: string, clientMsgId: string): Promise<ChatMessage> {
    const c = await this.conv(convId);
    const m = c.messages.find((x) => x.client_msg_id === clientMsgId && x.status === 'failed');
    if (!m) throw new AgentIMError('not_found', 'no failed message with this client_msg_id');
    m.status = 'sending';
    delete m.error;
    c.dirty.add(m.msg_id);
    await this.persist(c);
    this.emit('messages', { convId, messages: this.messages(convId) });
    return this.deliver(convId, m, Date.now() + this.o.sendRetryMs);
  }

  /** Drops a failed message from the local list. */
  async discard(convId: string, clientMsgId: string) {
    const c = await this.conv(convId);
    for (const x of c.messages) if (x.client_msg_id === clientMsgId && x.status !== 'sent') c.gone.add(x.msg_id);
    c.messages = c.messages.filter((x) => !(x.client_msg_id === clientMsgId && x.status !== 'sent'));
    await this.persist(c);
    this.emit('messages', { convId, messages: this.messages(convId) });
  }

  /** After a reconnect, sends what was still being sent (e.g. after a reload). */
  private async flushOutbox() {
    for (const [convId, c] of this.convs) {
      for (const m of c.messages.filter((x) => x.status === 'sending')) {
        void this.deliver(convId, m, Math.max(m.created_at + this.o.sendRetryMs, Date.now() + 5000));
      }
    }
  }

  edit(msgId: string, text: string) {
    return this.call('edit', { msg_id: msgId, text });
  }
  recall(msgId: string) {
    return this.call('recall', { msg_id: msgId });
  }
  /** Marks the conversation read up to a seq (default: the latest known). */
  async read(convId: string, seq?: number) {
    const c = await this.conv(convId);
    return this.call('read', { conv_id: convId, seq: seq ?? c.lastSeq });
  }
  /**
   * The conversation on screen (null: none). Streamed AI replies arrive
   * token by token only for it; others show when complete. Kept across
   * reconnects.
   */
  async focus(convId: string | null) {
    this.focused = convId;
    if (this.ws) await this.call('focus', { conv_id: convId });
  }
  /** Says the user is typing (sent at most every 3 s per conversation). */
  typing(convId: string) {
    const last = this.typingAt.get(convId) ?? 0;
    if (Date.now() - last < 3000) return Promise.resolve();
    this.typingAt.set(convId, Date.now());
    return this.call('signal', { conv_id: convId, kind: 'typing' }).catch(() => {});
  }
  createGroup(title: string, memberIds: string[]): Promise<string> {
    return this.call('create_conv', { kind: 'group', title, members: memberIds }).then((r) => r.conv_id);
  }
  openDirect(userId: string): Promise<string> {
    return this.call('create_conv', { kind: 'dm', title: '', members: [userId] }).then((r) => r.conv_id);
  }

  // ----------------------------------------------------- AI interaction

  /** Approves an agent's draft (optionally edited). */
  approve(draftId: string, text?: string) {
    return this.call('approve', text === undefined ? { draft_id: draftId } : { draft_id: draftId, text });
  }
  reject(draftId: string) {
    return this.call('reject', { draft_id: draftId });
  }
  /** Answers an agent's ask-a-human request. */
  decide(requestId: string, decision: string, text?: string) {
    return this.call('decide', text === undefined ? { request_id: requestId, decision } : { request_id: requestId, decision, text });
  }
  /** Responds to a card's button or form. */
  respond(msgId: string, blockId: string, response: { action?: string; values?: Record<string, unknown> }) {
    return this.call('respond', { msg_id: msgId, block_id: blockId, ...response });
  }
  feedback(msgId: string, rating: 'up' | 'down', reason?: string) {
    return this.call('feedback', reason === undefined ? { msg_id: msgId, rating } : { msg_id: msgId, rating, reason });
  }
  /** Takes over (or gives back) one of your agents in a conversation. */
  takeover(convId: string, agentId: string) {
    return this.call('takeover', { conv_id: convId, agent_id: agentId });
  }
  release(convId: string, agentId: string) {
    return this.call('release', { conv_id: convId, agent_id: agentId });
  }
  /** Stops a streamed reply (your own agent's, or your own). */
  interrupt(msgId: string) {
    return this.call('stream_cancel', { msg_id: msgId });
  }
  /** Pending ask-a-human requests and drafts waiting for this user. */
  inbox() {
    return this.call('inbox');
  }
}
