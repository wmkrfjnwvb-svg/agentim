// Protocol types (docs/05-operations.md §7.1). Unknown fields and event
// types pass through untouched: a newer server never breaks an older SDK.

export type SenderType = 'human' | 'agent' | 'system';

export interface Msg {
  msg_id: string;
  client_msg_id?: string | null;
  conv_id: string;
  created_seq: number;
  sender_id: string;
  sender_name: string;
  /** Shown as an AI label wherever the message is shown (EU AI Act art. 50). */
  sender_type: SenderType;
  kind: string;
  content: string;
  payload?: unknown;
  envelope?: Record<string, unknown> | null;
  version: number;
  /** `committed`; streamed: `streaming`, then `complete`, `failed` or `cancelled`; `recalled`, ... */
  state: string;
  state_reason?: string | null;
  reply_to?: string | null;
  mentions?: string[];
  created_at: number;
  updated_at: number;
  [k: string]: unknown;
}

export interface Event {
  conv_id: string;
  seq: number;
  type: string;
  actor_id?: string | null;
  msg?: Msg | null;
  data?: unknown;
  created_at: number;
  [k: string]: unknown;
}

export interface SideEvent {
  conv_id: string;
  side_seq: number;
  type: string;
  data?: unknown;
  [k: string]: unknown;
}

/** A message as the app shows it: server state plus local sending state. */
export interface ChatMessage extends Msg {
  /** `sending` and `failed` exist only locally; `sent` once the server has it. */
  status: 'sending' | 'sent' | 'failed';
  /** Text streamed so far while `state` is `streaming`. */
  streamed?: string;
  error?: { code: string; message: string };
}

export interface Me {
  id: string;
  name: string;
  kind?: string;
  [k: string]: unknown;
}

export type ConnectionState = 'connecting' | 'connected' | 'reconnecting' | 'closed';

export class AgentIMError extends Error {
  readonly code: string;
  readonly detail: string;
  constructor(code: string, message: string) {
    super(`${code}: ${message}`);
    this.code = code;
    this.detail = message;
  }
}

/** Errors worth sending again with the same client_msg_id. */
export const RETRYABLE = new Set(['unavailable', 'overloaded', 'rate_limited', 'disconnected', 'timeout']);
