// End-to-end check of the agent SDK against a real server it starts itself
// (docs/en/design/agents.md). A person (alice) talks to two agents
// over WebSocket; the agents use the SDK.
//
//   AGENTIM_BIN=server/target/debug/agentim node sdk/agent-node/e2e.mjs

import { spawn, execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import zlib from 'node:zlib';
import { Agent, Connection } from './index.mjs';
import { AgentIM, createUserToken } from '../node/index.mjs';

const BIN = path.resolve(process.env.AGENTIM_BIN ?? 'server/target/debug/agentim');
const PORT = Number(process.env.PORT ?? 20000 + Math.floor(Math.random() * 20000));
const URL = `http://127.0.0.1:${PORT}`;
const DIR = fs.mkdtempSync(path.join(os.tmpdir(), 'agentim-e2e-'));
const DB = path.join(DIR, 'e2e.db');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let failures = 0;
const check = (cond, what) => { console.log(`${cond ? 'ok  ' : 'FAIL'} ${what}`); if (!cond) failures++; };
const cli = (...args) => JSON.parse(execFileSync(BIN, ['--db', DB, ...args], { stdio: ['ignore', 'pipe', 'ignore'] }).toString());

// A w x h RGB PNG, made here (no image library needed).
function png(w, h) {
  const chunk = (type, data) => {
    const len = Buffer.alloc(4); len.writeUInt32BE(data.length);
    const td = Buffer.concat([Buffer.from(type), data]);
    const crc = Buffer.alloc(4); crc.writeUInt32BE(zlib.crc32(td));
    return Buffer.concat([len, td, crc]);
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0); ihdr.writeUInt32BE(h, 4); ihdr[8] = 8; ihdr[9] = 2;
  const rows = Buffer.alloc((w * 3 + 1) * h);
  for (let y = 0; y < h; y++) for (let x = 0; x < w; x++) rows[y * (w * 3 + 1) + 1 + x * 3] = (x * 7) % 256;
  return Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk('IHDR', ihdr), chunk('IDAT', zlib.deflateSync(rows)), chunk('IEND', Buffer.alloc(0))]);
}

let server;
for (const sig of ['SIGINT', 'SIGTERM']) process.on(sig, () => { server?.kill(); process.exit(130); });
async function startServer() {
  // In its own folder: files go to ./data/files there.
  server = spawn(BIN, ['--db', DB, '--addr', `127.0.0.1:${PORT}`], { stdio: 'ignore', cwd: DIR });
  for (let i = 0; i < 100; i++) {
    try { if ((await fetch(`${URL}/readyz`)).ok) return; } catch {}
    await sleep(100);
  }
  throw new Error('server did not start');
}
async function stopServer() {
  const done = new Promise((r) => server.once('exit', r));
  server.kill();
  await done;
}

// A person's client: syncs, then keeps every event and side event it gets.
class Person {
  constructor(token) { this.token = token; this.events = []; this.sides = []; }
  async connect() {
    this.conn = new Connection(URL, this.token, (f) => {
      if (f.type === 'event') this.events.push(f.event);
      if (f.type === 'side') this.sides.push(f.event);
      // Pushes could not continue (e.g. a new conversation): sync again.
      if (f.type === 'resync') this.sync().catch(() => {});
    }, { device: 'e2e-browser' });
    await this.conn.open();
    await this.sync();
  }
  async sync() {
    for (;;) {
      const r = await this.conn.call('sync', { limit: 500 });
      for (const c of r.convs) {
        this.events.push(...(c.events ?? []).map((e) => e.event));
        this.sides.push(...(c.side_events ?? []));
      }
      if (!r.more) break;
    }
  }
  say(conv, text) { return this.conn.call('send', { conv_id: conv, client_msg_id: crypto.randomUUID(), text }); }
  /** Waits for an event matching `pred` (default 20 s). */
  async wait(pred, what, ms = 20000) {
    const end = Date.now() + ms;
    while (Date.now() < end) {
      const hit = this.events.find(pred) ?? this.sides.find(pred);
      if (hit) return hit;
      await sleep(50);
    }
    throw new Error(`timed out waiting for ${what}`);
  }
  /** A message from `sender` whose final content includes `text`. */
  reply(sender, text) {
    return this.wait((e) => e.msg && e.msg.sender_id === sender && e.msg.content.includes(text) && e.msg.state !== 'streaming', `"${text}"`);
  }
}

