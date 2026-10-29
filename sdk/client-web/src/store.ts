// The browser cache (docs/12 §2.9). The core decides what to write; this
// writes it atomically: the queue in its own small database (it must
// survive a cache that is rebuilt), the rest in the cache database.
// Message bodies and queued operations are encrypted with AES-GCM under
// a non-extractable WebCrypto key kept in the database itself.

/** One atomic write to the cache: everything in it is written or nothing is. */
export interface Batch {
  /** The batch number; the core waits for each in order. */
  id: number;
  /** Conversations to write (replacing the stored ones). */
  convs: Array<{ conv_id: string; [k: string]: unknown }>;
  /** Messages to write. */
  put_messages: Array<Record<string, any>>;
  /** Messages to delete, as `[conv_id, key]`. */
  remove_messages: Array<[string, string]>;
  /** Queued operations to write (the offline queue). */
  put_ops: Array<Record<string, any>>;
  /** Queued operations that are done, by id. */
  remove_ops: string[];
  /** Which account the cache belongs to; a different account wipes it. */
  meta?: { account?: string | null } | null;
}

/** What the cache holds at start: the account, the conversations, the queue and recent messages. */
export interface Snapshot {
  /** The account the cache belongs to, or null when empty. */
  account: string | null;
  /** The stored conversations. */
  convs: unknown[];
  /** The stored offline queue, in order. */
  ops: unknown[];
  /** The newest messages of each conversation, up to the page size. */
  messages: unknown[];
}

/** Why a write failed, as the core names it. */
export type Failure = 'full' | 'corrupt' | 'unavailable';

/** Why the cache could not be written: `full`, `corrupt` or `unavailable`. */
export class StoreError extends Error {
  constructor(
    public kind: Failure,
    message: string,
  ) {
    super(message);
  }
}

/**
 * A cache for the client (IndexedDB in browsers, memory elsewhere); implement it to keep the cache
 * somewhere else.
 */
export interface Store {
  /** `indexeddb` or `memory`. */
  readonly kind: 'indexeddb' | 'memory';
  /** Opens the cache; fails with `StoreError` when it cannot. */
  open(): Promise<void>;
  /** Reads what the cache holds, with up to `page` messages per conversation. */
  snapshot(page: number): Promise<Snapshot>;
  /** Reads up to `limit` older messages of a conversation before `beforeSeq`. */
  page(convId: string, beforeSeq: number, limit: number): Promise<unknown[]>;
  /** Writes one batch atomically. */
  write(b: Batch): Promise<void>;
  /** Deletes everything (sign-out, a different account). */
  wipe(): Promise<void>;
  /** `navigator.storage.persist()` result, when asked. */
  persisted?: boolean;
}

/** Nothing kept after the page closes. */
export class MemoryStore implements Store {
  /** `memory`. */
  readonly kind = 'memory';
  private account: string | null = null;
  private convs = new Map<string, unknown>();
  private msgs = new Map<string, Record<string, any>>();
  private ops = new Map<string, Record<string, any>>();

  /** Nothing to open. */
  async open() {}

  /**
   * What the core reads at start: the account, conversations, the queue and the newest `page`
   * messages of each conversation.
   */
  async snapshot(page: number): Promise<Snapshot> {
    const messages: unknown[] = [];
    for (const c of this.convs.keys()) {
      const ms = [...this.msgs.values()].filter((m) => m.conv_id === c).sort((a, b) => a.created_seq - b.created_seq);
      messages.push(...ms.slice(-page));
    }
    const ops = [...this.ops.values()].sort((a, b) => a.n - b.n);
    return { account: this.account, convs: [...this.convs.values()], ops, messages };
  }

  /** Messages of a conversation before a seq, for paging back. */
  async page(convId: string, before: number, limit: number) {
    const ms = [...this.msgs.values()]
      .filter((m) => m.conv_id === convId && m.created_seq < before)
      .sort((a, b) => a.created_seq - b.created_seq);
    return ms.slice(-limit);
  }

  /** Writes a batch from the core atomically. */
  async write(b: Batch) {
    for (const o of b.put_ops) this.ops.set(o.id, o);
    for (const id of b.remove_ops) this.ops.delete(id);
    for (const c of b.convs) this.convs.set(c.conv_id, c);
    for (const m of b.put_messages) this.msgs.set(`${m.conv_id}\u0000${m.msg_id}`, m);
    for (const [c, k] of b.remove_messages) this.msgs.delete(`${c}\u0000${k}`);
    if (b.meta) this.account = b.meta.account ?? null;
  }

