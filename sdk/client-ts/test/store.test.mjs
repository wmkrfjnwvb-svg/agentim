// The three local stores behave the same (docs/12 §2.1, docs/05 §6.2):
// account ownership, schema and damage handling, newest-page reads and
// paging, positions that only move forward, the operation queue, size
// accounting and eviction, encryption.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import 'fake-indexeddb/auto';
import { MemoryStore, SqliteStore, IndexedDBStore, nodeSqlite, aesCodec, keyFromBytes, newKeyBytes, customCodec } from '../dist/index.js';

const msg = (id, seq, extra = {}) => ({
  msg_id: id, conv_id: 'c1', created_seq: seq, sender_id: 'u1', sender_name: 'a', sender_type: 'human',
  kind: 'text', content: `text ${id}`, version: 1, state: 'committed', created_at: seq, updated_at: seq, status: 'sent', ...extra,
});
const unsent = (id, at, extra = {}) => msg(id, Number.MAX_SAFE_INTEGER, { status: 'sending', created_at: at, ...extra });
const conv = (convId, lastSeq, openedAt = 0, info) => ({ convId, lastSeq, openedAt, ...(info === undefined ? {} : { info }) });
const op = (id, convId, createdAt) => ({ id, convId, op: 'send', args: { text: id }, createdAt, attempts: 0, state: 'pending' });
const ids = (ms) => ms.map((m) => m.msg_id);

const dir = mkdtempSync(join(tmpdir(), 'agentim-store-'));
let n = 0;
// Each kind: a fresh store, and a way to open the same data again (a reload).
const kinds = {
  memory: (o) => {
    const s = new MemoryStore();
    return { store: s, reopen: () => s };
  },
  sqlite: (o) => {
    const path = join(dir, `s${++n}.db`);
    const mk = (o2 = o) => new SqliteStore(nodeSqlite(new DatabaseSync(path)), o2);
    return { store: mk(), reopen: mk, path };
  },
  indexeddb: (o) => {
    const name = `t${++n}`;
    const mk = (o2 = o) => new IndexedDBStore(name, o2);
    return { store: mk(), reopen: mk, name };
  },
};

