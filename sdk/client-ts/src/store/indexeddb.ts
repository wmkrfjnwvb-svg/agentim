// IndexedDB store for browsers. Content is encoded (maybe encrypted)
// before each transaction: an IndexedDB transaction ends at the first
// await that is not one of its own requests.

import { SCHEMA_VERSION, StorageError, byOrder, jsonCodec, type Codec, type ConvRow, type OutboxItem, type PageQuery, type Store, type WriteBatch } from './types.js';
import type { ChatMessage } from '../types.js';

interface MsgRow {
  convId: string;
  msgId: string;
  seq: number;
  sent: number;
  createdAt: number;
  bytes: number;
  body: string;
}
interface ConvRec extends ConvRow {
  /** Stored message bytes of this conversation (for the size limit). */
  bytes: number;
}

export interface IndexedDBOptions {
  codec?: Codec;
  log?: (m: string) => void;
}

const req = <T>(r: IDBRequest<T>) =>
  new Promise<T>((resolve, reject) => {
    r.onsuccess = () => resolve(r.result);
    r.onerror = () => reject(r.error);
  });

const done = (t: IDBTransaction) =>
  new Promise<void>((resolve, reject) => {
    t.oncomplete = () => resolve();
    t.onerror = () => reject(t.error);
    t.onabort = () => reject(t.error ?? new Error('transaction aborted'));
  });

export class IndexedDBStore implements Store {
  private db: Promise<IDBDatabase> | null = null;
  private codec: Codec;
  /** Use one name per server and user, e.g. `chat.example.com:u_123`. */
  constructor(
    private name: string,
    private o: IndexedDBOptions = {},
  ) {
    this.codec = o.codec ?? jsonCodec;
  }

  private connect(): Promise<IDBDatabase> {
    return new Promise((resolve, reject) => {
      const r = indexedDB.open(`agentim:${this.name}`, SCHEMA_VERSION);
      r.onupgradeneeded = () => {
        const db = r.result;
        // Older layouts are a cache: dropped and rebuilt from the server.
        for (const s of Array.from(db.objectStoreNames)) db.deleteObjectStore(s);
        db.createObjectStore('meta');
        db.createObjectStore('convs', { keyPath: 'convId' });
        const m = db.createObjectStore('messages', { keyPath: ['convId', 'msgId'] });
        m.createIndex('page', ['convId', 'sent', 'seq']);
        m.createIndex('conv', 'convId');
        const o = db.createObjectStore('outbox', { keyPath: 'id' });
        o.createIndex('conv', 'convId');
      };
      // Another tab still holds an older version open: it is asked to close.
      r.onblocked = () => this.o.log?.('local store upgrade waits for another tab');
      r.onsuccess = () => {
        const db = r.result;
        db.onversionchange = () => db.close();
        resolve(db);
      };
      r.onerror = () => reject(r.error);
    });
  }

  private async get(): Promise<IDBDatabase> {
    if (!this.db) this.db = this.connect();
    return this.db;
  }

  private async destroy() {
    if (this.db) (await this.db.catch(() => null))?.close();
    this.db = null;
    await new Promise<void>((resolve, reject) => {
      const r = indexedDB.deleteDatabase(`agentim:${this.name}`);
      r.onsuccess = () => resolve();
      r.onerror = () => reject(r.error);
      r.onblocked = () => resolve();
    });
  }

  async open(account: string) {
    let reset = false;
    try {
      let db = await this.get();
      if (db.version > SCHEMA_VERSION) throw new Error('newer schema');
      const t = db.transaction('meta', 'readonly');
      const owner = await req(t.objectStore('meta').get('account'));
      if (owner !== undefined && owner !== account) {
        this.o.log?.('local store belonged to another account; reset');
        await this.destroy();
        db = await this.get();
        reset = true;
      }
      const w = db.transaction('meta', 'readwrite');
      w.objectStore('meta').put(account, 'account');
      await done(w);
    } catch (e) {
      this.o.log?.(`local store damaged (${e}); rebuilding`);
      try {
        await this.destroy();
        const db = await this.get();
        const w = db.transaction('meta', 'readwrite');
        w.objectStore('meta').put(account, 'account');
        await done(w);
      } catch (e2) {
        throw new StorageError(`local store cannot be opened: ${e2}`);
      }
      reset = true;
    }
    return { reset };
  }

