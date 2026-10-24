// The browser SDK (the Rust core in wasm) against a real `agentim`, in
// Node: two people talk, edits and recalls, the conversation list, the
// offline queue in IndexedDB surviving a restart and sent once, a new
// device paging back, a refused token.
//
//   AGENTIM_BIN=../../server/target/debug/agentim node --test test/

import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { indexedDB } from 'fake-indexeddb';
import { AgentIMClient, IndexedDBStore, MemoryStore, TokenRefused } from '../dist/index.js';

const BIN = process.env.AGENTIM_BIN ?? fileURLToPath(new URL(`../../../server/target/debug/agentim${process.platform === 'win32' ? '.exe' : ''}`, import.meta.url));
const PORT = 19100 + Math.floor(Math.random() * 300);
const URL_ = `http://127.0.0.1:${PORT}`;
let server;
let dir;
const clients = [];
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function until(what, f, ms = 15000) {
  const end = Date.now() + ms;
  for (;;) {
    if (await f()) return;
    if (Date.now() > end) assert.fail(`timed out: ${what}`);
    await sleep(25);
  }
}

before(async () => {
  dir = mkdtempSync(join(tmpdir(), 'agentim-web-'));
  server = spawn(BIN, ['--demo', '--db', join(dir, 'a.db'), '--addr', `127.0.0.1:${PORT}`], { stdio: 'ignore' });
  await until('server ready', async () => fetch(`${URL_}/readyz`).then((r) => r.ok).catch(() => false), 30000);
});
after(async () => {
  for (const c of clients) await c.dispose().catch(() => {});
  server?.kill();
  rmSync(dir, { recursive: true, force: true });
});

let n = 0;
async function person(name) {
  const r = await fetch(`${URL_}/api/dev/users`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', origin: URL_ },
    body: JSON.stringify({ name: `${name}${++n}` }),
  }).then((r) => r.json());
  return { id: r.user.id, name: r.user.name, token: r.token };
}

async function client(p, opts = {}) {
  const c = await AgentIMClient.create({
    url: URL_,
    deviceId: opts.deviceId ?? `${p.name}-d`,
    tokenProvider: opts.tokenProvider ?? (async () => p.token),
    store: opts.store ?? new MemoryStore(),
    ...opts,
  });
  clients.push(c);
  return c;
}

const connected = (c) => until('connected', () => c.connection.get().state === 'connected');
const texts = (v) => v.get().filter((m) => m.status === 'sent').map((m) => m.content);

test('two people talk; edits, recalls and the list follow', async () => {
  const alice = await person('alice');
  const bob = await person('bob');
  const a = await client(alice);
  const b = await client(bob);
  await Promise.all([a.connect(), b.connect()]);
  await Promise.all([connected(a), connected(b)]);
  const { conv_id } = await a.call('create_conv', { kind: 'group', title: 'web', members: [bob.id] });
  const av = a.messages(conv_id);
  const r = await a.send(conv_id, 'hello from wasm');
  assert.match(r.key, /^local:/);
  // Shown at once, then sent.
  assert.ok(av.get().some((m) => m.content === 'hello from wasm'));
  await until('alice sees it sent', () => texts(av).includes('hello from wasm'));
  await until('bob has the conversation', () => b.conversations.get().some((c) => c.conv_id === conv_id));
  const bv = b.messages(conv_id);
  await until('bob sees it', () => texts(bv).includes('hello from wasm'));
  await b.send(conv_id, 'hi back');
  await until('alice sees the answer', () => texts(av).join('|') === 'hello from wasm|hi back');
  const mine = av.get().find((m) => m.content === 'hello from wasm');
  await a.edit(conv_id, { msgId: mine.msg_id }, 'hello, edited');
  await until('bob sees the edit', () => texts(bv)[0] === 'hello, edited');
  await a.recall(conv_id, { msgId: mine.msg_id });
  await until('bob sees the recall', () => bv.get()[0].state === 'recalled');
  // Subscriptions fire; get() keeps the same object until it changes.
  let calls = 0;
  const off = bv.subscribe(() => calls++);
  const before_ = bv.get();
  assert.equal(bv.get(), before_);
  await a.send(conv_id, 'ping');
  await until('the subscriber is told', () => calls > 0);
  off();
  // Operations are reported; a refused send shows as failed.
  const ops = [];
  a.on('operation', (e) => ops.push(e));
  await a.send('c_not_mine', 'nope', { clientMsgId: 'bad1' });
  const nv = a.messages('c_not_mine');
  await until('failed', () => nv.get().some((m) => m.status === 'failed'));
  assert.ok(ops.some((o) => o.result === 'failed'));
  await a.discard('c_not_mine', 'bad1');
  await until('discarded', () => nv.get().length === 0);
  // Online-only calls answer with the server's code.
  await assert.rejects(a.call('get_request', { request_id: 'nope' }), (e) => e.code === 'not_found');
  const d = await a.debugState();
  assert.equal(d.state, 'connected');
});

