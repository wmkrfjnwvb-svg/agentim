# AgentIM 文档（中文）

英文是主文档，在 [`en/`](../README.md)；这里是逐页对应的中文版，结构和功能点编号与英文一致。English: [docs/README.md](../README.md).

这些文档写给开发 AgentIM、或想了解它内部怎么工作的人。**用 AgentIM 做产品**的开发者请看文档站（源码在 [`site/`](../../site)，发布在 agentim.co）：快速开始、指南、API 和 SDK 参考。

## 文档怎么组织

| 层 | 回答什么 | 页面 |
|---|---|---|
| **产品** | AgentIM 是什么、给谁用、保证什么、完整的功能清单 | [产品](product.md) |
| **技术总览** | 各部分怎么组合：程序、数据模型、写入路径、协议、集群 | [架构](architecture.md) |
| **详细设计** | 每个板块一页：功能点、每个功能点怎么实现、限制和错误规则、覆盖它的测试 | 见下表 |
| **质量** | 怎么测试、每套测试覆盖什么、业务规则目录 | [测试](testing.md)、[业务规则目录](../en/business-rules.md)（由代码生成，英文） |

## 详细设计（按板块）

每个设计页的结构相同：

1. **用途**：这个板块做什么、给谁用。
2. **功能点**：编号列表（例如 `MSG-04`）。代码评审、问题单和测试表都用这个编号。
3. **设计**：数据、流程、限制和原因。
4. **接口**：操作、端点、事件。
5. **规则和错误**：业务规则和返回的错误码。
6. **测试**：每个功能点由哪些测试文件、哪些测试覆盖。
7. **限制和下一步**：还没做的。

| 板块 | 页面 | 代码 |
|---|---|---|
| 消息：会话、日志、成员、文件、扩展 | [messaging](design/messaging.md) | `server/crates/agentim-core/src/`：`logic.rs`、`logic/`、`hub.rs`、`filestore.rs` |
| 同步和客户端 SDK | [sync-and-clients](design/sync-and-clients.md) | `hub/sync.rs`、`sdk/client-rust/`、`sdk/client-*` |
| Agent：接入、Agent 之间、历史、护栏 | [agents](design/agents.md) | `hub/agents.rs`、`hub/history.rs`、`logic/tasks.rs`、`mcp.rs`、`guard.rs`、`sdk/agent-*` |
| 审批和人的控制 | [approvals](design/approvals.md) | `logic.rs`（草稿）、`logic/requests.rs`、`hub/requests.rs`、`policy.rs` |
| 内置助手：模型、知识、Skills、连接 | [assistants](design/assistants.md) | `ai/`、`hub/assistants.rs`、`hub/knowledge.rs`、`hub/skills.rs`、`hub/connections.rs` |
| 聊天组件：卡片、表单、步骤、待办、主题 | [chat-components](design/chat-components.md) | `logic/cards.rs`、`sdk/react/`、`sdk/react-native-ui/` |
| 开放 API、Webhook 和发送前拦截 | [open-api](design/open-api.md) | `open_api.rs`、`webhook.rs` |
| 接入身份：谁怎么连、令牌、密钥、权限、管理员角色 | [access](design/access.md) | `auth.rs`、`hub/login.rs`、`registry.rs`、`admin_api.rs` |
| 安全和合规 | [security](design/security.md) | `crypto.rs`、`hub/compliance.rs`、`logic/compliance.rs` |
| 链路和业务规则 | [tracing](design/tracing.md) | `trace.rs`、`otel.rs`、`hub/dye.rs`、`hub/msgtrace.rs`、`hub/verify.rs` |
| 集群和数据库主从 | [cluster](design/cluster.md) | `server/crates/agentim-cluster/`、`shard.rs`、`replication.rs` |
| 多租户 | [multi-tenancy](design/multi-tenancy.md) | `tenant.rs`、`hub/silos.rs`、`hub/tenants.rs`、`hub/tenant_webhooks.rs` |
| 开发者体验：安装、发布、SDK 包、控制台、文档站 | [developer-experience](design/developer-experience.md) | `deploy/`、`.github/workflows/`、`server/console/`、`site/` |

## 让文档一直是真的

- 增加或改变行为的改动，在同一个 PR 里更新这个板块的设计页，中英文都改。英文为准，中文结构和编号保持一致。
- 新功能有新的功能点编号，测试表写上覆盖它的测试。没有测试的功能点，要么是文档错了，要么是测试缺了。
- 业务规则目录由代码生成（`AGENTIM_WRITE_RULES=1 cargo test --test rules`），过期时 CI 失败。
- 错的内容直接删掉，不在下面追加更正。
