// AgentIM agent SDK for Node.js 22+ (docs/10-agent-ecosystem.md §5.2).
// No dependencies: Node's built-in WebSocket. An agent connects with its
// key, receives what concerns it in order (sync, then pushes), and acts.
//
//   const agent = new Agent({ url: 'https://im.example.com', key: process.env.AGENTIM_KEY });
//   agent.onMessage(async (msg, ctx) => { await ctx.reply(`you said: ${msg.content}`); });
//   await agent.start();
//
// Delivery is at least once: a batch is acknowledged after its handlers
// finish, so a handler that throws (or a crash) gets the event again;
// use `event.seq` per conversation to skip what was already done.

export class AgentIMError extends Error {
  constructor(code, message) {
    super(`${code}: ${message}`);
    this.code = code;
    this.detail = message;
  }
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const uid = () => globalThis.crypto.randomUUID();

/** One WebSocket connection: requests by `rid`, pushes to a callback. */
export class Connection {
  constructor(url, token, onPush) {
    const base = url.replace(/^http/, 'ws').replace(/\/$/, '');
    this.wsUrl = `${base}/ws?token=${encodeURIComponent(token)}`;
    this.onPush = onPush;
    this.pending = new Map();
    this.rid = 0;
    this.ws = null;
    this.welcome = null;
    this.closed = null;
  }

  open() {
    return new Promise((resolve, reject) => {
      const ws = new WebSocket(this.wsUrl);
      this.ws = ws;
      let opened = false;
      this.closed = new Promise((done) => {
        ws.addEventListener('close', (e) => {
          for (const [, p] of this.pending) p.reject(new AgentIMError('disconnected', `connection closed (${e.code} ${e.reason})`));
          this.pending.clear();
          if (!opened) reject(new AgentIMError('unauthorized', `could not connect (${e.code})`));
          done(e);
        });
      });
      // A refused handshake (e.g. 401) reports an error, maybe no close.
      let failed = false;
      ws.addEventListener('error', () => {
        if (opened || failed) return;
        failed = true;
        reject(new AgentIMError('unauthorized', 'could not connect'));
        try { ws.close(); } catch {}
      });
      ws.addEventListener('message', (m) => {
        const f = JSON.parse(m.data);
        if (f.type === 'welcome') { opened = true; this.welcome = f; resolve(f); return; }
        if (f.type === 'res') {
          const p = this.pending.get(f.rid);
          if (p) { this.pending.delete(f.rid); f.ok ? p.resolve(f) : p.reject(new AgentIMError(f.error.code, f.error.message)); }
          return;
        }
        this.onPush?.(f);
      });
    });
  }

  /** Sends a request and waits for its response (throws AgentIMError). */
  call(op, args = {}) {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) return Promise.reject(new AgentIMError('disconnected', 'not connected'));
    const rid = ++this.rid;
    return new Promise((resolve, reject) => {
      this.pending.set(rid, { resolve, reject });
      this.ws.send(JSON.stringify({ op, rid, ...args }));
    });
  }

  close() { this.ws?.close(); }
}

/** What a handler gets to act in the conversation of an event. */
class Ctx {
  constructor(agent, event) {
    this.agent = agent;
    this.event = event;
    this.convId = event.conv_id;
    this.msg = event.msg ?? null;
  }
  /** Replies to the message (or just sends in the conversation). */
  reply(text, opts = {}) {
    return this.agent.send(this.convId, text, { replyTo: this.msg?.msg_id, ...opts });
  }
  /** A streamed reply: `await s.append(text)`, then `await s.end()`. */
  stream() { return this.agent.stream(this.convId, { replyTo: this.msg?.msg_id }); }
  progress(kind, text, data) { return this.agent.progress(this.convId, kind, text, { data }); }
}

export class Agent {
  /** @param {{url: string, key: string, name?: string, log?: (m: string) => void}} o */
  constructor({ url, key, name = 'agentim-agent-node', log = () => {} }) {
    this.url = url;
    this.key = key;
    this.clientName = name;
    this.log = log;
    this.handlers = { message: [], event: [], card: [], handback: [] };
    this.waiting = new Map(); // request_id -> resolve
    this.me = null;
    this.conn = null;
    this.stopped = false;
    this.queue = Promise.resolve(); // pushes are handled one at a time, in order
    this.lastSeq = new Map();
  }

