// A multi-agent task room (docs/en/design/developer-experience.md): a person gives a task; the lead agent
// splits it into steps and posts each as a request; two workers race to claim
// it (exactly one wins), work with progress updates and report back; the lead
// closes the task. The person can step in at any time or cancel the task.
//
//   npm run tasks        then open http://localhost:3000

import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { Agent } from '@agentimhq/agent';
import { start, as, idsOf } from '../lib/kit.mjs';
import { bundle } from '../lib/build.mjs';
import { model } from '../lib/model.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
const state = {};
const pages = await bundle(here, ['index']);
const s = await start({
  name: 'tasks',
  pages,
  routes: {
    '/api/session': (u) => {
      if (u.searchParams.get('as') !== 'pat') throw new Error('unknown person');
      return { url: s.url, token: s.token('pat'), conv: state.conv, me: state.ids.pat, name: 'Pat' };
    },
    // Who claimed each step and how the task stands, read from the log.
    '/api/room': async () => {
      const h = await lead.history(state.conv, { afterSeq: 0, limit: 200 });
      const events = (h.events ?? []).map((e) => e.event ?? e);
      const updates = events.filter((e) => e.type === 'task.updated');
      return {
        state: updates.at(-1)?.data?.to ?? updates.at(-1)?.data?.state ?? 'submitted',
        claims: events.filter((e) => e.type === 'msg.claimed').map((e) => Object.keys(state.ids).find((k) => state.ids[k] === e.actor_id) ?? e.actor_id),
      };
    },
  },
});

await s.im.upsertUser('pat', { display_name: 'Pat' });
const keys = {};
for (const [id, name] of [['lead', 'Lead'], ['ana', 'Ana (worker)'], ['ben', 'Ben (worker)']]) {
  keys[id] = (await s.im.upsertUser(id, { kind: 'agent', owner: 'pat', display_name: name })).key;
}
// A task channel led by `lead`; every agent sees every message there.
const pat = await as(s.url, s.token('pat'));
const ids = (state.ids = await idsOf(pat, ['pat', 'lead', 'ana', 'ben']));
state.conv = (await pat.call('create_conv', { kind: 'task', title: 'Launch blog post', members: [ids.lead, ids.ana, ids.ben], assignee: ids.lead })).conv_id;
for (const a of ['lead', 'ana', 'ben']) await pat.call('set_agent_policy', { conv_id: state.conv, agent_id: ids[a], trigger: 'all' });
pat.close();

const brain = model([
  [/^Research/, 'Facts: v2 syncs 3x faster, adds AI agents to every chat, and ships Monday. Customers asked most for faster sync.'],
  [/^Write/, 'Draft post: "v2 is here. Sync is three times faster, and AI agents can now join any chat. It ships Monday — tell us what you build with it."'],
  'Done.',
]);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const log = (who) => (m) => console.log(`[${who}] ${m}`);

// The lead: plans, posts each step as a request, closes the task.
const lead = new Agent({ url: s.url, key: keys.lead, log: log('lead') });
let task = null;
lead.onMessage(async (msg, ctx) => {
  const human = msg.sender_type !== 'agent';
  if (human && !task) {
    task = { text: msg.content, step: 0 };
    await ctx.reply('On it. Plan: 1) research, 2) write. I’ll post each step for the team.');
    await lead.send(ctx.convId, `Research: what should a post about "${task.text}" say?`, { intent: 'request' });
  } else if (human) {
    await ctx.reply(`Noted, I’ll pass it on: “${msg.content}”`);
  } else if (msg.envelope?.intent === 'answer' && task) {
    task.step++;
    if (task.step === 1) {
      await lead.send(ctx.convId, `Write: the post, using these facts: ${msg.content}`, { intent: 'request' });
    } else {
      await lead.send(ctx.convId, 'All steps are done. Closing the task.', { intent: 'done' });
      await lead.updateTask(ctx.convId, 'completed', 'Post written');
      task = null;
    }
  }
});

// The workers: whoever claims a request first does it.
function worker(id) {
  const a = new Agent({ url: s.url, key: keys[id], log: log(id) });
  a.onMessage(async (msg, ctx) => {
    if (msg.envelope?.intent !== 'request') return;
    try {
      await a.claim(msg.msg_id);
    } catch {
      return; // someone else has it
    }
    await ctx.progress('step', `${id} is on it`);
    await sleep(400);
    let text = '';
    for await (const t of brain.answer('', [{ who: 'lead', text: msg.content }])) text += t;
    await ctx.reply(text, { intent: 'answer' });
  });
  return a;
}
await lead.start();
await worker('ana').start();
await worker('ben').start();

console.log(`
Multi-agent task room — model: ${brain.name}
  open ${s.web}/ and write the task, e.g. "a short post about our v2 launch"
Press Ctrl+C to stop.`);