  /** Deletes everything (logout, another account). */
  async wipe() {
    this.account = null;
    this.convs.clear();
    this.msgs.clear();
    this.ops.clear();
  }
}

const req = <T>(r: IDBRequest<T>) =>
  new Promise<T>((ok, no) => {
    r.onsuccess = () => ok(r.result);
    r.onerror = () => no(r.error);
  });

const done = (tx: IDBTransaction) =>
  new Promise<void>((ok, no) => {
    tx.oncomplete = () => ok();
    tx.onerror = () => no(tx.error);
    tx.onabort = () => no(tx.error ?? new DOMException('aborted', 'AbortError'));
  });

function failure(e: unknown): StoreError {
  const name = (e as { name?: string })?.name ?? '';
  const kind: Failure = name === 'QuotaExceededError' ? 'full' : name === 'DataError' || name === 'VersionError' ? 'corrupt' : 'unavailable';
  return new StoreError(kind, `${name}: ${(e as Error)?.message ?? e}`);
}

function openDb(name: string, upgrade: (db: IDBDatabase) => void, idb: IDBFactory): Promise<IDBDatabase> {
  const r = idb.open(name, 1);
  r.onupgradeneeded = () => upgrade(r.result);
  return req(r);
}

/**
 * IndexedDB, one per account and device (`name`). Asks the browser to keep
 * it (`navigator.storage.persist()`) unless told not to.
 */
export class IndexedDBStore implements Store {
  /** `indexeddb`. */
  readonly kind = 'indexeddb';
  /** Whether the browser agreed to keep the cache (`navigator.storage.persist()`). */
  persisted?: boolean;
  private cache!: IDBDatabase;
  private queue!: IDBDatabase;
  private key!: CryptoKey;

  constructor(
    private name = 'agentim',
    private opts: { persist?: boolean; indexedDB?: IDBFactory } = {},
  ) {}

  private get idb(): IDBFactory {
    return this.opts.indexedDB ?? globalThis.indexedDB;
  }

  /** Opens (or creates) the cache and the queue databases and the encryption key. */
  async open() {
    try {
      this.cache = await openDb(
        `${this.name}-cache`,
        (db) => {
          db.createObjectStore('meta');
          db.createObjectStore('convs', { keyPath: 'conv_id' });
          const m = db.createObjectStore('messages', { keyPath: ['conv_id', 'msg_id'] });
          m.createIndex('seq', ['conv_id', 'seq']);
        },
        this.idb,
      );
      this.queue = await openDb(`${this.name}-queue`, (db) => db.createObjectStore('ops', { keyPath: 'id' }), this.idb);
    } catch (e) {
      throw failure(e);
    }
    // The key never leaves the browser (not extractable).
    const tx = this.queue.transaction('ops', 'readonly');
    const k = (await req(tx.objectStore('ops').get('\u0000key'))) as { key?: CryptoKey } | undefined;
    if (k?.key) {
      this.key = k.key;
    } else {
      this.key = await crypto.subtle.generateKey({ name: 'AES-GCM', length: 256 }, false, ['encrypt', 'decrypt']);
      const w = this.queue.transaction('ops', 'readwrite');
      w.objectStore('ops').put({ id: '\u0000key', key: this.key });
      await done(w);
    }
    if (this.opts.persist !== false && typeof navigator !== 'undefined' && navigator.storage?.persist) {
      this.persisted = await navigator.storage.persist().catch(() => false);
    }
  }

  private async seal(v: unknown): Promise<{ iv: Uint8Array; data: ArrayBuffer }> {
    const iv = crypto.getRandomValues(new Uint8Array(12));
    const data = await crypto.subtle.encrypt({ name: 'AES-GCM', iv }, this.key, new TextEncoder().encode(JSON.stringify(v)));
    return { iv, data };
  }

  /** None when it cannot be opened (another key): treated as missing. */
  private async open_(s: { iv: Uint8Array; data: ArrayBuffer }): Promise<any | undefined> {
    try {
      const b = await crypto.subtle.decrypt({ name: 'AES-GCM', iv: s.iv as BufferSource }, this.key, s.data);
      return JSON.parse(new TextDecoder().decode(b));
    } catch {
      return undefined;
    }
  }