try {
  await startServer();
  const key = cli('api-key', 'create', '--name', 'e2e', '--scopes', 'users:write,agents:write').key;
  const im = new AgentIM({ baseUrl: URL, apiKey: key });
  await im.upsertUser('alice');
  // The developer's backend signs alice's token itself (docs/en/design/access.md).
  const token = createUserToken(cli('signing-key', 'create', '--name', 'e2e'), 'alice');
  const alice = new Person(token);
  await alice.connect();
  // The backend gives alice two agents; her app finds their ids.
  const agent = async (id) => {
    const r = await im.upsertUser(id, { kind: 'agent', owner: 'alice' });
    const u = (await alice.conn.call('find_user', { name: id })).user;
    return { key: r.key, agent: { id: u.id } };
  };
  const one = await agent('nbot');
  const two = await agent('nbot2');
  const conv = (await alice.conn.call('create_conv', { kind: 'group', title: 'e2e', members: [one.agent.id, two.agent.id] })).conv_id;

  // A conversation nbot is not in.
  const elsewhere = (await alice.conn.call('create_conv', { kind: 'group', title: 'without nbot', members: [two.agent.id] })).conv_id;
  // The agents.
  let failedOnce = false;
  let refusedRuns = 0;
  const bot = new Agent({ url: URL, key: one.key, log: (m) => console.log('[nbot]', m) });
  bot.onMessage(async (msg, ctx) => {
    const t = msg.content;
    if (msg.sender_id === two.agent.id) return ctx.reply('peer answered');
    if (t.includes('hello')) {
      const s = await ctx.stream();
      await s.append('Hi ');
      await ctx.progress('step', 'streaming');
      await s.append('there');
      await s.end();
    } else if (t.includes('form')) {
      // A form only the asker fills in, waited for inside the handler (docs/en/design/chat-components.md).
      const s = await ctx.steps(['Ask for details', 'Refund']);
      await s.start(0);
      const v = await ctx.askForm('Refund', [{ id: 'reason', label: 'Reason', type: 'select', options: ['Broken', 'Late'], required: true }, { id: 'day', label: 'Pickup', type: 'date' }]);
      await s.done(0);
      await s.wait(1, 'Waiting for approval');
      await ctx.showCard('Refund', [{ type: 'facts', items: [{ label: 'Reason', value: v.reason }, { label: 'Pickup', value: v.day, format: 'date' }] }]);
      await ctx.reply(`form: ${v.reason} ${v.day}`);
    } else if (t.includes('card')) {
      await bot.sendCard(ctx.convId, 'Go on? (yes/no)', { blocks: [{ type: 'buttons', id: 'go', options: [{ id: 'yes', label: 'Yes' }, { id: 'no', label: 'No' }] }] }, { replyTo: msg.msg_id });
    } else if (t.includes('approve')) {
      const d = await bot.askHuman(ctx.convId, { kind: 'approve', title: 'Ship it?' });
      await ctx.reply(`decision: ${d.state}`);
    } else if (t.includes('state')) {
      const w = await bot.putState('plan', { step: 1 }, 0);
      let conflict = '';
      try { await bot.putState('plan', { step: 2 }, 0); } catch (e) { conflict = e.code; }
      const r = await bot.getState('plan');
      await ctx.reply(`state ${w.version} ${conflict} ${r.value.step} ${await bot.getState('nope')}`);
    } else if (t.includes('context')) {
      const c = await bot.context(ctx.convId);
      await ctx.reply(`context has ${c.events.length} events`);
    } else if (t.includes('refused')) {
      refusedRuns++;
      await bot.claim(msg.msg_id); // not a request: refused, and not retried
    } else if (t.includes('fail once')) {
      if (!failedOnce) { failedOnce = true; throw new Error('first try fails'); }
      await ctx.reply('retried');
    } else if (t.includes('ask peer')) {
      await bot.send(ctx.convId, 'nbot2, are you there?', { to: ['nbot2'], intent: 'request' });
    } else if (t.includes('handoff')) {
      await bot.handoff(ctx.convId, 'a person should take this');
    } else if (t.includes('picture')) {
      const sent = await bot.sendFile(ctx.convId, 'chart.png', png(640, 480), { caption: 'the chart', replyTo: msg.msg_id });
      const doc = await bot.sendFile(ctx.convId, 'report.txt', Buffer.from('the report'), { mime: 'text/plain' });
      const link = await bot.fileUrl(ctx.convId, doc.file.file_id);
      const body = await (await fetch(link.url)).text();
      let outside = '';
      try { await bot.uploadFile(elsewhere, 'x.txt', Buffer.from('x')); } catch (e) { outside = e.code; }
      await ctx.reply(`files ${sent.file.kind} ${sent.file.thumb} ${doc.file.kind} ${body} ${outside}`);
    } else if (t.includes('trace')) {
      await ctx.reply(`trace ${ctx.traceId}`);
    } else if (t.includes('sources')) {
      await ctx.reply('see the docs', { sources: [{ title: 'Docs', url: 'https://example.com/docs' }] });
    }
  });
  bot.onCardResponse(async (r, ctx) => ctx.reply(`card: ${r.action}`));
  bot.onHandback(async (d, ctx) => ctx.reply(`back: ${d.note}`));
  await bot.start();
  check(bot.me.id === one.agent.id && bot.me.kind === 'agent', 'the agent connects with its key');
  // Its configuration came with the login; a role given by the backend
  // arrives by itself (docs/en/design/agents.md).
  check(bot.config && bot.config.role === null && /^[0-9a-f]{16}$/.test(bot.config.version), 'the configuration comes with the login');
  const seen = [];
  bot.onConfig((c) => seen.push(c.role?.name ?? null));
  const put = (path, body, type) => fetch(`${URL}${path}`, { method: 'PUT', headers: { authorization: `Bearer ${key}`, 'content-type': type }, body });
  check((await put('/v1/roles/greeter', '---\ndescription: Greets people\n---\nSay hello first.', 'text/markdown')).ok, 'the backend stores a role');
  check((await put('/v1/agents/nbot/role', JSON.stringify({ role: 'greeter' }), 'application/json')).ok, 'and gives it to the agent');
  for (let i = 0; i < 50 && bot.config.role?.name !== 'greeter'; i++) await sleep(100);
  check(bot.config.role?.instructions === 'Say hello first.' && seen.includes('greeter'), 'the agent has its new role without reconnecting');
  // Skills on disk (docs/en/design/assistants.md): a skill the role names is written where
  // Claude Code looks, its reference file fetched with the agent's key, and
  // removed again when the role no longer names it.
  {
    const dev = cli('admin-key', 'create', '--name', 'e2e-dev', '--role', 'developer').key;
    const adm = (path, body, type) => fetch(`${URL}/admin/v1/app${path}`, { method: 'PUT', headers: { authorization: `Bearer ${dev}`, 'content-type': type }, body });
    check((await adm('/skills/refunds', '---\nname: refunds\ndescription: Refund rules\n---\nRefunds over $100 need a manager.\n', 'text/markdown')).ok, 'a skill is stored');
    check((await adm('/skills/refunds/files/docs/policy.md', 'Policy: 30 days.', 'text/plain')).ok, 'with a reference file');
    const FILES = path.join(DIR, 'workdir');
    bot.files = { dir: FILES };
    await put('/v1/roles/greeter', '---\ndescription: Greets people\nskills: [refunds]\n---\nSay hello first.', 'text/markdown');
    const skill = path.join(FILES, '.claude', 'skills', 'refunds');
    for (let i = 0; i < 50 && !fs.existsSync(path.join(skill, 'docs', 'policy.md')); i++) await sleep(100);
    check(fs.readFileSync(path.join(skill, 'SKILL.md'), 'utf8').includes('Refunds over $100'), 'the SDK writes the skill where Claude Code looks');
    check(fs.readFileSync(path.join(skill, 'docs', 'policy.md'), 'utf8') === 'Policy: 30 days.', 'with its reference file');
    const other = await fetch(`${URL}/api/agent/skills/refunds/files/docs/policy.md`, { headers: { authorization: `Bearer ${two.key}` } });
    check(other.status === 404, 'an agent whose role does not name the skill cannot read it');
    await put('/v1/roles/greeter', '---\ndescription: Greets people\n---\nSay hello first.', 'text/markdown');
    for (let i = 0; i < 50 && fs.existsSync(skill); i++) await sleep(100);
    check(!fs.existsSync(skill), 'and removes it when the role no longer names it');
    bot.files = null;
  }
  const peer = new Agent({ url: URL, key: two.key });
  peer.onMessage(async (msg, ctx) => { if (msg.sender_id === one.agent.id) await ctx.reply('here'); });
  await peer.start();

  const a = one.agent.id;
  await alice.say(conv, '@nbot hello');
  const hi = await alice.reply(a, 'Hi there');
  check(hi.msg.sender_type === 'agent', 'a streamed reply arrives complete, marked as AI');
  check(alice.sides.some((s) => s.type === 'progress' && s.data.text === 'streaming'), 'progress reaches the person');

  await alice.say(conv, '@nbot form please');
  const form = await alice.wait((e) => e.msg && e.msg.kind === 'agentim.card' && e.msg.payload?.blocks?.some((b) => b.type === 'form'), 'the form');
  check(form.msg.payload.to?.length === 1 && form.msg.payload.to[0] !== a && form.msg.payload.once === true, 'askForm: only the asker answers, once');
  await alice.conn.call('respond', { msg_id: form.msg.msg_id, block_id: 'form', values: { reason: 'Broken', day: '2026-03-02' } });
  await alice.reply(a, 'form: Broken 2026-03-02');
  check(true, 'askForm resolves to the answers inside the handler');
  const shown = await alice.wait((e) => e.msg && e.msg.kind === 'agentim.card' && e.msg.payload?.blocks?.[0]?.type === 'facts', 'the data card');
  check(shown.msg.payload.blocks[0].items[1].format === 'date', 'showCard sends a data card');
  const st = alice.sides.filter((x) => x.type === 'progress');
  check(st.some((x) => x.data.kind === 'plan' && x.data.data?.steps?.length === 2) && ['running', 'done', 'waiting'].every((k) => st.some((x) => x.data.kind === 'step' && x.data.data?.state === k)), 'steps: a plan, then running, done, waiting');
  // An agent may send 5 messages at once in a conversation: let it refill.
  await new Promise((r) => setTimeout(r, 4000));

  await alice.say(conv, '@nbot card');
  const card = await alice.wait((e) => e.msg && e.msg.kind === 'agentim.card' && e.msg.payload?.blocks?.[0]?.id === 'go', 'the card');
  await alice.conn.call('respond', { msg_id: card.msg.msg_id, block_id: 'go', action: 'yes' });
  await alice.reply(a, 'card: yes');
  check(true, 'a card is answered and the agent gets the answer');

  await alice.say(conv, '@nbot approve');
  const asked = await alice.wait((e) => e.type === 'request.created', 'the request');
  await alice.conn.call('decide', { request_id: asked.data.request.request_id, decision: 'approve' });
  await alice.reply(a, 'decision: approved');
  check(true, 'the agent asks a person and gets the decision');

  await alice.say(conv, '@nbot state');
  await alice.reply(a, 'state 1 conflict 1 null');
  check(true, 'checkpoints: created, conflicting write refused, read back, missing is null');

  await alice.say(conv, '@nbot context');
  const ctxReply = await alice.reply(a, 'context has');
  check(Number(ctxReply.msg.content.split(' ')[2]) > 3, 'the agent reads its context');

  await alice.say(conv, '@nbot sources');
  const src = await alice.reply(a, 'see the docs');
  check(src.msg.envelope?.sources?.[0]?.title === 'Docs', 'sources travel with the reply');

  // The agent gets the trace of the message that woke it, and its reply is
  // stored in that trace (docs/en/design/tracing.md), as the admin API shows.
  const tracedAsk = await alice.say(conv, '@nbot trace');
  const traced = await alice.reply(a, 'trace ');
  const traceId = traced.msg.content.split(' ')[1];
  check(/^[0-9a-f]{32}$/.test(traceId), 'ctx.traceId is a trace id');
  const admin = cli('admin-key', 'create', '--name', 'e2e', '--role', 'operator').key;
  const traceOf = async (id) => (await (await fetch(`${URL}/admin/v1/messages/${id}/trace`, { headers: { authorization: `Bearer ${admin}` } })).json()).trace?.trace_id;
  check((await traceOf(tracedAsk.msg_id)) === traceId, "ctx.trace is the trace of the person's message");
  check((await traceOf(traced.msg.msg_id)) === traceId, 'the reply is sent in the same trace');

  await sleep(3000); // the agent's per-conversation message budget refills (three messages follow)
  await alice.say(conv, '@nbot picture');
  await alice.reply(a, 'files image true file the report forbidden');
  const pic = await alice.wait((e) => e.msg && e.msg.kind === 'image', 'the image');
  check(pic.msg.payload?.width === 640 && pic.msg.payload?.caption === 'the chart', 'the image message carries the file and its caption');
  const thumb = await alice.conn.call('file_url', { conv_id: conv, file_id: pic.msg.payload.file_id, variant: 'thumb' });
  const tb = Buffer.from(await (await fetch(new globalThis.URL(thumb.url, URL))).arrayBuffer());
  const sof = tb.indexOf(Buffer.from([0xff, 0xc0])); // a baseline JPEG's frame header: height, then width
  check(thumb.variant === 'thumb' && tb[0] === 0xff && tb[1] === 0xd8 && tb.readUInt16BE(sof + 7) === 320 && tb.readUInt16BE(sof + 5) === 240, 'the person reads a 320 x 240 JPEG thumbnail');
  check(true, 'an agent sends an image and a file, reads one back, and cannot upload elsewhere');

  await alice.say(conv, '@nbot fail once');
  await alice.reply(a, 'retried');
  check(failedOnce, 'a failed handler gets the message again (at least once)');

  await alice.say(conv, '@nbot refused');
  await sleep(1500);
  await alice.say(conv, '@nbot sources again');
  await alice.reply(a, 'see the docs');
  check(refusedRuns === 1, `a handler refused by the server is not run again (${refusedRuns})`);

  await alice.say(conv, '@nbot ask peer');
  await alice.reply(a, 'peer answered');
  check(true, 'two agents address each other with `to` and answer');

  await alice.say(conv, '@nbot handoff');
  await alice.wait((e) => e.type === 'agent.state' && e.data.handoff, 'the handoff');
  await alice.conn.call('handback', { conv_id: conv, agent_id: a, note: 'done' });
  await alice.reply(a, 'back: done');
  check(true, 'handed off to a person and back');

  // The server restarts: the agent reconnects by itself.
  await stopServer();
  await startServer();
  alice.conn.close();
  await sleep(300);
  const before = alice.events.length;
  await alice.connect();
  alice.events.splice(0, before);
  await sleep(1500);
  await alice.say(conv, '@nbot hello again');
  await alice.reply(a, 'Hi there');
  check(true, 'after a server restart the agent reconnects and answers');

  await bot.stop();
  await peer.stop();
} catch (e) {
  console.log(`FAIL ${e.stack ?? e}`);
  failures++;
} finally {
  // Windows keeps the database file locked until the process has exited.
  if (server && server.exitCode === null) {
    const exited = new Promise((r) => server.once('exit', r));
    server.kill();
    await Promise.race([exited, sleep(5000)]);
  }
  fs.rmSync(DIR, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
}
if (failures) { console.error(`${failures} check(s) failed`); process.exit(1); }
console.log('agent SDK end-to-end (Node) passed');
process.exit(0);
