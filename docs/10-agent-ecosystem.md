# 10 和 Agent 生态的关系：集成、自己做、不做（方案待确认：D48–D52）

调研详情见 [research/agent-ecosystem-2026.md](research/agent-ecosystem-2026.md)（Agent SDK 和平台、周边组件、大模型基础设施）。

## 1. 定位

Agent 生态已经很拥挤：写 Agent 有十几个成熟的 SDK，记忆、工具、沙箱、观测、模型网关都有专门的产品。**我们不和它们竞争，而是做它们都缺的那一层**：

- 所有 SDK 的"会话"都是一个人对一个 Agent 的一次运行，没有成员、没有多方之间的顺序、没有投递保证；Claude 的会话存储官方说明是"尽力而为、失败会丢"。
- 所有 SDK 都有"暂停等人批准"，但都把"谁有权批准、只生效一次、持久保存、审计"留给开发者自己做（OpenAI 文档原话如此）。
- 托管平台（Managed Agents、AgentCore、Foundry、Google Agent Runtime）都想自己保存状态，而且有的不支持零数据保留、有的可能用存储的内容改进服务；客户要的是一个中立、可自托管、数据归自己的地方。

所以 AgentIM 是：**任何框架写的 Agent 都能接进来的、持久、有序、多方、可治理的通信和记录层**，单个 Rust 程序，不绑定任何云。

## 2. 要提供的集成（按价值排序，D48）

| # | 集成 | 内容 | 成本 |
|---|---|---|---|
| 1 | MCP 服务 | 发消息、流式回复、读历史、列会话、请人决定、交给人；所有 SDK 都是 MCP 客户端，一次接入覆盖全部 | 低 |
| 2 | 人工介入桥 | 各 SDK 暂停的方式不同（OpenAI 的中断 + 运行状态、Claude 的 `canUseTool`、LangGraph 的 `interrupt`、Vercel 的 `needsApproval`、Pydantic AI 的延迟工具、Temporal 的信号）；每个 SDK 一个小助手：把暂停的状态作为不透明数据挂在我们的"请人决定"上，决定后恢复；持久化执行引擎通过回调收到决定 | 中 |
| 3 | Agent SDK（Python、TS 先做） | `@agentim.on_message(...)` 这样的入口；把我们的流式回复对接到各 SDK 的事件流（OpenAI、Vercel、AG-UI） | 中 |
| 4 | 会话存储适配器 | 让 AgentIM 成为这些 SDK 的历史存储：OpenAI `Session`（4 个方法）、Claude `SessionStore`（有官方一致性测试）、LangGraph 检查点；先做这三个。SDK 的内部条目是模型专属、不透明的，作为隐藏的"Agent 状态"条目存（即 [09 §6](09-agent-communication.md) 的 L2），不和会话里可见的消息混在一起 | 每个中等 |
| 5 | A2A 网关 | 每个 Agent 一张 Agent Card；A2A 任务对应我们的会话和任务（[03 §9.6](03-ai-native.md)） | 中高 |
| 6 | 反过来：AgentIM 作为一个渠道 | 给 Vercel Chat SDK 写 AgentIM 适配器、给 Agno 写接口、做一个自托管 ChatKit 的桥。**OpenAI Agent Builder 2026-11-30 停用，用户要迁移到自托管 ChatKit，这是一个时间窗口** | 每个低 |
| 7 | OpenTelemetry 导出 | 投递、审批、接管的追踪和 Agent 自己的追踪连起来（消息里带 `traceparent`）；不做追踪界面 | 低 |
| 8 | AG-UI | 有争议（有观点认为会被基于 MCP 的界面取代），放在原生 SDK 之后 | 中 |

其他集成（排在后面）：企业身份（接受 Okta ID-JAG、Entra Agent ID 的令牌，D50）；可插拔的护栏分类器（D51）；第三方 OAuth 令牌交给 Auth0 Token Vault / Arcade 保管；记忆连接器（把会话事件推给 Mem0、Zep、Letta，**删除请求跟着传过去**）；发布到 AGNTCY / MCP 注册表；沙箱和浏览器的实时画面作为结构化消息、接管就是把浏览器交给人。

## 3. 自己做（属于通信和治理的核心）

1. Agent 注册、负责人、凭据、急停（M6 已计划）。
2. 统一的"请人决定"、绑定参数的审批、接管：MCP 的 `input_required`、A2A 的 `input-required`、LangGraph 的 `interrupt`、Temporal 的信号、AP2 的支付授权都汇到这里。
3. 每条内容的来源和信任标记（人、Agent、外部 A2A、工具输出），对应 OWASP Agentic 风险和"防注入设计模式"。
4. 通信策略（谁能给谁发、什么级别的数据能出去、预算），在进程内执行，不在热路径上调用外部服务。
5. 审计和 AI 标记（EU AI Act 第 50 条）。
6. MCP 服务、A2A 网关（协议入口）。
7. **用量计量**（D49）：每次模型调用的用量事件（Agent、负责人、单元、会话 / 消息、厂商、模型、区域、输入 / 输出 / 缓存 / 推理 token、费用），用厂商返回的数字；可版本化的价格表；按 Agent、负责人、单元汇总，软上限提醒、硬上限暂停；另外统计业务单位（会话、解决的问题、批准的动作），方便客户按结果或按动作计费。

## 4. 不做（别人已经做好）

Agent 循环、工具调用、交接、模型抽象；可视化 Agent 搭建器（OpenAI 都在停用）；长期语义记忆、RAG；沙箱、代码执行、浏览器；支付通道；通用工作流引擎；追踪界面和评估平台；第三方 API 的工具网关和注册表；实时语音管线；各种 IM 连接器（Slack、Teams、WhatsApp 桥）。（D52）

## 5. 模型调用（D49）

不内置完整的多厂商模型网关：功能已经商品化（LiteLLM、Portkey、Envoy、Kong、Azure APIM），各家 API 一直在变维护成本高，网关存着所有厂商的密钥是供应链攻击的目标（LiteLLM 2026-03），而且很多 Agent 直接调模型、本来就绕过网关。建议：

1. **先做用量上报接口**（M6）：直接调模型的 Agent 上报用量，计量和预算照样工作；标明"自报"。
2. **可选的轻量模型出口**（之后）：Rust 写的透传代理，只支持 OpenAI 兼容和 Anthropic Messages 两种格式；每个 Agent 一个虚拟密钥、预算、token 上限、**区域白名单**（例如只允许欧盟）、审计和消息关联；其他需求转给客户自选的上游网关。经过它的用量标为"强制"。
3. **自托管模型**：提供和 Ollama（开发）、vLLM / SGLang（生产）、llm-d（Kubernetes）配合的部署示例；提醒许可陷阱（Llama 多模态在欧盟不可用、部分模型的来源问题）；不打包模型权重。

## 6. 欧盟合规相关（补充到 [04](04-security-compliance.md)）

- 第 50 条：Agent 身份和消息上的 AI 标记（[03 §9.6.2](03-ai-native.md)）。
- 按组织设置模型厂商和区域白名单；Anthropic 自己的 API 没有欧盟区域这一点要在界面上可见。
- Agent 注册信息里记录厂商、模型、区域、是否零数据保留，作为 GDPR 第 30 条的处理记录和 DPIA 的依据。
- 提示词和回复的记录可以关闭或脱敏；删除请求要传到用量记录（只保留汇总）和外部适配器。
