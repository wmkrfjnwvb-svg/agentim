// Smoke test against a running server: AGENTIM_URL, AGENTIM_KEY (a key
// with every scope) and AGENTIM_KID / AGENTIM_SECRET (a signing key). Run
// by CI (.github/workflows/server.yml).
import assert from "node:assert/strict";
import { AgentIM, AgentIMError, createUserToken } from "./index.mjs";

const im = new AgentIM({ baseUrl: process.env.AGENTIM_URL, apiKey: process.env.AGENTIM_KEY });
const run = `node${Date.now()}`;
const [a, b] = [`${run}_a`, `${run}_b`];

await im.upsertUser(a, { display_name: "A" });
await im.batchUsers([{ id: b }]);
assert.equal((await im.getUser(a)).display_name, "A");
// A token signed here logs the person in over the WebSocket.
const signing = { kid: process.env.AGENTIM_KID, secret: process.env.AGENTIM_SECRET };
const welcome = (token, device) => new Promise((resolve, reject) => {
  const q = new URLSearchParams({ token, ...(device ? { device } : {}) });
  const ws = new WebSocket(`${process.env.AGENTIM_URL.replace(/^http/, "ws")}/ws?${q}`);
  ws.onmessage = (e) => { ws.close(); resolve(JSON.parse(e.data)); };
  ws.onerror = () => reject(new Error("refused"));
});
assert.equal((await welcome(createUserToken(signing, a), "smoke-1")).me.name, a);
await assert.rejects(welcome(createUserToken({ kid: signing.kid, secret: "wrong" }, a), "smoke-1"));
assert.deepEqual(await im.revokeTokens(a), { revoked: 1 });
// Agent keys.
const bot = `${run}_bot`;
await im.upsertUser(bot, { kind: "agent", owner: a });
const k = await im.issueAgentKey(bot);
assert.match(k.key, /^atk_/);
assert.equal((await welcome(k.key)).me.name, bot);
assert.deepEqual(await im.revokeAgentKey(bot, k.device_id), { revoked: 1 });
await assert.rejects(im.revokeAgentKey(bot, k.device_id), (e) => e.code === "not_found");
const k2 = await im.issueAgentKey(bot, { revokeOthers: true });
assert.ok(k2.revoked.length >= 1);
assert.equal((await im.revokeAgentKeys(bot)).revoked, 1);
assert.deepEqual((await im.listAgents(a)).agents.map((x) => x.id), [bot]);
assert.equal((await im.stopAgent(bot, { reason: "smoke" })).stopped, true);
assert.equal((await im.resumeAgent(bot)).stopped, false);
assert.ok((await im.exportAgent(bot)).sha256);
assert.equal((await im.destroyAgent(bot)).destroyed, true);

const conv = await im.createConversation({ as: "system", owner: a, kind: "group", title: "smoke", members: [b] });
const sent = await im.sendMessage(conv.id, { as: a, text: "hello", clientMsgId: "m1" });
const again = await im.sendMessage(conv.id, { as: a, text: "hello", clientMsgId: "m1" });
assert.equal(again.duplicate, true);
assert.equal(again.msg_id, sent.msg_id);
await im.sendMessage(conv.id, { as: "system", text: "notice" });
await im.editMessage(sent.msg_id, { as: a, text: "hello!" });
const dm = await im.sendToUser(b, { as: a, text: "hi" });
const batch = await im.batchSend({ as: b, to: [a, "nobody"], text: "promo" });
assert.deepEqual(batch.results.map((r) => r.ok), [true, false]);
const h = await im.history(conv.id, { limit: 200 });
assert.ok(h.events.some((e) => e.msg?.content === "hello!"));
assert.ok(h.events.some((e) => e.msg?.sender_id === "_system"));
await im.markRead(dm.conv_id, b, 1);

await assert.rejects(im.sendMessage(conv.id, { as: "nobody", text: "x" }), (e) => e instanceof AgentIMError && e.code === "not_found" && e.status === 404);
const outsider = `${run}_c`;
await im.upsertUser(outsider);
await assert.rejects(im.sendMessage(conv.id, { as: outsider, text: "x" }), (e) => e.code === "forbidden");

// Moderation: a silenced member cannot send.
await im.silenceMember(conv.id, b, { seconds: 60, reason: "smoke" });
await assert.rejects(im.sendMessage(conv.id, { as: b, text: "x" }), (e) => e.code === "forbidden");
assert.equal((await im.unsilenceMember(conv.id, b)).changed, true);
assert.equal((await im.reports()).reports.length >= 0, true);
// Devices, export, deletion.
assert.ok(Array.isArray((await im.listDevices(a)).devices));
const lines = await im.exportUser(a);
assert.ok(lines.some((l) => l.type === "profile"));
await im.deleteUser(outsider);
assert.ok(["running", "done"].includes((await im.erasure(outsider)).state));
await im.recallMessage(sent.msg_id, "system");
await im.dissolveConversation(conv.id, "system");
await assert.rejects(im.getConversation(conv.id), (e) => e.code === "not_found");
console.log("node sdk smoke ok");
