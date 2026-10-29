/**
 * An error answered by the server: `code` is the error code (`forbidden`, `not_found`, `conflict`,
 * `bad_request`, `rate_limited`, …), `status` the HTTP status.
 */
export class AgentIMError extends Error {
  /** The HTTP status of the answer. */
  status: number;
  /** Same codes as the WebSocket protocol: forbidden, not_found, conflict, blocked, ... */
  code: string;
  /** The server's explanation (the problem document's `detail`). */
  detail: string;
}

/** A user id, or "system". */
export type As = string;

/** A message to send through the open API. */
export interface Message {
  /** Who sends it: a user id (a member) or `system`. */
  as: As;
  /** The text (for a custom kind, its plain-text fallback). */
  text: string;
  /** A custom kind (default `text`). */
  kind?: string;
  /** The structured content of a custom kind. */
  payload?: Record<string, unknown>;
  /** Your id for the message: sending again with the same id never doubles it. */
  clientMsgId?: string;
  /** The message it replies to. */
  replyTo?: string;
  /** Shows it only to this member (an ephemeral notice): not stored in the conversation's history. */
  toUser?: string;
  /** Routing envelope (docs/03-agents.md §6.2–§6.3): members this asks to act, topics, intent, references. */
  to?: string[];
  /** Topics the message is about, for members that follow only some topics. */
  topics?: string[];
  /** What the message is: `request`, `proposal`, `answer`, `status` or `done`. */
  intent?: "request" | "proposal" | "answer" | "status" | "done";
  /** Ids of the messages it refers to. */
  refs?: string[];
  /** Sources the message is based on. */
  sources?: { title: string; url: string; excerpt?: string }[];
}

/** An agent's record. */
export interface Agent {
  /** The agent's user id. */
  id: string;
  /** The person responsible for it. */
  owner: string;
  /** A team agent (stays when its owner leaves; can be transferred). */
  team: boolean;
  /** The data unit (database) its conversations live in. */
  unit: string;
  /** Whether it is deactivated (cannot connect). */
  deactivated: boolean;
  /** Whether it was stopped with the kill switch. */
  stopped: boolean;
  /** Its keys (id, name, created, last used), never the secrets. */
  keys: any[];
}

/** A user's attributes, for create and update. */
export interface UserAttrs {
  /** `human` (default) or `agent`. */
  kind?: "human" | "agent";
  /** For an agent: the person responsible for it. */
  owner?: string;
  /** The data unit (database) the user's data lives in, for data residency. */
  unit?: string;
  /** A team agent (stays when its owner leaves; can be transferred). */
  team?: boolean;
  /** The name others see. */
  display_name?: string;
  /** The avatar URL. */
  avatar?: string;
  /** Your own fields (department, plan…), returned as they are. */
  fields?: Record<string, unknown>;
}

/** Signs a client token for a user (HS256 JWT, docs/13 §4.2); no call to AgentIM. */
export function createUserToken(key: { kid: string; secret: string }, userId: string, opts?: { expiresIn?: number }): string;

/**
 * The open API client for your backend: an API key with the scopes the calls need. Retries network
 * errors and 429/5xx with backoff; POSTs reuse one Idempotency-Key across retries.
 */
