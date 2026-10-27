// AgentIM client for browsers and Node (docs/12 §2). The rules live in the
// Rust core (wasm); this file gives it a WebSocket, a cache, timers and the
// page's network and visibility signals, and turns its events into
// subscribable state.
//
//   const client = await AgentIMClient.create({
//     url: 'https://chat.example.com',
//     deviceId: localStorage.deviceId ??= crypto.randomUUID(),
//     tokenProvider: () => fetch('/my/token').then((r) => r.text()),
//   });
//   client.conversations.subscribe(render);
//   client.connect();
//   await client.send(convId, 'hello');   // shown at once, sent when it can be
//
// One connection per account across tabs: the first tab holds it (Web
// Locks), the others ask it over a BroadcastChannel (docs/12 §2.10).

import { IndexedDBStore, MemoryStore, StoreError, type Store } from './store.js';
import { WasmCore, type Effect, type WasmSource } from './wasm.js';
import { AgentIMError, Messages, Observable, TokenRefused, type Connection, type Message } from './view.js';

export { AgentIMError, Messages, Observable, TokenRefused };
export type { Connection, Message };

export interface ClientOptions {
  /** `https://chat.example.com`. */
  url: string;
  /** This device's id, the same every run (1..64 of A-Z a-z 0-9 _ -). */
  deviceId: string;
  deviceName?: string;
  platform?: string;
  /** A client token from your backend; throw `TokenRefused` to stop. */
  tokenProvider: () => Promise<string>;
  /** The cache: IndexedDB by default where there is one, else memory. */
  store?: Store | 'indexeddb' | 'memory';
  /** Name of the IndexedDB databases (default `agentim-<deviceId>`). */
  storeName?: string;
  /** The core module (default: `agentim_core.wasm` next to this file). */
  wasm?: WasmSource;
  /** Share one connection between tabs (default: where the browser can). */
  multiTab?: boolean;
  WebSocket?: typeof WebSocket;
  clientName?: string;
  clientVersion?: string;
  log?: (m: string) => void;
}

/** Thrown by a token provider when the backend refuses the user. */
type Listener = (e: any) => void;

/** The tab that holds the connection: core, socket, store, timers. */
class Engine {
  private core!: WasmCore;
  private store!: Store;
  private ws: WebSocket | null = null;
  private chain: Promise<void> = Promise.resolve();
  private timer: ReturnType<typeof setTimeout> | null = null;
  private generation = 0;
  private stopped = false;
  private detach: Array<() => void> = [];

  constructor(
    private o: ClientOptions,
    private emit: Listener,
  ) {}

  async start() {
    this.core = await WasmCore.create(
      {
        url: this.o.url,
        device_id: this.o.deviceId,
        device_name: this.o.deviceName ?? null,
        platform: this.o.platform ?? null,
        client_name: this.o.clientName ?? 'agentim-web',
        client_version: this.o.clientVersion ?? '0.1.0',
        seed: Math.floor(Math.random() * 2 ** 52),
      },
      this.o.wasm,
    );
    let failed: string | null = null;
    const want = this.o.store ?? (globalThis.indexedDB ? 'indexeddb' : 'memory');
    this.store = typeof want === 'object' ? want : want === 'indexeddb' ? new IndexedDBStore(this.o.storeName ?? `agentim-${this.o.deviceId}`) : new MemoryStore();
    try {
      await this.store.open();
    } catch (e) {
      // Private mode, blocked storage: work in memory.
      this.o.log?.(`cache unavailable: ${e}`);
      failed = 'unavailable';
      this.store = new MemoryStore();
    }
    let snap;
    try {
      snap = await this.store.snapshot(50);
    } catch (e) {
      failed = e instanceof StoreError ? e.kind : 'corrupt';
      await this.store.wipe().catch(() => {});
      snap = { account: null, convs: [], ops: [], messages: [] };
    }
    this.signals();
    this.feed({ restored: snap });
    if (failed) this.feed({ storage_failed: failed });
  }

