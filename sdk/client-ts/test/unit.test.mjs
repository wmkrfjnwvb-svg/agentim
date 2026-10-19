// Ordering rules with a scripted server, so races are exact (docs/05 §6).

import { test, after } from 'node:test';
import assert from 'node:assert/strict';
import { AgentIMClient } from '../dist/index.js';

const msg = (seq) => ({
  msg_id: `m${seq}`, conv_id: 'c1', created_seq: seq, sender_id: 'u2', sender_name: 'b', sender_type: 'human',
  kind: 'text', content: `t${seq}`, version: 1, state: 'committed', created_at: seq, updated_at: seq,
});
const ev = (seq) => ({ conv_id: 'c1', seq, type: 'msg.created', msg: msg(seq), created_at: seq });

/** A fake WebSocket: answers by a handler, frames can be injected. */
function fakeServer(handle) {
  const s = { sockets: [], acks: [] };
  s.WS = class {
    constructor() {
      this.readyState = 1;
      this.l = { message: [], close: [], error: [] };
      s.sockets.push(this);
      setTimeout(() => this.emit({ type: 'welcome', me: { id: 'u1', name: 'a' } }));
    }
    addEventListener(t, f) { this.l[t]?.push(f); }
    emit(f) { for (const g of this.l.message) g({ data: JSON.stringify(f) }); }
    send(raw) {
      const r = JSON.parse(raw);
      if (r.op === 'ack') s.acks.push(...r.acks);
      const out = handle(r, this) ?? {};
      setTimeout(() => this.emit({ type: 'res', ok: true, rid: r.rid, ...out }));
    }
    close() {
      this.readyState = 3;
      for (const g of this.l.close) g({ code: 1000 });
    }
  };
  return s;
}

const open = [];
after(() => open.forEach((c) => c.close()));
const mk = (o) => {
  const c = new AgentIMClient(o);
  open.push(c);
  return c;
};
const contents = (c) => c.messages('c1').map((m) => m.content);
const until = async (f, ms = 2000) => {
  const end = Date.now() + ms;
  while (!f()) {
    if (Date.now() > end) throw new Error('timed out');
    await new Promise((r) => setTimeout(r, 5));
  }
};

test('a sync answer that overtakes queued pushes does not hide them', async () => {
  let syncs = 0;
  const srv = fakeServer((r) => {
    if (r.op !== 'sync') return;
    syncs++;
    // The second sync answers with 4..5: the server counted 2..3 as sent.
    return syncs === 2 ? { convs: [{ conv_id: 'c1', events: [{ event: ev(4) }, { event: ev(5) }], upto: 5 }], more: false } : { convs: [], more: false };
  });
  const c = mk({ url: 'http://x', tokenProvider: async () => 't', WebSocket: srv.WS, positionCheckMs: 1e9 });
  await c.connect();
  const ws = srv.sockets[0];
  ws.emit({ type: 'event', event: ev(1) });
  await until(() => contents(c).length === 1);
  // A resync starts a sync; the pushes of 2 and 3 arrive after its answer.
  ws.emit({ type: 'resync', conv_id: 'c1' });
  await until(() => syncs === 2);
  ws.emit({ type: 'event', event: ev(2) });
  ws.emit({ type: 'event', event: ev(3) });
  await until(() => contents(c).length === 5);
  assert.deepEqual(contents(c), ['t1', 't2', 't3', 't4', 't5']);
  // Never acknowledged past a hole.
  for (const a of srv.acks) assert.ok(a.seq === undefined || a.seq <= 5);
  const firstFive = srv.acks.findIndex((a) => a.seq === 5);
  const threeBefore = srv.acks.slice(0, firstFive).some((a) => a.seq === 3 || a.seq === 1);
  assert.ok(threeBefore, `acks in order: ${JSON.stringify(srv.acks)}`);
  assert.ok(!srv.acks.slice(0, srv.acks.findIndex((a) => a.seq === 3)).some((a) => a.seq > 3), 'no ack past the hole');
  c.close();
});

test('a duplicate or old push changes nothing; a hole that stays makes the client reconnect', async () => {
  const srv = fakeServer((r) => (r.op === 'sync' ? { convs: [], more: false } : undefined));
  const c = mk({ url: 'http://x', tokenProvider: async () => 't', WebSocket: srv.WS, positionCheckMs: 1e9 });
  await c.connect();
  const ws = srv.sockets[0];
  ws.emit({ type: 'event', event: ev(1) });
  ws.emit({ type: 'event', event: ev(1) });
  await until(() => contents(c).length === 1);
  await new Promise((r) => setTimeout(r, 30));
  assert.deepEqual(contents(c), ['t1']);
  // 3 without 2, and the sync has nothing: a new connection is opened.
  ws.emit({ type: 'event', event: ev(3) });
  await until(() => srv.sockets.length === 2, 3000);
  assert.deepEqual(contents(c), ['t1'], '3 waits for 2');
  c.close();
});

test('unknown frames and event types pass without harm', async () => {
  const srv = fakeServer((r) => (r.op === 'sync' ? { convs: [], more: false } : undefined));
  const c = mk({ url: 'http://x', tokenProvider: async () => 't', WebSocket: srv.WS, positionCheckMs: 1e9 });
  const seen = [];
  c.on('event', (e) => seen.push(e.type));
  await c.connect();
  const ws = srv.sockets[0];
  ws.emit({ type: 'from_the_future', x: 1 });
  ws.emit({ type: 'event', event: { conv_id: 'c1', seq: 1, type: 'new.kind', created_at: 1 } });
  ws.emit({ type: 'event', event: ev(2) });
  await until(() => seen.length === 2);
  assert.deepEqual(seen, ['new.kind', 'msg.created']);
  assert.deepEqual(contents(c), ['t2']);
  c.close();
});