for (const [kind, make] of Object.entries(kinds)) {
  test(`${kind}: owner account: kept for the same user, wiped for another`, async () => {
    const { store, reopen } = make();
    assert.deepEqual(await store.open('u1'), { reset: false }, 'a fresh store is not a reset');
    await store.write({ convs: [conv('c1', 1)], put: [msg('m1', 1)], outboxPut: [op('o1', 'c1', 1)] });
    assert.deepEqual(await store.open('u1'), { reset: false }, 'opening twice is fine');
    const again = reopen();
    assert.deepEqual(await again.open('u1'), { reset: false });
    assert.deepEqual(ids(await again.messages('c1')), ['m1'], 'survives a reload');
    // Another user on the same device sees nothing of the first one.
    assert.deepEqual(await again.open('u2'), { reset: true });
    assert.deepEqual(await again.convs(), []);
    assert.deepEqual(await again.messages('c1'), []);
    assert.deepEqual(await again.outbox(), []);
    assert.equal(await again.size(), 0);
    await again.close();
  });

  test(`${kind}: newest page, unsent last, paging back, boundaries`, async () => {
    const { store } = make();
    await store.open('u1');
    assert.equal(await store.loadConv('nope'), undefined, 'unknown conversation');
    assert.deepEqual(await store.messages('nope'), []);
    // 120 sent (written out of order), 2 unsent.
    const all = Array.from({ length: 120 }, (_, i) => msg(`m${i + 1}`, i + 1));
    await store.write({ convs: [conv('c1', 120)], put: [...all.slice(60), unsent('local:b', 2000), unsent('local:a', 1000)] });
    await store.write({ put: all.slice(0, 60) });
    let page = await store.messages('c1');
    assert.equal(page.length, 52, 'default page is 50 sent plus the unsent ones');
    assert.deepEqual(ids(page).slice(0, 2), ['m71', 'm72']);
    assert.deepEqual(ids(page).slice(-3), ['m120', 'local:a', 'local:b'], 'unsent after sent, by time');
    page = await store.messages('c1', { beforeSeq: 71, limit: 50 });
    assert.deepEqual([page[0].msg_id, page.at(-1).msg_id, page.length], ['m21', 'm70', 50], 'older page has no unsent ones');
    page = await store.messages('c1', { beforeSeq: 21, limit: 50 });
    assert.deepEqual(ids(page), all.slice(0, 20).map((m) => m.msg_id), 'last page is short');
    assert.deepEqual(await store.messages('c1', { beforeSeq: 1 }), [], 'nothing before the first');
    assert.deepEqual(ids(await store.messages('c1', { limit: 1 })), ['m120', 'local:a', 'local:b']);
    await store.close();
  });

  test(`${kind}: replace, remove, isolation, content kept exactly`, async () => {
    const { store } = make();
    await store.open('u1');
    await store.write({ convs: [conv('c1', 3), conv('c2', 1)], put: [msg('m1', 1), msg('m2', 2), unsent('local:x', 5), { ...msg('z1', 1), conv_id: 'c2' }] });
    // The local copy becomes the stored message in one write.
    await store.write({ put: [msg('m2', 2, { content: 'edited', version: 2 }), msg('m3', 3)], remove: [{ convId: 'c1', key: 'local:x' }, { convId: 'c1', key: 'never-there' }] });
    const c1 = await store.messages('c1');
    assert.deepEqual(ids(c1), ['m1', 'm2', 'm3']);
    assert.equal(c1[1].content, 'edited');
    // A key removed in another conversation does not touch this one.
    await store.write({ remove: [{ convId: 'c2', key: 'm1' }] });
    assert.equal((await store.messages('c1')).length, 3);
    assert.deepEqual(ids(await store.messages('c2')), ['z1']);
    await store.write({ put: [msg('m4', 4, { content: 'Größe 你好 🚀 "q" \\ \u0000', payload: { a: [1, { b: null }], n: 1.5 } })] });
    const m4 = (await store.messages('c1')).at(-1);
    assert.equal(m4.content, 'Größe 你好 🚀 "q" \\ \u0000');
    assert.deepEqual(m4.payload, { a: [1, { b: null }], n: 1.5 });
    assert.deepEqual((await store.convs()).map((c) => c.convId).sort(), ['c1', 'c2']);
    await store.clear();
    assert.deepEqual(await store.convs(), []);
    assert.deepEqual(await store.messages('c1'), []);
    assert.equal(await store.size(), 0);
    await store.close();
  });

  test(`${kind}: positions and use times only move forward; info is kept unless given`, async () => {
    const { store } = make();
    await store.open('u1');
    await store.write({ convs: [conv('c1', 5, 100, { title: 'a' })] });
    await store.write({ convs: [conv('c1', 3, 50)] });
    assert.deepEqual(await store.loadConv('c1'), conv('c1', 5, 100, { title: 'a' }));
    await store.write({ convs: [conv('c1', 6, 200, { title: 'b' })] });
    assert.deepEqual(await store.loadConv('c1'), conv('c1', 6, 200, { title: 'b' }));
    // Racing writers (two tabs, a sync and a push): the highest wins.
    await Promise.all(Array.from({ length: 60 }, (_, i) => store.write({ convs: [conv('c1', (i * 37) % 61 + 6, i)], put: [msg(`r${i}`, 100 + i)] })));
    const c = await store.loadConv('c1');
    assert.equal(c.lastSeq, 66);
    assert.equal(c.openedAt, 200);
    assert.equal((await store.messages('c1', { limit: 1000 })).length, 60, 'every write kept');
    await store.close();
  });

  test(`${kind}: queued operations: order, replace, remove, survive a reload`, async () => {
    const { store, reopen } = make();
    await store.open('u1');
    assert.deepEqual(await store.outbox(), []);
    await store.write({ outboxPut: [op('b', 'c1', 2), op('a', 'c1', 2), op('z', 'c2', 1)] });
    assert.deepEqual((await store.outbox()).map((x) => x.id), ['z', 'a', 'b'], 'by time, then id');
    await store.write({ outboxPut: [{ ...op('a', 'c1', 2), attempts: 3, state: 'failed', error: { code: 'forbidden', message: 'no' } }], outboxRemove: ['z', 'unknown'] });
    const r = reopen();
    await r.open('u1');
    const again = await r.outbox();
    assert.deepEqual(again.map((x) => x.id), ['a', 'b']);
    assert.deepEqual(again[0], { ...op('a', 'c1', 2), attempts: 3, state: 'failed', error: { code: 'forbidden', message: 'no' } });
    await store.close();
  });

  test(`${kind}: size counts what is stored; eviction frees the least recently used`, async () => {
    const { store } = make();
    await store.open('u1');
    const fill = (c, k) => Array.from({ length: k }, (_, i) => ({ ...msg(`${c}-${i}`, i + 1), conv_id: c, content: 'x'.repeat(1000) }));
    await store.write({ convs: [conv('old', 10, 1), conv('mid', 10, 2), conv('busy', 10, 3), conv('new', 10, 4)], put: [...fill('old', 10), ...fill('mid', 10), ...fill('busy', 10), ...fill('new', 10)] });
    const one = (await store.size()) / 4;
    assert.ok(one > 10000);
    // Replacing a message does not count it twice; removing it frees it.
    await store.write({ put: fill('old', 10) });
    assert.equal(await store.size(), one * 4);
    await store.write({ put: [{ ...msg('local:u', 0), conv_id: 'old', status: 'sending', content: 'x'.repeat(1000) }] });
    await store.write({ outboxPut: [op('q1', 'busy', 1)] });
    assert.deepEqual(await store.evict(one * 10), [], 'under the limit: nothing goes');
    // Needs two conversations out: 'old' (oldest), 'busy' is queued, 'mid' is on screen, so 'new'.
    const gone = await store.evict(one * 2 + 2000, new Set(['mid']));
    assert.deepEqual(gone, ['old', 'new']);
    assert.deepEqual(ids(await store.messages('old')), ['local:u'], 'unsent messages stay');
    assert.equal((await store.messages('mid')).length, 10);
    assert.equal((await store.messages('busy')).length, 10);
    assert.equal((await store.messages('new')).length, 0);
    assert.ok((await store.size()) <= one * 2 + 2000);
    assert.equal((await store.loadConv('old')).lastSeq, 10, 'positions stay: what is gone is not synced again');
    assert.deepEqual(await store.evict(0, new Set(['mid'])), [], 'nothing more may go');
    await store.close();
  });

  test(`${kind}: encryption: readable with the key, rows bound to their place, plain rows still read`, async () => {
    if (kind === 'memory') return; // nothing stored outside the process
    const key = await keyFromBytes(newKeyBytes());
    const logs = [];
    const plain = make();
    await plain.store.open('u1');
    await plain.store.write({ convs: [conv('c1', 1)], put: [msg('m1', 1, { content: 'before encryption' })] });
    await plain.store.close();
    // Encryption turned on later: old rows are read, new ones encrypted.
    const enc = plain.reopen({ codec: aesCodec(key), log: (m) => logs.push(m) });
    await enc.open('u1');
    await enc.write({ put: [msg('m2', 2, { content: 'secret words' })], outboxPut: [op('o1', 'c1', 1)] });
    assert.deepEqual((await enc.messages('c1')).map((m) => m.content), ['before encryption', 'secret words']);
    assert.equal((await enc.outbox())[0].args.text, 'o1');
    if (kind === 'sqlite') {
      const raw = new DatabaseSync(plain.path);
      const bodies = raw.prepare('SELECT msg_id, body FROM agentim_messages').all();
      assert.ok(!bodies.find((r) => r.msg_id === 'm2').body.includes('secret'), 'stored text is not readable');
      assert.ok(!raw.prepare('SELECT body FROM agentim_outbox').all()[0].body.includes('o1'));
      // Moving an encrypted row to another place is detected (the row is skipped).
      raw.prepare(`INSERT INTO agentim_messages (conv_id, msg_id, seq, sent, created_at, bytes, body) VALUES ('c1', 'm3', 3, 1, 3, 1, ?)`).run(bodies.find((r) => r.msg_id === 'm2').body);
      raw.close();
      assert.deepEqual(ids(await enc.messages('c1')), ['m1', 'm2']);
      assert.ok(logs.some((l) => l.includes('m3')));
    }
    await enc.close();
    // Another key: encrypted rows are skipped, the store still opens.
    const wrong = plain.reopen({ codec: aesCodec(await keyFromBytes(newKeyBytes())), log: () => {} });
    await wrong.open('u1');
    assert.deepEqual(ids(await wrong.messages('c1')), ['m1']);
    assert.deepEqual(await wrong.outbox(), []);
    await wrong.close();
  });

  test(`${kind}: the app's own encryption functions`, async () => {
    const rot = (s) => [...s].map((c) => String.fromCharCode(c.charCodeAt(0) ^ 7)).join('');
    const seen = [];
    const codec = customCodec({ encrypt: (p) => (seen.push(p), rot(p)), decrypt: rot });
    const { store } = make({ codec });
    await store.open('u1');
    await store.write({ convs: [conv('c1', 1)], put: [msg('m1', 1, { content: 'hi' })] });
    assert.equal((await store.messages('c1'))[0].content, 'hi');
    if (kind !== 'memory') assert.ok(seen.some((p) => p.includes('"hi"')));
    // A row decoded for another place is refused.
    const text = await codec.encode({ v: 1 }, 'c1:m1');
    await assert.rejects(codec.decode(text, 'c1:m2'), /does not belong/);
    await store.close();
  });
}