  /** The page's network and visibility, as the core wants them. */
  private signals() {
    const g = globalThis as any;
    const net = () => this.feed({ network: { online: g.navigator?.onLine !== false, generation: ++this.generation } });
    const vis = () => this.feed({ foreground: g.document?.visibilityState !== 'hidden' });
    const on = (t: any, ev: string, f: () => void) => {
      if (!t?.addEventListener) return;
      t.addEventListener(ev, f);
      this.detach.push(() => t.removeEventListener(ev, f));
    };
    on(g.window, 'online', net);
    on(g.window, 'offline', net);
    on(g.navigator?.connection, 'change', net);
    on(g.document, 'visibilitychange', vis);
    on(g.document, 'resume', vis);
    on(g.window, 'pageshow', vis);
  }

  feed(input: unknown) {
    this.chain = this.chain.then(() => this.process(input)).catch((e) => this.o.log?.(`agentim: ${e}`));
  }

  private async process(input: unknown) {
    if (this.stopped) return;
    const fx = this.core.handle(input);
    for (const e of fx) await this.run(e);
    this.arm();
  }

  private arm() {
    if (this.timer) clearTimeout(this.timer);
    this.timer = null;
    const d = this.core.nextDeadline();
    if (d !== null && !this.stopped) this.timer = setTimeout(() => this.feed('tick'), Math.max(0, d - Date.now()));
  }

  private async run(e: Effect) {
    if ('need_token' in e) {
      this.o.tokenProvider().then(
        (t) => this.feed({ token: t }),
        (err) => this.feed({ token_error: { refused: err instanceof TokenRefused } }),
      );
    } else if ('connect' in e) {
      this.open(e.connect.url, e.connect.token);
    } else if ('send' in e) {
      try {
        this.ws?.send(e.send);
      } catch {
        // The close event follows.
      }
    } else if ('close' in e) {
      const ws = this.ws;
      this.ws = null;
      if (ws) {
        ws.onopen = ws.onmessage = ws.onclose = ws.onerror = null;
        try {
          ws.close(e.close.code, e.close.reason);
        } catch {
          ws.close();
        }
      }
    } else if ('persist' in e) {
      try {
        await this.store.write(e.persist);
        this.feed({ persisted: e.persist.id });
      } catch (err) {
        this.feed({ storage_failed: err instanceof StoreError ? err.kind : 'unavailable' });
      }
    } else if ('load_page' in e) {
      const p = e.load_page;
      const messages = await this.store.page(p.conv_id, p.before_seq, p.limit).catch(() => []);
      this.feed({ page_loaded: { req: p.req, conv_id: p.conv_id, messages } });
    } else if ('wipe_store' in e) {
      await this.store.wipe().catch(() => {});
    } else if ('emit' in e) {
      this.emit(e.emit);
    } else if ('reply' in e) {
      const tag = this.tags.get(e.reply.id);
      this.tags.delete(e.reply.id);
      this.emit({ type: 'reply', ...e.reply, id: tag ?? e.reply.id });
    }
  }

  private open(url: string, token: string) {
    const WS = this.o.WebSocket ?? globalThis.WebSocket;
    let ws: WebSocket;
    try {
      // Browsers cannot set Authorization on a WebSocket: the token goes in
      // a subprotocol, never in the URL (proxy logs keep URLs; D129).
      ws = new WS(url, ['agentim', `agentim.bearer.${base64url(token)}`]);
    } catch (err) {
      this.feed({ connect_failed: { detail: String(err) } });
      return;
    }
    this.ws = ws;
    let opened = false;
    ws.onopen = () => {
      opened = true;
      this.feed('opened');
    };
    ws.onmessage = (m) => this.feed({ frame: String(m.data) });
    ws.onclose = (c) => {
      if (this.ws === ws) this.ws = null;
      // Browsers do not say why a handshake failed (no HTTP status).
      if (opened) this.feed({ closed: { code: c.code, reason: c.reason } });
      else this.feed({ connect_failed: { detail: `closed ${c.code}` } });
    };
  }

