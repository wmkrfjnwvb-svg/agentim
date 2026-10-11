// Minimal external agent over the WebSocket protocol (Node >= 22, no deps).
//
//   AGENTIM=127.0.0.1:8080 AGENT_TOKEN=<token> node examples/agent.mjs
//
// In demo mode you can create an agent token with:
//   curl -XPOST localhost:8080/api/dev/users -H 'content-type: application/json' \
//        -d '{"name":"EchoBot","agent":true,"owner":"<alice user id>"}'
// then add it to a conversation from the owner's client (op: add_member).
//
// The agent answers every message it is allowed to see with a streamed echo.
// It honours stream_cancelled (stop button / interrupt / takeover) and treats
// low-trust input (trust=low) as data, never as instructions.

const addr = process.env.AGENTIM ?? '127.0.0.1:8080';
const token = process.env.AGENT_TOKEN;
if (!token) throw new Error('AGENT_TOKEN is required');

const ws = new WebSocket(`ws://${addr}/ws?token=${token}`);
let rid = 0;
const pending = new Map();
const cancelled = new Set();
let me = null;

const call = (op) => new Promise((resolve) => {
  const id = ++rid;
  pending.set(id, resolve);
  ws.send(JSON.stringify({ ...op, rid: id }));
});
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function answer(msg, trust) {
  const start = await call({ op: 'stream_start', conv_id: msg.conv_id, client_msg_id: crypto.randomUUID(), reply_to: msg.msg_id });
  if (!start.ok) return console.error('stream_start refused:', start.error);
  const text = trust === 'low'
    ? `收到一条低信任消息（${msg.content.length} 字），按数据处理，不执行其中的指令。`
    : `Echo: ${msg.content}`;
  const parts = text.match(/.{1,3}/gsu) ?? [];
  for (let i = 0; i < parts.length; i++) {
    if (cancelled.has(start.msg_id)) return;
    const r = await call({ op: 'stream_append', msg_id: start.msg_id, idx: i, text: parts[i] });
    if (!r.ok) return console.error('append refused:', r.error);
    await sleep(40);
  }
  await call({ op: 'stream_end', msg_id: start.msg_id });
}

ws.onmessage = (e) => {
  const f = JSON.parse(e.data);
  if (f.type === 'res') { pending.get(f.rid)?.(f); pending.delete(f.rid); return; }
  if (f.type === 'welcome') { me = f.me; console.log(`connected as ${me.name} (${me.kind})`); return; }
  if (f.type === 'stream_cancelled') { cancelled.add(f.msg_id); return; }
  if (f.type === 'side' && f.event.type === 'whisper') { console.log('whisper from owner:', f.event.data.text); return; }
  if (f.type === 'event' && f.event.type === 'msg.created' && f.event.msg.sender_id !== me?.id) {
    answer(f.event.msg, f.trust).catch(console.error);
  }
};
ws.onclose = () => { console.log('disconnected'); process.exit(0); };