test('keyFromBytes: only 32-byte keys', async () => {
  await assert.rejects(keyFromBytes(new Uint8Array(16)), /32 bytes/);
  await assert.rejects(keyFromBytes(new Uint8Array(0)), /32 bytes/);
  assert.equal(newKeyBytes().length, 32);
  assert.notDeepEqual(newKeyBytes(), newKeyBytes());
});

test('aesCodec: tampered or moved content is refused', async () => {
  const c = aesCodec(await keyFromBytes(newKeyBytes()));
  const t = await c.encode({ a: 1 }, 'c1:m1');
  assert.deepEqual(await c.decode(t, 'c1:m1'), { a: 1 });
  await assert.rejects(c.decode(t, 'c1:m2'));
  const flipped = t.slice(0, -4) + (t.at(-4) === 'A' ? 'B' : 'A') + t.slice(-3);
  await assert.rejects(c.decode(flipped, 'c1:m1'));
  assert.notEqual(await c.encode({ a: 1 }, 'c1:m1'), t, 'a fresh IV each time');
});

test('sqlite: a first-version cache is dropped and rebuilt', async () => {
  const path = join(dir, `v1-${++n}.db`);
  const raw = new DatabaseSync(path);
  raw.exec(`CREATE TABLE agentim_convs (conv_id TEXT PRIMARY KEY, last_seq INTEGER NOT NULL)`);
  raw.exec(`CREATE TABLE agentim_messages (conv_id TEXT, key TEXT, seq INTEGER, sent INTEGER, created_at INTEGER, body TEXT)`);
  raw.exec(`INSERT INTO agentim_convs VALUES ('c1', 9)`);
  raw.close();
  const s = new SqliteStore(nodeSqlite(new DatabaseSync(path)));
  assert.deepEqual(await s.open('u1'), { reset: true });
  assert.deepEqual(await s.convs(), []);
  await s.write({ convs: [conv('c1', 1)], put: [msg('m1', 1)] });
  assert.deepEqual(ids(await s.messages('c1')), ['m1']);
  await s.close();
});