  /** Commands of every tab, under the engine's own numbers. */
  private tags = new Map<number, string>();
  private seq = 0;

  command(tag: string, cmd: unknown) {
    const id = ++this.seq;
    this.tags.set(id, tag);
    this.feed({ command: { id, cmd } });
  }

  async stop() {
    this.stopped = true;
    if (this.timer) clearTimeout(this.timer);
    for (const d of this.detach) d();
    if (this.ws) {
      this.ws.onclose = null;
      this.ws.close(1000);
    }
    await this.chain;
    this.core.free();
  }
}

let tabSeq = 0;

/** Commands that do no harm when the core gets them twice. */
const SAFE_TWICE = new Set(['state', 'connect', 'close', 'logout', 'conversations', 'messages', 'send', 'read', 'focus', 'discard', 'pending', 'debug_state']);

export class AgentIMClient {
  readonly connection = new Observable<Connection>({ state: 'closed' });
  readonly conversations = new Observable<any[]>([]);
  private convMap = new Map<string, any>();
  private views = new Map<string, Messages>();
  private listeners = new Map<string, Set<Listener>>();
  private pending = new Map<number, { cmd: Record<string, unknown>; ok: (v: any) => void; no: (e: AgentIMError) => void; timer: ReturnType<typeof setTimeout> }>();
  private next = 0;
  private engine: Engine | null = null;
  private channel: BroadcastChannel | null = null;
  private tab = `${Date.now().toString(36)}-${++tabSeq}-${Math.random().toString(36).slice(2)}`;
  private releaseLock: (() => void) | null = null;
  private leaderReady: Promise<void> = Promise.resolve();
  private wantConnect = false;

  private constructor(private o: ClientOptions) {}

  static async create(o: ClientOptions): Promise<AgentIMClient> {
    const c = new AgentIMClient(o);
    const g = globalThis as any;
    const shared = (o.multiTab ?? true) && g.navigator?.locks && g.BroadcastChannel;
    if (!shared) {
      await c.lead();
      return c;
    }
    const name = `agentim:${o.storeName ?? o.deviceId}`;
    c.channel = new BroadcastChannel(name);
    c.channel.onmessage = (m) => c.fromChannel(m.data);
    // The first tab to hold the lock leads; the others follow until it goes.
    await new Promise<void>((started) => {
      let led = false;
      g.navigator.locks
        .request(name, { ifAvailable: true }, async (lock: unknown) => {
          if (!lock) {
            started();
            // Wait in line to take over when the leader goes.
            g.navigator.locks.request(name, async () => {
              // Carry on what the old leader was doing: connected or trying.
              const was = c.connection.get().state;
              if (was !== 'closed' && was !== 'failed') c.wantConnect = true;
              c.connection.set({ state: 'closed' });
              await c.lead();
              await c.resubscribe();
              await new Promise<void>((r) => (c.releaseLock = r));
            });
            return;
          }
          led = true;
          await c.lead();
          started();
          await new Promise<void>((r) => (c.releaseLock = r));
        })
        .catch(() => started());
      if (led) started();
    });
    // A follower starts from the leader's state.
    if (!c.engine) {
      await c
        .command({ cmd: 'state' })
        .then((r) => c.connection.set({ state: r.state, reason: r.reason }))
        .catch(() => {});
    }
    return c;
  }

  private async lead() {
    const engine = new Engine(this.o, (e) => this.fromEngine(e));
    this.leaderReady = engine.start();
    await this.leaderReady;
    this.engine = engine;
    if (this.wantConnect) engine.command(`${this.tab}:0`, { cmd: 'connect' });
    // Asked while no tab led (the old leader closing): asked again when
    // it is safe to do twice, else refused so the caller decides.
    for (const [n, p] of this.pending) {
      if (SAFE_TWICE.has(String(p.cmd.cmd))) {
        engine.command(`${this.tab}:${n}`, p.cmd);
      } else {
        clearTimeout(p.timer);
        this.pending.delete(n);
        p.no(new AgentIMError({ code: 'disconnected', message: 'the tab holding the connection closed; try again', retryable: true }));
      }
    }
  }

