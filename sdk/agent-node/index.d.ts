// Types for the AgentIM agent SDK (docs/en/design/agents.md).

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
  /**
   * `committed`, `streaming`, then `complete` (a finished stream), `cancelled` (stopped; what
   * arrived stays) or `failed`; `recalled`.
   */
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
/** A value shown in a card: formatted for the viewer's locale (docs/en/design/chat-components.md). */
export type CardFormat = 'text' | 'number' | `currency:${string}` | 'date' | 'datetime' | 'percent';
/** The colour of a badge or a fact. */
export type Tone = 'neutral' | 'success' | 'warning' | 'danger' | 'info';
/** A card block (docs/en/design/chat-components.md): shown data, buttons, a form, or your own `x-…` block. */
export type CardBlock =
  | { type: 'text'; text: string }
  | { type: 'facts'; items: { label: string; value: string | number | boolean | null; format?: CardFormat; tone?: Tone }[] }
  | { type: 'table'; caption?: string; columns: { label: string; align?: 'start' | 'center' | 'end'; format?: CardFormat }[]; rows: (string | number | boolean | null)[][] }
  | { type: 'list'; id?: string; select?: boolean; items: { id: string; title: string; subtitle?: string; meta?: string; badge?: { label: string; tone?: Tone } }[] }
  | { type: 'buttons'; id: string; options: { id: string; label: string; style?: 'primary' | 'danger' }[] }
  | { type: 'form'; id: string; submit: string; fields: FormField[] }
  /** Your own block: clients that don't know it show `fallback` ("drop": nothing). */
  | { type: `x-${string}`; id?: string; data?: unknown; fallback: string };
/** A form field (docs/en/design/chat-components.md). */
export interface FormField {
  /** Its key in the answers (a-z 0-9 _ -). */
  id: string;
  /** What people read above it. */
  label: string;
  /** What it takes. */
  type: 'text' | 'textarea' | 'number' | 'select' | 'checkbox' | 'date' | 'time' | 'email' | `x-${string}`;
  /** It must be filled in. */
  required?: boolean;
  /** Shown in it while empty. */
  placeholder?: string;
  /** A short line under it. */
  hint?: string;
  /** Its value at first (must itself be valid). */
  default?: unknown;
  /** select */
  options?: string[];
  /** number, date (YYYY-MM-DD), time (HH:MM); text and textarea: most characters */
  min?: number | string;
  /** number, date, time: the largest; text and textarea: most characters. */
  max?: number | string;
  /** text: a regular expression the whole answer matches, and the message when it doesn't */
  pattern?: string;
  /** The message when `pattern` doesn't match. */
  error?: string;
  /** x-…: your control's settings */
  config?: unknown;
}
/** The steps of one reply (docs/en/design/chat-components.md); each change is a `progress` event. */
export interface Steps {
  /** Step `i` is in progress. */
  start(i: number, detail?: string): Promise<any>;
  /** Step `i` is done. */
  done(i: number, detail?: string): Promise<any>;
  /** Step `i` waits (for a person: say who in `detail`). */
  wait(i: number, detail?: string): Promise<any>;
  /** Step `i` failed (`detail`: why, shown to people). */
  fail(i: number, detail?: string): Promise<any>;
  /** Step `i` is not needed after all. */
  skip(i: number, detail?: string): Promise<any>;
}

/** Options of a message the agent sends. */
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
  /** A W3C traceparent to send it in (replies from a handler use the trigger's trace). */
  trace?: string;
}

