// Local cache (docs/05-operations.md §6.2). Like the large IM SDKs: SQLite
// on devices (React Native, Electron, Node), IndexedDB in browsers. A cache
// only: anything lost is synced again from the server, except unsent
// messages, which live in the same tables.
//
// Writes are per message: a conversation with many messages is not
// rewritten for each new one.

import type { ChatMessage } from './types.js';

export interface ConvRecord {
  convId: string;
  /** Highest seq applied and acknowledged. */
  lastSeq: number;
  /** By created_seq; unsent messages (no seq yet) at the end. */
  messages: ChatMessage[];
}

export interface Store {
  load(convId: string): Promise<ConvRecord | undefined>;
  /** Saves the position, the given messages (insert or replace) and removes the given keys, together. */
  write(convId: string, lastSeq: number, put: ChatMessage[], remove: string[]): Promise<void>;
  list(): Promise<string[]>;
  /** Everything (logout). */
  clear(): Promise<void>;
}

/** The key a message is stored under (its local id until the server names it). */
export const keyOf = (m: ChatMessage) => m.msg_id;

const ordered = (ms: ChatMessage[]) =>
  ms.sort((a, b) => (a.status === 'sent' ? a.created_seq : Infinity) - (b.status === 'sent' ? b.created_seq : Infinity) || a.created_at - b.created_at);

export class MemoryStore implements Store {
  private m = new Map<string, { lastSeq: number; messages: Map<string, ChatMessage> }>();
  async load(convId: string) {
    const r = this.m.get(convId);
    if (!r) return undefined;
    return { convId, lastSeq: r.lastSeq, messages: ordered([...r.messages.values()].map((x) => structuredClone(x))) };
  }
  async write(convId: string, lastSeq: number, put: ChatMessage[], remove: string[]) {
    let r = this.m.get(convId);
    if (!r) this.m.set(convId, (r = { lastSeq, messages: new Map() }));
    r.lastSeq = lastSeq;
    for (const k of remove) r.messages.delete(k);
    for (const x of put) r.messages.set(keyOf(x), structuredClone(x));
  }
  async list() {
    return [...this.m.keys()];
  }
  async clear() {
    this.m.clear();
  }
}

/** Browser cache in IndexedDB, one database per name (use one per server and user). */
export class IndexedDBStore implements Store {
  private db: Promise<IDBDatabase>;
  constructor(name: string) {
    this.db = new Promise((resolve, reject) => {
      const req = indexedDB.open(`agentim:${name}`, 1);
      req.onupgradeneeded = () => {
        req.result.createObjectStore('convs', { keyPath: 'convId' });
        const m = req.result.createObjectStore('messages', { keyPath: ['convId', 'key'] });
        m.createIndex('conv', 'convId');
      };
      req.onsuccess = () => resolve(req.result);
      req.onerror = () => reject(req.error);
    });
  }
  private done(t: IDBTransaction) {
    return new Promise<void>((resolve, reject) => {
      t.oncomplete = () => resolve();
      t.onerror = () => reject(t.error);
      t.onabort = () => reject(t.error);
    });
  }
  async load(convId: string) {
    const db = await this.db;
    const t = db.transaction(['convs', 'messages'], 'readonly');
    const conv = t.objectStore('convs').get(convId);
    const rows = t.objectStore('messages').index('conv').getAll(convId);
    await this.done(t);
    if (!conv.result) return undefined;
    return { convId, lastSeq: conv.result.lastSeq as number, messages: ordered((rows.result as any[]).map((r) => r.m)) };
  }
  async write(convId: string, lastSeq: number, put: ChatMessage[], remove: string[]) {
    const db = await this.db;
    const t = db.transaction(['convs', 'messages'], 'readwrite');
    t.objectStore('convs').put({ convId, lastSeq });
    const m = t.objectStore('messages');
    for (const k of remove) m.delete([convId, k]);
    for (const x of put) m.put({ convId, key: keyOf(x), m: x });
    await this.done(t);
  }
  async list() {
    const db = await this.db;
    const t = db.transaction('convs', 'readonly');
    const r = t.objectStore('convs').getAllKeys();
    await this.done(t);
    return r.result as string[];
  }
  async clear() {
    const db = await this.db;
    const t = db.transaction(['convs', 'messages'], 'readwrite');
    t.objectStore('convs').clear();
    t.objectStore('messages').clear();
    await this.done(t);
  }
}