  /** A new leader: the views this tab follows are asked for again. */
  private async resubscribe() {
    await this.command({ cmd: 'conversations' }).then((r) => this.resetConversations(r.conversations)).catch(() => {});
    for (const v of this.views.values()) {
      await this.command({ cmd: 'messages', conv_id: v.convId })
        .then((r) => v.reset(r.messages, r.has_older, r.older_end))
        .catch(() => {});
    }
  }

  // ------------------------------------------------------------ plumbing

  /** Events of the engine in this tab: used here and passed to the others. */
  private fromEngine(e: any) {
    if (e.type === 'need_token') return;
    if (e.type === 'reply' && typeof e.id === 'string' && !e.id.startsWith(this.tab)) {
      this.channel?.postMessage({ kind: 'event', event: e });
      return;
    }
    this.dispatch(e);
    if (e.type !== 'reply') this.channel?.postMessage({ kind: 'event', event: e });
  }

  private fromChannel(m: any) {
    if (m?.kind === 'event') {
      if (m.event.type === 'reply' && !(typeof m.event.id === 'string' && m.event.id.startsWith(this.tab))) return;
      this.dispatch(m.event);
    } else if (m?.kind === 'command' && this.engine) {
      this.engine.command(m.id, m.cmd);
    }
  }

  private dispatch(e: any) {
    switch (e.type) {
      case 'reply': {
        const p = this.pending.get(Number(String(e.id).split(':').pop()));
        if (!p) break;
        clearTimeout(p.timer);
        this.pending.delete(Number(String(e.id).split(':').pop()));
        if (e.ok) p.ok(e.result);
        else p.no(new AgentIMError(e.error));
        return;
      }
      case 'connection':
        this.connection.set({ state: e.state, reason: e.reason, retryAt: e.retry_at });
        break;
      case 'conversations':
        if (e.reset) this.convMap.clear();
        for (const c of e.changes ?? []) this.convMap.set(c.conversation.conv_id, c.conversation);
        this.publishConversations();
        break;
      case 'messages': {
        const v = this.views.get(e.conv_id);
        v?.applyEvent(e);
        break;
      }
    }
    for (const f of this.listeners.get(e.type) ?? []) {
      try {
        f(e);
      } catch {
        // The listener's own error.
      }
    }
  }

  private resetConversations(list: any[]) {
    this.convMap = new Map(list.map((c) => [c.conv_id, c]));
    this.publishConversations();
  }

  private publishConversations() {
    const list = [...this.convMap.values()]
      .filter((c) => !c.left && !c.dissolved)
      .sort((a, b) => (b.last_message?.created_at ?? 0) - (a.last_message?.created_at ?? 0));
    this.conversations.set(list);
  }

  /** Any core command (`{cmd: …}`); resolves with its result. */
  async command(cmd: Record<string, unknown>): Promise<any> {
    const n = ++this.next;
    const id = `${this.tab}:${n}`;
    return new Promise((ok, no) => {
      const timer = setTimeout(() => {
        this.pending.delete(n);
        no(new AgentIMError({ code: 'timeout', message: 'no answer from the client core', retryable: true }));
      }, 60_000);
      this.pending.set(n, { cmd, ok, no, timer });
      if (this.engine) this.engine.command(id, cmd);
      else this.channel?.postMessage({ kind: 'command', id, cmd });
    });
  }

  // ------------------------------------------------------------- the API

  /** Events: `operation`, `side`, `signal`, `reset`, `storage`, `event`, `read`, `mute`. */
  on(type: string, fn: Listener): () => void {
    let s = this.listeners.get(type);
    if (!s) this.listeners.set(type, (s = new Set()));
    s.add(fn);
    return () => s!.delete(fn);
  }

