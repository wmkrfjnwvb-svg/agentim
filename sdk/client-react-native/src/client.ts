// AgentIM client for React Native (docs/12 §2.12): the native core through
// JSI (`global.__agentim`, cpp/AgentIMJsi.cpp). The same API as
// @agentimhq/client-web, so the hooks of @agentimhq/react work here too.
//
//   const client = AgentIMClient.create({
//     url: 'https://chat.example.com',
//     deviceId: savedDeviceId,
//     storeDir: `${RNFS.DocumentDirectoryPath}/agentim`,
//     tokenProvider: () => backend.token(),
//   });
//   AppState.addEventListener('change', (s) => client.setForeground(s === 'active'));
//   NetInfo.addEventListener((s) => client.setNetwork(!!s.isConnected));
//   client.connect();

import { AgentIMError, Messages, Observable, TokenRefused, type Connection, type Message } from '@agentimhq/client-web/view';

export { AgentIMError, Messages, Observable, TokenRefused, type Connection, type Message };

// Timers of the React Native runtime (no DOM or Node types here).
declare function setTimeout(f: () => void, ms: number): unknown;
declare function clearTimeout(t: unknown): void;

interface Binding {
  create(config: string, storeDir: string | null, onEvent: (json: string) => void): number;
  command(h: number, id: number, cmd: string): number;
  token(h: number, token: string | null, refused: boolean): void;
  network(h: number, online: boolean, generation: number): void;
  foreground(h: number, foreground: boolean): void;
  free(h: number): void;
  version(): string;
}

function binding(): Binding {
  const b = (globalThis as any).__agentim as Binding | undefined;
  if (!b) throw new AgentIMError({ code: 'unavailable', message: 'the AgentIM native module is not installed (see README)' });
  return b;
}

export interface ClientOptions {
  url: string;
  /** This device's id, the same every run (1..64 of A-Z a-z 0-9 _ -). */
  deviceId: string;
  /** A client token from your backend; throw `TokenRefused` to stop. */
  tokenProvider: () => Promise<string>;
  /** Where the cache lives (the app's documents directory); none keeps it in memory. */
  storeDir?: string;
  deviceName?: string;
  platform?: string;
  clientName?: string;
  clientVersion?: string;
  commandTimeoutMs?: number;
}

type Listener = (e: any) => void;

export class AgentIMClient {
  readonly connection = new Observable<Connection>({ state: 'closed' });
  readonly conversations = new Observable<any[]>([]);
  private h = 0;
  private next = 0;
  private generation = 0;
  private pending = new Map<number, { ok: (v: any) => void; no: (e: AgentIMError) => void; timer: ReturnType<typeof setTimeout> }>();
  // Applied in event order, before later events (a view's first snapshot).
  private inOrder = new Map<number, (r: any) => void>();
  private views = new Map<string, Messages>();
  private convs = new Map<string, any>();
  private listeners = new Map<string, Set<Listener>>();

  private constructor(private o: ClientOptions) {}

  static version(): string {
    return binding().version();
  }

  static create(o: ClientOptions): AgentIMClient {
    const c = new AgentIMClient(o);
    const cfg = JSON.stringify({
      url: o.url,
      device_id: o.deviceId,
      device_name: o.deviceName ?? null,
      platform: o.platform ?? 'react-native',
      client_name: o.clientName ?? 'agentim-react-native',
      client_version: o.clientVersion ?? '0.1.0',
    });
    c.h = binding().create(cfg, o.storeDir ?? null, (json) => c.onEvent(json));
    if (!c.h) throw new AgentIMError({ code: 'bad_request', message: 'the client could not start (check url and deviceId)' });
    return c;
  }

