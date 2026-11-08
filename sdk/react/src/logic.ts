// What the cards, steps and inbox components need without the DOM
// (docs/en/design/chat-components.md): answers to a card, checking a field, steps from progress
// events, the inbox. React Native's components use these too.

import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from 'react';
import type { Message } from '@agentimhq/client-web/view';
import { useClient } from './hooks.js';
import type { Strings } from './strings.js';
import { foldStep, type Step } from './format.js';

/** One answer to a card (from `card_answers` or a live `card.response`). */
export interface CardAnswer {
  block_id: string;
  by: string;
  by_name?: string | null;
  action?: string | null;
  /** Only for the one who answered and the card's sender. */
  values?: Record<string, unknown>;
  at?: number;
  seq?: number;
}

export const interactiveBlock = (b: any) =>
  b?.type === 'buttons' || b?.type === 'form' || (b?.type === 'list' && b.select) || (typeof b?.type === 'string' && b.type.startsWith('x-') && b.id);

/** The answers to a card message, kept current. */
export function useCardAnswers(m: Message, enabled: boolean, me?: string, mine?: boolean): [CardAnswer[], (a: CardAnswer) => void] {
  const client = useClient() as any;
  const [answers, setAnswers] = useState<CardAnswer[]>([]);
  const add = (a: CardAnswer) =>
    setAnswers((list) => (list.some((x) => (a.seq !== undefined && x.seq === a.seq) || (x.by === a.by && x.block_id === a.block_id && x.seq === undefined)) ? list.map((x) => (x.by === a.by && x.block_id === a.block_id && x.seq === undefined ? { ...x, ...a } : x)) : [...list, a]));
  useEffect(() => {
    if (!enabled || !m.msg_id || m.status === 'sending') return;
    let on = true;
    Promise.resolve(client.call?.('card_answers', { msg_id: m.msg_id }))
      .then((r: any) => {
        if (on && Array.isArray(r?.answers)) for (const a of r.answers) add(a);
      })
      .catch(() => {});
    const off = client.on?.('event', (e: any) => {
      const ev = e?.event ?? e;
      if (ev?.type !== 'card.response' || ev.data?.msg_id !== m.msg_id) return;
      const by = ev.data.by;
      add({ block_id: ev.data.block_id, by, action: ev.data.action, values: by === me || mine ? ev.data.values : undefined, at: ev.created_at, seq: ev.seq });
    });
    return () => {
      on = false;
      off?.();
    };
  }, [client, m.msg_id, enabled, m.status]);
  return [answers, add];
}

/** Whether a value is a valid answer to a field (as the server checks it). */
export function fieldError(f: any, v: unknown, strings: Strings): string | undefined {
  const empty = v === undefined || v === null || v === '';
  if (empty) return f.required && f.type !== 'checkbox' ? strings.fieldRequired : undefined;
  const bad = f.error ?? strings.fieldInvalid;
  switch (f.type) {
    case 'text':
    case 'textarea':
      if (typeof v !== 'string' || (f.max && v.length > f.max)) return bad;
      if (f.type === 'text' && f.pattern) {
        try {
          if (!new RegExp(`^(?:${f.pattern})$`, 'u').test(v)) return bad;
        } catch {
          // A pattern the browser can't read: the server checks it.
        }
      }
      return undefined;
    case 'number':
      if (typeof v !== 'number' || Number.isNaN(v) || (typeof f.min === 'number' && v < f.min) || (typeof f.max === 'number' && v > f.max)) return bad;
      return undefined;
    case 'email':
      return typeof v === 'string' && /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(v) ? undefined : bad;
    case 'date':
    case 'time':
      if (typeof v !== 'string' || (f.min && v < f.min) || (f.max && v > f.max)) return bad;
      return undefined;
    default:
      return undefined;
  }
}

type Key = string; // `${conv}\u0000${msgId or ''}`

/** One store per client: every list on the page reads from it. */
class StepStore {
  steps = new Map<Key, Step[]>();
  subs = new Set<() => void>();
  off?: () => void;
  constructor(client: any) {
    this.off = client.on?.('side', (e: any) => {
      const ev = e?.event ?? e;
      if (ev?.type !== 'progress' || !ev.conv_id) return;
      const d = ev.data ?? {};
      const key = `${ev.conv_id}\u0000${d.msg_id ?? ''}`;
      const before = this.steps.get(key) ?? [];
      const after = foldStep(before, { kind: d.kind, text: d.text, data: d.data, at: ev.created_at });
      if (after === before) return;
      this.steps.set(key, after);
      for (const f of this.subs) f();
    });
  }
  get(conv: string, msgId?: string): Step[] {
    return this.steps.get(`${conv}\u0000${msgId ?? ''}`) ?? EMPTY;
  }
}
const EMPTY: Step[] = [];
const stores = new WeakMap<object, StepStore>();
function storeOf(client: any): StepStore {
  let s = stores.get(client);
  if (!s) {
    s = new StepStore(client);
    stores.set(client, s);
  }
  return s;
}