  private row(c: ConvRec): ConvRow {
    const { bytes: _b, ...r } = c;
    return r;
  }

  async loadConv(convId: string) {
    const db = await this.get();
    const c = await req<ConvRec | undefined>(db.transaction('convs').objectStore('convs').get(convId));
    return c && this.row(c);
  }

  async convs() {
    const db = await this.get();
    return (await req<ConvRec[]>(db.transaction('convs').objectStore('convs').getAll())).map((c) => this.row(c));
  }

  async messages(convId: string, q: PageQuery = {}) {
    const db = await this.get();
    const limit = q.limit ?? 50;
    const t = db.transaction('messages');
    const page = t.objectStore('messages').index('page');
    const rows: MsgRow[] = [];
    const upper = q.beforeSeq ?? Number.POSITIVE_INFINITY;
    await new Promise<void>((resolve, reject) => {
      const c = page.openCursor(IDBKeyRange.bound([convId, 1, Number.NEGATIVE_INFINITY], [convId, 1, upper], false, true), 'prev');
      c.onsuccess = () => {
        const cur = c.result;
        if (!cur || rows.length >= limit) return resolve();
        rows.push(cur.value as MsgRow);
        cur.continue();
      };
      c.onerror = () => reject(c.error);
    });
    if (q.beforeSeq === undefined) {
      rows.push(...(await req<MsgRow[]>(page.getAll(IDBKeyRange.bound([convId, 0, Number.NEGATIVE_INFINITY], [convId, 0, Number.POSITIVE_INFINITY])))));
    }
    const out: ChatMessage[] = [];
    for (const r of rows) {
      try {
        out.push((await this.codec.decode(r.body, `${convId}:${r.msgId}`)) as ChatMessage);
      } catch (e) {
        this.o.log?.(`skipping an unreadable message ${r.msgId}: ${e}`);
      }
    }
    return out.sort(byOrder);
  }

  async write(b: WriteBatch) {
    const put = await Promise.all(
      (b.put ?? []).map(async (m): Promise<MsgRow> => {
        const body = await this.codec.encode(m, `${m.conv_id}:${m.msg_id}`);
        const sent = m.status === 'sent' ? 1 : 0;
        return { convId: m.conv_id, msgId: m.msg_id, seq: sent ? m.created_seq : 0, sent, createdAt: m.created_at, bytes: body.length, body };
      }),
    );
    const outbox = await Promise.all((b.outboxPut ?? []).map(async (x) => ({ id: x.id, convId: x.convId, createdAt: x.createdAt, body: await this.codec.encode(x, `outbox:${x.id}`) })));
    const db = await this.get();
    const t = db.transaction(['convs', 'messages', 'outbox'], 'readwrite');
    const convs = t.objectStore('convs');
    const msgs = t.objectStore('messages');
    // Byte changes per conversation, applied to its row at the end.
    const delta = new Map<string, number>();
    const add = (c: string, n: number) => delta.set(c, (delta.get(c) ?? 0) + n);
    for (const c of b.convs ?? []) delta.set(c.convId, delta.get(c.convId) ?? 0);
    for (const r of b.remove ?? []) {
      add(r.convId, 0);
      const g = msgs.get([r.convId, r.key]);
      g.onsuccess = () => {
        if (g.result) {
          add(r.convId, -(g.result as MsgRow).bytes);
          msgs.delete([r.convId, r.key]);
        }
      };
    }
    for (const row of put) {
      add(row.convId, 0);
      const g = msgs.get([row.convId, row.msgId]);
      g.onsuccess = () => {
        add(row.convId, row.bytes - ((g.result as MsgRow | undefined)?.bytes ?? 0));
        msgs.put(row);
      };
    }
    for (const id of b.outboxRemove ?? []) t.objectStore('outbox').delete(id);
    for (const x of outbox) t.objectStore('outbox').put(x);
    // After the reads above (requests run in order within a transaction).
    const given = new Map((b.convs ?? []).map((c) => [c.convId, c]));
    const last = msgs.count();
    last.onsuccess = () => {
      for (const [id, d] of delta) {
        const g = convs.get(id);
        g.onsuccess = () => {
          const old = g.result as ConvRec | undefined;
          const c = given.get(id);
          if (!old && !c) return; // messages without a conversation row yet
          // Positions only move forward (two tabs, two writers).
          convs.put({
            convId: id,
            lastSeq: Math.max(old?.lastSeq ?? 0, c?.lastSeq ?? 0),
            openedAt: Math.max(old?.openedAt ?? 0, c?.openedAt ?? 0),
            info: c?.info !== undefined ? c.info : old?.info,
            bytes: Math.max(0, (old?.bytes ?? 0) + d),
          } satisfies ConvRec);
        };
      }
    };
    try {
      await done(t);
    } catch (e) {
      throw new StorageError(`local write failed: ${e}`);
    }
  }