  private onEvent(json: string) {
    const e = JSON.parse(json);
    switch (e.type) {
      case 'need_token':
        this.o.tokenProvider().then(
          (t) => this.h && binding().token(this.h, t, false),
          (err) => this.h && binding().token(this.h, null, err instanceof TokenRefused),
        );
        return;
      case 'reply': {
        const apply = this.inOrder.get(e.id);
        this.inOrder.delete(e.id);
        if (e.ok) apply?.(e.result);
        const p = this.pending.get(e.id);
        if (!p) return;
        this.pending.delete(e.id);
        clearTimeout(p.timer);
        if (e.ok) p.ok(e.result);
        else p.no(new AgentIMError(e.error));
        return;
      }
      case 'connection':
        this.connection.set({ state: e.state, reason: e.reason, retryAt: e.retry_at });
        break;
      case 'conversations': {
        if (e.reset) this.convs.clear();
        for (const ch of e.changes ?? []) this.convs.set(ch.conversation.conv_id, ch.conversation);
        const list = [...this.convs.values()]
          .filter((c) => !c.left && !c.dissolved)
          .sort((a, b) => (b.last_message?.created_at ?? 0) - (a.last_message?.created_at ?? 0));
        this.conversations.set(list);
        break;
      }
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

  /** Any core command (`{cmd: …}`); resolves with its result. */
  command(cmd: Record<string, unknown>, applyInOrder?: (r: any) => void): Promise<any> {
    if (!this.h) return Promise.reject(new AgentIMError({ code: 'closed', message: 'the client is closed' }));
    const id = ++this.next;
    return new Promise((ok, no) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        this.inOrder.delete(id);
        no(new AgentIMError({ code: 'timeout', message: 'no answer from the client core', retryable: true }));
      }, this.o.commandTimeoutMs ?? 60_000);
      this.pending.set(id, { ok, no, timer });
      if (applyInOrder) this.inOrder.set(id, applyInOrder);
      if (binding().command(this.h, id, JSON.stringify(cmd)) !== 0) {
        clearTimeout(timer);
        this.pending.delete(id);
        this.inOrder.delete(id);
        no(new AgentIMError({ code: 'bad_request', message: 'not a command' }));
      }
    });
  }

  /** Events: `operation`, `side`, `signal`, `reset`, `storage`, `event`, `read`, `mute`. */
  on(type: string, fn: Listener): () => void {
    let s = this.listeners.get(type);
    if (!s) this.listeners.set(type, (s = new Set()));
    s.add(fn);
    return () => s!.delete(fn);
  }

  /** NetInfo: call on every change. */
  setNetwork(online: boolean) {
    if (this.h) binding().network(this.h, online, ++this.generation);
  }

  /** AppState: `active` is the foreground. */
  setForeground(foreground: boolean) {
    if (this.h) binding().foreground(this.h, foreground);
  }

  messages(convId: string): Messages {
    let v = this.views.get(convId);
    if (!v) {
      const view = (v = new Messages(convId, this as any));
      this.views.set(convId, view);
      this.command({ cmd: 'messages', conv_id: convId }, (r) => view.reset(r.messages, r.has_older, r.older_end)).catch(() => {});
    }
    return v;
  }

  connect() {
    return this.command({ cmd: 'connect' });
  }
  close() {
    return this.command({ cmd: 'close' });
  }
  /** Logs out here: closes and wipes the cache and the queue (GDPR). */
  logout() {
    return this.command({ cmd: 'logout' });
  }
  send(convId: string, text: string, opts: { kind?: string; payload?: unknown; replyTo?: string; clientMsgId?: string } = {}) {
    return this.command({ cmd: 'send', conv_id: convId, text, kind: opts.kind, payload: opts.payload, reply_to: opts.replyTo, client_msg_id: opts.clientMsgId });
  }
  /**
   * Sends a file or a photo from a path (`file://` URIs work): shown at once
   * as uploading, sent when it is up. A photo loses its EXIF and GPS (unless
   * `keepMetadata`) and gets a 320-pixel thumbnail (docs/02 §2.7).
   */
  sendFile(convId: string, path: string, opts: { name?: string; mime?: string; caption?: string; replyTo?: string; clientMsgId?: string; keepMetadata?: boolean } = {}) {
    return this.command({
      cmd: 'send_file', conv_id: convId, path: path.startsWith('file://') ? decodeURIComponent(path.slice(7)) : path, name: opts.name, mime: opts.mime,
      caption: opts.caption, reply_to: opts.replyTo, client_msg_id: opts.clientMsgId, keep_metadata: opts.keepMetadata,
    });
  }
  /** A link (valid a few minutes) to read a file: `original` or `thumb`. */
  fileUrl(convId: string, fileId: string, variant: 'original' | 'thumb' = 'original'): Promise<{ url: string; variant: string; expires_at: number }> {
    return this.command({ cmd: 'file_url', conv_id: convId, file_id: fileId, variant });
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
  pendingOps() {
    return this.command({ cmd: 'pending' }).then((r) => r.ops);
  }
  call(op: string, args: Record<string, unknown> = {}) {
    return this.command({ cmd: 'call', op, args });
  }
  debugState() {
    return this.command({ cmd: 'debug_state' });
  }

  /** Stops the client; no event arrives after it. Waiting calls fail with `closed`. */
  dispose() {
    if (!this.h) return;
    const h = this.h;
    this.h = 0;
    binding().free(h);
    for (const p of this.pending.values()) {
      clearTimeout(p.timer);
      p.no(new AgentIMError({ code: 'closed', message: 'the client is closed' }));
    }
    this.pending.clear();
    this.inOrder.clear();
  }
}
