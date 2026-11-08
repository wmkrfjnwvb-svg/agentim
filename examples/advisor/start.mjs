// An advisor and a client, with an AI aide (docs/en/design/developer-experience.md, docs/en/product.md): Chris
// (the client) writes to a financial advisor, Ava. Ava's aide drafts each
// reply; Chris sees nothing until Ava approves it, edited or not (approval
// rules, docs/en/design/agents.md). The aide also passes Ava private notes, which only Ava
// sees (`to_user`), e.g. what to check before answering.
//
//   npm run advisor      then open http://localhost:3000 (Ava, the advisor)
//                        and http://localhost:3000/?as=chris (Chris, the client)

import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { randomUUID } from 'node:crypto';
import { Agent } from '@agentimhq/agent';
import { start, as, idsOf } from '../lib/kit.mjs';
import { bundle } from '../lib/build.mjs';
import { model, transcript } from '../lib/model.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
const people = { ava: 'Ava (advisor)', chris: 'Chris (client)' };
const state = {};

const pages = await bundle(here, ['index']);
const s = await start({
  name: 'advisor',
  pages,
  routes: {
    '/api/session': (u) => {
      const who = u.searchParams.get('as');
      if (!people[who]) throw new Error('unknown person');
      return { url: s.url, token: s.token(who), conv: state.conv, me: state.ids[who], name: people[who], advisor: who === 'ava', agent: state.ids.aide };
    },
  },
});

await s.im.upsertUser('ava', { display_name: 'Ava' });
await s.im.upsertUser('chris', { display_name: 'Chris' });
const aide = await s.im.upsertUser('aide', { kind: 'agent', owner: 'ava', display_name: 'Ava’s aide' });
state.conv = (await s.im.createConversation({ as: 'ava', kind: 'group', title: 'Chris — retirement plan', members: ['chris', 'aide'] })).id;
const ava = await as(s.url, s.token('ava'));
state.ids = await idsOf(ava, ['ava', 'chris', 'aide']);
// The aide answers every message, and Ava approves each of its messages
// before Chris sees it.
await ava.call('set_agent_policy', { conv_id: state.conv, agent_id: state.ids.aide, trigger: 'all', approval: { mode: 'all' } });
ava.close();

// What the aide tells Ava (privately) and drafts for Chris.
const notes = [
  [/(sell|cash out|everything)/i, 'Chris wants to sell everything. Check the risk profile (moderate) and the tax on gains before answering; a suitability note is needed.'],
  [/(crypto|bitcoin)/i, 'Chris asks about crypto. Our policy: at most 5% of the portfolio, and only after a risk talk.'],
  'Nothing to flag. Chris has a moderate risk profile and retires in 2031.',
];
const brain = model([
  [/(sell|cash out|everything)/i, 'I understand the worry when markets fall. Before selling, let’s look at your plan together: selling now would lock in losses and may mean tax on past gains. Could we talk on Thursday?'],
  [/(crypto|bitcoin)/i, 'Crypto can be part of a plan, but only a small part. Let’s talk about how much risk you are comfortable with first.'],
  'Thanks, Chris. Let me look at this and get back to you today.',
]);
const SYSTEM = 'You draft replies for a financial advisor to a client. Plain, careful, no promises of returns. The advisor reviews every draft.';

const agent = new Agent({ url: s.url, key: aide.key, log: (m) => console.log(`[aide] ${m}`) });
agent.onMessage(async (msg, ctx) => {
  if (msg.sender_id !== state.ids.chris) return; // only the client's messages
  // A private note to Ava: only Ava (and the aide) sees it.
  const note = notes.find((n) => Array.isArray(n) && n[0].test(msg.content))?.[1] ?? notes.at(-1);
  await agent.call('send', { conv_id: ctx.convId, client_msg_id: randomUUID(), text: note, to_user: 'ava' });
  // The reply: a draft until Ava approves it.
  let text = '';
  for await (const t of brain.answer(SYSTEM, await transcript(agent, ctx.convId))) text += t;
  const r = await ctx.reply(text);
  if (r.draft_id) console.log(`[aide] draft ${r.draft_id} waits for Ava`);
});
await agent.start();

console.log(`
Advisor and client with an AI aide — model: ${brain.name}
  Ava (advisor, approves drafts): ${s.web}/
  Chris (client):                 ${s.web}/?as=chris
Write as Chris, e.g. "Markets are down, should I sell everything?". Press Ctrl+C to stop.`);
