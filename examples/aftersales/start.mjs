// After-sales assistant (docs/en/design/developer-experience.md, docs/en/product.md): a customer asks the shop's
// assistant about an order. It looks the order up, changes the address at
// once, and refunds only with approval: up to $30 the customer confirms it
// in a card, above that a manager (Sam) approves it in the desk. Asked for a
// person, it hands the conversation to Sam, who answers and hands it back.
// The assistant runs inside the server (docs/en/design/assistants.md): no agent process of yours.
//
//   npm run aftersales   then open http://localhost:3000 (customer)
//                        and http://localhost:3000/desk (Sam, the manager)

import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { start, as, idsOf } from '../lib/kit.mjs';
import { bundle } from '../lib/build.mjs';
import { shop, spec } from '../lib/shop.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
const people = { maya: 'Maya (customer)', sam: 'Sam (manager)' };
const state = {};

// Your shop: the orders API, and (without ANTHROPIC_API_KEY) a scripted model.
const acme = await shop();
const pages = await bundle(here, ['index', 'desk']);
const s = await start({
  name: 'aftersales',
  pages,
  // The orders API runs on this machine: let the server call private addresses.
  config: '[ai]\nprivate_addresses = true\n',
  routes: {
    '/api/session': (u) => {
      const who = u.searchParams.get('as');
      if (!people[who]) throw new Error('unknown person');
      return { url: s.url, token: s.token(who), conv: state.conv, me: state.ids[who], name: people[who], agent: state.ids.helpbot };
    },
  },
});

// The people and the assistant, as your backend would create them.
await s.im.upsertUser('maya', { display_name: 'Maya' });
await s.im.upsertUser('sam', { display_name: 'Sam' });
await s.im.upsertUser('helpbot', { kind: 'agent', owner: 'sam', display_name: 'Acme assistant' });

// The assistant, set up once by a developer (the console does the same):
// a model, the orders API with its approval rules, a role, and the hand-off.
const key = process.env.ANTHROPIC_API_KEY;
const brain = key
  ? { service: { provider: 'anthropic', api_key: key }, model: process.env.ANTHROPIC_MODEL ?? 'claude-sonnet-5-5' }
  : { service: { provider: 'deepseek', base_url: `${acme.url}/v1`, api_key: 'unused' }, model: 'deepseek-flash' };
await s.admin('PUT', '/model-services/main', brain.service);
await s.admin('PUT', '/connections/orders', {
  kind: 'api',
  url: `${acme.url}/api`,
  auth_header: 'X-Api-Key',
  secret: acme.apiKey,
  spec,
  // Every write asks first, except where a rule says otherwise (docs/en/design/assistants.md).
  writes: 'confirm',
  approvals: [
    // Above $30 (or no amount): Sam approves; up to $30: the customer confirms.
    { tool: 'createRefund', amount_field: 'amount', over: 30, approvers: ['sam'] },
    // Changing the address of an order not shipped yet: no approval.
    { tool: 'updateAddress', confirm: false },
  ],
});
await s.admin(
  'PUT',
  '/roles/aftersales',
  `---
name: aftersales
description: Acme's after-sales assistant
apis: [orders]
---
You are Acme's after-sales assistant. Help customers with their orders: look them up,
change the delivery address, refund them. Refund the order's full total unless the
customer asks for less. Be brief and friendly. If the customer asks for a person, or
you cannot help, hand the conversation to a person.
`,
);
await s.admin('PUT', '/agents/helpbot/role', { role: 'aftersales' });
await s.admin('PUT', '/agents/helpbot/assistant', { service: 'main', model: brain.model, settings: { handoff: true } });

// Maya's conversation with the shop: the assistant answers every message;
// Sam is in it to approve refunds and to take over.
const sam = await as(s.url, s.token('sam'));
state.ids = await idsOf(sam, ['maya', 'sam', 'helpbot']);
state.conv = (await s.im.createConversation({ as: 'maya', kind: 'group', title: 'Acme support', members: ['helpbot', 'sam'] })).id;
await sam.call('set_agent_policy', { conv_id: state.conv, agent_id: state.ids.helpbot, trigger: 'all' });
sam.close();
process.on('exit', () => acme.stop());

console.log(`
After-sales assistant — model: ${key ? brain.model : 'scripted (set ANTHROPIC_API_KEY for Claude)'}
  customer: ${s.web}/
  manager:  ${s.web}/desk
Try: "Where is order 1001?", "Refund order 1001", "Refund order 1002",
     "Change the address of order 1002 to 5 Main St", "I want to talk to a person".
Press Ctrl+C to stop.`);
