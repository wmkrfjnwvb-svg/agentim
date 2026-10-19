// In-memory store: tests, one-off programs, server-side rendering. Same
// rules as the persistent stores (the shared store tests check it).

import { byOrder, jsonCodec, type ConvRow, type OutboxItem, type PageQuery, type Store, type WriteBatch } from './types.js';
import type { ChatMessage } from '../types.js';

interface Rec {
  m: ChatMessage;
  bytes: number;
}

export class MemoryStore implements Store {
  private account: string | undefined;
  private convRows = new Map<string, ConvRow>();
  private msgs = new Map<string, Map<string, Rec>>();
  private queue = new Map<string, OutboxItem>();

  async open(account: string) {
    if (this.account !== undefined && this.account !== account) {
      await this.clear();
      this.account = account;
      return { reset: true };
    }
    this.account = account;
    return { reset: false };
  }

  async loadConv(convId: string) {
    const c = this.convRows.get(convId);
    return c && structuredClone(c);
  }

  async convs() {
    return [...this.convRows.values()].map((c) => structuredClone(c));
  }

  async messages(convId: string, q: PageQuery = {}) {
    const all = [...(this.msgs.get(convId)?.values() ?? [])].map((r) => r.m);
    const sent = all.filter((m) => m.status === 'sent' && (q.beforeSeq === undefined || m.created_seq < q.beforeSeq)).sort(byOrder);
    const page = sent.slice(Math.max(0, sent.length - (q.limit ?? 50)));
    const unsent = q.beforeSeq === undefined ? all.filter((m) => m.status !== 'sent') : [];
    return [...page, ...unsent].sort(byOrder).map((m) => structuredClone(m));
  }

  async write(b: WriteBatch) {
    for (const c of b.convs ?? []) {
      const old = this.convRows.get(c.convId);
      this.convRows.set(c.convId, {
        convId: c.convId,
        lastSeq: Math.max(old?.lastSeq ?? 0, c.lastSeq),
        openedAt: Math.max(old?.openedAt ?? 0, c.openedAt),
        info: c.info !== undefined ? structuredClone(c.info) : old?.info,
      });
    }
    for (const r of b.remove ?? []) this.msgs.get(r.convId)?.delete(r.key);
    for (const m of b.put ?? []) {
      let c = this.msgs.get(m.conv_id);
      if (!c) this.msgs.set(m.conv_id, (c = new Map()));
      c.set(m.msg_id, { m: structuredClone(m), bytes: (await jsonCodec.encode(m, '')).length });
    }
    for (const id of b.outboxRemove ?? []) this.queue.delete(id);
    for (const x of b.outboxPut ?? []) this.queue.set(x.id, structuredClone(x));
  }

  async outbox() {
    return [...this.queue.values()].sort((a, b) => a.createdAt - b.createdAt || (a.id < b.id ? -1 : 1)).map((x) => structuredClone(x));
  }

  async size() {
    let n = 0;
    for (const c of this.msgs.values()) for (const r of c.values()) n += r.bytes;
    return n;
  }

  async evict(bytes: number, protect = new Set<string>()) {
    let size = await this.size();
    const queued = new Set([...this.queue.values()].map((x) => x.convId));
    const gone: string[] = [];
    const order = [...this.convRows.values()].sort((a, b) => a.openedAt - b.openedAt || (a.convId < b.convId ? -1 : 1));
    for (const c of order) {
      if (size <= bytes) break;
      const m = this.msgs.get(c.convId);
      if (!m || protect.has(c.convId) || queued.has(c.convId)) continue;
      let freed = 0;
      for (const [k, r] of m) {
        if (r.m.status === 'sent') {
          freed += r.bytes;
          m.delete(k);
        }
      }
      if (freed) {
        size -= freed;
        gone.push(c.convId);
      }
    }
    return gone;
  }

  async clear() {
    this.convRows.clear();
    this.msgs.clear();
    this.queue.clear();
    this.account = undefined;
  }

  async close() {}
}
