// Local cache (docs/05-operations.md §6.2). A cache only: anything lost is
// synced again from the server, except unsent messages, which are kept
// with the rest of a conversation's record.

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
  save(rec: ConvRecord): Promise<void>;
  list(): Promise<string[]>;
  clear(): Promise<void>;
}

export class MemoryStore implements Store {
  private m = new Map<string, ConvRecord>();
  async load(convId: string) {
    const r = this.m.get(convId);
    return r && structuredClone(r);
  }
  async save(rec: ConvRecord) {
    this.m.set(rec.convId, structuredClone(rec));
  }
  async list() {
    return [...this.m.keys()];
  }
  async clear() {
    this.m.clear();
  }
}

/** Browser cache in IndexedDB, one database per (server, user). */
export class IndexedDBStore implements Store {
  private db: Promise<IDBDatabase>;
  constructor(name: string) {
    this.db = new Promise((resolve, reject) => {
      const req = indexedDB.open(`agentim:${name}`, 1);
      req.onupgradeneeded = () => req.result.createObjectStore('convs', { keyPath: 'convId' });
      req.onsuccess = () => resolve(req.result);
      req.onerror = () => reject(req.error);
    });
  }
  private async tx<T>(mode: IDBTransactionMode, f: (s: IDBObjectStore) => IDBRequest<T>): Promise<T> {
    const db = await this.db;
    return new Promise((resolve, reject) => {
      const r = f(db.transaction('convs', mode).objectStore('convs'));
      r.onsuccess = () => resolve(r.result);
      r.onerror = () => reject(r.error);
    });
  }
  load(convId: string) {
    return this.tx('readonly', (s) => s.get(convId) as IDBRequest<ConvRecord | undefined>);
  }
  async save(rec: ConvRecord) {
    await this.tx('readwrite', (s) => s.put(rec));
  }
  async list() {
    return (await this.tx('readonly', (s) => s.getAllKeys())) as string[];
  }
  async clear() {
    await this.tx('readwrite', (s) => s.clear());
  }
}