test('sqlite: a newer schema (an app downgrade) is rebuilt, not misread', async () => {
  const path = join(dir, `v9-${++n}.db`);
  const s = new SqliteStore(nodeSqlite(new DatabaseSync(path)));
  await s.open('u1');
  await s.write({ convs: [conv('c1', 1)] });
  await s.close();
  const raw = new DatabaseSync(path);
  raw.exec(`UPDATE agentim_meta SET v = '99' WHERE k = 'schema'`);
  raw.close();
  const s2 = new SqliteStore(nodeSqlite(new DatabaseSync(path)));
  assert.deepEqual(await s2.open('u1'), { reset: true });
  assert.deepEqual(await s2.convs(), []);
  await s2.close();
});

test('sqlite: a damaged store is rebuilt; one that cannot be is a storage error', async () => {
  const logs = [];
  const real = nodeSqlite(new DatabaseSync(join(dir, `bad-${++n}.db`)));
  let fail = true;
  const flaky = { ...real, all: (sql, p) => (fail && sql.startsWith('SELECT COUNT') ? Promise.reject(new Error('database disk image is malformed')) : real.all(sql, p)) };
  const s = new SqliteStore(flaky, { log: (m) => logs.push(m) });
  assert.deepEqual(await s.open('u1'), { reset: true });
  assert.ok(logs.some((l) => l.includes('malformed')));
  fail = false;
  await s.write({ convs: [conv('c1', 1)] });
  assert.equal((await s.loadConv('c1')).lastSeq, 1);
  const dead = { run: () => Promise.reject(new Error('disk full')), all: () => Promise.reject(new Error('disk full')) };
  await assert.rejects(new SqliteStore(dead).open('u1'), (e) => e.code === 'storage');
});