  async outbox() {
    const db = await this.get();
    const rows = await req<{ id: string; createdAt: number; body: string }[]>(db.transaction('outbox').objectStore('outbox').getAll());
    rows.sort((a, b) => a.createdAt - b.createdAt || (a.id < b.id ? -1 : 1));
    const out: OutboxItem[] = [];
    for (const r of rows) {
      try {
        out.push((await this.codec.decode(r.body, `outbox:${r.id}`)) as OutboxItem);
      } catch (e) {
        this.o.log?.(`skipping an unreadable queued operation ${r.id}: ${e}`);
      }
    }
    return out;
  }

  async size() {
    const db = await this.get();
    return (await req<ConvRec[]>(db.transaction('convs').objectStore('convs').getAll())).reduce((n, c) => n + (c.bytes ?? 0), 0);
  }

  async evict(bytes: number, protect = new Set<string>()) {
    const db = await this.get();
    const t0 = db.transaction(['convs', 'outbox']);
    const all = await req<ConvRec[]>(t0.objectStore('convs').getAll());
    const queued = new Set((await req<{ convId: string }[]>(t0.objectStore('outbox').getAll())).map((x) => x.convId));
    let size = all.reduce((n, c) => n + (c.bytes ?? 0), 0);
    const gone: string[] = [];
    for (const c of all.sort((a, b) => a.openedAt - b.openedAt || (a.convId < b.convId ? -1 : 1))) {
      if (size <= bytes) break;
      if (protect.has(c.convId) || queued.has(c.convId) || !c.bytes) continue;
      const t = db.transaction(['convs', 'messages'], 'readwrite');
      const msgs = t.objectStore('messages');
      let kept = 0;
      const cur = msgs.index('conv').openCursor(IDBKeyRange.only(c.convId));
      cur.onsuccess = () => {
        const x = cur.result;
        if (!x) {
          t.objectStore('convs').put({ ...c, bytes: kept });
          return;
        }
        const row = x.value as MsgRow;
        // Unsent messages stay (they are what the user wrote).
        if (row.sent) x.delete();
        else kept += row.bytes;
        x.continue();
      };
      await done(t);
      size -= c.bytes - kept;
      if (c.bytes > kept) gone.push(c.convId);
    }
    return gone;
  }

  async clear() {
    const db = await this.get();
    const t = db.transaction(['meta', 'convs', 'messages', 'outbox'], 'readwrite');
    for (const s of ['convs', 'messages', 'outbox']) t.objectStore(s).clear();
    t.objectStore('meta').delete('account');
    await done(t);
  }

  async close() {
    if (this.db) (await this.db.catch(() => null))?.close();
    this.db = null;
  }
}
