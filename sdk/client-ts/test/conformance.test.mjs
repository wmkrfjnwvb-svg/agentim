// The client SDK against a real `agentim` server (docs/12 §2.1): ordering,
// exactly once, retries without duplicates, offline sync, lost pushes,
// streaming, refusals, signals, the local cache.
//
//   AGENTIM_BIN=../../server/target/debug/agentim node --test test/

import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { AgentIMClient, MemoryStore, SqliteStore, nodeSqlite } from '../dist/index.js';

import { fileURLToPath } from 'node:url';
const BIN = process.env.AGENTIM_BIN ?? fileURLToPath(new URL(`../../../server/target/debug/agentim${process.platform === 'win32' ? '.exe' : ''}`, import.meta.url));
const PORT = 18600 + Math.floor(Math.random() * 300);
const URL_ = `http://127.0.0.1:${PORT}`;
let server;
let dir;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function until(what, f, ms = 10000, info) {
  const end = Date.now() + ms;
  for (;;) {
    if (await f()) return;
    if (Date.now() > end) assert.fail(`timed out: ${what}${info ? ` (${info()})` : ''}`);
    await sleep(25);
  }
}

before(async () => {
  dir = mkdtempSync(join(tmpdir(), 'agentim-sdk-'));
  server = spawn(BIN, ['--demo', '--db', join(dir, 'a.db'), '--addr', `127.0.0.1:${PORT}`], { stdio: 'ignore' });
  await until('server ready', async () => fetch(`${URL_}/readyz`).then((r) => r.ok).catch(() => false), 30000);
});
after(() => {
  for (const c of clients) c.close();
  server?.kill();
  rmSync(dir, { recursive: true, force: true });
});

let n = 0;
async function user(name, agent = false, owner) {
  const r = await fetch(`${URL_}/api/dev/users`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ name: `${name}${++n}`, agent, owner }),
  }).then((r) => r.json());
  return { id: r.user.id, token: r.token };
}

/** A WebSocket the test can cut, or whose incoming frames it can drop. */
function faultyWS(ctl) {
  return class extends WebSocket {
    constructor(url) {
      super(url);
      ctl.sockets.push(this);
    }
    addEventListener(type, fn, opts) {
      if (type !== 'message') return super.addEventListener(type, fn, opts);
      return super.addEventListener(type, (m) => {
        const f = JSON.parse(m.data);
        if (ctl.drop?.(f)) return;
        fn(m);
      }, opts);
    }
  };
}

const clients = [];
function client(u, extra = {}) {
  const ctl = { sockets: [], drop: null };
  const c = new AgentIMClient({ url: URL_, tokenProvider: async () => u.token, WebSocket: faultyWS(ctl), ...extra });
  c.ctl = ctl;
  clients.push(c);
  return c;
}

/** Raw protocol connection (an agent, or the other side). */
async function raw(token) {
  const ws = new WebSocket(`${URL_.replace('http', 'ws')}/ws?token=${token}`);
  let rid = 0;
  const pending = new Map();
  await new Promise((resolve) => ws.addEventListener('message', (m) => {
    const f = JSON.parse(m.data);
    if (f.type === 'welcome') resolve();
    if (f.type === 'res') pending.get(f.rid)?.(f);
  }));
  return {
    call: (op, args = {}) => new Promise((resolve) => {
      pending.set(++rid, resolve);
      ws.send(JSON.stringify({ op, rid, ...args }));
    }),
    close: () => ws.close(),
  };
}

const texts = (c, conv) => c.messages(conv).filter((m) => m.kind === 'text').map((m) => m.content);

