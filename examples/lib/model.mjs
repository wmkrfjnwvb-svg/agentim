// The model behind the examples' agents. Without a key it is a scripted fake,
// so every example runs with no account anywhere; with ANTHROPIC_API_KEY set
// it is Claude (ANTHROPIC_MODEL, default claude-sonnet-5-5). Your own agent
// would use its framework here (see ../agents/).

/**
 * `script`: `[[/pattern/i, 'answer'], …, 'fallback']` for the fake.
 * Returns `{ name, answer(system, transcript) }`, an async generator of text.
 */
export function model(script) {
  const key = process.env.ANTHROPIC_API_KEY;
  if (key) return claude(key, process.env.ANTHROPIC_MODEL ?? 'claude-sonnet-5-5');
  return {
    name: 'scripted (set ANTHROPIC_API_KEY for a real model)',
    async *answer(_system, transcript) {
      const last = transcript.at(-1)?.text ?? '';
      const hit = script.find((s) => Array.isArray(s) && s[0].test(last));
      const text = hit ? hit[1] : script.find((s) => typeof s === 'string') ?? 'OK.';
      for (const w of text.split(/(?<= )/)) {
        await new Promise((r) => setTimeout(r, 40));
        yield w;
      }
    },
  };
}

function claude(key, modelId) {
  return {
    name: modelId,
    async *answer(system, transcript) {
      const r = await fetch('https://api.anthropic.com/v1/messages', {
        method: 'POST',
        headers: { 'x-api-key': key, 'anthropic-version': '2023-06-01', 'content-type': 'application/json' },
        body: JSON.stringify({
          model: modelId,
          max_tokens: 800,
          system,
          messages: [{ role: 'user', content: transcript.map((m) => `${m.who}: ${m.text}`).join('\n') }],
        }),
      });
      if (!r.ok) throw new Error(`model: ${r.status} ${await r.text()}`);
      const j = await r.json();
      yield j.content.filter((c) => c.type === 'text').map((c) => c.text).join('');
    },
  };
}

/** The conversation so far, as the model reads it. */
export async function transcript(agent, convId, maxTokens = 2000) {
  const c = await agent.context(convId, maxTokens);
  const seen = new Map();
  for (const e of c.events ?? []) {
    const m = e.event?.msg;
    if (m?.content) seen.set(m.msg_id, { who: m.sender_name ?? m.sender_id, text: m.content });
  }
  return [...seen.values()];
}
