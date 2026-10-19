// SQLite store for devices: React Native (expo-sqlite, op-sqlite),
// Electron and Node (better-sqlite3, node:sqlite). The app passes a small
// driver; `nodeSqlite`, `expoSqlite` and `opSqlite` adapt the common ones.

import { SCHEMA_VERSION, StorageError, byOrder, jsonCodec, type Codec, type ConvRow, type OutboxItem, type PageQuery, type Store, type WriteBatch } from './types.js';
import type { ChatMessage } from '../types.js';

export interface SqlDriver {
  run(sql: string, params?: unknown[]): Promise<void>;
  all(sql: string, params?: unknown[]): Promise<Record<string, unknown>[]>;
  close?(): Promise<void>;
}

const T = {
  meta: 'agentim_meta',
  convs: 'agentim_convs',
  messages: 'agentim_messages',
  outbox: 'agentim_outbox',
};

const DDL = [
  `CREATE TABLE IF NOT EXISTS ${T.meta} (k TEXT PRIMARY KEY, v TEXT NOT NULL)`,
  `CREATE TABLE IF NOT EXISTS ${T.convs} (conv_id TEXT PRIMARY KEY, last_seq INTEGER NOT NULL, opened_at INTEGER NOT NULL, info TEXT)`,
  `CREATE TABLE IF NOT EXISTS ${T.messages} (conv_id TEXT NOT NULL, msg_id TEXT NOT NULL, seq INTEGER NOT NULL, sent INTEGER NOT NULL, created_at INTEGER NOT NULL, bytes INTEGER NOT NULL, body TEXT NOT NULL, PRIMARY KEY (conv_id, msg_id))`,
  `CREATE INDEX IF NOT EXISTS agentim_messages_page ON ${T.messages} (conv_id, sent, seq)`,
  `CREATE TABLE IF NOT EXISTS ${T.outbox} (id TEXT PRIMARY KEY, conv_id TEXT NOT NULL, created_at INTEGER NOT NULL, body TEXT NOT NULL)`,
];

export interface SqliteOptions {
  /** Content encoding; `aesCodec(key)` to encrypt (docs/12 §2.1). */
  codec?: Codec;
  log?: (m: string) => void;
}

export class SqliteStore implements Store {
  private chain: Promise<unknown> = Promise.resolve();
  private codec: Codec;
  constructor(
    private db: SqlDriver,
    private o: SqliteOptions = {},
  ) {
    this.codec = o.codec ?? jsonCodec;
  }

  /** One statement sequence at a time: transactions never interleave. */
  private serial<T>(f: () => Promise<T>): Promise<T> {
    const r = this.chain.then(f);
    this.chain = r.catch(() => {});
    return r;
  }

  private async wipe() {
    // Also the tables of the first version (agentim_convs without opened_at).
    for (const t of Object.values(T)) await this.db.run(`DROP TABLE IF EXISTS ${t}`);
    for (const d of DDL) await this.db.run(d);
  }

  open(account: string) {
    return this.serial(async () => {
      let reset = false;
      try {
        await this.db.run('PRAGMA journal_mode = WAL').catch(() => {});
        // Another process (a second window) may hold the write lock briefly.
        await this.db.run('PRAGMA busy_timeout = 5000').catch(() => {});
        await this.db.run(DDL[0]);
        const meta = new Map((await this.db.all(`SELECT k, v FROM ${T.meta}`)).map((r) => [String(r.k), String(r.v)]));
        const schema = Number(meta.get('schema') ?? 0);
        const old = schema === 0 && (await this.db.all(`SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?`, [T.convs])).length > 0;
        if (old || schema > SCHEMA_VERSION || (meta.get('account') ?? account) !== account) {
          // A first-version cache, a newer app's schema, another user's data.
          this.o.log?.(`local store reset (schema ${schema}, account ${meta.get('account') ? 'changed' : 'same'})`);
          await this.wipe();
          reset = true;
        } else {
          for (const d of DDL) await this.db.run(d);
        }
        await this.db.run(`INSERT OR REPLACE INTO ${T.meta} (k, v) VALUES ('schema', ?), ('account', ?)`, [String(SCHEMA_VERSION), account]);
        // Readable end to end, or it is rebuilt.
        await this.db.all(`SELECT COUNT(*) AS n FROM ${T.messages}`);
      } catch (e) {
        this.o.log?.(`local store damaged (${e}); rebuilding`);
        try {
          await this.wipe();
          await this.db.run(`INSERT OR REPLACE INTO ${T.meta} (k, v) VALUES ('schema', ?), ('account', ?)`, [String(SCHEMA_VERSION), account]);
        } catch (e2) {
          throw new StorageError(`local store cannot be opened: ${e2}`);
        }
        reset = true;
      }
      return { reset };
    });
  }