test('messages arrive in order, each once, and local copies become the real ones', async () => {
  const [ua, ub] = [await user('alice'), await user('bob')];
  const a = client(ua);
  const b = client(ub);
  await a.connect();
  await b.connect();
  // The race (sync answers overtaking queued pushes) needs a few rounds.
  for (let round = 0; round < 8; round++) {
  const conv = await a.createGroup('g', [ub.id]);
  const want = Array.from({ length: 30 }, (_, i) => `m${i}`);
  const sent = await Promise.all(want.map((t) => a.send(conv, t)));
  assert.ok(sent.every((m) => m.status === 'sent' && Number.isFinite(m.created_seq)), JSON.stringify(sent.filter((m) => m.status !== 'sent').map((m) => [m.status, m.error])));
  await until('bob has all', () => texts(b, conv).length === 30, 10000, () => `${texts(b, conv).length} ${b.state} ${JSON.stringify(texts(b, conv))}`);
  const got = b.messages(conv).filter((m) => m.kind === 'text');
  const seqs = got.map((m) => m.created_seq);
  assert.deepEqual(seqs, [...seqs].sort((x, y) => x - y), 'in seq order');
  assert.equal(new Set(got.map((m) => m.msg_id)).size, 30, 'each once');
  assert.deepEqual(new Set(texts(b, conv)), new Set(want));
  // The sender's list has no local leftovers.
  assert.equal(a.messages(conv).filter((m) => m.status !== 'sent').length, 0);
  assert.equal(texts(a, conv).length, 30);
  }
  a.close();
  b.close();
});

test('a send cut off mid-way is retried with the same id and stored once', async () => {
  const [ua, ub] = [await user('alice'), await user('bob')];
  const a = client(ua);
  const b = client(ub);
  await a.connect();
  await b.connect();
  const conv = await a.createGroup('g', [ub.id]);
  // Drop the answer to the send and cut the connection: the client cannot
  // know whether it was stored.
  a.ctl.drop = (f) => f.type === 'res' && f.seq !== undefined && f.msg_id !== undefined;
  const p = a.send(conv, 'exactly once');
  await sleep(150);
  a.ctl.drop = null;
  a.ctl.sockets.at(-1).close();
  const m = await p;
  assert.equal(m.status, 'sent');
  await until('bob has it', () => texts(b, conv).includes('exactly once'));
  await sleep(300);
  assert.equal(texts(b, conv).filter((t) => t === 'exactly once').length, 1);
  assert.equal(texts(a, conv).filter((t) => t === 'exactly once').length, 1);
  a.close();
  b.close();
});

for (const kind of ['memory', 'sqlite']) test(`offline, then back (${kind} cache): what was missed is synced in order, nothing twice`, async () => {
  const [ua, ub] = [await user('alice'), await user('bob')];
  const path = join(dir, `cache-${ub.id}.db`);
  const store = kind === 'memory' ? new MemoryStore() : new SqliteStore(nodeSqlite(new DatabaseSync(path)));
  const a = client(ua);
  let b = client(ub, { store });
  await a.connect();
  await b.connect();
  const conv = await a.createGroup('g', [ub.id]);
  await a.send(conv, 'before');
  await until('bob has before', () => texts(b, conv).includes('before'));
  b.close();
  for (const t of ['o1', 'o2', 'o3', 'o4', 'o5']) await a.send(conv, t);
  // Same cache (as after a reload, for SQLite a new connection to the
  // file): shown before connecting, then caught up.
  b = client(ub, { store: kind === 'memory' ? store : new SqliteStore(nodeSqlite(new DatabaseSync(path))) });
  assert.deepEqual(await b.load(conv).then((ms) => ms.filter((m) => m.kind === 'text').map((m) => m.content)), ['before']);
  await b.connect();
  await until('bob caught up', () => texts(b, conv).length === 6);
  assert.deepEqual(texts(b, conv), ['before', 'o1', 'o2', 'o3', 'o4', 'o5']);
  a.close();
  b.close();
});

