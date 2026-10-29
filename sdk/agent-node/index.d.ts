// Types for the AgentIM agent SDK (docs/03-agents.md §2.4).

/** A request the server refused; `code` is the protocol's error code. */
export class AgentIMError extends Error {
  /** The protocol's error code: forbidden, not_found, conflict, bad_request, rate_limited, taken_over, ... */
  code: string;
  /** The server's explanation. */
  detail: string;
}

/** A message as an agent receives it. */
export interface Msg {
  /** The message id. */
  msg_id: string;
  /** The conversation it is in. */
  conv_id: string;
  /** Who sent it. */
  sender_id: string;
  /** The sender's display name. */
  sender_name: string;
  /** "human" or "agent" — set by the server, never by the sender. */
  sender_type: 'human' | 'agent' | 'system';
  /** `text`, `image`, `file`, `card` or a custom kind. */
  kind: string;
  /** The text (for other kinds, their plain-text fallback). */
  content: string;
  /** The structured content of a non-text kind. */
  payload?: unknown;
  /** The routing envelope: who it asks to act (`to`), its topics, intent, references and sources. */
  envelope?: { to?: string[]; topics?: string[]; intent?: string; refs?: string[]; sources?: Source[] };
  /** The message it replies to. */
  reply_to?: string;
  /** `committed`, `streaming`, `cancelled` (a stopped stream; what arrived stays), `failed` or `recalled`. */
  state: string;
}

/** One conversation event: a message, an edit, a member change, a decision… */
export interface Event {
  /** The conversation. */
  conv_id: string;
  /** Its position in the conversation (increasing, no gaps). */
  seq: number;
  /** What happened: `msg.created`, `msg.updated`, `msg.recalled`, `members.joined`… */
  type: string;
  /** Who did it. */
  actor_id?: string;
  /** The event's details. */
  data: any;
  /** The message, for message events. */
  msg?: Msg;
  /** When it happened (Unix ms). */
  created_at: number;
}

/** A source a reply is based on: shown as a citation under it. */
export interface Source { title: string; url: string; excerpt?: string }

/** Options for a message: a reply, the routing envelope, sources, an id for retries. */
export interface SendOptions {
  /** The message this replies to. */
  replyTo?: string;
  /** Members this asks to act; others see it but need not answer. */
  to?: string[];
  /** Topics the message is about, for members that follow only some topics. */
  topics?: string[];
  /** What the message is: `request`, `proposal`, `answer`, `status` or `done`. */
  intent?: 'request' | 'proposal' | 'answer' | 'status' | 'done';
  /** Ids of the messages it refers to. */
  refs?: string[];
  /** Sources the message is based on, shown as citations. */
  sources?: Source[];
  /** Your id for the message: sending again with the same id never doubles it. */
  clientMsgId?: string;
}

/** A reply that streams in: append text, then end it (or cancel). */
export interface Stream {
  /** The id of the reply being streamed. */
  msgId: string;
  /** Adds text to the end of the reply; members watching see it at once. */
  append(text: string): Promise<any>;
  /** Finishes the reply: it becomes a normal message. */
  end(): Promise<any>;
  /** Stops the reply; what arrived stays, marked `cancelled`. */
  cancel(): Promise<any>;
}

/**
 * What a handler gets for one message: the event, the message and ways to answer in the same
 * conversation.
 */
export interface Ctx {
  /** The conversation. */
  convId: string;
  /** The event that triggered the handler. */
  event: Event;
  /** The message, or null for other events. */
  msg: Msg | null;
  /** Replies in the conversation. */
  reply(text: string, opts?: SendOptions): Promise<any>;
  /** Starts a streaming reply. */
  stream(): Promise<Stream>;
  /** Shows what the agent is doing (thinking, a tool call, a step…) above its reply. */
  progress(kind: ProgressKind, text: string, data?: unknown): Promise<any>;
}

/** What a progress line shows. */
export type ProgressKind = 'thinking' | 'tool_call' | 'tool_result' | 'plan' | 'step' | 'error';

/** How a person answered a request: `state`, the `response` and who decided. */
export interface Decision { state: 'approved' | 'rejected' | 'answered' | 'chosen' | 'expired' | 'cancelled' | 'timeout' | string; response?: any; decided_by?: string }

/**
 * One WebSocket connection with an agent key: requests by id, pushes to a callback. `Agent`
 * manages one for you (reconnecting); use this directly only for scripts and tests.
 */
export class Connection {
  constructor(url: string, token: string, onPush?: (frame: any) => void);
  /** Opens the connection and waits for the welcome frame (who the agent is, its limits). */
  open(): Promise<any>;
  /**
   * Sends a request (`op` and its arguments) and waits for its answer; rejects with `AgentIMError`
   * when refused.
   */
  call(op: string, args?: Record<string, unknown>): Promise<any>;
  /** Closes the connection. */
  close(): void;
}

/**
 * An agent: connects with its key, receives what concerns it in order (sync, then pushes), runs
 * your handlers and acts. Delivery is at least once: a batch is acknowledged after its handlers
 * finish.
 */