  private convOf(r: Record<string, unknown>): ConvRow {
    return {
      convId: String(r.conv_id),
      lastSeq: Number(r.last_seq),
      openedAt: Number(r.opened_at),
      info: r.info == null ? undefined : JSON.parse(String(r.info)),
    };
  }

  loadConv(convId: string) {
    return this.serial(async () => {
      const r = await this.db.all(`SELECT * FROM ${T.convs} WHERE conv_id = ?`, [convId]);
      return r.length ? this.convOf(r[0]) : undefined;
    });
  }

  convs() {
    return this.serial(async () => (await this.db.all(`SELECT * FROM ${T.convs}`)).map((r) => this.convOf(r)));
  }

  messages(convId: string, q: PageQuery = {}) {
    return this.serial(async () => {
      const limit = q.limit ?? 50;
      const rows =
        q.beforeSeq === undefined
          ? [
              ...(await this.db.all(`SELECT msg_id, body FROM ${T.messages} WHERE conv_id = ? AND sent = 1 ORDER BY seq DESC LIMIT ?`, [convId, limit])),
              ...(await this.db.all(`SELECT msg_id, body FROM ${T.messages} WHERE conv_id = ? AND sent = 0`, [convId])),
            ]
          : await this.db.all(`SELECT msg_id, body FROM ${T.messages} WHERE conv_id = ? AND sent = 1 AND seq < ? ORDER BY seq DESC LIMIT ?`, [convId, q.beforeSeq, limit]);
      const out: ChatMessage[] = [];
      for (const r of rows) {
        try {
          out.push((await this.codec.decode(String(r.body), `${convId}:${r.msg_id}`)) as ChatMessage);
        } catch (e) {
          // One unreadable row (changed key, damaged) does not hide the rest.
          this.o.log?.(`skipping an unreadable message ${r.msg_id}: ${e}`);
        }
      }
      return out.sort(byOrder);
    });
  }

  write(b: WriteBatch) {
    // Encoding (maybe encryption) first: the transaction stays short.
    const prepared = (async () => ({
      put: await Promise.all((b.put ?? []).map(async (m) => ({ m, body: await this.codec.encode(m, `${m.conv_id}:${m.msg_id}`) }))),
      outbox: await Promise.all((b.outboxPut ?? []).map(async (x) => ({ x, body: await this.codec.encode(x, `outbox:${x.id}`) }))),
    }))();
    return this.serial(async () => {
      const { put, outbox } = await prepared;
      await this.db.run('BEGIN IMMEDIATE');
      try {
        for (const c of b.convs ?? []) {
          // Positions only move forward (two tabs, two writers).
          await this.db.run(
            `INSERT INTO ${T.convs} (conv_id, last_seq, opened_at, info) VALUES (?, ?, ?, ?)
             ON CONFLICT (conv_id) DO UPDATE SET last_seq = MAX(last_seq, excluded.last_seq), opened_at = MAX(opened_at, excluded.opened_at), info = COALESCE(excluded.info, info)`,
            [c.convId, c.lastSeq, c.openedAt, c.info === undefined ? null : JSON.stringify(c.info)],
          );
        }
        for (const r of b.remove ?? []) await this.db.run(`DELETE FROM ${T.messages} WHERE conv_id = ? AND msg_id = ?`, [r.convId, r.key]);
        for (const { m, body } of put) {
          const sent = m.status === 'sent' ? 1 : 0;
          await this.db.run(`INSERT OR REPLACE INTO ${T.messages} (conv_id, msg_id, seq, sent, created_at, bytes, body) VALUES (?, ?, ?, ?, ?, ?, ?)`, [
            m.conv_id,
            m.msg_id,
            sent ? m.created_seq : 0,
            sent,
            m.created_at,
            body.length,
            body,
          ]);
        }
        for (const id of b.outboxRemove ?? []) await this.db.run(`DELETE FROM ${T.outbox} WHERE id = ?`, [id]);
        for (const { x, body } of outbox) {
          await this.db.run(`INSERT OR REPLACE INTO ${T.outbox} (id, conv_id, created_at, body) VALUES (?, ?, ?, ?)`, [x.id, x.convId, x.createdAt, body]);
        }
        await this.db.run('COMMIT');
      } catch (e) {
        await this.db.run('ROLLBACK').catch(() => {});
        throw new StorageError(`local write failed: ${e}`);
      }
    });
  }

