// Smoke test against a running server: AGENTIM_URL and AGENTIM_KEY (a key
// with every scope). Run by CI (.github/workflows/server.yml).
import assert from "node:assert/strict";
import { AgentIM, AgentIMError } from "./index.mjs";

const im = new AgentIM({ baseUrl: process.env.AGENTIM_URL, apiKey: process.env.AGENTIM_KEY });
const run = `node${Date.now()}`;
const [a, b] = [`${run}_a`, `${run}_b`];

await im.upsertUser(a, { display_name: "A" });
await im.batchUsers([{ id: b }]);
assert.equal((await im.getUser(a)).display_name, "A");
const { token } = await im.issueToken(a, { deviceName: "smoke" });
assert.match(token, /^atk_/);

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

await im.recallMessage(sent.msg_id, "system");
await im.dissolveConversation(conv.id, "system");
await assert.rejects(im.getConversation(conv.id), (e) => e.code === "not_found");
console.log("node sdk smoke ok");