  connect() {
    this.wantConnect = true;
    return this.command({ cmd: 'connect' });
  }

  close() {
    this.wantConnect = false;
    return this.command({ cmd: 'close' });
  }

  /** Logs out here: closes and wipes the cache and the queue (GDPR). */
  logout() {
    this.wantConnect = false;
    return this.command({ cmd: 'logout' });
  }

  /** Stops this tab's part (the next tab takes over the connection). */
  async dispose() {
    for (const p of this.pending.values()) clearTimeout(p.timer);
    this.pending.clear();
    await this.engine?.stop();
    this.engine = null;
    this.releaseLock?.();
    this.channel?.close();
  }

  /** The conversation's messages, kept up to date while subscribed. */
  messages(convId: string): Messages {
    let v = this.views.get(convId);
    if (!v) {
      v = new Messages(convId, this);
      this.views.set(convId, v);
      const view = v;
      void this.command({ cmd: 'messages', conv_id: convId }).then((r) => view.reset(r.messages, r.has_older, r.older_end));
    }
    return v;
  }

  /** Loads the conversation list (it updates by itself afterwards). */
  async refreshConversations() {
    const r = await this.command({ cmd: 'conversations' });
    this.resetConversations(r.conversations);
    return this.conversations.get();
  }

  send(convId: string, text: string, opts: { kind?: string; payload?: unknown; replyTo?: string; clientMsgId?: string } = {}) {
    // Chosen here, so asking twice (a leader change) is still one message.
    const cm = opts.clientMsgId ?? `${Date.now().toString(16)}${crypto.getRandomValues(new Uint32Array(3)).reduce((s, x) => s + x.toString(16).padStart(8, '0'), '')}`;
    return this.command({ cmd: 'send', conv_id: convId, text, kind: opts.kind, payload: opts.payload, reply_to: opts.replyTo, client_msg_id: cm });
  }
  edit(convId: string, target: { msgId?: string; clientMsgId?: string }, text: string) {
    return this.command({ cmd: 'edit', conv_id: convId, msg_id: target.msgId, client_msg_id: target.clientMsgId, text });
  }
  recall(convId: string, target: { msgId?: string; clientMsgId?: string }) {
    return this.command({ cmd: 'recall', conv_id: convId, msg_id: target.msgId, client_msg_id: target.clientMsgId });
  }
  read(convId: string, seq?: number) {
    return this.command({ cmd: 'read', conv_id: convId, seq });
  }
  feedback(convId: string, msgId: string, rating: 'up' | 'down', reason?: string) {
    return this.command({ cmd: 'feedback', conv_id: convId, msg_id: msgId, rating, reason });
  }
  respond(convId: string, msgId: string, blockId: string, r: { action?: string; values?: Record<string, unknown> }) {
    return this.command({ cmd: 'respond', conv_id: convId, msg_id: msgId, block_id: blockId, ...r });
  }
  retry(convId: string, clientMsgId: string) {
    return this.command({ cmd: 'retry', conv_id: convId, client_msg_id: clientMsgId });
  }
  discard(convId: string, clientMsgId: string) {
    return this.command({ cmd: 'discard', conv_id: convId, client_msg_id: clientMsgId });
  }
  focus(convId: string | null) {
    return this.command({ cmd: 'focus', conv_id: convId });
  }
  /** What waits in the offline queue. */
  pendingOps() {
    return this.command({ cmd: 'pending' }).then((r) => r.ops);
  }
  /** A request that needs the server now (approvals, decisions, queries). */
  call(op: string, args: Record<string, unknown> = {}) {
    return this.command({ cmd: 'call', op, args });
  }
  debugState() {
    return this.command({ cmd: 'debug_state' });
  }
}

/** UTF-8, base64url without padding (the subprotocol's alphabet). */
function base64url(s: string): string {
  let bin = '';
  for (const b of new TextEncoder().encode(s)) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}