test('a lost push is found by the next one, and the last one by the position check', async () => {
  const [ua, ub] = [await user('alice'), await user('bob')];
  const a = client(ua);
  const b = client(ub, { positionCheckMs: 300 });
  await a.connect();
  await b.connect();
  const conv = await a.createGroup('g', [ub.id]);
  await a.send(conv, 'first');
  await until('first', () => texts(b, conv).includes('first'));
  b.ctl.drop = (f) => f.type === 'event' && f.event?.msg?.content === 'lost';
  await a.send(conv, 'lost');
  await a.send(conv, 'after');
  await until('gap filled', () => texts(b, conv).includes('lost') && texts(b, conv).includes('after'));
  assert.deepEqual(texts(b, conv), ['first', 'lost', 'after']);
  // The last message lost, nothing after it: the position check finds it.
  b.ctl.drop = (f) => f.type === 'event' && f.event?.msg?.content === 'last';
  await a.send(conv, 'last');
  await until('position check', () => texts(b, conv).includes('last'), 5000);
  assert.deepEqual(texts(b, conv), ['first', 'lost', 'after', 'last']);
  a.close();
  b.close();
});

test('an agent streams a reply; the client shows it growing, then the whole text', async () => {
  const ua = await user('alice');
  const bot = await user('bot', true, ua.id);
  const a = client(ua);
  await a.connect();
  const conv = await a.createGroup('g', [bot.id]);
  const g = await raw(bot.token);
  const s = await g.call('stream_start', { conv_id: conv, client_msg_id: 'st' });
  assert.equal(s.ok, true, JSON.stringify(s));
  const seen = [];
  const off = a.on('messages', ({ messages }) => {
    const m = messages.find((x) => x.msg_id === s.msg_id);
    if (m?.state === 'streaming') seen.push(m.streamed);
  });
  // Token by token only for the conversation on screen; deltas are not
  // stored (D81).
  await a.focus(conv);
  await until('streaming message', () => a.messages(conv).some((m) => m.msg_id === s.msg_id && m.state === 'streaming'));
  await g.call('stream_append', { msg_id: s.msg_id, idx: 0, text: 'Hel' });
  await g.call('stream_append', { msg_id: s.msg_id, idx: 1, text: 'lo' });
  await until('growing', () => seen.includes('Hello'));
  await g.call('stream_end', { msg_id: s.msg_id });
  await until('done', () => a.messages(conv).some((m) => m.msg_id === s.msg_id && m.state === 'complete'));
  const m = a.messages(conv).find((x) => x.msg_id === s.msg_id);
  assert.equal(m.content, 'Hello');
  assert.equal(m.sender_type, 'agent', 'labelled as AI');
  off();
  g.close();
  a.close();
});

test('a refused send fails at once with the server code; failed messages can be dropped', async () => {
  const [ua, ub] = [await user('alice'), await user('bob')];
  const a = client(ua);
  await a.connect();
  const conv = await client(ub).connect().then(async () => a.createGroup('g', [ub.id]));
  const t0 = Date.now();
  const m = await a.send('c_doesnotexist', 'nowhere');
  assert.equal(m.status, 'failed');
  assert.ok(['not_found', 'forbidden'].includes(m.error.code), m.error.code);
  assert.ok(Date.now() - t0 < 2000, 'not retried');
  await a.discard('c_doesnotexist', m.client_msg_id);
  assert.equal(a.messages('c_doesnotexist').length, 0);
  // Too long: bad_request, also not retried.
  const big = await a.send(conv, 'x'.repeat(70 * 1024));
  assert.equal(big.status, 'failed');
  assert.equal(big.error.code, 'bad_request');
  a.close();
});

test('typing reaches the other member', async () => {
  const [ua, ub] = [await user('alice'), await user('bob')];
  const a = client(ua);
  const b = client(ub);
  await a.connect();
  await b.connect();
  const conv = await a.createGroup('g', [ub.id]);
  const got = [];
  b.on('signal', (s) => got.push(s));
  await a.typing(conv);
  await a.typing(conv); // throttled: one signal
  await until('typing', () => got.length >= 1);
  await sleep(200);
  assert.equal(got.length, 1);
  assert.equal(got[0].kind, 'typing');
  assert.equal(got[0].from, ua.id);
  a.close();
  b.close();
});
