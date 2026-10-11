# 调研：Agent 生态全景——SDK、组件、大模型基础设施（2026-10-11）

结论和设计见 [10 和 Agent 生态的关系](../10-agent-ecosystem.md)。标注：[已核实] 查看了官方来源；[二手] 只有二手来源，引用前再核实；[推断] 我们的判断。

## 1. Agent SDK 和平台

| SDK / 平台 | 语言 | GitHub 星数 / 许可 | 历史怎么存 | 人工介入 | 对外接口 |
|---|---|---|---|---|---|
| OpenAI Agents SDK | Python、JS | 3 万（py）/ MIT | **可插拔的 `Session` 接口**（4 个方法），自带 SQLite、Redis、SQLAlchemy、MongoDB 等实现 [已核实，[文档](https://openai.github.io/openai-agents-python/sessions/)] | 工具需要批准时返回中断，运行状态可序列化、在别的进程恢复；**审批人认证、只消费一次、服务端保存快照都留给开发者自己做** [已核实，[文档](https://openai.github.io/openai-agents-python/human_in_the_loop/)] | MCP；可插拔的追踪 |
| OpenAI AgentKit / ChatKit | 托管 + JS / Python | chatkit-js 2 千 / Apache-2.0 | 自托管 ChatKit 要自己实现 `Store` | — | **Agent Builder 2026-11-30 停用**，官方让用户改用自托管 ChatKit [已核实，[文档](https://developers.openai.com/api/docs/guides/chatkit.md)] |
| Claude Agent SDK | Python、TS | 8 千（py）/ Anthropic 商业条款 | 本地 JSONL + **`SessionStore` 适配接口**（`append` / `load`，附一致性测试；官方说明它是"尽力而为的镜像"，失败时会丢批次）[已核实，[文档](https://code.claude.com/docs/en/agent-sdk/session-storage)] | `canUseTool` 回调、权限模式 [已核实] | MCP、子 Agent、技能 |
| Claude Managed Agents | REST | 2026-04 起公测 | 服务端事件日志；**不适用零数据保留和 HIPAA** [已核实，[文档](https://platform.claude.com/docs/en/managed-agents/overview)] | 运行中可以插话、打断 | MCP |
| Google ADK | Python、Go、Java、JS | 2.2 万（py）/ Apache-2.0 | `SessionService`：内存、数据库、Vertex [已核实，[文档](https://adk.dev/sessions/session/)] | 工具确认 | A2A（Google 发起）、AG-UI |
| Microsoft Agent Framework 1.0 | .NET、Python | 1.4 万 / MIT | 可插拔的聊天历史存储 [推断] | 工作流 | MCP、AG-UI；Semantic Kernel 和 AutoGen 只维护不再发展 [已核实，[博客](https://devblogs.microsoft.com/agent-framework/microsoft-agent-framework-version-1-0/)] |
| AWS Strands + AgentCore | Python、TS | 8.8 千 / Apache-2.0 | 文件、S3 | — | AgentCore：运行环境、记忆、网关（MCP）、身份、观测（OTel）、策略（Cedar）、注册表、评估、支付 [已核实，[文档](https://docs.aws.amazon.com/bedrock-agentcore/latest/devguide/what-is-bedrock-agentcore.html)] |
| LangChain / LangGraph v1 | Python、JS | LangChain 14.8 万、LangGraph 4.3 万 / MIT | 按 `thread_id` 的检查点（内存、SQLite、Postgres）+ 长期存储 [已核实，[文档](https://docs.langchain.com/oss/python/langgraph/persistence)] | `interrupt()` 暂停、`Command(resume=...)` 继续 | — |
| CrewAI | Python | 6 万 / MIT | 内部记忆 | 任务上的人工输入 | 2026-01 加了 A2A [二手] |
| Mastra | TS | 2.9 万 | 每类数据可以用不同的存储 [已核实] | 有 | MCP、AG-UI |
| Pydantic AI | Python | 2.1 万 / MIT | 自己保存 | 延迟工具 | MCP、AG-UI、OTel；8 种持久化执行后端 [已核实，[文档](https://pydantic.dev/docs/ai/overview/)] |
| Vercel AI SDK v6 / v7 | TS | 2.7 万 | 自己保存 | `needsApproval` [已核实] | **Chat SDK：Slack、Teams、Google Chat、Discord、Telegram、WhatsApp 等适配器，支持自定义适配器** [已核实，[README](https://cdn.jsdelivr.net/gh/vercel/chat@main/README.md)] |
| Agno | Python | 4.3 万 / Apache-2.0 | 数据库，按（用户，会话） | — | Slack、WhatsApp、Telegram、A2A 接口 [已核实] |
| Cloudflare Agents | TS | 5.8 千 / MIT | 每个 Agent 一个 SQLite | 有 | 聊天、语音、邮件、MCP [已核实] |
| Letta | Python | 2.5 万 / Apache-2.0 | 自己的 Postgres；Agent File 导出 | — | REST、MCP |
| Temporal / Restate / Inngest / DBOS | 多语言 | — | 自己的日志 | 信号 / 等待事件 | 和 OpenAI Agents SDK、Pydantic AI、Vercel AI SDK 有官方集成 [已核实] |

**所有 SDK 的"会话"都是一个人对一个 Agent 的一次运行**：没有成员、没有多方之间的顺序保证、没有已读、没有投递保证。这正是 AgentIM 补的空白。

## 2. Agent 周边组件

| 类别 | 主要玩家 | 要点 |
|---|---|---|
| 记忆 | Mem0、Zep / Graphiti、Letta、Cognee、AgentCore Memory、Vertex Memory Bank | 没有统一接口；从对话里抽取事实、跨会话检索；抽出来的记忆离开了原有的保留期、删除、加密边界 [二手 / 已核实] |
| 工具网关和注册表 | 官方 MCP 注册表（预览）、Docker / Kong / 微软 MCP 网关、Arcade（2026-08 收购 Smithery）、Composio、Zapier MCP | MCP 已移交 Linux 基金会的 Agentic AI Foundation [已核实]；**第三方 OAuth 令牌保管交给 Auth0 Token Vault、Arcade 等，自己存刷新令牌是负担** |
| 沙箱、浏览器 | E2B、Daytona、Vercel Sandbox、Cloudflare、Browserbase、Steel、Browser Use、Anthropic / OpenAI 的电脑操作 | 这是 Agent 用的工具，不是通信层；和我们的接触点：危险操作前审批、把截图和进度发进会话、浏览器"实时画面"交给人接管 |
| 身份和授权 | Entra Agent ID（GA）、Okta Cross App Access（ID-JAG，IETF 草案；MCP 的企业授权扩展 SEP-990 已稳定）、OpenFGA（CNCF 孵化）、Cedar（CNCF 沙箱） | 企业会要求用自己的身份系统给 Agent 认证；成员权限检查必须留在我们进程内，外部授权服务只用于不在热路径上的企业策略 |
| 支付 | Google AP2、OpenAI + Stripe ACP、x402、Visa / Mastercard Agent 令牌 | 标准还在分裂；对我们只是"Agent 要花钱 → 带金额和商户的审批" |
| 观测和评估 | OTel GenAI 约定（仍是开发中）、Langfuse（MIT，2026-01 被 ClickHouse 收购）、LangSmith、Arize Phoenix、Braintrust、Datadog | 导出 OTel 就能接所有后端 |
| 护栏和安全 | OWASP Agentic Top 10（2025-12）、NeMo Guardrails、LlamaFirewall、Lakera（被 Check Point 收购）、Protect AI（被 Palo Alto 收购）；研究：CaMeL、"防提示词注入的设计模式" | 分类器是商品、变化快，做成可插拔；**架构层的防御（内容来源标记、绑定参数的审批、最小权限、按发送者的信任等级）属于通信层，只有我们知道每段内容是谁写的** |
| 持久化执行 | Temporal、Restate、DBOS、Inngest、LangGraph | 让 Agent 自己的进程不怕崩溃；我们是多方共享的持久记录；接口：幂等发送（已有）+ "等人决定"用信号回调 |
| 发现和目录 | A2A Agent Card、AGNTCY 目录、MIT NANDA、MCP 注册表 | 我们导出签名的 Agent Card，发布到外部目录是小适配器 |

## 3. 大模型基础设施

| 方面 | 要点 |
|---|---|
| API 形态 | 两种主流：OpenAI Chat Completions（几乎都支持）和 Responses / Open Responses（2026-01 发布的多厂商规范）；第三种是 Anthropic Messages（Ollama 也支持了）[已核实 / 二手] |
| 价格 | 缓存命中便宜 90%–97.5%，批量便宜 50%，可以叠加；Claude 4.6 起 100 万上下文按标准价，Gemini 超过 20 万约 2 倍；**同样的文字在不同模型上 token 数不同（Claude 4.7 起多约 30%）**，计量必须用厂商返回的用量，不能自己估算 [已核实] |
| 计费模式 | 按结果（Intercom Fin 每次解决 $0.99）、按动作（Salesforce Agentforce 每次约 $0.10）、按 token、座位 + 用量 [已核实 / 二手] |
| 自托管推理 | vLLM（生产默认）、SGLang（适合多轮聊天）、llama.cpp / Ollama（单机）、TensorRT-LLM / NIM（NVIDIA）、llm-d（Kubernetes，2026-03 进入 CNCF 沙箱）；HF TGI 已只维护不发展 [已核实] |
| 模型网关 | LiteLLM（**2026-03 发生供应链攻击**，恶意版本窃取 API 密钥）、Portkey（开源后据称被 Palo Alto 收购）、Envoy AI Gateway、Kong（按 token 限流是企业版功能）、Azure APIM、OpenRouter、Cloudflare [已核实 / 二手] |
| 欧盟数据驻留 | Azure OpenAI EU 数据区、AWS Bedrock `eu.*`、OpenAI 欧洲驻留（新项目、零数据保留）；**Anthropic 自己的 API 没有欧盟区域**，要在欧盟用 Claude 得走 Bedrock、Vertex 或 Azure；Mistral 在欧盟、开放权重 [已核实] |
| 许可陷阱 | Llama 4 多模态模型的许可排除欧盟；DeepSeek 在意大利被下架、德国要求下架，买家会质疑来源 [已核实 / 二手] |
| 欧盟 AI 法案 | 通用模型提供者的义务 2025-08 起；第 50 条透明度 2026-08-02 起（未推迟）；高风险推迟到 2027-12 / 2028-08；**自托管 AgentIM 的客户是"部署者"**，我们要让他们容易做到告知、记录、人工监督 |
| 语音 | OpenAI Realtime、LiveKit Agents（开源）、Pipecat、Twilio；以后做通话时媒体交给 LiveKit / SIP，我们只管信令、把转写存进日志、人工接管 |