/**
 * The few calls the SQLite store needs, so any driver fits: expo-sqlite,
 * op-sqlite (React Native), better-sqlite3 or node:sqlite (Electron, Node).
 * See `nodeSqlite` for an example adapter.
 */
export interface SqlDriver {
  run(sql: string, params?: unknown[]): Promise<void>;
  all(sql: string, params?: unknown[]): Promise<Record<string, unknown>[]>;
}

/** Device cache in SQLite: one row per message, written in one transaction per batch. */
export class SqliteStore implements Store {
  private ready: Promise<void>;
  private chain: Promise<unknown> = Promise.resolve();
  constructor(private db: SqlDriver) {
    this.ready = (async () => {
      await db.run('PRAGMA journal_mode = WAL');
      await db.run('CREATE TABLE IF NOT EXISTS agentim_convs (conv_id TEXT PRIMARY KEY, last_seq INTEGER NOT NULL)');
      await db.run(
        'CREATE TABLE IF NOT EXISTS agentim_messages (conv_id TEXT NOT NULL, key TEXT NOT NULL, seq INTEGER NOT NULL, sent INTEGER NOT NULL, created_at INTEGER NOT NULL, body TEXT NOT NULL, PRIMARY KEY (conv_id, key))',
      );
      await db.run('CREATE INDEX IF NOT EXISTS agentim_messages_order ON agentim_messages (conv_id, sent DESC, seq, created_at)');
    })();
  }
  /** One statement sequence at a time: transactions never interleave. */
  private serial<T>(f: () => Promise<T>): Promise<T> {
    const r = this.chain.then(() => this.ready).then(f);
    this.chain = r.catch(() => {});
    return r;
  }
  load(convId: string) {
    return this.serial(async () => {
      const c = await this.db.all('SELECT last_seq FROM agentim_convs WHERE conv_id = ?', [convId]);
      if (!c.length) return undefined;
      const rows = await this.db.all('SELECT body FROM agentim_messages WHERE conv_id = ? ORDER BY sent DESC, seq, created_at', [convId]);
      return { convId, lastSeq: Number(c[0].last_seq), messages: rows.map((r) => JSON.parse(String(r.body)) as ChatMessage) };
    });
  }
  write(convId: string, lastSeq: number, put: ChatMessage[], remove: string[]) {
    return this.serial(async () => {
      await this.db.run('BEGIN');
      try {
        await this.db.run(
          'INSERT INTO agentim_convs (conv_id, last_seq) VALUES (?, ?) ON CONFLICT (conv_id) DO UPDATE SET last_seq = excluded.last_seq',
          [convId, lastSeq],
        );
        for (const k of remove) await this.db.run('DELETE FROM agentim_messages WHERE conv_id = ? AND key = ?', [convId, k]);
        for (const m of put) {
          const sent = m.status === 'sent' ? 1 : 0;
          await this.db.run(
            'INSERT OR REPLACE INTO agentim_messages (conv_id, key, seq, sent, created_at, body) VALUES (?, ?, ?, ?, ?, ?)',
            [convId, keyOf(m), sent ? m.created_seq : 0, sent, m.created_at, JSON.stringify(m)],
          );
        }
        await this.db.run('COMMIT');
      } catch (e) {
        await this.db.run('ROLLBACK').catch(() => {});
        throw e;
      }
    });
  }
  list() {
    return this.serial(async () => (await this.db.all('SELECT conv_id FROM agentim_convs')).map((r) => String(r.conv_id)));
  }
  clear() {
    return this.serial(async () => {
      await this.db.run('DELETE FROM agentim_messages');
      await this.db.run('DELETE FROM agentim_convs');
    });
  }
}

/** Adapter for Node's built-in `node:sqlite` (`new DatabaseSync(path)`), and anything with the same shape (better-sqlite3). */
export function nodeSqlite(db: { exec(sql: string): void; prepare(sql: string): { run(...p: unknown[]): unknown; all(...p: unknown[]): unknown[] } }): SqlDriver {
  return {
    async run(sql, params = []) {
      if (params.length) db.prepare(sql).run(...params);
      else db.exec(sql);
    },
    async all(sql, params = []) {
      return db.prepare(sql).all(...params) as Record<string, unknown>[];
    },
  };
}
