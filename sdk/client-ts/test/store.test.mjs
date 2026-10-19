// The three local stores behave the same (docs/05 §6.2): order, replace,
// remove, position, isolation between conversations, clear, reopen.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import 'fake-indexeddb/auto';
import { MemoryStore, SqliteStore, IndexedDBStore, nodeSqlite } from '../dist/index.js';

const msg = (id, seq, extra = {}) => ({
  msg_id: id, conv_id: 'c1', created_seq: seq, sender_id: 'u1', sender_name: 'a', sender_type: 'human',
  kind: 'text', content: `text ${id}`, version: 1, state: 'committed', created_at: seq, updated_at: seq, status: 'sent', ...extra,
});

const dir = mkdtempSync(join(tmpdir(), 'agentim-store-'));
let n = 0;
const kinds = {
  memory: () => ({ store: new MemoryStore(), reopen: null }),
  sqlite: () => {
    const path = join(dir, `s${++n}.db`);
    return { store: new SqliteStore(nodeSqlite(new DatabaseSync(path))), reopen: () => new SqliteStore(nodeSqlite(new DatabaseSync(path))) };
  },
  indexeddb: () => {
    const name = `t${++n}`;
    return { store: new IndexedDBStore(name), reopen: () => new IndexedDBStore(name) };
  },
};

for (const [kind, make] of Object.entries(kinds)) {
  test(`${kind}: order, replace, remove, position, clear`, async () => {
    const { store, reopen } = make();
    assert.equal(await store.load('c1'), undefined, 'unknown conversation');
    // Out of order writes come back by seq; unsent ones last, by time.
    await store.write('c1', 3, [msg('m3', 3), msg('m1', 1), msg('local:x', Number.MAX_SAFE_INTEGER, { status: 'sending', created_at: 99 })], []);
    await store.write('c1', 3, [msg('m2', 2)], []);
    let r = await store.load('c1');
    assert.equal(r.lastSeq, 3);
    assert.deepEqual(r.messages.map((m) => m.msg_id), ['m1', 'm2', 'm3', 'local:x']);
    // Replace (a newer version) and remove (the local copy, once sent).
    await store.write('c1', 4, [msg('m2', 2, { content: 'edited', version: 2 }), msg('m4', 4)], ['local:x']);
    r = await store.load('c1');
    assert.equal(r.lastSeq, 4);
    assert.deepEqual(r.messages.map((m) => m.msg_id), ['m1', 'm2', 'm3', 'm4']);
    assert.equal(r.messages[1].content, 'edited');
    // Removing what is not there is fine; empty writes move the position.
    await store.write('c1', 5, [], ['nope']);
    assert.equal((await store.load('c1')).lastSeq, 5);
    // Conversations do not mix.
    await store.write('c2', 1, [{ ...msg('z1', 1), conv_id: 'c2' }], []);
    assert.deepEqual((await store.load('c2')).messages.map((m) => m.msg_id), ['z1']);
    assert.equal((await store.load('c1')).messages.length, 4);
    assert.deepEqual((await store.list()).sort(), ['c1', 'c2']);
    // Unicode and nested values come back as written.
    await store.write('c1', 5, [msg('m5', 5, { content: 'Größe 你好 🚀', payload: { a: [1, { b: null }] } })], []);
    const m5 = (await store.load('c1')).messages.find((m) => m.msg_id === 'm5');
    assert.equal(m5.content, 'Größe 你好 🚀');
    assert.deepEqual(m5.payload, { a: [1, { b: null }] });
    if (reopen) {
      const again = await reopen().load('c1');
      assert.equal(again.messages.length, 5, 'survives a reopen');
    }
    await store.clear();
    assert.equal(await store.load('c1'), undefined);
    assert.deepEqual(await store.list(), []);
  });

  test(`${kind}: concurrent writes are all kept`, async () => {
    const { store } = make();
    await Promise.all(Array.from({ length: 50 }, (_, i) => store.write('c1', i + 1, [msg(`m${i + 1}`, i + 1)], [])));
    const r = await store.load('c1');
    assert.equal(r.messages.length, 50);
    assert.deepEqual(r.messages.map((m) => m.created_seq), Array.from({ length: 50 }, (_, i) => i + 1));
  });
}

test('cleanup', () => rmSync(dir, { recursive: true, force: true }));