export class AgentIM {
  constructor(opts: { baseUrl: string; apiKey: string; retries?: number; fetch?: typeof fetch });
  /**
   * Any open API call: `method`, a path under `/v1`, an optional JSON body. Use the typed methods
   * when there is one.
   */
  request(method: string, path: string, body?: unknown, opts?: { idempotencyKey?: string }): Promise<any>;
  /** Logs a person out everywhere: client tokens issued until now are refused. */
  revokeTokens(userId: string): Promise<{ revoked: number }>;
  /**
   * A new key for an agent (`agents:write`). `revokeOthers` revokes every other key of the agent
   * in the same write (rotation). The key is shown only here.
   */
  issueAgentKey(agentId: string, opts?: { revokeOthers?: boolean }): Promise<{ key: string; device_id: string; revoked: string[]; connect: any }>;
  /** Revokes every key of an agent and disconnects it. */
  revokeAgentKeys(agentId: string): Promise<{ revoked: number }>;
  /** Revokes one key of an agent (by its device id) and disconnects that connection. */
  revokeAgentKey(agentId: string, deviceId: string): Promise<{ revoked: number }>;
  /** The agents a person owns, with their keys (never the secrets). */
  listAgents(userId: string): Promise<{ agents: Agent[] }>;
  /**
   * Stops an agent at once (kill switch): it is disconnected and every request is refused until
   * `resumeAgent`.
   */
  stopAgent(agentId: string, opts?: { reason?: string }): Promise<Agent>;
  /** Resumes a stopped agent. */
  resumeAgent(agentId: string): Promise<Agent>;
  /**
   * Gives a team agent to another person of its unit; `rotateKeys` revokes its keys in the same
   * write.
   */
  transferAgent(agentId: string, opts: { owner: string; rotateKeys?: boolean }): Promise<Agent>;
  /**
   * Destroys an agent for good: keys revoked, memberships ended, checkpoints deleted. Its messages
   * stay in the conversations; its name cannot be used again.
   */
  destroyAgent(agentId: string): Promise<{ id: string; destroyed: boolean; conversations: number }>;
  /**
   * Exports an agent (settings, checkpoints, optionally its conversations) as a package signed by
   * this deployment, to import elsewhere.
   */
  exportAgent(agentId: string, opts?: { withConversations?: boolean }): Promise<any>;
  /** `allowUnverified`: accept a package this deployment cannot verify (unsigned, or from another deployment). */
  importAgent(opts: { package: any; owner: string; name?: string; allowUnverified?: boolean }): Promise<{ agent: Agent; key: string; device_id: string; connect: any; verified: boolean }>;
  /** A person's devices (one per client token), with when each was last seen. */
  listDevices(userId: string): Promise<{ devices: any[] }>;
  /** Revokes one device's token and disconnects it. */
  logoutDevice(userId: string, deviceId: string): Promise<{ revoked: number }>;
  /**
   * Deletes a person's account (GDPR art. 17): logged out everywhere, then their content is erased
   * in the background. Check progress with `erasure`.
   */
  deleteUser(userId: string): Promise<any>;
  /** Progress of an account deletion: `running` or `done`, with what was erased. */
  erasure(userId: string): Promise<any>;
  /**
   * Everything about a person (GDPR art. 15 and 20) as JSON lines: profile, devices,
   * conversations, their messages, reports. Pages with `after`; collects all pages.
   */
  exportUser(userId: string, opts?: { after?: string }): Promise<any[]>;
  /** The moderation queue: reports by state, newest first, in pages. */
  reports(opts?: { state?: "open" | "resolved"; limit?: number; offset?: number }): Promise<{ reports: any[]; has_more: boolean }>;
  /** One report with its target and what was decided. */
  getReport(id: string): Promise<any>;
  /**
   * Decides a report (DSA art. 16, 17): no action, remove the content, suspend or ban the user;
   * the reason is shown to the people affected.
   */
  resolveReport(id: string, d: { action: "none" | "remove_content" | "suspend_user" | "ban_user"; reason: string; ground?: string; automated?: boolean; days?: number }): Promise<any>;
  /** Silences a member of a conversation for `seconds`: they read but cannot send. */
  silenceMember(convId: string, userId: string, opts: { seconds: number; reason?: string }): Promise<{ user_id: string; silenced_until: number; changed: boolean; seq?: number }>;
  /** Lifts a silence early. */
  unsilenceMember(convId: string, userId: string): Promise<{ user_id: string; silenced_until: null; changed: boolean; seq?: number }>;
  /**
   * Creates or updates a user with your own id: a person, or an agent with an `owner` (then the
   * answer carries its first key).
   */
  upsertUser(id: string, attrs?: UserAttrs): Promise<any>;
  /** A user by your id. */
  getUser(id: string): Promise<any>;
  /** Changes a user's display name, avatar or custom fields. */
  updateUser(id: string, attrs: Pick<UserAttrs, "display_name" | "avatar" | "fields">): Promise<any>;
  /** Deactivates a user: logged out everywhere and refused until reactivated; their data stays. */
  deactivateUser(id: string): Promise<any>;
  /** Reactivates a deactivated user. */
  reactivateUser(id: string): Promise<any>;
  /** Creates or updates up to 100 users in one call; one result per user. */
  batchUsers(users: Array<UserAttrs & { id: string }>): Promise<{ results: any[] }>;
  /** The conversations a user is in. */
  userConversations(id: string): Promise<{ convs: any[] }>;
  /** Creates a conversation as a user or as the system (`as: "system"`, then name an `owner`). */
  createConversation(c: { as: As; kind: "dm" | "group"; title: string; members?: string[]; owner?: string; fields?: Record<string, unknown> }): Promise<any>;
  /** A conversation: kind, title, members, custom fields. */
  getConversation(id: string): Promise<any>;
  /** Renames a conversation or replaces its custom fields. */
  updateConversation(id: string, c: { as: As; title?: string; fields?: Record<string, unknown> }): Promise<any>;
  /** Dissolves a conversation: everyone loses access and the content is deleted. */
  dissolveConversation(id: string, as: As): Promise<any>;
  /** Adds users to a conversation; one result per user. */
  addMembers(id: string, as: As, users: string[]): Promise<{ results: any[] }>;
  /** Removes a member. */
  removeMember(id: string, user: string, as: As): Promise<any>;
  /** Makes a member an admin or a member, or the owner (transfers ownership). */
  setRole(id: string, user: string, role: "owner" | "admin" | "member", as: As): Promise<any>;
  /**
   * Sends a message as a member or as the system. With `clientMsgId` a retry returns the first
   * result (`duplicate: true`).
   */
  sendMessage(convId: string, m: Message): Promise<{ conv_id: string; seq?: number; msg_id?: string; duplicate?: boolean }>;
  /** Uploads a whole file (at most 64 MiB); the server removes EXIF and GPS and makes a thumbnail. */
  uploadFile(convId: string, opts: { as: string; name: string; data: Uint8Array | ArrayBuffer | Blob; mime?: string }): Promise<FileInfo>;
  /** Uploads a file and sends it as an image or file message. */
  sendFile(convId: string, opts: { as: string; name: string; data: Uint8Array | ArrayBuffer | Blob; mime?: string; caption?: string; replyTo?: string }): Promise<{ conv_id: string; seq?: number; msg_id?: string; file: FileInfo }>;
  /** A link (valid 5 minutes) to read a file. */
  fileUrl(convId: string, fileId: string, opts?: { as?: string; variant?: "original" | "thumb" }): Promise<{ url: string; variant: string; expires_at: number; file: FileInfo }>;
  /** Sends to a user directly: the direct conversation with them is opened if needed. */
  sendToUser(userId: string, m: Message): Promise<any>;
  /**
   * Sends the same message to up to 100 users (each in their direct conversation); one result per
   * user.
   */
  batchSend(m: { as: As; to: string[]; text: string; kind?: string; payload?: Record<string, unknown>; clientMsgId?: string }): Promise<{ results: any[] }>;
  /** Edits a message as its sender (or the system). */
  editMessage(id: string, m: { as: As; text: string; payload?: Record<string, unknown> }): Promise<{ seq: number }>;
  /** Recalls a message for everyone. */
  recallMessage(id: string, as: As): Promise<{ seq: number }>;
  /** A conversation's events after a seq, in pages (your backend's own records, search, analytics). */
  history(convId: string, opts?: { afterSeq?: number; limit?: number }): Promise<{ events: any[]; next_after_seq: number; has_more: boolean }>;
  /** Marks a conversation read up to `seq` for a user. */
  markRead(convId: string, as: As, seq: number): Promise<{}>;
}

/** A stored file (docs/02-architecture.md §2.7). */
export interface FileInfo {
  /** The file id; send it in a message or ask for its URL. */
  file_id: string;
  /** The file name as uploaded. */
  name: string;
  /** The size in bytes. */
  size: number;
  /** The content type. */
  mime: string;
  /** `image` (shown inline, has a size) or `file`. */
  kind: "image" | "file";
  /** An image's width in pixels, upright (null for other files). */
  width: number | null;
  /** An image's height in pixels, upright (null for other files). */
  height: number | null;
  /** Whether it has a thumbnail. */
  thumb: boolean;
  /** Whether it is marked as made by AI (EU AI Act Art. 50); agents' uploads always are. */
  ai_generated: boolean;
}