export class Agent {
  /** `positionCheckMs`: how often the agent checks it has every message (default 30 s). */
  constructor(o: { url: string; key: string; name?: string; log?: (m: string) => void; positionCheckMs?: number });
  /** The agent itself, after `start()`. */
  me: { id: string; name: string; kind: string; owner_id?: string } | null;
  /** Messages that ask this agent to act (named, replied to, or by its trigger). */
  onMessage(fn: (msg: Msg, ctx: Ctx, item: any) => unknown): this;
  /** Every event delivered to the agent, woken or not. */
  onEvent(fn: (ev: Event, item: any) => unknown): this;
  /** Called when a person answers one of the agent's cards (a button or a filled form). */
  onCardResponse(fn: (r: { msg_id: string; block_id: string; action?: string; values?: Record<string, unknown>; by: string }, ctx: Ctx) => unknown): this;
  /** Called when the person a conversation was handed off to hands it back, with their note. */
  onHandback(fn: (d: { by: string; note?: string }, ctx: Ctx) => unknown): this;
  /**
   * Connects (retrying the first connection for up to 15 seconds), syncs what it missed and starts
   * running the handlers.
   */
  start(): Promise<this>;
  /** Stops: closes the connection; no handler runs after this. */
  stop(): Promise<void>;
  /**
   * Any protocol request by name (see the client protocol reference); the typed methods cover the
   * common ones.
   */
  call(op: string, args?: Record<string, unknown>): Promise<any>;
  /**
   * Sends a message, with an optional envelope (`to`, `topics`, `intent`, `refs`) and `sources`.
   * Under approval it becomes a draft (`draft_id`).
   */
  send(convId: string, text: string, opts?: SendOptions): Promise<{ msg_id?: string; seq?: number; draft_id?: string; wake?: string[] }>;
  /** Sends a card (buttons, forms) with the text clients without cards show. */
  sendCard(convId: string, text: string, card: unknown, opts?: { replyTo?: string; clientMsgId?: string }): Promise<any>;
  /** Uploads a whole file (at most 64 MiB); EXIF and GPS removed, thumbnail made by the server. */
  uploadFile(convId: string, name: string, data: Uint8Array | ArrayBuffer | Blob, opts?: { mime?: string }): Promise<AgentFile>;
  /** Uploads a file and sends it as an image or file message. */
  sendFile(convId: string, name: string, data: Uint8Array | ArrayBuffer | Blob, opts?: { mime?: string; caption?: string; replyTo?: string; clientMsgId?: string }): Promise<{ seq: number; msg_id: string; file: AgentFile }>;
  /** An absolute link (valid 5 minutes) to read a file of a message. */
  fileUrl(convId: string, fileId: string, opts?: { variant?: 'original' | 'thumb' }): Promise<{ url: string; variant: string; expires_at: number }>;
  /**
   * Starts a streamed reply; append text, then end it. People see it as it is written and can stop
   * it.
   */
  stream(convId: string, opts?: { replyTo?: string }): Promise<Stream>;
  /**
   * Reports progress in the side channel (`thinking`, `plan`, `step`, `tool_call`, `tool_result`, `error`), optionally tied to a
   * message.
   */
  progress(convId: string, kind: ProgressKind, text: string, opts?: { msgId?: string; data?: unknown }): Promise<any>;
  /** Asks a person to approve, answer or choose, and waits for the decision (or the timeout). */
  askHuman(convId: string, o: { kind?: 'approve' | 'answer' | 'choose'; title: string; context?: unknown; options?: string[]; assignees?: string[]; ttlSecs?: number; timeoutMs?: number }): Promise<Decision>;
  /**
   * Hands the conversation to a person (its owner, or `to`) with a note; the agent stays quiet
   * until it is handed back.
   */
  handoff(convId: string, note: string, to?: string): Promise<any>;
  /**
   * Reads the conversation's events the agent may see, by seq (`afterSeq` or `beforeSeq`, up to
   * `limit`).
   */
  history(convId: string, opts?: { afterSeq?: number; beforeSeq?: number; limit?: number }): Promise<any>;
  /** The conversation for the model: stored summaries and recent messages within `maxTokens`. */
  context(convId: string, maxTokens?: number): Promise<any>;
  /** Searches the messages the agent may see, in one conversation or all. */
  search(query: string, opts?: { convId?: string; limit?: number }): Promise<any>;
  /** Stores a summary of a range of seqs, used by `context` in place of those messages. */
  putSummary(convId: string, fromSeq: number, toSeq: number, text: string): Promise<any>;
  /** Reads a checkpoint; `null` when there is none. */
  getState(key: string): Promise<{ key: string; value: any; version: number } | null>;
  /**
   * Writes a checkpoint at the version read (0 creates); a concurrent write makes it fail with
   * `conflict`.
   */
  putState(key: string, value: unknown, version: number): Promise<{ key: string; version: number }>;
  /**
   * Claims a request (`intent: request`) for this agent; the first claim wins, the others get
   * `conflict`.
   */
  claim(msgId: string): Promise<any>;
  /** Moves a task channel to a state (`submitted`, `working`, `input_required`, `completed`, `failed`, `canceled`) with a note. */
  updateTask(convId: string, state: string, note?: string): Promise<any>;
}

/** An uploaded file's record. */
export interface AgentFile {
  /** The file id; send it in a message or ask for its URL. */
  file_id: string;
  /** `image` (shown inline, has a size) or `file`. */
  kind: 'image' | 'file';
  /** The file name as uploaded. */
  name: string;
  /** The content type. */
  mime: string;
  /** The size in bytes. */
  size: number;
  /** An image's width in pixels, upright (null for other files). */
  width?: number | null;
  /** An image's height in pixels, upright (null for other files). */
  height?: number | null;
  /** Whether it has a thumbnail. */
  thumb: boolean;
  /** Whether it is marked as made by AI (EU AI Act Art. 50); agents' uploads always are. */
  ai_generated: boolean;
}