  onMessage(fn) { this.handlers.message.push(fn); return this; }
  onEvent(fn) { this.handlers.event.push(fn); return this; }
  onCardResponse(fn) { this.handlers.card.push(fn); return this; }
  onHandback(fn) { this.handlers.handback.push(fn); return this; }

  /**
   * Connects and keeps the agent connected until `stop()`. The first
   * connection is retried for up to `startTimeoutMs` (the server looks up
   * a key it has not seen yet at most once a second), then it throws.
   */
  async start({ startTimeoutMs = 15000 } = {}) {
    const end = Date.now() + startTimeoutMs;
    let wait = 300;
    for (;;) {
      try { await this.#connect(); break; } catch (e) {
        if (Date.now() + wait > end) throw e;
        this.log(`connect failed (${e.message}); retrying in ${wait} ms`);
        await sleep(wait);
        wait = Math.min(wait * 2, 3000);
      }
    }
    this.#keepAlive();
    return this;
  }

  async stop() { this.stopped = true; this.conn?.close(); }

  async #connect() {
    const conn = new Connection(this.url, this.key, (f) => this.#push(f));
    const w = await conn.open();
    this.conn = conn;
    this.me = w.me;
    await conn.call('hello', { client: { name: this.clientName, version: '0.1.0' } });
    // Sync rounds and pushes go through one queue, so handlers see events
    // in order.
    await this.#enqueue(() => this.#sync()).catch(() => {});
  }

