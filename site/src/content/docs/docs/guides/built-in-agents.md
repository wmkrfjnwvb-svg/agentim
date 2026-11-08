---
title: "Built-in agents"
description: "Agents the server runs for you: pick a model, give them a role, knowledge, skills and connections."
sidebar:
  order: 1
---

A built-in agent is an agent the AgentIM server answers for itself. You choose a model service and a model; the agent then answers in conversations where it is messaged directly or mentioned, streaming its reply, with its role's instructions and the conversation as context. No code runs on your side.

## 1. A model service

Under **AI models** in the console (or `PUT /admin/v1/app/model-services/{name}`), add a provider and your key:

| Provider | `provider` | Models |
|---|---|---|
| Claude (Anthropic) | `anthropic` | from Anthropic's model list |
| OpenAI (including Codex models) | `openai` | from OpenAI's model list (chat models only) |
| Google Gemini | `gemini` | through Google's OpenAI-compatible endpoint |
| DeepSeek | `deepseek` | DeepSeek's models |
| Any OpenAI-compatible service | `compatible` | with your `base_url` |

The key is checked with the provider before it is saved, stored encrypted, and never shown again (only its last four characters). A deployment can also offer services with its own keys, read from environment variables:

```toml
[[ai.builtin]]
name = "claude"
provider = "anthropic"
key_env = "AGENTIM_AI_ANTHROPIC_KEY"   # the key itself never goes in a file
daily_tokens = 2000000                  # per app per day; 0 = no limit
```

With a deployment's key only models with a known price can be used, so every call's cost is known (the price table ships in `builtin/prices.toml`; replace it with `ai.prices_file`).

## 2. An agent

Under **Built-in agents**, enable one of five ready roles — `assistant`, `translator`, `notes`, `support`, `moderator` — with a maintainer, a service and a model. That creates a team agent with the role. Or turn any existing agent into a built-in one:

```bash
curl -X PUT "$AGENTIM/admin/v1/app/agents/helper/assistant" \
  -H "Authorization: Bearer $ADMIN_KEY" -H "content-type: application/json" \
  -d '{"service": "claude", "model": "claude-sonnet-5-5",
       "settings": {"replies_per_minute": 10, "replies_per_day": 1000, "only_knowledge": false}}'
```

Its instructions are its [role](/docs/concepts/agents/) (a Markdown file). Each message gets exactly one reply, even across restarts and cluster nodes. When the model can't answer, the agent says so (your `fallback` text); limits (replies per minute and per day, tokens per day per service) answer with a short notice instead of calling the model.

## 3. Knowledge

Upload files to an agent (`PUT …/agents/{id}/knowledge/{file name}` with the file as the body): Markdown, text, CSV, JSON or PDF, up to 5 MB each and 50 files. The text is stored encrypted and searched for each question; the best-matching parts go into the prompt and the agent cites them by name. With `only_knowledge` it answers only from them, and with `handoff` it hands questions they don't cover to a person.

## 4. Skills and connections

A role's header names the skills and connections its agents use:

```markdown
---
name: support
description: Answers customers
skills: [refunds]
mcp: [shop]
apis: [orders]
---
Answer customers politely…
```

- **Skills** use the Agent Skills format: a `SKILL.md` (with `name` and `description`) and reference files, under **Skills** or `PUT /admin/v1/app/skills/{name}`. The agent sees each skill's name and description and loads the rest when it needs it. Scripts are not run.
- **Connections** are remote MCP servers (Streamable HTTP) and HTTP APIs described by an OpenAPI 3 document, under **Connections** or `PUT /admin/v1/app/connections/{name}`. Their tools become functions the model can call. Only read-only tools are used unless you allow writes. With **Ask first** (`writes: confirm`), each call that changes something waits: the agent asks the person it is answering to approve it (the request shows the tool and its arguments, never the credential), runs it once after approval with the arguments that were shown, and then tells them the result. A refusal, an expiry (one hour) or a connection no longer allowing it means the call never runs. The credential is stored encrypted and sent only to the connection's own address.

Agents you run yourself with an [Agent SDK](/docs/agent-sdks/) get the same skills and connections in their configuration. The SDKs can write them to disk where Claude Code and the Claude Agent SDK look (`.claude/skills`, `.mcp.json`): see [Skills and MCP servers on disk](/docs/agent-sdks/#skills-and-mcp-servers-on-disk).

## Usage and cost

`GET /admin/v1/app/assistants/usage` lists every model call with its tokens (input, output, cache reads and writes) and cost in nano-dollars, never the prompt or the reply. On AgentIM Cloud, built-in keys come with the Pro and Scale plans, and tokens are paid from prepaid AI credit at the provider's official prices; the first top-up chooses the provider.