  outbox() {
    return this.serial(async () => {
      const out: OutboxItem[] = [];
      for (const r of await this.db.all(`SELECT id, body FROM ${T.outbox} ORDER BY created_at, id`)) {
        try {
          out.push((await this.codec.decode(String(r.body), `outbox:${r.id}`)) as OutboxItem);
        } catch (e) {
          this.o.log?.(`skipping an unreadable queued operation ${r.id}: ${e}`);
        }
      }
      return out;
    });
  }

  size() {
    return this.serial(async () => Number((await this.db.all(`SELECT COALESCE(SUM(bytes), 0) AS n FROM ${T.messages}`))[0].n));
  }

  evict(bytes: number, protect = new Set<string>()) {
    return this.serial(async () => {
      const gone: string[] = [];
      let size = Number((await this.db.all(`SELECT COALESCE(SUM(bytes), 0) AS n FROM ${T.messages}`))[0].n);
      if (size <= bytes) return gone;
      const order = await this.db.all(
        `SELECT c.conv_id, (SELECT COALESCE(SUM(bytes), 0) FROM ${T.messages} m WHERE m.conv_id = c.conv_id AND m.sent = 1) AS b
         FROM ${T.convs} c WHERE NOT EXISTS (SELECT 1 FROM ${T.outbox} o WHERE o.conv_id = c.conv_id) ORDER BY c.opened_at, c.conv_id`,
      );
      for (const r of order) {
        if (size <= bytes) break;
        const id = String(r.conv_id);
        if (protect.has(id) || Number(r.b) === 0) continue;
        // Unsent messages stay (they are what the user wrote).
        await this.db.run(`DELETE FROM ${T.messages} WHERE conv_id = ? AND sent = 1`, [id]);
        size -= Number(r.b);
        gone.push(id);
      }
      return gone;
    });
  }

  clear() {
    return this.serial(async () => {
      for (const t of [T.messages, T.convs, T.outbox]) await this.db.run(`DELETE FROM ${t}`);
      await this.db.run(`DELETE FROM ${T.meta} WHERE k = 'account'`);
    });
  }

  close() {
    return this.serial(async () => {
      await this.db.close?.();
    });
  }
}

/** `node:sqlite` (`new DatabaseSync(path)`) or better-sqlite3 (`new Database(path)`). */
export function nodeSqlite(db: {
  exec(sql: string): void;
  prepare(sql: string): { run(...p: unknown[]): unknown; all(...p: unknown[]): unknown[] };
  close?(): void;
}): SqlDriver {
  return {
    async run(sql, params = []) {
      if (params.length) db.prepare(sql).run(...params);
      else db.exec(sql);
    },
    async all(sql, params = []) {
      return db.prepare(sql).all(...params) as Record<string, unknown>[];
    },
    async close() {
      db.close?.();
    },
  };
}

/** expo-sqlite (`await SQLite.openDatabaseAsync(name)`). */
export function expoSqlite(db: {
  runAsync(sql: string, params: unknown[]): Promise<unknown>;
  execAsync(sql: string): Promise<void>;
  getAllAsync(sql: string, params: unknown[]): Promise<unknown[]>;
  closeAsync?(): Promise<void>;
}): SqlDriver {
  return {
    async run(sql, params = []) {
      if (params.length) await db.runAsync(sql, params);
      else await db.execAsync(sql);
    },
    async all(sql, params = []) {
      return (await db.getAllAsync(sql, params)) as Record<string, unknown>[];
    },
    async close() {
      await db.closeAsync?.();
    },
  };
}

/** op-sqlite (`open({ name })`); `execute` returns `{ rows }`. */
export function opSqlite(db: { execute(sql: string, params?: unknown[]): Promise<{ rows?: unknown[] | { _array: unknown[] } }>; close?(): void }): SqlDriver {
  const rows = (r: { rows?: unknown[] | { _array: unknown[] } }) => (Array.isArray(r.rows) ? r.rows : (r.rows?._array ?? [])) as Record<string, unknown>[];
  return {
    async run(sql, params = []) {
      await db.execute(sql, params);
    },
    async all(sql, params = []) {
      return rows(await db.execute(sql, params));
    },
    async close() {
      db.close?.();
    },
  };
}