/** The steps of one reply (`msgId`), or the conversation's unattached ones. */
export function useSteps(convId: string, msgId?: string): Step[] {
  const store = storeOf(useClient());
  return useSyncExternalStore(
    (f) => {
      store.subs.add(f);
      return () => store.subs.delete(f);
    },
    () => store.get(convId, msgId),
    () => store.get(convId, msgId),
  );
}

/** An inbox item as the server gives it (`type`: request, draft, handoff). */
export type InboxItem = Record<string, any> & { type: string; conv_id: string; created_at: number };

/** What the tabs group by: approvals, questions, drafts, hand-offs, or your own. */
export function inboxKind(i: InboxItem): string {
  if (i.type === 'request') return i.kind === 'approve' ? 'request' : 'question';
  return i.type;
}

export function inboxId(i: InboxItem): string {
  return i.request_id ?? i.draft_id ?? i.id ?? `${i.type}:${i.conv_id}:${i.agent_id ?? ''}`;
}

/** What happened to an item handled here (shown until the next refresh). */
export interface InboxOutcome {
  state: string;
  by?: string;
  at: number;
}

const EVENTS = /^(request\.|draft\.)/;

/**
 * The inbox, kept current: it reloads (debounced) when a request, a draft
 * or a hand-off changes, and every 30 seconds.
 */
export function useInbox({ limit = 100 }: { limit?: number } = {}) {
  const client = useClient() as any;
  const [items, setItems] = useState<InboxItem[]>([]);
  const [more, setMore] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string>();
  const [outcomes, setOutcomes] = useState<Record<string, InboxOutcome>>({});
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  const refresh = useCallback(async () => {
    try {
      const r = await client.call('inbox', { limit });
      setItems(r?.items ?? []);
      setMore(!!r?.more);
      setError(undefined);
    } catch (e: any) {
      setError(e?.message ?? String(e));
    } finally {
      setLoading(false);
    }
  }, [client, limit]);
  useEffect(() => {
    refresh();
    const soon = () => {
      clearTimeout(timer.current);
      timer.current = setTimeout(refresh, 250);
    };
    const offSide = client.on?.('side', (e: any) => {
      const ev = e?.event ?? e;
      if (EVENTS.test(ev?.type ?? '')) {
        // Decided elsewhere: say so on the item until it goes.
        const id = ev.data?.request_id ?? ev.data?.draft_id;
        // Only a decision ends an item (not `draft.created`, `request.created` …).
        const DONE: Record<string, string> = {
          'request.decided': ev.data?.state ?? 'decided',
          'request.expired': 'expired',
          'request.cancelled': 'cancelled',
          'draft.approved': 'approved',
          'draft.decided': ev.data?.state ?? 'approved',
          'draft.rejected': 'rejected',
          'draft.discarded': 'rejected',
          'draft.expired': 'expired',
        };
        const state = DONE[ev.type];
        if (id && state) setOutcomes((o) => (o[id] ? o : { ...o, [id]: { state, by: ev.data?.decided_by ?? ev.data?.by, at: Date.now() } }));
        soon();
      }
    });
    const offEvent = client.on?.('event', (e: any) => {
      const ev = e?.event ?? e;
      if (ev?.type === 'agent.state' || EVENTS.test(ev?.type ?? '')) soon();
    });
    const t = setInterval(refresh, 30_000);
    return () => {
      offSide?.();
      offEvent?.();
      clearInterval(t);
      clearTimeout(timer.current);
    };
  }, [client, refresh]);
  const act = useCallback(
    async (id: string, op: string, args: Record<string, unknown>, state: string) => {
      await client.call(op, args);
      setOutcomes((o) => ({ ...o, [id]: { state, by: 'me', at: Date.now() } }));
      refresh();
    },
    [client, refresh],
  );
  return {
    items,
    more,
    loading,
    error,
    outcomes,
    refresh,
    /** Approve or decline a request (`text`: a comment or the reason). */
    decide: (i: InboxItem, decision: 'approve' | 'reject', text?: string) =>
      act(inboxId(i), 'decide', { request_id: i.request_id, decision, ...(text ? { text } : {}) }, decision === 'approve' ? 'approved' : 'rejected'),
    /** Answer a question: free text, or one of its options. */
    answer: (i: InboxItem, r: { text?: string; choice?: string }) =>
      act(inboxId(i), 'decide', { request_id: i.request_id, decision: i.kind === 'choose' ? 'choose' : 'answer', ...r }, i.kind === 'choose' ? 'chosen' : 'answered'),
    /** Send an agent's draft (edited: `text`). */
    approveDraft: (i: InboxItem, text?: string) => act(inboxId(i), 'approve', { draft_id: i.draft_id, ...(text && text !== i.content ? { text } : {}) }, 'approved'),
    discardDraft: (i: InboxItem) => act(inboxId(i), 'reject', { draft_id: i.draft_id }, 'rejected'),
    /** Give a handed-over conversation back to its agent. */
    handBack: (i: InboxItem, note?: string) => act(inboxId(i), 'handback', { conv_id: i.conv_id, agent_id: i.agent_id, ...(note ? { note } : {}) }, 'handed_back'),
  };
}

export type Inbox = ReturnType<typeof useInbox>;

