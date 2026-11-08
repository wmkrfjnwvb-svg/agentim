"""Template: a LangGraph agent connected through the AgentIM agent SDK. Each
AgentIM conversation is a LangGraph thread, so the graph keeps its own state
per conversation; the role's instructions come from AgentIM.

Not run in CI (it needs a model key):
    pip install langgraph "langchain[anthropic]"
    AGENTIM_URL=http://127.0.0.1:3001 AGENTIM_AGENT_KEY=ak_… ANTHROPIC_API_KEY=… python agents/langgraph_agent.py
"""

import asyncio
import os

from langgraph.checkpoint.memory import InMemorySaver
from langgraph.prebuilt import create_react_agent
from agentim_agent import Agent

im = Agent(os.environ["AGENTIM_URL"], os.environ["AGENTIM_AGENT_KEY"])
graphs = {}


def graph_for(instructions):
    if instructions not in graphs:
        graphs[instructions] = create_react_agent(
            os.environ.get("LANGGRAPH_MODEL", "anthropic:claude-sonnet-5-5"),
            tools=[],
            prompt=instructions,
            checkpointer=InMemorySaver(),
        )
    return graphs[instructions]


@im.on_message
async def handle(msg, ctx):
    instructions = (ctx.role or {}).get("instructions", "You are a helpful assistant.")
    out = await graph_for(instructions).ainvoke(
        {"messages": [{"role": "user", "content": msg["content"]}]},
        {"configurable": {"thread_id": ctx.conv_id}},
    )
    await ctx.reply(out["messages"][-1].content)


asyncio.run(im.run())