/** A reply that streams in: append text, then end it (or cancel). */
export interface Stream {
  /** The id of the reply being streamed. */
  msgId: string;
  /** Adds text to the end of the reply; members watching see it at once. */
  append(text: string): Promise<any>;
  /** Finishes the reply: its state becomes `complete`. */
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
  /**
   * The W3C traceparent of the request that woke the agent (null when the
   * event came by sync). Replies, streams and progress from this context are
   * sent in the same trace; pass it on to your model calls and services.
   */
  trace: string | null;
  /** The trace id part of `trace`, for logs, or null. */
  traceId: string | null;
  /** The agent's configuration now (docs/en/design/agents.md). */
  readonly config: AgentConfig | null;
  /** Its role, or null. */
  readonly role: Role | null;
  /** Replies in the conversation. */
  reply(text: string, opts?: SendOptions): Promise<any>;
  /** Starts a streaming reply. */
  stream(): Promise<Stream>;
  /** Shows what the agent is doing (thinking, a tool call, a step…) above its reply. */
  progress(kind: ProgressKind, text: string, data?: unknown): Promise<any>;
  /** Shows data as a card in reply (docs/en/design/chat-components.md). */
  showCard(title: string, blocks: CardBlock[], opts?: { text?: string }): Promise<any>;
  /** Asks the person who wrote with a form only they can fill in; resolves to the answers (docs/en/design/chat-components.md). */
  askForm(title: string, fields: FormField[], opts?: { intro?: string; submit?: string; to?: string[]; timeoutMs?: number }): Promise<Record<string, unknown>>;
  /** Steps people see next to the reply (docs/en/design/chat-components.md). */
  steps(titles: string[], opts?: { msgId?: string }): Promise<Steps>;
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

/** A role given to the agent (docs/en/design/agents.md): a Markdown file in the app's library. */
export interface Role {
  /** Its name in the library (a-z 0-9 -). */
  name: string;
  /** What the role is for. */
  description: string;
  /** The role's instructions: use them as (part of) your system prompt. */
  instructions: string;
  /** Labels people see and address agents by. */
  tags: string[];
  /** Declared for your runtime; the server does not enforce them. */
  tools: string[];
  /** The model the role asks for (`inherit` and the like are kept as written), or null. */
  model: string | null;
  /** Skills by name (the library comes later; docs/en/design/agents.md K2). */
  skills: string[];
  /** MCP servers by name. */
  mcp: string[];
  /** External APIs by name. */
  apis: string[];
  /** Every other header key, as written (e.g. Claude Code's `color`). */
  metadata: Record<string, unknown>;
  /** Goes up by one each time the role is changed. */
  version: number;
}

/** What the server gives the agent: its role and (later) skills, MCP servers and APIs. */
export interface AgentConfig {
  /** Changes whenever the content does. */
  version: string;
  /** The agent's role, or null when it has none. */
  role: Role | null;
  /** Skills its role names (Agent Skills format; files are fetched by `writeFiles`). */
  skills: Skill[];
  /** Remote MCP servers its role names, with their credential header. */
  mcp: McpServer[];
  /** HTTP APIs its role names, with their OpenAPI document. */
  apis: Api[];
}

/** A skill (docs/en/design/assistants.md). */
export interface Skill {
  /** Its name (lowercase letters, digits, hyphens). */
  name: string;
  /** When to use it (shown to the model). */
  description: string;
  /** Goes up on every change, its files included. */
  version: number;
  /** The instructions (the SKILL.md body). */
  instructions: string;
  /** The SKILL.md as written. */
  markdown: string;
  /** Paths of its reference files. */
  files: string[];
}

/** A remote MCP server (Streamable HTTP). */
export interface McpServer {
  /** The connection's name. */
  name: string;
  /** Its Streamable HTTP endpoint. */
  url: string;
  /** `read_only`, `confirm` (ask a person before each write call) or `allow`. */
  writes: string;
  /** Goes up on every change. */
  version: number;
  /** The credential header to send (only to `url`). */
  headers?: Record<string, string>;
}

/** An HTTP API. */
export interface Api {
  /** The connection's name. */
  name: string;
  /** Its base URL. */
  url: string;
  /** `read_only` (GET and HEAD operations only), `confirm` (ask a person before each write call) or `allow`. */
  writes: string;
  /** Goes up on every change. */
  version: number;
  /** The credential header to send (only to `url`). */
  headers?: Record<string, string>;
  /** Its OpenAPI 3 document. */
  spec: Record<string, unknown>;
}

/** Where `writeFiles` puts things, relative to `dir`. */
export interface FilesOptions {
  /** Default `.`. */
  dir?: string;
  /** Default `.claude/skills`. */
  skillsDir?: string;
  /** Default `.mcp.json`. */
  mcpFile?: string;
  /** Default `.agentim/apis`. */
  apisDir?: string;
}

/**
 * An agent: connects with its key, receives what concerns it in order (sync, then pushes), runs
 * your handlers and acts. Delivery is at least once: a batch is acknowledged after its handlers
 * finish.
 */
export class Agent {
  /**
   * `positionCheckMs`: how often the agent checks it has every message (default 30 s). `files`:
   * keep skills, MCP servers and API documents written to disk (`writeFiles`) whenever the
   * configuration changes (`true` for the defaults).
   */
  constructor(o: { url: string; key: string; name?: string; log?: (m: string) => void; positionCheckMs?: number; files?: FilesOptions | true | null });
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
  /** The configuration from the server (docs/en/design/agents.md), kept current; null before `start()`. */
  config: AgentConfig | null;
  /** Called with the configuration when it arrives at login and whenever it changes. */
  onConfig(fn: (c: AgentConfig) => unknown): this;
  /** Fetches the configuration again (the SDK does it on `config.updated`). */
  refreshConfig(): Promise<AgentConfig | null>;
  /** The MCP servers (name, url, headers), as agent frameworks take them. */
  mcpServers(): { name: string; url: string; headers: Record<string, string> }[];
  /**
   * Writes skills (`.claude/skills/<name>/SKILL.md` and files), MCP servers (`.mcp.json`) and API
   * documents (`.agentim/apis/<name>.openapi.json`) where agent frameworks look; replaces or
   * removes only what it wrote before. Returns the names written.
   */
  writeFiles(o?: FilesOptions): Promise<{ skills: string[]; mcp: string[]; apis: string[] }>;
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
  /** Sends a form card and waits for the answers (docs/en/design/chat-components.md). */
  askForm(convId: string, title: string, fields: FormField[], opts?: { to?: string[]; intro?: string; submit?: string; replyTo?: string; timeoutMs?: number }): Promise<Record<string, unknown>>;
  /** Sends a card (docs/en/design/chat-components.md); `text` is what clients without cards show. */
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
  stream(convId: string, opts?: { replyTo?: string; trace?: string }): Promise<Stream>;
  /**
   * Reports progress in the side channel (`thinking`, `plan`, `step`, `tool_call`, `tool_result`, `error`), optionally tied to a
   * message.
   */
  progress(convId: string, kind: ProgressKind, text: string, opts?: { msgId?: string; data?: unknown; trace?: string }): Promise<any>;
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

/** A traceparent in the same trace with a new span id; undefined for a missing or invalid one. */
export function childTrace(traceparent: string | null | undefined): string | undefined;