  /**
   * What the core reads at start: the account, conversations, the queue and the newest `page`
   * messages of each conversation.
   */
  async snapshot(page: number): Promise<Snapshot> {
    const tx = this.cache.transaction(['meta', 'convs', 'messages'], 'readonly');
    const account = ((await req(tx.objectStore('meta').get('account'))) as string | undefined) ?? null;
    const convs = (await req(tx.objectStore('convs').getAll())) as Array<{ conv_id: string }>;
    const sealed: Array<{ iv: Uint8Array; data: ArrayBuffer }> = [];
    const idx = tx.objectStore('messages').index('seq');
    for (const c of convs) {
      const range = IDBKeyRange.bound([c.conv_id, -Infinity], [c.conv_id, Infinity]);
      const rows: any[] = [];
      await new Promise<void>((ok, no) => {
        const cur = idx.openCursor(range, 'prev');
        cur.onerror = () => no(cur.error);
        cur.onsuccess = () => {
          const r = cur.result;
          if (!r || rows.length >= page) return ok();
          rows.push(r.value);
          r.continue();
        };
      });
      sealed.push(...rows.reverse().map((r) => r.body));
    }
    const messages = (await Promise.all(sealed.map((s) => this.open_(s)))).filter((m) => m !== undefined);
    const q = this.queue.transaction('ops', 'readonly');
    const rows = ((await req(q.objectStore('ops').getAll())) as any[]).filter((r) => r.id !== '\u0000key');
    const ops = (await Promise.all(rows.map((r) => this.open_(r.body)))).filter((o) => o !== undefined).sort((a, b) => a.n - b.n);
    return { account, convs, ops, messages };
  }

  /** Messages of a conversation before a seq, for paging back. */
  async page(convId: string, before: number, limit: number) {
    const tx = this.cache.transaction('messages', 'readonly');
    const range = IDBKeyRange.bound([convId, -Infinity], [convId, before], false, true);
    const rows: any[] = [];
    await new Promise<void>((ok, no) => {
      const cur = tx.objectStore('messages').index('seq').openCursor(range, 'prev');
      cur.onerror = () => no(cur.error);
      cur.onsuccess = () => {
        const r = cur.result;
        if (!r || rows.length >= limit) return ok();
        rows.push(r.value);
        r.continue();
      };
    });
    const out = await Promise.all(rows.reverse().map((r) => this.open_(r.body)));
    return out.filter((m) => m !== undefined);
  }

  /** Writes a batch from the core in one transaction (the queue in its own database). */
  async write(b: Batch) {
    try {
      // Encrypt first: a transaction closes when it waits on anything else.
      const ops = await Promise.all(b.put_ops.map(async (o) => ({ id: o.id, body: await this.seal(o) })));
      const msgs = await Promise.all(
        b.put_messages.map(async (m) => ({ conv_id: m.conv_id, msg_id: m.msg_id, seq: m.created_seq, body: await this.seal(m) })),
      );
      if (ops.length || b.remove_ops.length) {
        const q = this.queue.transaction('ops', 'readwrite', { durability: 'strict' } as IDBTransactionOptions);
        const s = q.objectStore('ops');
        for (const o of ops) s.put(o);
        for (const id of b.remove_ops) s.delete(id);
        await done(q);
      }
      if (!b.convs.length && !msgs.length && !b.remove_messages.length && !b.meta) return;
      const tx = this.cache.transaction(['meta', 'convs', 'messages'], 'readwrite');
      if (b.meta) tx.objectStore('meta').put(b.meta.account ?? null, 'account');
      for (const c of b.convs) tx.objectStore('convs').put(c);
      for (const m of msgs) tx.objectStore('messages').put(m);
      for (const [c, k] of b.remove_messages) tx.objectStore('messages').delete([c, k]);
      await done(tx);
    } catch (e) {
      throw e instanceof StoreError ? e : failure(e);
    }
  }

  /** Deletes both databases (logout, another account). */
  async wipe() {
    const tx = this.cache.transaction(['meta', 'convs', 'messages'], 'readwrite');
    for (const s of ['meta', 'convs', 'messages']) tx.objectStore(s).clear();
    await done(tx);
    const q = this.queue.transaction('ops', 'readwrite');
    const s = q.objectStore('ops');
    // Everything but the key.
    s.delete(IDBKeyRange.lowerBound('\u0001'));
    await done(q);
  }
}
