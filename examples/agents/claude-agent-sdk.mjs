// Template: an agent built with the Claude Agent SDK, connected through the
// AgentIM agent SDK. The agent's role, skills and MCP servers come from
// AgentIM (docs/en/design/assistants.md): `files` writes them where Claude Code looks
// (.claude/skills, .mcp.json) and keeps them current.
//
// Not run in CI (it needs an Anthropic API key):
//   npm install @anthropic-ai/claude-agent-sdk
//   AGENTIM_URL=http://127.0.0.1:3001 AGENTIM_AGENT_KEY=ak_… ANTHROPIC_API_KEY=… node agents/claude-agent-sdk.mjs

import { query } from '@anthropic-ai/claude-agent-sdk';
import { Agent } from '@agentimhq/agent';

const workdir = process.env.WORKDIR ?? './agent-workdir';
const agent = new Agent({ url: process.env.AGENTIM_URL, key: process.env.AGENTIM_AGENT_KEY, files: { dir: workdir } });

agent.onMessage(async (msg, ctx) => {
  const s = await ctx.stream();
  try {
    for await (const m of query({
      prompt: msg.content,
      options: {
        cwd: workdir,
        // The role's instructions from AgentIM.
        systemPrompt: ctx.role?.instructions,
        // Load the skills and MCP servers AgentIM wrote into the folder.
        settingSources: ['project'],
      },
    })) {
      if (m.type === 'result' && m.subtype === 'success') await s.append(m.result);
    }
    await s.end();
  } catch (e) {
    await s.append(`Sorry, something went wrong (${e.message}).`);
    await s.end();
  }
});

await agent.start();
console.log('Claude Agent SDK agent connected');