test('sqlite: a failed write leaves nothing behind', async () => {
  const real = nodeSqlite(new DatabaseSync(join(dir, `tx-${++n}.db`)));
  let failOn = null;
  const d = { ...real, run: (sql, p) => (failOn && sql.includes(failOn) ? Promise.reject(new Error('boom')) : real.run(sql, p)) };
  const s = new SqliteStore(d);
  await s.open('u1');
  failOn = 'agentim_outbox';
  await assert.rejects(s.write({ convs: [conv('c1', 5)], put: [msg('m1', 1)], outboxPut: [op('o1', 'c1', 1)] }), (e) => e.code === 'storage');
  failOn = null;
  assert.equal(await s.loadConv('c1'), undefined);
  assert.deepEqual(await s.messages('c1'), []);
  // The store goes on working after a failed write.
  await s.write({ convs: [conv('c1', 5)] });
  assert.equal((await s.loadConv('c1')).lastSeq, 5);
});

test('indexeddb: a newer version (an app downgrade) is rebuilt', async () => {
  const name = `idb-new-${++n}`;
  await new Promise((resolve, reject) => {
    const r = indexedDB.open(`agentim:${name}`, 99);
    r.onupgradeneeded = () => r.result.createObjectStore('future');
    r.onsuccess = () => (r.result.close(), resolve());
    r.onerror = () => reject(r.error);
  });
  const s = new IndexedDBStore(name, { log: () => {} });
  assert.deepEqual(await s.open('u1'), { reset: true });
  await s.write({ convs: [conv('c1', 1)], put: [msg('m1', 1)] });
  assert.deepEqual(ids(await s.messages('c1')), ['m1']);
  await s.close();
});

test('indexeddb: two tabs write the same conversation: positions only go forward, size stays right', async () => {
  const name = `idb-tabs-${++n}`;
  const [a, b] = [new IndexedDBStore(name), new IndexedDBStore(name)];
  await a.open('u1');
  await b.open('u1');
  for (let round = 0; round < 5; round++) {
    await Promise.all(
      Array.from({ length: 40 }, (_, i) => (i % 2 ? a : b).write({ convs: [conv('c1', round * 100 + ((i * 13) % 40), i)], put: [msg(`m${i % 10}`, i % 10 + 1, { content: `r${round}-${i}` })] })),
    );
  }
  assert.equal((await a.loadConv('c1')).lastSeq, 439);
  assert.equal((await b.messages('c1')).length, 10);
  // Size equals the sum of what is stored.
  const one = await (async () => {
    const m = new MemoryStore();
    await m.open('u1');
    await m.write({ put: await b.messages('c1') });
    return m.size();
  })();
  assert.equal(await a.size(), one);
  await a.close();
  await b.close();
});

test('cleanup', () => rmSync(dir, { recursive: true, force: true }));
