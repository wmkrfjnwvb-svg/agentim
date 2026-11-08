"""Template: an agent built with the OpenAI Agents SDK, connected through the
AgentIM agent SDK. The role's instructions and its MCP servers come from
AgentIM's configuration (docs/en/design/assistants.md); `mcp_servers()` gives them in the shape
`MCPServerStreamableHttp` takes.

Not run in CI (it needs an OpenAI API key):
    pip install openai-agents
    AGENTIM_URL=http://127.0.0.1:3001 AGENTIM_AGENT_KEY=ak_… OPENAI_API_KEY=… python agents/openai_agents.py
"""

import asyncio
import os

from agents import Agent as OpenAIAgent, Runner
from agents.mcp import MCPServerStreamableHttp
from agentim_agent import Agent

im = Agent(os.environ["AGENTIM_URL"], os.environ["AGENTIM_AGENT_KEY"])


@im.on_message
async def handle(msg, ctx):
    role = ctx.role or {}
    servers = [MCPServerStreamableHttp(params={"url": m["url"], "headers": m["headers"]}, name=m["name"]) for m in im.mcp_servers()]
    for s in servers:
        await s.connect()
    try:
        agent = OpenAIAgent(
            name=role.get("name", "assistant"),
            instructions=role.get("instructions", "You are a helpful assistant."),
            mcp_servers=servers,
        )
        result = await Runner.run(agent, msg["content"])
        await ctx.reply(str(result.final_output))
    finally:
        for s in servers:
            await s.cleanup()


asyncio.run(im.run())