test('the queue survives in IndexedDB and is sent once', async () => {
  const alice = await person('alice');
  const bob = await person('bob');
  const name = `idb-${alice.name}`;
  let a = await client(alice, { store: new IndexedDBStore(name, { indexedDB, persist: false }) });
  await a.connect();
  await connected(a);
  const { conv_id } = await a.call('create_conv', { kind: 'group', title: 'q', members: [bob.id] });
  await a.close();
  for (const t of ['one', 'two', 'three']) await a.send(conv_id, t);
  await a.dispose();
  // A new page load: the cache shows them before connecting.
  a = await client(alice, { store: new IndexedDBStore(name, { indexedDB, persist: false }) });
  const v = a.messages(conv_id);
  await until('restored', () => v.get().length === 3);
  assert.deepEqual(v.get().map((m) => [m.content, m.status]), [['one', 'sending'], ['two', 'sending'], ['three', 'sending']]);
  await a.connect();
  await until('sent', () => texts(v).join('|') === 'one|two|three');
  assert.deepEqual(await a.pendingOps(), []);
  const b = await client(bob);
  await b.connect();
  await connected(b);
  const bv = b.messages(conv_id);
  await until('bob has three', () => texts(bv).join('|') === 'one|two|three');
  // Bodies are encrypted at rest.
  const raw = await new Promise((ok) => {
    const r = indexedDB.open(`${name}-cache`);
    r.onsuccess = () => {
      const tx = r.result.transaction('messages', 'readonly');
      const g = tx.objectStore('messages').getAll();
      g.onsuccess = () => ok(g.result);
    };
  });
  assert.ok(raw.length >= 3);
  assert.ok(!JSON.stringify(raw).includes('three'), 'no plain text in the cache');
});

test('a new device pages back through history', async () => {
  const alice = await person('alice');
  const a = await client(alice);
  await a.connect();
  await connected(a);
  const { conv_id } = await a.call('create_conv', { kind: 'group', title: 'h', members: [] });
  for (let i = 0; i < 80; i++) await a.send(conv_id, `m${i}`);
  const av = a.messages(conv_id);
  await until('all sent', () => texts(av).length === 80, 60000);
  const other = await client(alice, { deviceId: `${alice.name}-other` });
  await other.connect();
  await connected(other);
  const v = other.messages(conv_id);
  await until('the newest page', () => v.get().length === 30);
  assert.equal(v.hasOlder, true);
  while (await v.loadOlder());
  assert.equal(v.olderEnd, 'start');
  assert.deepEqual(texts(v), Array.from({ length: 80 }, (_, i) => `m${i}`));
});

test('a refused token stops the client', async () => {
  const alice = await person('alice');
  const a = await client(alice, {
    tokenProvider: async () => {
      throw new TokenRefused('no');
    },
  });
  await a.connect();
  await until('failed', () => a.connection.get().state === 'failed');
  assert.equal(a.connection.get().reason, 'unauthenticated');
});