  #enqueue(fn) {
    const run = this.queue.then(fn);
    this.queue = run.catch(async (e) => {
      // Not acknowledged: this connection acknowledges nothing more (an ack
      // is a position); reconnect, and the server gives it again.
      this.log(`handler failed: ${e.stack ?? e}`);
      const conn = this.conn;
      if (!conn) return;
      conn.failed = true;
      // Slowed down by the server: give it time before the retry.
      if (e.code === 'rate_limited') await sleep(2000);
      conn.close();
    });
    return run;
  }

  async #keepAlive() {
    let wait = 500;
    while (!this.stopped) {
      await this.conn.closed;
      if (this.stopped) return;
      this.log(`disconnected; reconnecting in ${wait} ms`);
      await sleep(wait);
      try { await this.#connect(); wait = 500; } catch (e) { this.log(`reconnect failed: ${e.message}`); wait = Math.min(wait * 2, 30000); }
    }
  }

  // Sync rounds until nothing is left; each batch is acknowledged after
  // its handlers ran (at least once).
  async #sync() {
    for (;;) {
      const r = await this.conn.call('sync', { limit: 200 });
      const acks = [];
      for (const c of r.convs) {
        for (const item of c.events ?? []) await this.#handle(item);
        for (const s of c.side_events ?? []) await this.#side(s);
        if (c.upto !== undefined) acks.push({ conv_id: c.conv_id, seq: c.upto, side_seq: c.side_upto ?? 0 });
      }
      if (acks.length) await this.conn.call('ack', { acks });
      if (!r.more) return;
    }
  }

  #push(f) {
    // A decision a handler is waiting for must not queue behind it.
    if (f.type === 'side') this.#side(f.event);
    const conn = this.conn;
    this.#enqueue(async () => {
      if (conn !== this.conn || conn.failed) return;
      if (f.type === 'event') {
        await this.#handle(f);
        await this.conn.call('ack', { acks: [{ conv_id: f.event.conv_id, seq: f.event.seq }] });
      } else if (f.type === 'side') {
        await this.conn.call('ack', { acks: [{ conv_id: f.event.conv_id, side_seq: f.event.side_seq }] });
      } else if (f.type === 'resync') {
        await this.#sync();
      }
    }).catch(() => {});
  }

  async #handle(item) {
    const ev = item.event;
    this.lastSeq.set(ev.conv_id, ev.seq);
    for (const h of this.handlers.event) await h(ev, item);
    if (!item.wake) return;
    const ctx = new Ctx(this, ev);
    if (ev.msg && ev.msg.sender_id !== this.me.id && (ev.type === 'msg.created' || ev.type === 'msg.updated')) {
      for (const h of this.handlers.message) await h(ev.msg, ctx, item);
    } else if (ev.type === 'card.response') {
      for (const h of this.handlers.card) await h(ev.data, ctx);
    } else if (ev.type === 'agent.state' && ev.data.handback) {
      for (const h of this.handlers.handback) await h(ev.data, ctx);
    }
  }

  #side(s) {
    if (s.type?.startsWith('request.') && s.data?.request_id && this.waiting.has(s.data.request_id)) {
      this.waiting.get(s.data.request_id)(s.data);
      this.waiting.delete(s.data.request_id);
    }
  }

  /** Any protocol operation (docs/05 §7.1). */
  call(op, args) { return this.conn.call(op, args); }

  /** Sends a message; `opts`: replyTo, to, topics, intent, refs, sources, clientMsgId. */
  send(convId, text, opts = {}) {
    return this.conn.call('send', {
      conv_id: convId, text, client_msg_id: opts.clientMsgId ?? uid(),
      reply_to: opts.replyTo, to: opts.to, topics: opts.topics, intent: opts.intent, refs: opts.refs, sources: opts.sources,
    });
  }

  /** Sends a card (docs/03 §9.6.1.1); `text` is what clients without cards show. */
  sendCard(convId, text, card, opts = {}) {
    return this.conn.call('send', { conv_id: convId, text, kind: 'agentim.card', payload: card, client_msg_id: opts.clientMsgId ?? uid(), reply_to: opts.replyTo });
  }

  /** A streamed reply; cancelled if `end` is never called before `fail`. */
  async stream(convId, { replyTo } = {}) {
    const r = await this.conn.call('stream_start', { conv_id: convId, client_msg_id: uid(), reply_to: replyTo });
    const id = r.msg_id;
    let idx = 0;
    return {
      msgId: id,
      append: (text) => this.conn.call('stream_append', { msg_id: id, idx: idx++, text }),
      end: () => this.conn.call('stream_end', { msg_id: id }),
      cancel: () => this.conn.call('stream_cancel', { msg_id: id }),
    };
  }

  progress(convId, kind, text, { msgId, data } = {}) {
    return this.conn.call('progress', { conv_id: convId, kind, text, msg_id: msgId, data });
  }

  /**
   * Asks a person to decide and waits for the outcome (approve, answer or
   * choose; docs/03 §9.3.4). Resolves with {state, response, decided_by}.
   */
  async askHuman(convId, { kind = 'approve', title, context, options, assignees, ttlSecs, timeoutMs = 3600_000 } = {}) {
    const r = await this.conn.call('request_human', { conv_id: convId, kind, title, context, options, assignees, ttl_secs: ttlSecs, client_req_id: uid() });
    const id = r.request_id;
    const decided = new Promise((resolve) => this.waiting.set(id, resolve));
    // Decided before we started waiting?
    const now = await this.conn.call('get_request', { request_id: id });
    if (now.state !== 'pending') { this.waiting.delete(id); return { state: now.state, response: now.response, decided_by: now.decided_by }; }
    const timeout = sleep(timeoutMs).then(() => ({ state: 'timeout' }));
    return Promise.race([decided, timeout]);
  }

  handoff(convId, note, to) { return this.conn.call('handoff', { conv_id: convId, note, to }); }
  history(convId, opts = {}) { return this.conn.call('history', { conv_id: convId, after_seq: opts.afterSeq, before_seq: opts.beforeSeq, limit: opts.limit }); }
  context(convId, maxTokens) { return this.conn.call('context', { conv_id: convId, max_tokens: maxTokens }); }
  search(query, opts = {}) { return this.conn.call('search', { query, conv_id: opts.convId, limit: opts.limit }); }
  putSummary(convId, fromSeq, toSeq, text) { return this.conn.call('put_summary', { conv_id: convId, from_seq: fromSeq, to_seq: toSeq, text }); }

  /** A checkpoint: {value, version} or null when the key does not exist. */
  async getState(key) {
    try { return await this.conn.call('state_get', { key }); } catch (e) { if (e.code === 'not_found') return null; throw e; }
  }
  /** Writes a checkpoint; `version` is the one read (0 creates). Conflict: someone wrote in between. */
  putState(key, value, version) { return this.conn.call('state_put', { key, value, version }); }
  claim(msgId) { return this.conn.call('claim', { msg_id: msgId }); }
  updateTask(convId, state, note) { return this.conn.call('update_task', { conv_id: convId, state, note }); }
}
