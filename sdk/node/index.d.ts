export class AgentIMError extends Error {
  status: number;
  /** Same codes as the WebSocket protocol: forbidden, not_found, conflict, blocked, ... */
  code: string;
  detail: string;
}

/** A user id, or "system". */
export type As = string;

export interface Message {
  as: As;
  text: string;
  kind?: string;
  payload?: Record<string, unknown>;
  clientMsgId?: string;
  replyTo?: string;
  toUser?: string;
  /** Routing envelope (docs/03-agents.md §6.2–§6.3): members this asks to act, topics, intent, references. */
  to?: string[];
  topics?: string[];
  intent?: "request" | "proposal" | "answer" | "status" | "done";
  refs?: string[];
  /** Sources the message is based on. */
  sources?: { title: string; url: string; excerpt?: string }[];
}

export interface UserAttrs {
  kind?: "human" | "agent";
  owner?: string;
  unit?: string;
  display_name?: string;
  avatar?: string;
  fields?: Record<string, unknown>;
}

/** Signs a client token for a user (HS256 JWT, docs/13 §4.2); no call to AgentIM. */
export function createUserToken(key: { kid: string; secret: string }, userId: string, opts?: { expiresIn?: number }): string;

export class AgentIM {
  constructor(opts: { baseUrl: string; apiKey: string; retries?: number; fetch?: typeof fetch });
  request(method: string, path: string, body?: unknown, opts?: { idempotencyKey?: string }): Promise<any>;
  /** Logs a person out everywhere: client tokens issued until now are refused. */
  revokeTokens(userId: string): Promise<{ revoked: number }>;
  issueAgentKey(agentId: string, opts?: { revokeOthers?: boolean }): Promise<{ key: string; device_id: string; revoked: string[]; connect: any }>;
  revokeAgentKeys(agentId: string): Promise<{ revoked: number }>;
  revokeAgentKey(agentId: string, deviceId: string): Promise<{ revoked: number }>;
  upsertUser(id: string, attrs?: UserAttrs): Promise<any>;
  getUser(id: string): Promise<any>;
  updateUser(id: string, attrs: Pick<UserAttrs, "display_name" | "avatar" | "fields">): Promise<any>;
  deactivateUser(id: string): Promise<any>;
  reactivateUser(id: string): Promise<any>;
  batchUsers(users: Array<UserAttrs & { id: string }>): Promise<{ results: any[] }>;
  userConversations(id: string): Promise<{ convs: any[] }>;
  createConversation(c: { as: As; kind: "dm" | "group"; title: string; members?: string[]; owner?: string; fields?: Record<string, unknown> }): Promise<any>;
  getConversation(id: string): Promise<any>;
  updateConversation(id: string, c: { as: As; title?: string; fields?: Record<string, unknown> }): Promise<any>;
  dissolveConversation(id: string, as: As): Promise<any>;
  addMembers(id: string, as: As, users: string[]): Promise<{ results: any[] }>;
  removeMember(id: string, user: string, as: As): Promise<any>;
  setRole(id: string, user: string, role: "owner" | "admin" | "member", as: As): Promise<any>;
  sendMessage(convId: string, m: Message): Promise<{ conv_id: string; seq?: number; msg_id?: string; duplicate?: boolean }>;
  sendToUser(userId: string, m: Message): Promise<any>;
  batchSend(m: { as: As; to: string[]; text: string; kind?: string; payload?: Record<string, unknown>; clientMsgId?: string }): Promise<{ results: any[] }>;
  editMessage(id: string, m: { as: As; text: string; payload?: Record<string, unknown> }): Promise<{ seq: number }>;
  recallMessage(id: string, as: As): Promise<{ seq: number }>;
  history(convId: string, opts?: { afterSeq?: number; limit?: number }): Promise<{ events: any[]; next_after_seq: number; has_more: boolean }>;
  markRead(convId: string, as: As, seq: number): Promise<{}>;
}
