// The local store (docs/12 §2.1 step A): what every backend (SQLite,
// IndexedDB, memory) keeps and guarantees.
//
// - One store per account; a store opened for another user wipes itself
//   (no data of one user is ever shown to another).
// - A cache: anything but the offline queue can be rebuilt from the server.
//   A store that fails to open or migrate is wiped and recreated.
// - Positions only move forward, also when two tabs write at once.
// - Content is encoded by a codec (JSON, or encrypted); ids, seqs and
//   times stay readable so the store can query and evict.

import type { ChatMessage } from '../types.js';

/** Schema of the stored tables; bump with a migration in each backend. */
export const SCHEMA_VERSION = 2;

/** What is kept per conversation besides its messages. */
export interface ConvRow {
  convId: string;
  /** Highest seq applied and acknowledged (never moves back in a write). */
  lastSeq: number;
  /** Last time the app opened it (eviction order). */
  openedAt: number;
  /** List data (title, members, unread, last message), as last known. */
  info?: unknown;
}

/** An operation waiting to reach the server (docs/12 §2.1 step B). */
export interface OutboxItem {
  /** Idempotency key: client_msg_id for sends, an operation id otherwise. */
  id: string;
  convId: string;
  op: string;
  args: Record<string, unknown>;
  createdAt: number;
  attempts: number;
  state: 'pending' | 'failed';
  error?: { code: string; message: string };
}

export interface WriteBatch {
  convs?: ConvRow[];
  /** Insert or replace, keyed by (conv_id, msg_id). */
  put?: ChatMessage[];
  /** (convId, msg_id) of messages to delete. */
  remove?: { convId: string; key: string }[];
  outboxPut?: OutboxItem[];
  outboxRemove?: string[];
}

export interface PageQuery {
  /** Only messages with created_seq below this (older pages). */
  beforeSeq?: number;
  /** At most this many sent messages (default 50). Unsent ones come with the newest page. */
  limit?: number;
}

export interface Store {
  /**
   * Opens (creates, migrates) for this account. A store that belonged to
   * another account, has an unknown schema or fails to open is wiped and
   * recreated; returns whether that happened.
   */
  open(account: string): Promise<{ reset: boolean }>;
  loadConv(convId: string): Promise<ConvRow | undefined>;
  convs(): Promise<ConvRow[]>;
  /** The newest page (or the page before `beforeSeq`), oldest first. */
  messages(convId: string, q?: PageQuery): Promise<ChatMessage[]>;
  /** Everything in one transaction. */
  write(b: WriteBatch): Promise<void>;
  outbox(): Promise<OutboxItem[]>;
  /** Bytes of stored message content (what the size limit counts). */
  size(): Promise<number>;
  /** Drops the messages of the conversation opened longest ago until under `bytes`; protected ones and those with queued operations are kept. Returns the conversations emptied. */
  evict(bytes: number, protect?: Set<string>): Promise<string[]>;
  /** Everything (logout). */
  clear(): Promise<void>;
  close(): Promise<void>;
}

/** Turns stored content into text and back (JSON, or encrypted). */
export interface Codec {
  encode(value: unknown, aad: string): Promise<string>;
  decode(text: string, aad: string): Promise<unknown>;
}

export const jsonCodec: Codec = {
  async encode(v) {
    return JSON.stringify(v);
  },
  async decode(t) {
    return JSON.parse(t);
  },
};

/** Sent messages by seq, unsent ones after them by time. */
export function byOrder(a: ChatMessage, b: ChatMessage) {
  const sa = a.status === 'sent' ? a.created_seq : Number.POSITIVE_INFINITY;
  const sb = b.status === 'sent' ? b.created_seq : Number.POSITIVE_INFINITY;
  return sa - sb || a.created_at - b.created_at;
}

/** Storage failure (disk full, quota, corrupted) — retryable once space is freed. */
export class StorageError extends Error {
  readonly code = 'storage';
}
