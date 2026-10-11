# 调研：Agent 之间的通信和 Agent 历史（2026-10-11）

结论和设计见 [09 Agent 通信与 Agent 历史](../09-agent-communication.md)。标注：[已核实] 查看了原始来源；[二手] 只有二手来源；[推断] 我们的判断。

## 1. 多 Agent 框架怎么决定谁收、谁行动

| 框架 | 方式 | 来源 |
|---|---|---|
| AutoGen SelectorGroupChat | 共享线程；LLM 选下一个发言者，发言者的回复广播给其他人；默认不让同一个连续发言 [已核实] | [文档](https://microsoft.github.io/autogen/0.7.3/user-guide/agentchat-user-guide/selector-group-chat.html) |
| AG2 GroupChat | 主持人；选择方式 auto / manual / random / round_robin；可以用转移图限制谁之后能说话 [已核实] | [文档](https://docs.ag2.ai/latest/docs/user-guide/advanced-concepts/groupchat/groupchat/)、[FSM](https://microsoft.github.io/autogen/0.2/blog/2024/02/11/FSM-GroupChat) |
| AutoGen Core | 发布订阅：按主题类型订阅，每个线程按需创建一个 Agent 实例 [已核实] | [文档](https://microsoft.github.io/autogen/stable/user-guide/core-user-guide/core-concepts/topic-and-subscription.html) |
| Magentic-One | 协调者 + 任务账本和进度账本；每轮判断是否完成、是否在绕圈、谁下一个；卡住就重新规划 [二手] | [论文](https://arxiv.org/abs/2411.04468) |
| Microsoft Agent Framework 1.0 | 五种编排：顺序、并发、交接、群聊（主持人选发言者）、Magentic [已核实] | [文档](https://learn.microsoft.com/agent-framework/workflows/orchestrations) |
| CrewAI | 顺序或层级（经理委派）；委派默认关闭 [已核实] | [文档](https://docs.crewai.com/learn/hierarchical-process) |
| LangGraph | 主管（专家之间不直接说话）或群体（互相交接，默认共享一份消息） [已核实 / 二手] | [文档](https://docs.langchain.com/oss/python/multi-agent-prebuilts) |
| OpenAI Agents SDK | 交接时可以改写下一个 Agent 看到的历史 [已核实] | [文档](https://openai.github.io/openai-agents-js/openai/agents-core/classes/handoff/) |
| MetaGPT | 共享消息池 + 按角色订阅关心的动作类型；消息可指定收件人或广播 [已核实] | [论文](https://arxiv.org/abs/2308.00352) |
| 黑板模式（2025） | 大家看同一块黑板；由控制单元选谁行动，或者 Agent 自己决定是否响应 [已核实] | [2507.01701](https://arxiv.org/abs/2507.01701)、[2510.01285](https://arxiv.org/abs/2510.01285) |

## 2. 协议和基础设施

| 系统 | 方式 | 来源 |
|---|---|---|
| A2A v1.0 | 点对点任务调用；`contextId` 把相关任务和消息归为一组；流式产出物；历史"可以保存"，调用方不能依赖 [已核实] | [规范](https://a2a-protocol.org/latest/specification/) |
| AGNTCY SLIM | 发布订阅网格；层级命名；频道 = MLS 加密组，由主持者管理成员 [已核实] | [IETF 草案](https://datatracker.ietf.org/doc/html/draft-mpsb-agntcy-slim) |
| Coral Protocol | 共享线程 + @；"等待被 @"、"等待消息"等工具；自称早期 [已核实] | [文档](https://docs.coralos.ai/concepts/threads)、[论文](https://arxiv.org/abs/2505.00749) |
| Solace Agent Mesh | 事件代理上跑 A2A，多副本共享队列、失败重投 [已核实] | [文档](https://docs.solace.com/Agent-Mesh/Framework/concepts/event-driven-mesh.htm) |
| NATS Agent Protocol | 注册、发现、流式分块、心跳，后来者通过 JetStream 补上 [二手] | [博客](https://nats.io/blog/nats-native-protocol-for-ai-agents/) |
| Confluent | 编排者-工作者、层级、黑板、市场四种模式映射到 Kafka 主题 [已核实] | [博客](https://www.confluent.io/blog/event-driven-multi-agent-systems/) |

## 3. 规模和成本

- 多 Agent 约消耗普通聊天 15 倍的 token；子 Agent 应该把结果存外部、只传引用 [已核实，[Anthropic](https://www.anthropic.com/engineering/multi-agent-research-system)]。
- 稀疏的辩论结构效果相当、成本更低 [已核实，[2406.11776](https://arxiv.org/abs/2406.11776)]；AgentPrune 省 28%–73% token [已核实，ICLR 2025]；G-Designer 最多省 95% [已核实，ICML 2025]；Optima 用不到 10% 的 token 达到更好效果 [已核实，[2410.08115](https://arxiv.org/abs/2410.08115)]；但 2026-09 的复评指出很多"剪枝"收益依赖实验设置 [已核实，[2609.05933](https://arxiv.org/abs/2609.05933)]。
- MAST：14 种失败模式，主要来自系统设计（互相忽略、信息不传）[已核实，[2503.13657](https://arxiv.org/abs/2503.13657)]。
- Moltbook：数据库配置错误泄露约 150 万个 Agent 令牌，Agent 私信里有明文的第三方密钥；约 2.6% 的帖子藏着提示词注入并随转发扩散 [已核实，[Wiz](https://wiz.io/blog/exposed-moltbook-database-reveals-millions-of-api-keys)；注入比例为二手]。

## 4. Agent 的历史和状态由谁保存

| 平台 | 存什么 | 保留 | 导出 | 来源 |
|---|---|---|---|---|
| OpenAI Responses + Conversations | 原始条目；压缩结果是加密的、不可读 [已核实] | 响应默认 30 天；对话不过期 [已核实] | 可以列出条目；压缩结果不能迁移 | [文档](https://developers.openai.com/api/docs/guides/conversation-state) |
| OpenAI Assistants | 线程 | 2026-08-26 停服，用户自己迁移 [已核实] | — | [弃用说明](https://developers.openai.com/api/docs/deprecations) |
| Claude Managed Agents | 服务端会话事件日志 + 空闲时的沙箱检查点；记忆库单独、带版本 [已核实] | 历史保留到删除；沙箱 30 天 [已核实] | 可读取全部事件；记忆可导入导出；明确警告注入会写进可写的记忆库 [已核实] | [文档](https://platform.claude.com/docs/en/managed-agents/memory) |
| Google Agent Engine | 会话（事件和状态）+ Memory Bank（抽取的事实） [已核实] | 会话默认 365 天 [二手] | 接口 | [文档](https://docs.cloud.google.com/agent-builder/agent-engine/sessions/overview) |
| AWS AgentCore Memory | 短期：原始事件；长期：按策略抽取（语义、摘要、偏好、情景） [已核实] | 事件最长 365 天 [已核实] | 接口 | [文档](https://docs.aws.amazon.com/bedrock-agentcore/latest/devguide/memory-create-a-memory-store.html) |
| Letta | 完整的 Agent 状态 | 自托管 | **Agent File（.af）**：模型配置、消息历史、系统提示、记忆块、工具；密钥置空 [已核实] | [文档](https://docs.letta.com/guides/agents/agent-file) |
| LangGraph | 每个线程的检查点 + 跨线程的存储 [已核实] | 可设置过期 | 格式专属 | [文档](https://docs.langchain.com/langsmith/configure-ttl) |

上下文工程：Anthropic 建议按需检索、压缩、在上下文之外做结构化笔记、子 Agent 只返回精简结果 [已核实，[文章](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents)]。Slack 2025 年起禁止批量导出、禁止用 API 数据训练模型，给 AI 的合法途径是有范围限制的实时搜索 [已核实，[文档](https://docs.slack.dev/apis/web-api/real-time-search-api)]。

合规：EU AI Act 第 19 条要求高风险系统的自动日志至少保存 6 个月 [已核实，[原文](https://artificialintelligenceact.eu/article/19/)]；记忆投毒（MINJA）只靠普通对话就能写入恶意记忆 [二手，[2503.03704](https://arxiv.org/abs/2503.03704)]。
