// AgentIM server SDK for Node.js (18+). Thin client for the open API /v1
// (docs/07-open-api.md). No dependencies.

import { createHmac, randomUUID } from "node:crypto";

/** An error answered by the server (RFC 9457 problem details). */
export class AgentIMError extends Error {
  constructor(status, code, detail) {
    super(`${status} ${code}: ${detail}`);
    this.name = "AgentIMError";
    this.status = status;
    this.code = code;
    this.detail = detail;
  }
}

const enc = encodeURIComponent;
const b64u = (s) => Buffer.from(s).toString("base64url");

/**
 * Signs a client token for one of your users (docs/13 §4.2): HS256 JWT with
 * the signing key from `agentim signing-key create`. Do this in your
 * backend after your own login; give the token to your app. No call to
 * AgentIM is made.
 * @param {{kid: string, secret: string}} key
 * @param {string} userId
 * @param {{expiresIn?: number}} [opts] seconds (default one hour; at most `auth.max_token_days`)
 */
export function createUserToken({ kid, secret }, userId, { expiresIn = 3600 } = {}) {
  if (!kid || !secret) throw new Error("kid and secret are required");
  const iat = Math.floor(Date.now() / 1000);
  const head = b64u(JSON.stringify({ alg: "HS256", typ: "JWT", kid }));
  const body = b64u(JSON.stringify({ sub: userId, iat, exp: iat + expiresIn }));
  const sig = createHmac("sha256", secret).update(`${head}.${body}`).digest("base64url");
  return `${head}.${body}.${sig}`;
}

export class AgentIM {
  /**
   * @param {{baseUrl: string, apiKey: string, retries?: number, fetch?: typeof fetch}} opts
   */
  constructor({ baseUrl, apiKey, retries = 3, fetch: f } = {}) {
    if (!baseUrl || !apiKey) throw new Error("baseUrl and apiKey are required");
    this.baseUrl = baseUrl.replace(/\/+$/, "");
    this.apiKey = apiKey;
    this.retries = retries;
    this.fetch = f ?? globalThis.fetch;
  }

  /** Low-level call. POSTs get an Idempotency-Key reused across retries. */
  async request(method, path, body, { idempotencyKey } = {}) {
    const headers = { authorization: `Bearer ${this.apiKey}` };
    if (body !== undefined) headers["content-type"] = "application/json";
    if (method === "POST") headers["idempotency-key"] = idempotencyKey ?? randomUUID();
    const retryable = method === "GET" || method === "PUT" || method === "POST";
    for (let attempt = 0; ; attempt++) {
      let res;
      try {
        res = await this.fetch(this.baseUrl + path, {
          method,
          headers,
          body: body === undefined ? undefined : JSON.stringify(body),
        });
      } catch (e) {
        if (retryable && attempt < this.retries) {
          await sleep(backoff(attempt));
          continue;
        }
        throw e;
      }
      const text = await res.text();
      const data = text ? JSON.parse(text) : {};
      if (res.ok) return data;
      if (retryable && attempt < this.retries && (res.status === 429 || res.status === 503)) {
        const after = Number(res.headers.get("retry-after"));
        await sleep(after > 0 ? after * 1000 : backoff(attempt));
        continue;
      }
      throw new AgentIMError(res.status, data.code ?? "error", data.detail ?? text);
    }
  }

  // ---- login (client tokens are signed with createUserToken)
  /** Logs a person out everywhere: tokens issued until now are refused. */
  revokeTokens(userId) { return this.request("DELETE", `/v1/users/${enc(userId)}/tokens`); }

  // ---- agent keys
  issueAgentKey(agentId, { revokeOthers = false } = {}) {
    return this.request("POST", `/v1/agents/${enc(agentId)}/keys`, { revoke_others: revokeOthers });
  }
  revokeAgentKeys(agentId) { return this.request("DELETE", `/v1/agents/${enc(agentId)}/keys`); }
  revokeAgentKey(agentId, deviceId) {
    return this.request("DELETE", `/v1/agents/${enc(agentId)}/keys/${enc(deviceId)}`);
  }

  // ---- users
  upsertUser(id, attrs = {}) { return this.request("PUT", `/v1/users/${enc(id)}`, attrs); }
  getUser(id) { return this.request("GET", `/v1/users/${enc(id)}`); }
  updateUser(id, attrs) { return this.request("PATCH", `/v1/users/${enc(id)}`, attrs); }
  deactivateUser(id) { return this.request("POST", `/v1/users/${enc(id)}:deactivate`); }
  reactivateUser(id) { return this.request("POST", `/v1/users/${enc(id)}:reactivate`); }
  batchUsers(users) { return this.request("POST", "/v1/users:batch", { users }); }
  userConversations(id) { return this.request("GET", `/v1/users/${enc(id)}/conversations`); }

  // ---- conversations (`as` is a user id or "system")
  createConversation(c) { return this.request("POST", "/v1/conversations", c); }
  getConversation(id) { return this.request("GET", `/v1/conversations/${enc(id)}`); }
  updateConversation(id, { as, title, fields }) {
    return this.request("PATCH", `/v1/conversations/${enc(id)}`, { as, title, fields });
  }
  dissolveConversation(id, as) {
    return this.request("DELETE", `/v1/conversations/${enc(id)}?as=${enc(as)}`);
  }
  addMembers(id, as, users) {
    return this.request("POST", `/v1/conversations/${enc(id)}/members`, { as, users });
  }
  removeMember(id, user, as) {
    return this.request("DELETE", `/v1/conversations/${enc(id)}/members/${enc(user)}?as=${enc(as)}`);
  }
  setRole(id, user, role, as) {
    return this.request("PATCH", `/v1/conversations/${enc(id)}/members/${enc(user)}`, { as, role });
  }

  // ---- messages
  sendMessage(convId, m) {
    return this.request("POST", `/v1/conversations/${enc(convId)}/messages`, msg(m));
  }
  sendToUser(userId, m) { return this.request("POST", `/v1/users/${enc(userId)}/messages`, msg(m)); }
  batchSend({ as, to, text, kind, payload, clientMsgId }) {
    return this.request("POST", "/v1/messages:batch-send", {
      as, to, text, kind, payload, client_msg_id: clientMsgId,
    });
  }
  editMessage(id, { as, text, payload }) {
    return this.request("PATCH", `/v1/messages/${enc(id)}`, { as, text, payload });
  }
  recallMessage(id, as) { return this.request("POST", `/v1/messages/${enc(id)}:recall`, { as }); }
  history(convId, { afterSeq = 0, limit = 50 } = {}) {
    return this.request("GET", `/v1/conversations/${enc(convId)}/messages?after_seq=${afterSeq}&limit=${limit}`);
  }
  markRead(convId, as, seq) {
    return this.request("POST", `/v1/conversations/${enc(convId)}/read`, { as, seq });
  }
}

function msg({ as, text, kind, payload, clientMsgId, replyTo, toUser, to, topics, intent, refs, sources }) {
  return { as, text, kind, payload, client_msg_id: clientMsgId, reply_to: replyTo, to_user: toUser, to, topics, intent, refs, sources };
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const backoff = (attempt) => Math.min(200 * 2 ** attempt, 5000);
