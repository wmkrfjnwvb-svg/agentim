// "Your shop's backend" for the after-sales example: an orders API the
// assistant calls (described by OpenAPI), and a scripted model service in
// the OpenAI chat format, so the example runs with no account anywhere.
// With ANTHROPIC_API_KEY set the server uses Claude instead of the script.

import http from 'node:http';

/** The orders, as your shop knows them. */
export const orders = {
  1001: { id: '1001', item: 'Wireless headphones', total: 25, status: 'delivered', address: '12 Rue Oberkampf, Paris' },
  1002: { id: '1002', item: 'Bluetooth speaker', total: 120, status: 'delivered', address: '12 Rue Oberkampf, Paris' },
};

/** What the assistant may call (OpenAPI 3; the operationIds are the tools). */
export const spec = {
  openapi: '3.0.3',
  info: { title: 'Acme orders', version: '1' },
  paths: {
    '/orders/{id}': {
      get: { operationId: 'getOrder', summary: 'Looks up an order: item, total, status, address', parameters: [{ name: 'id', in: 'path', required: true, schema: { type: 'string' } }] },
    },
    '/orders/{id}/address': {
      patch: {
        operationId: 'updateAddress',
        summary: 'Changes the delivery address of an order',
        parameters: [
          { name: 'id', in: 'path', required: true, schema: { type: 'string' } },
          { name: 'address', in: 'query', required: true, schema: { type: 'string' } },
        ],
      },
    },
    '/refunds': {
      post: {
        operationId: 'createRefund',
        summary: 'Refunds an order (amount in USD)',
        parameters: [
          { name: 'order', in: 'query', required: true, schema: { type: 'string' } },
          { name: 'amount', in: 'query', required: true, schema: { type: 'number' } },
        ],
      },
    },
  },
};

const API_KEY = 'shop-secret';

/** Starts the shop on a free port: `{ url, apiKey, refunds, stop }`. */
export async function shop() {
  const refunds = [];
  const server = http.createServer(async (req, res) => {
    const u = new URL(req.url, 'http://x');
    let body = '';
    for await (const c of req) body += c;
    const send = (status, v, type = 'application/json') => res.writeHead(status, { 'content-type': type }).end(typeof v === 'string' ? v : JSON.stringify(v));
    if (u.pathname === '/v1/models') return send(200, { data: [{ id: 'deepseek-flash' }] });
    if (u.pathname === '/v1/chat/completions') return send(200, completion(JSON.parse(body || '{}')), 'text/event-stream');
    if (!u.pathname.startsWith('/api/')) return send(404, {});
    if (req.headers['x-api-key'] !== API_KEY) return send(401, { error: 'bad key' });
    const p = u.pathname.slice(4);
    let m;
    if (req.method === 'GET' && (m = p.match(/^\/orders\/(\w+)$/))) {
      return orders[m[1]] ? send(200, orders[m[1]]) : send(404, { error: `no order ${m[1]}` });
    }
    if (req.method === 'PATCH' && (m = p.match(/^\/orders\/(\w+)\/address$/))) {
      const o = orders[m[1]];
      if (!o) return send(404, { error: `no order ${m[1]}` });
      o.address = u.searchParams.get('address') ?? o.address;
      return send(200, { order: o.id, address: o.address });
    }
    if (req.method === 'POST' && p === '/refunds') {
      // The same Idempotency-Key is the same refund (the server sends one).
      const key = req.headers['idempotency-key'];
      const old = refunds.find((r) => key && r.key === key);
      if (old) return send(200, old);
      const r = { id: `rf_${refunds.length + 1}`, order: u.searchParams.get('order'), amount: Number(u.searchParams.get('amount')), key };
      refunds.push(r);
      if (orders[r.order]) orders[r.order].status = 'refunded';
      return send(200, r);
    }
    send(404, {});
  });
  await new Promise((r) => server.listen(0, '127.0.0.1', r));
  const url = `http://127.0.0.1:${server.address().port}`;
  return { url, apiKey: API_KEY, refunds, stop: () => server.close() };
}

// ------------------------------------------------------------------ the script

const sse = (chunks) => chunks.map((c) => `data: ${JSON.stringify(c)}\n\n`).join('') + 'data: [DONE]\n\n';
const text = (t) =>
  sse([
    ...t.split(/(?<= )/).map((w) => ({ choices: [{ index: 0, delta: { content: w } }] })),
    { choices: [{ index: 0, delta: {}, finish_reason: 'stop' }] },
    { choices: [], usage: { prompt_tokens: 200, completion_tokens: 30 } },
  ]);
