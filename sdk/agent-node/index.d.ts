// Types for the AgentIM agent SDK (docs/10-agent-ecosystem.md §5.2).

export class AgentIMError extends Error {
  /** The protocol's error code: forbidden, not_found, conflict, bad_request, rate_limited, taken_over, ... */
  code: string;
  detail: string;
}

export interface Msg {
  msg_id: string;
  conv_id: string;
  sender_id: string;
  sender_name: string;
  /** "human" or "agent" — set by the server, never by the sender. */
  sender_type: 'human' | 'agent' | 'system';
  kind: string;
  content: string;
  payload?: unknown;
  envelope?: { to?: string[]; topics?: string[]; intent?: string; refs?: string[]; sources?: Source[] };
  reply_to?: string;
  state: string;
}

export interface Event {
  conv_id: string;
  seq: number;
  type: string;
  actor_id?: string;
  data: any;
  msg?: Msg;
  created_at: number;
}

export interface Source { title: string; url: string; excerpt?: string }

export interface SendOptions {
  replyTo?: string;
  to?: string[];
  topics?: string[];
  intent?: 'request' | 'proposal' | 'answer' | 'status' | 'done';
  refs?: string[];
  sources?: Source[];
  clientMsgId?: string;
}

export interface Stream {
  msgId: string;
  append(text: string): Promise<any>;
  end(): Promise<any>;
  cancel(): Promise<any>;
}

export interface Ctx {
  convId: string;
  event: Event;
  msg: Msg | null;
  reply(text: string, opts?: SendOptions): Promise<any>;
  stream(): Promise<Stream>;
  progress(kind: ProgressKind, text: string, data?: unknown): Promise<any>;
}

export type ProgressKind = 'thinking' | 'tool_call' | 'tool_result' | 'plan' | 'step' | 'error';

export interface Decision { state: 'approved' | 'rejected' | 'answered' | 'chosen' | 'expired' | 'cancelled' | 'timeout' | string; response?: any; decided_by?: string }

export class Connection {
  constructor(url: string, token: string, onPush?: (frame: any) => void);
  open(): Promise<any>;
  call(op: string, args?: Record<string, unknown>): Promise<any>;
  close(): void;
}

export class Agent {
  constructor(o: { url: string; key: string; name?: string; log?: (m: string) => void });
  /** The agent itself, after `start()`. */
  me: { id: string; name: string; kind: string; owner_id?: string } | null;
  /** Messages that ask this agent to act (named, replied to, or by its trigger). */
  onMessage(fn: (msg: Msg, ctx: Ctx, item: any) => unknown): this;
  /** Every event delivered to the agent, woken or not. */
  onEvent(fn: (ev: Event, item: any) => unknown): this;
  onCardResponse(fn: (r: { msg_id: string; block_id: string; action?: string; values?: Record<string, unknown>; by: string }, ctx: Ctx) => unknown): this;
  onHandback(fn: (d: { by: string; note?: string }, ctx: Ctx) => unknown): this;
  start(): Promise<this>;
  stop(): Promise<void>;
  call(op: string, args?: Record<string, unknown>): Promise<any>;
  send(convId: string, text: string, opts?: SendOptions): Promise<{ msg_id?: string; seq?: number; draft_id?: string; wake?: string[] }>;
  sendCard(convId: string, text: string, card: unknown, opts?: { replyTo?: string; clientMsgId?: string }): Promise<any>;
  stream(convId: string, opts?: { replyTo?: string }): Promise<Stream>;
  progress(convId: string, kind: ProgressKind, text: string, opts?: { msgId?: string; data?: unknown }): Promise<any>;
  askHuman(convId: string, o: { kind?: 'approve' | 'answer' | 'choose'; title: string; context?: unknown; options?: string[]; assignees?: string[]; ttlSecs?: number; timeoutMs?: number }): Promise<Decision>;
  handoff(convId: string, note: string, to?: string): Promise<any>;
  history(convId: string, opts?: { afterSeq?: number; beforeSeq?: number; limit?: number }): Promise<any>;
  context(convId: string, maxTokens?: number): Promise<any>;
  search(query: string, opts?: { convId?: string; limit?: number }): Promise<any>;
  putSummary(convId: string, fromSeq: number, toSeq: number, text: string): Promise<any>;
  getState(key: string): Promise<{ key: string; value: any; version: number } | null>;
  putState(key: string, value: unknown, version: number): Promise<{ key: string; version: number }>;
  claim(msgId: string): Promise<any>;
  updateTask(convId: string, state: string, note?: string): Promise<any>;
}