const tool = (n, name, args) =>
  sse([
    { choices: [{ index: 0, delta: { tool_calls: [{ index: 0, id: `call_${n}`, type: 'function', function: { name, arguments: JSON.stringify(args) } }] }, finish_reason: 'tool_calls' }] },
    { choices: [], usage: { prompt_tokens: 200, completion_tokens: 20 } },
  ]);
const content = (m) => (typeof m.content === 'string' ? m.content : (m.content ?? []).map((p) => p.text ?? '').join(''));

/** What a real model would do here, for the few things the demo asks. */
function completion(body) {
  const msgs = body.messages ?? [];
  const users = msgs.filter((m) => m.role === 'user').map(content);
  const all = msgs.filter((m) => m.role === 'user' || m.role === 'system').map(content).join('\n');
  const asked = (users.at(-1) ?? '').split('Answer this message:\n').at(-1);
  const tools = msgs.filter((m) => m.role === 'tool').map(content);
  const last = tools.at(-1);
  const order = asked.match(/\b(\d{4})\b/)?.[1];
  // After a person decided (the server's note).
  if (/approved it and it ran/.test(all)) {
    let r = {};
    try {
      r = JSON.parse(all.match(/The result:\n(\{.*\})/)?.[1] ?? '{}');
    } catch {}
    return text(r.amount ? `Done: $${r.amount} for order ${r.order} is on its way back to your card (refund ${r.id}). It shows up in 3–5 business days.` : 'Done.');
  }
  if (/declined, so it was not run/.test(all)) return text("The refund was declined, so I haven't made it. A person from our team can tell you more.");
  // After a tool ran in this answer.
  if (last !== undefined) {
    if (/^Not done yet/.test(last)) {
      return text(/not the person you are talking to/.test(last)
        ? 'Refunds this large need a manager to approve them. I have asked our team; you will see the answer here.'
        : 'Please confirm the refund in the card below; I make it once you approve.');
    }
    if (/"address"/.test(last) && /updateAddress|address/.test(asked)) return text(`Done: order ${order} now ships to ${JSON.parse(last).address ?? 'the new address'}.`);
    if (/takes the conversation over/.test(last)) return text("I've passed you to a person on our team. They'll answer here shortly.");
    if (/^Form sent:/.test(last)) return text('Please fill in the form above, and I’ll take it from there.');
    // The card is shown: one short sentence, no repeating it.
    if (/sees the card now/.test(last)) return text('Here are the details. Anything else I can do for this order?');
    try {
      const o = JSON.parse(last);
      if (o.item)
        return tool(tools.length, 'show_card', {
          title: `Order ${o.id}`,
          blocks: [{ type: 'facts', items: [
            { label: 'Item', value: o.item },
            { label: 'Total', value: o.total, format: 'currency:USD' },
            { label: 'Status', value: o.status[0].toUpperCase() + o.status.slice(1), tone: o.status === 'delivered' ? 'success' : o.status === 'refunded' ? 'info' : 'warning' },
            { label: 'Ships to', value: o.address },
          ] }],
        });
    } catch {}
    return text('Sorry, I could not do that just now.');
  }
  // A new question.
  if (/\b(person|human|manager|someone)\b/i.test(asked)) return tool(0, 'handoff_to_person', { note: `The customer asked: "${asked.replace(/^[^:]*: /, '')}"` });
  // The answers to the refund form: now the refund (which may need approval).
  if (order && /filled in the form "Refund order/.test(all)) return tool(0, 'orders__createRefund', { order, amount: orders[order]?.total ?? 0 });
  // A refund: first why, with a form.
  if (order && /refund/i.test(asked))
    return tool(0, 'ask_form', {
      title: `Refund order ${order}`,
      intro: 'Two quick questions, then I’ll start the refund.',
      submit: 'Request refund',
      fields: [
        { id: 'reason', label: 'Reason', type: 'select', options: ['Arrived late', 'Damaged', 'Not as described', 'Changed my mind'], required: true },
        { id: 'details', label: 'Anything else?', type: 'textarea', placeholder: 'Optional' },
      ],
    });
  if (order && /address/i.test(asked)) {
    const to = asked.match(/\bto (.+)$/i)?.[1]?.replace(/[.!]$/, '') ?? '';
    return tool(0, 'orders__updateAddress', { id: order, address: to });
  }
  if (order) return tool(0, 'orders__getOrder', { id: order });
  return text("Hi! I'm the Acme assistant. I can look up an order, change its delivery address or refund it. Which order is it (e.g. 1001)?");
}
