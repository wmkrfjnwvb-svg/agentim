# 05 需求与架构评审（v0.1 → v0.2）

> 状态：评审结论（2026-10-10）
> 评审对象：[01 产品需求](01-product-requirements.md)、[02 架构设计](02-architecture.md)、[03 AI 原生能力](03-ai-native.md)、[04 版本划分](04-editions.md)、[调研](research/market-and-competitors.md)
> 方法：逐条核对一致性与可实现性；对调研结论回到一手资料核实；补充遗漏的竞品、协议与合规约束。

## 0. 结论

**方向成立，规格不足。当前文档可以作为愿景，不能直接作为实现依据。**

保留不变的部分：

- 三根支柱（个人免费、企业可控、AI 原生）和"个人能管好自己的 Agent，企业才需要管别人的 Agent"的分层原则。
- 安全护栏放在免费版。
- 待审批消息不分配 `conv_seq`。
- 纯人类会话不挂 AI 钩子。
- `tenant_id` 从第一天起进入模型。
- 只保证会话内有序；推送是提示、拉取是真相；读扩散加网关聚合。

必须修改的有 8 项（P0）。它们都会改变数据模型或协议，越晚改代价越大：

| # | 问题 | 结论 |
|---|---|---|
| P0-1 | 编辑、撤回、流式结束都只改 `version`、不占 seq，按 `last_seq` 增量同步的客户端永远收不到这些变化 | `conv_seq` 改为**会话事件序号**：所有全员可见的变化都是一个事件，都占 seq（§3.1） |
| P0-2 | 耳语、主管可见、仅某人可见的消息如果占 seq，其他人会看到永远补不齐的空洞；如果不占 seq，就没有顺序 | 主日志只放**全员可见**事件；定向可见内容放**侧信道**，带锚点 seq（§3.2） |
| P0-3 | 流式消息没有状态机。Agent 崩溃后消息永远停在"生成中"；增量不落盘，与"不丢消息"的承诺冲突 | 定义 `streaming → complete / cancelled / failed`，加看门狗超时、定期检查点，并写明持久性边界（§3.3） |
| P0-4 | "工具调用审批"无法强制执行：Agent 运行在 IM 外部，工具也不经过 IM | 写明**信任模型**：IM 只能强制它经手的东西（消息、成员、托管的 MCP 工具），其他属于协作式（§4.1） |
| P0-5 | 群里加入 Agent，等于把全员消息交给第三方模型。文档没有讨论读取范围、知情和告知 | 群内 Agent 默认**只接收 @ 和回复**；加入时向全员发系统通知；AI 身份由服务端标注，不可伪造（§4.2） |
| P0-6 | 跳数上限、循环熔断没有数据支撑：消息里没有因果链，也没有跳数字段 | 消息增加 `causation_id`，`hop` 由**服务端计算**，不信任 Agent 上报（§4.3） |
| P0-7 | 用户级"会话变更日志"存在哪里没说，和"只有一层强一致"矛盾；去重表是否进状态机也没说 | 去重表必须进状态机；用户同步点放进按 `user_id` 分片的同一套存储（§3.4） |
| P0-8 | 海外合规缺项：EU AI Act 第 50 条已于 2026-08-02 生效；GDPR 删除权与只追加日志冲突；App Store 1.2 要求举报、拉黑、过滤；E2EE 没有表态 | 补成基础需求，见 §5 |

另有 P1 项 14 条（性能、容量、运维相关），P2 项 9 条（调研修正、文档矛盾），见 §6–§8。

**实施建议：先做单机版验证模型，再做集群。** 单机版的范围和验收标准见 §9。仓库已附带一个按本评审修订模型实现的单机版（`server/`），可以直接运行，查看演示效果；设计、协议和测试结果见 [06 单机版](06-standalone.md)。

---

## 1. 调研核实

### 1.1 原调研结论逐条核实

| 原文结论 | 核实结果 | 说明 |
|---|---|---|
| Telegram Bot API 9.5（2026-03）推出原生流式，仅私聊 | **部分更正** | `sendMessageDraft` 在 9.3（2025-12-31）加入，9.5 开放给所有 bot。官方文档写的是 "target private chat"，确认只支持私聊。草稿是 30 秒临时预览，生成结束必须再调用 `sendMessage` 落地。 |
| 群聊流式是空白 | **基本成立，需改措辞** | Telegram 群里只能靠反复编辑消息模拟流式。Matrix 有 MSC4471（流式临时事件更新，动机就是 LLM 流式），2026 年仍是提案，未被接受。Ably 的频道模型理论上不限人数，但没有公开针对 10 万人群的优化。建议改为"无成熟开源实现"。 |
| Ably AI Transport：一条回复是一条可追加的消息，合并推送，按消息计费 | **成立** | 默认 40ms 合并窗口（约 25 条/秒），可调范围 0–500ms。消息有 `streaming / complete / cancelled` 三种状态。晚加入的客户端拿到累积内容，不重放 token。官方文档自己承认："进程在 `run.end` 前被杀，run 会一直处于打开状态"。**AgentIM 应该吸收这套状态机，并补上看门狗。** |
| Stream 免费 1,000 MAU / 100 并发；Start $399、Elevate $599 | **成立** | 官网当前价格一致。另有 Maker 计划（每月 $100 额度）。官网 AI 相关只列出企业版的 AI 审核附加项。 |
| Slack 发布实时搜索 API 和 MCP Server | **成立** | 2026-02-17 GA。按用户 OAuth 权限访问，不导出数据。 |
| Mattermost Professional 含 Agent 平台和 token 计量 | **存疑** | 官网定价页仍把 Agentic platform（含 Agents Token Accounting）列在 Professional 下。但第三方定价追踪站记录 2026 年 5 月 token 计量移到了 Enterprise，并新增工具策略编辑器、Agent 创建等功能。两者矛盾，需要以官网对比表为准再核。 |
| A2A 一周年超过 150 家组织 | **成立，需补充** | v1.0 是首个稳定版（2026 年初），新增**签名 Agent Card**、多租户、JSON-RPC / gRPC / HTTP+JSON 三种绑定。 |

### 1.2 遗漏的竞品和信号（影响定位）

| 对象 | 为什么重要 |
|---|---|
| **Tuwunel**（Rust 写的 Matrix 服务端，Apache-2.0，conduwuit 官方继任者，瑞士政府资助） | "开源 + Rust + 可自托管 IM"已经有人做，而且有政府背书。**定位里不能再把"开源 Rust IM"当卖点**，差异化必须落在"Agent 治理"上。 |
| **Continuwuity**（社区维护的 Rust Matrix 服务端） | 同上。 |
| **Matrix / Element** | 欧洲政府采用增长；MSC4471 已经在推流式。要不要兼容 Matrix 是一个需要正式决策的问题（见 §9.3 ADR-1）。 |
| **Stoat（原 Revolt）** | 开源的 Discord 替代品，AGPL-3.0。 |
| **OpenIM** | 官方文档已经把自己定位为"IM 基础设施 + AI 交互网关"，支持多 Agent 和语音 Agent。原调研说"开源 IM 的 AI 多为外挂"，对 OpenIM 不完全成立，至少它在营销上已占位。 |
| **CometChat** | 2026-07 发布 "AI Agents in Chat"（自带 LLM、RAG、工具调用）。 |
| **Telegram 2026 年新能力** | 群内**临时消息**（只对某个用户和 bot 可见，10.2）；Guest 模式（bot 可以在自己不是成员的聊天里回复，10.0）；**bot 之间通信**（双方都开启才行）；**停止生成按钮**（`can_stop`，10.3）；**富消息 + Thinking 块**的流式（10.1）。这些都是 AgentIM 消息模型应该覆盖的能力。 |

**定位修正建议**：把"开源 + 自托管 + 协议层 AI 原生 + 大群流式"改为：

> **唯一把"Agent 治理"（身份、介入、护栏、审计）做进消息协议的开源 IM，单二进制部署，免费版即带护栏。**

"Rust"和"开源"是实现属性，不是差异化。

### 1.3 Agent 协议生态（影响协议设计）

| 协议 | 现状 | 对 AgentIM 的要求 |
|---|---|---|
| MCP | 2025-11-25 版加入 Tasks（实验性，用于长时间任务）。授权改为 OAuth Client ID Metadata Documents，并新增机器对机器的 client credentials。有资料称 2026-07-28 版已发布、Tasks 转为正式扩展，未在官方核实。 | 内置 MCP Server 的授权必须跟进最新规范，不能只用一个静态 API Key。 |
| A2A | v1.0 稳定版，带签名 Agent Card。 | Agent 注册信息应该能导出为 Agent Card，为后续互通预留。 |
| AG-UI | 事件化的 Agent ↔ 前端协议：`TEXT_MESSAGE_START / CONTENT / END`、`TOOL_CALL_START / ARGS / END / RESULT`、`RUN_STARTED / FINISHED / ERROR`、`STATE_SNAPSHOT / DELTA`。 | AgentIM 的流式事件和工具事件**应该能和 AG-UI 一一映射**。这样现成的 Agent 框架接入成本最低。 |

### 1.4 调研方法的缺口

- 全部是二手资料加厂商定价页，**没有一个目标用户访谈**。建议在写集群代码之前，先访谈 10–20 位目标开发者（AI 客服、AI 社交、企业内部 Agent 平台三类），验证"自托管 + Agent 治理"是不是真实的购买理由。
- 没有市场规模和付费意愿数据。商业模式只有结构，没有数字。

---

## 2. 产品需求（01）评审

### 2.1 定位与范围

| 问题 | 建议 |
|---|---|
| "框架"和"产品"边界模糊：一会儿说不做终端 App，一会儿说要参考客户端，而 App Store 审核要求是由客户端承担的 | 明确交付物：服务端二进制 + 协议规范 + SDK（先 TypeScript 和 Rust，再 Swift / Kotlin）+ Web 参考客户端。参考客户端必须满足商店审核要求（§5.3）。 |
| 目标用户四类并列，没有首要用户和首要场景 | 建议首要场景定为 **"AI 客服 / AI 助手接入现有产品"**。它同时用到人机会话、审批、接管、监控，最能体现差异化，也最容易收费。 |
| "AI 原生"没有可验证的定义 | 用协议能力来定义：流式事件、工具事件、介入事件、护栏事件都是协议原语，而不是应用层约定。 |

### 2.2 遗漏的功能需求

原文只有四个硬要求和 A1–A10，缺少 IM 的基本面。建议补齐下表，并逐项标注 v1 做不做：

| 类别 | 内容 | v1 建议 |
|---|---|---|
| 账号与设备 | 多端登录、设备列表、踢下线、每个设备独立的同步游标 | 做 |
| 消息操作 | 编辑、撤回、删除、引用回复、@ 提及、表情回应、转发 | 编辑、撤回、引用、@ 做；其余放 v1.x |
| 线程 | `thread_root`，线程内独立未读 | 数据模型预留 |
| 状态 | 在线状态、正在输入（Agent 用"正在思考"） | 做（软状态，不持久化） |
| 富消息与交互 | 按钮、卡片、表单。人工审批、Agent 让用户做选择都要用到（Telegram 富消息、Slack Block Kit） | 做最小集：按钮 + 回调 |
| 临时消息 | 群内只对某一人可见（Telegram 10.2 已有） | 做，走侧信道 |
| 文件与媒体 | 上传、预签名直传、缩略图、大小限制 | v1 只做元数据 + 外链 |
| 搜索 | 按权限过滤的全文搜索 | v1.x |
| 通知 | 免打扰、静音、只 @ 时通知 | 做最小集 |
| 安全与滥用 | 举报、拉黑、发送频率限制、垃圾检测 | **必须做**（商店审核要求） |
| 管理 | 管理 API：封禁、删除消息、导出 | 做最小集 |
| 数据生命周期 | 保留策略、用户注销与删除、数据导出 | 删除和导出**必须做**（GDPR） |
| 协议演进 | 协议版本协商、向后兼容规则 | **必须做** |

### 2.3 验收标准不可测

| 原文 | 问题 | 建议改为 |
|---|---|---|
| R1 "任一非存储节点宕机不影响消息正确性" | 没有恢复时间 | 网关宕机后客户端 P99 在 10 秒内恢复收发；分片选主 P99 < 5 秒；RPO = 0（已 ACK 的消息） |
| R3 "大群推送合并，不造成下行风暴" | 不可测 | 10 万人群、30% 在线、每秒 10 条消息时，单网关下行帧率不超过 X，推送服务 RPC 数不超过"在线网关数 × 窗口数" |
| 端到端 P99 < 200ms | 缺流式指标 | 再加：Agent 首个 token 到客户端 P99 < 150ms（同地域）；流式合并窗口 50–100ms |
| A1–A10 | 只有功能名，没有验收标准 | 每项写一个可以自动化测试的场景，例如"两个 Agent 互相回复，在第 N 跳被熔断，并产生一条系统事件" |
| （缺） | 可用性 SLO | 单机版不承诺；集群版 99.9% / 99.95% |
| （缺） | 数据上限 | 单条消息 ≤ 64KB、单条流式消息 ≤ 256KB、单个用户最多加入的会话数、单个会话的 Agent 数上限 |

---

## 3. 架构（02）评审：消息模型

### 3.1 P0-1：`conv_seq` 改为会话事件序号

**问题**：原设计里 `conv_seq` 只在创建消息时分配，编辑、撤回、流式结束、工具结果回填、表情回应只递增 `version`。客户端按 `last_seq` 增量拉取，已经同步过的客户端**永远收不到旧消息的变化**。03 文档里"流式违规就撤回整条消息"的功能，也依赖这个通知机制。

**修订**：

```
会话事件日志（每个会话一条，全员可见）
  conv_seq = 事件序号，单调递增、无空洞
  事件类型：
    msg.created     创建消息（流式消息在开始时创建，state=streaming）
    msg.updated     编辑、流式结束、工具结果回填、审核替换（version+1）
    msg.recalled    撤回
    member.joined / member.left / member.role_changed
    conv.updated    标题、策略变化
    agent.state     接管 / 交还 / 熔断（全员可见的那部分）
```

- 消息自身仍有不变的 `msg_id` 和 `created_seq`（在会话中的位置）。展示顺序按 `created_seq`，同步按事件 `conv_seq`。
- 客户端只需要维护一个 `last_seq`。增量拉取得到的就是"消息列表 + 状态变化"的完整差量。
- 代价：编辑和撤回也占 seq，未读数不能再用 `max_seq − read_seq` 直接计算（见 §6 P1-1）。

这和 Matrix 的事件模型、Slack 的 message_changed 事件是同一思路，属于业界做法。

### 3.2 P0-2：定向可见内容走侧信道

**问题**：`visibility = agent_only | supervisors` 的消息如果放在主日志里占 seq，普通成员就会看到 seq 空洞。按"发现空洞就拉取"的规则，他们会反复拉取，而且永远补不齐。如果改成拉取时返回占位，又会泄露"这里有一条你看不到的消息"，而且 seq 的间隔会暴露耳语的数量。

**修订**：

```
主日志（conv_seq）   ：只放全员可见事件，对每个成员都无空洞
侧信道（side_seq）   ：每个会话一条，存放定向可见事件
  每条侧信道事件带：
    audience      可见者列表（某个 Agent、Agent 所有者、主管、单个用户）
    anchor_seq    产生时主日志的最新 conv_seq，用于和主日志归并排序
  用于：耳语、临时消息（仅某人可见）、审批草稿、主管备注、私有的护栏通知
```

- 普通成员根本拿不到侧信道，没有空洞，也没有泄露。
- Agent 和主管同时读主日志和自己有权看的侧信道，按 `(anchor_seq, side_seq)` 归并，得到正确的上下文顺序。
- 待审批草稿天然属于侧信道：审批人可见，批准后才写入主日志并分配 `conv_seq`。原文"不分配 seq"的规则不变，只是有了明确的存放位置。

### 3.3 P0-3：流式消息状态机与持久性边界

**问题**：

1. `state` 只有 `draft | committed | recalled`，缺少"生成中"和"结束"状态。
2. Agent 或 Agent Gateway 崩溃时，已占位的消息会永远停在半成品状态。Ably 的官方文档也承认这个问题。
3. "增量不写 Raft"意味着用户已经看到的内容可能丢失，"断线续传"依赖的是 Agent Gateway 的内存。这和"不丢消息"的承诺冲突，文档没有说明边界。

**修订**：

```
            stream.start（分配 conv_seq，事件 msg.created，state=streaming）
                 │
                 ├── stream.append × N   不占 seq，按 (msg_id, chunk_idx) 排序，幂等
                 │      · 网关按 50–100ms 合并后，只推给正在看该会话的人
                 │      · 每 1 秒或每 4KB 写一次检查点（覆盖写 partial_content）
                 │
                 ├── stream.end    → state=complete   （事件 msg.updated，version+1）
                 ├── stream.cancel → state=cancelled  （用户、所有者或主管打断）
                 └── 看门狗：Agent 断开或 30 秒无增量 → state=failed(reason=timeout)
                      内容保留到最后一个检查点
```

**持久性承诺写明分两档**：

- **已结束的流式消息**（complete / cancelled / failed）：与普通消息同等，已 ACK 即不丢。
- **生成中的增量**：尽力送达；崩溃时最多丢失最后一个检查点之后的内容（≤ 1 秒）。

另外补充：

- 单条流式消息的上限：最长 10 分钟、最大 256KB，超过后强制结束。
- 每个 Agent 在每个会话里同时进行的流式数量有上限（解决 A3"并发轮次"的定义问题）。
- 停止按钮：客户端可以对流式消息发起取消，Agent 收到 `stream.cancelled` 事件（对齐 Telegram `can_stop`）。

### 3.4 P0-7：去重表与用户同步点放在哪里

- **去重表** `client_msg_id → conv_seq` 必须是复制状态机的一部分，并随快照持久化，否则 leader 切换后重试会产生重复消息。TTL 必须用 leader 提议时写入日志的时间戳计算，不能用各副本的本地时钟，否则各副本的状态会分叉。
- **用户级会话变更日志**（普通群写扩散用）：原文说"只有会话分片一层强一致"，但这个日志按用户维度组织，没有说放在哪里。如果放在软状态里，丢失后重连就会漏掉会话。修订：**同一套 Multi-Raft 存储，按 `user_id` 分片的"用户收件箱"**。它仍然属于"同一层"强一致存储，只是分片键不同；写扩散是异步的、可重放的（从会话提交事件流重建）。

### 3.5 原设计中正确、应当保留的部分

- 持久化后才 ACK、`client_msg_id` 去重、推拉结合、不做逐条送达回执。
- seq 分配与持久化在同一次写入中完成（拒绝 Redis INCR 的理由是对的）。
- 消息体读扩散、网关聚合扇出（10 万人群约 50 次 RPC）。
- 成员变更作为系统事件占 seq，成员记录 `join_seq / leave_seq`。
- 虚拟分片（4096）加 Raft 组映射，扩容只迁移虚拟分片。

---

## 4. AI 原生能力（03）评审

### 4.1 P0-4：信任模型（IM 能强制什么，不能强制什么）

原文设计"Agent 在 IM 外部运行"是对的，但由此推出一个文档没有面对的事实：**IM 只能强制它经手的东西**。

| 能力 | IM 能否强制 | 前提 |
|---|---|---|
| Agent 消息审批、撤回、接管、打断 | **能** | 消息必须经过 IM |
| Agent 的读取范围（能看到什么） | **能** | 只投递允许的消息 |
| 速率、预算（以消息和字符计） | **能** | |
| 预算（以 token 计） | **不能**，只能依赖 Agent 上报 | 除非模型调用也经过 IM 托管的网关 |
| 工具调用审批 | **只有工具由 IM 托管时才能**（经 IM 的 MCP 代理） | Agent 直连外部工具时，审批只是协作式的约定 |
| 外部副作用（发邮件、退款） | **不能** | 只能依靠工具托管 |

**修订**：

1. 03 文档把能力分为**强制**和**协作**两栏，不要让企业误以为"工具审批"是兜底保证。
2. 企业版的价值点因此更清晰：**托管工具网关（MCP 代理）**。只有经过它的工具调用，才能做到真正的审批、审计和按用户授权。

### 4.2 P0-5：Agent 的读取范围、知情与 AI 标识

| 问题 | 修订 |
|---|---|
| 群里加入 Agent，等于把全员消息交给第三方模型，其他成员不知情 | Agent 加入会话时产生全员可见的 `member.joined` 事件，事件中带 `kind=agent` 和所有者信息；客户端必须显式提示。 |
| Agent 默认能读多少 | 引入 `trigger` 模式：群聊默认 `mention`（只收到 @ 它的消息、回复它的消息，以及被授予的上下文），私聊默认 `all`。这和 Telegram bot 的隐私模式是同一思路。 |
| 上下文 API 的范围 | 只能读 Agent 加入以后（`join_seq` 之后）的全员事件，加上发给它的侧信道事件。群管理员可以单独授予"加入前的历史"。 |
| EU AI Act 第 50 条（2026-08-02 生效，Omnibus 没有推迟这一条）要求与人交互的 AI 系统必须让人知道对方是 AI | `sender_type` 由服务端根据认证身份写入，Agent **无法伪造为 human**；协议规定客户端必须展示 AI 标识；参考客户端默认实现。Agent 生成的图片等媒体预留机器可读标记字段。 |

### 4.3 P0-6：护栏需要因果链

原文的"每线程 Agent 跳数上限"既没有线程模型，也没有跳数字段。修订：

```
Message 增加：
  causation_id   这条消息在回应哪条消息（显式回复，或服务端推断为"触发它的最近一条消息"）
  hop            服务端计算：
                   人发的消息        → hop = 0
                   Agent 回应 hop=n  → hop = n+1
                 Agent 上报的值一律忽略
```

护栏规则（免费版内置，参数可配置）：

| 护栏 | 规则 | 触发后 |
|---|---|---|
| 跳数上限 | `hop > 8` 时拒绝发送 | Agent 收到错误；主日志写一条 `agent.state` 事件（熔断） |
| 重复检测 | 同一 Agent 在同一会话内，最近 N 条消息的规范化内容哈希重复 ≥ 3 次 | 熔断，冷却 5 分钟 |
| 速率 | 每个 Agent、每个会话一个令牌桶；大群更严格 | 拒绝，返回 `rate_limited` |
| 预算 | 每个 Agent 每日的消息数和字符数上限；可选接受 Agent 上报的 token 数 | 拒绝，返回 `budget_exceeded`，通知所有者 |
| 外部消息低信任 | 来自其他 Agent、外部用户、Guest 的消息，投递给 Agent 时带 `trust=low` 标签，并经过注入检测 | 标注，不拦截（规则类的高置信命中可以拦截） |

注意：Agent 之间可以通过 IM 以外的渠道（例如共享的外部系统）形成循环，IM 无法感知。这一点要写进限制说明。

### 4.4 人工介入的细节缺口

| 介入方式 | 缺口 | 修订 |
|---|---|---|
| 观察 | 社区版"所有者观察自己的 Agent"会让所有者看到群里其他人的消息，涉及隐私 | 所有者只能观察 Agent 自己能读到的范围（受 `trigger` 模式限制）；观察行为写入审计。企业版的主管观察需要组织策略授权，并可以配置是否告知会话成员。 |
| 耳语 | 群里有多个 Agent 时，"agent_only"指向不明 | 侧信道事件带明确的 `audience`（某个 Agent + 发起人） |
| 审批 | 没有超时、过期、批量处理 | 草稿默认 24 小时过期；批准时可以修改内容；审批动作写入审计 |
| 打断 | 与流式状态机的关系不明 | 打断 = `stream.cancel`，然后人可以接着发普通消息 |
| 接管 | 锁存在哪里、超时怎么办 | 锁是会话状态，存在主存储里（不是 Agent Gateway 的内存）；接管期间 Agent 发送会被拒绝（`taken_over`）；超时自动交还 |
| 撤回/修订 | 流式中途被判违规怎么办 | 先 `stream.cancel`，再 `msg.updated`，把内容替换为"该内容已被移除"的占位（version+1） |

### 4.5 过滤的位置

原文把三个检查点都说成"钩子"，但它们的位置其实不同。**这对"纯人类会话零开销"很关键**：

| 检查点 | 正确的位置 | 对人类会话的影响 |
|---|---|---|
| 进入 AI 之前（注入检测、脱敏） | **向 Agent 投递的路径上**（Agent Gateway），按接收方逐个 Agent 处理 | 零：不在提交路径上 |
| AI 输出之后 | Agent 的发送路径，在提交之前 | 零：只针对 Agent 发出的消息 |
| 流式过程中 | Agent Gateway 按块检查 | 零 |
| 企业全局数据防泄漏 | 提交路径（所有消息） | 有，企业自己选择开启 |

钩子的失败模式要写明：超时（默认 50ms）后 fail-open 还是 fail-closed，按策略配置。原文"IM 不运行大模型"和"注入检测"并不矛盾：社区版内置的是规则类检测，基于模型的审核作为可插拔的外部服务接入。

### 4.6 消息类型补充

在 `kind` 中补上：`interactive`（按钮 / 卡片，带回调）、`thinking`（推理过程，可折叠，默认不推送给大群）、`citation`（引用来源）。工具事件带 `call_id`，把 `tool_call` 和 `tool_result` 关联起来，并能映射到 AG-UI 的 `TOOL_CALL_*` 事件。

---

## 5. 合规与安全（P0-8，新增章节）

| 约束 | 适用 | 对需求的影响 |
|---|---|---|
| **EU AI Act 第 50 条**（2026-08-02 起适用，Digital Omnibus 只推迟了高风险条款，第 50 条照常生效） | 部署 AgentIM 并接入 Agent、面向欧盟用户的客户 | 框架提供不可伪造的 AI 身份标识，客户端必须展示（见 §4.2）。这对客户是直接卖点。 |
| **GDPR 删除权**（第 17 条） | 所有欧盟用户 | Raft 日志和快照是只追加的。方案：消息内容按会话或用户的密钥加密存储，删除时销毁密钥（crypto-shredding）；元数据里留一条"已删除"的事实记录。注意：业界对"销毁密钥是否等同删除"没有判例，需要法律确认。单机版先做逻辑删除加物理清理。 |
| **App Store 审核指南 1.2**（用户生成内容） | 参考客户端，以及所有基于 AgentIM 上架的 App | 必须有：用户协议、内容过滤、举报、拉黑，并在 24 小时内处理举报。服务端需要提供举报 API、拉黑语义（被拉黑者的消息不投递）和管理员处置 API。 |
| **英国 Online Safety Act / 欧盟 DSA** | 托管云，以及大规模部署的客户 | 举报与处置流程、透明度报告。私信服务在 DSA 下可能只属于"纯传输"类别，义务较少；英国的覆盖范围更广。自托管时义务在部署方，但框架需要提供工具。 |
| **端到端加密（E2EE）** | 海外用户的普遍预期；Google 和 Apple 已在 RCS 上推行基于 MLS（RFC 9420）的 E2EE | **必须明确表态**。建议：v1 不做 E2EE（AI 需要读取内容、服务端需要过滤），只做传输加密和静态加密；消息模型预留 `kind=encrypted` 内容和 MLS 扩展点。纯人类会话以后可以选择开启 E2EE，代价是这类会话不能有 Agent、不能做服务端过滤。这个取舍要写进定位，否则会被安全社区质疑。 |
| **OWASP LLM Top 10（2026 版）/ 智能体应用 Top 10** | 所有 Agent 场景 | 提示词注入仍是第 1 位，过度代理升到第 3 位。智能体 Top 10 单列了智能体间通信、身份与委托权限、记忆投毒。AgentIM 的低信任标签、最小权限（`trigger` 模式）和委托追溯（`on_behalf_of`）正好对应，可以作为卖点写进文档。 |

---

## 6. P1：性能、容量与运维（14 项）

| # | 问题 | 建议 |
|---|---|---|
| P1-1 | 未读数 = `max_seq − read_seq`：事件化之后，编辑、撤回、系统事件、自己发的消息都会被计入 | 按会话维护"计入未读的消息计数"（`msg_counter`，只统计别人发的、可见的新消息），未读 = `msg_counter − 已读时的 msg_counter`。仍然不需要按用户逐个存计数器。 |
| P1-2 | `read_seq` 按用户还是按设备 | 已读游标按用户（多端同步已读），同步游标按设备 |
| P1-3 | 发送方顺序：多设备同时发送、钩子拒绝了中间一条 | 每个会话的发送队列按设备维护；某条被拒绝时，后续消息照常提交，SDK 把被拒绝的那条标记为失败。"同一发送方有序"只对单个设备承诺。 |
| P1-4 | 单个会话等于单个 Raft 组的 leader，写入有上限；大群里的 Agent 会放大写入 | 写明单会话写入上限（例如 2000 条/秒），超过就限流；Agent 在大群的发言频率限制要按这个上限推算 |
| P1-5 | Router 中"群 → 网关订阅"的规模没有估算 | 100 万在线 × 平均 50 个会话 = 5000 万条订阅，需要估算内存，以及重连风暴时的订阅变更速率 |
| P1-6 | 重连风暴：一个网关宕机，15 万连接同时重连并拉取 | SDK 重连用指数退避加随机抖动；服务端对同步做准入控制；客户端先拉会话列表的 `max_seq`，再按需拉消息 |
| P1-7 | 普通群写扩散（用户变更日志）不在容量估算里 | 5 万条/秒 × 平均 200 人 = 每秒 1000 万次用户日志更新（合并前）。必须估算合并后的量，否则这里会成为真实瓶颈。 |
| P1-8 | Raft 写入放大和存储没有估算 | 3 副本 × fsync；存储量按日均消息量 × 平均大小 × 3 估算，并给出冷存储归档的时间点 |
| P1-9 | openraft 仍是 1.0 之前的版本（0.10 系列还在 alpha，各版本间有不兼容变更），公开的生产案例主要是 Databend 的元数据服务，没有找到公开的 Multi-Raft 大规模案例 | 把存储抽象为 `ConvStore` trait。单机版用嵌入式存储实现，集群版在 openraft、raft-rs（TiKV 的 Raft 实现）和外部存储之间正式选型（ADR-2）。每个节点上数千个 Raft 组需要"休眠组"机制，以减少心跳开销。 |
| P1-10 | QUIC 在企业网络里常被封 | 主协议用 WebSocket over 443，QUIC 作为可选优化，协商失败自动回退 |
| P1-11 | 多租户只有一个字段，没有隔离 | 每个租户的配额（连接数、消息速率、存储）和互不干扰的限流 |
| P1-12 | 策略钩子在 leader 的提交路径上，WASM 过滤器有慢查询风险 | 钩子要有超时和资源上限；超时策略可配置；钩子数量与延迟的指标 |
| P1-13 | `created_at` 用哪个时钟 | 由 leader 在提议时写入日志，保证各副本一致；不承诺跨会话的时间顺序 |
| P1-14 | 备份与恢复、版本升级、数据格式迁移都没有设计 | 单机版：在线备份命令；集群版：快照导出和滚动升级的兼容规则 |

## 7. P2：版本划分（04）与文档一致性

| # | 问题 | 建议 |
|---|---|---|
| P2-1 | 04 功能矩阵写社区版有"回执"，02 明确"不做逐条送达回执" | 改为"已读游标"（会话级），删去"回执" |
| P2-2 | 04 有"线程"，但 02 的消息模型里没有线程字段 | 增加 `thread_root`，或者把线程移出 v1 |
| P2-3 | 企业版写"HIPAA 支持"：自托管软件本身谈不上 HIPAA 合规，合规主体是部署方；BAA 只对托管云有意义 | 改为"支持 HIPAA 部署所需的控制项（审计、加密、访问控制）"；BAA 放到托管云 |
| P2-4 | 企业版写"数据驻留"：对自托管同样没有意义 | 移到托管云 |
| P2-5 | 举报、拉黑、内容过滤不在功能矩阵里 | 放进社区版（商店审核要求，属于基础能力） |
| P2-6 | 企业版定价单位没有定义 | 至少给出单位：按席位、按节点，还是按 Agent 数 |
| P2-7 | 护城河：Apache 2.0 + trait 插件意味着任何人都可以开源实现企业接口 | 风险已写明，但应补一句：护城河主要来自托管工具网关、合规认证和服务，而不是代码本身 |
| P2-8 | 调研中 Telegram 的版本与时间不准，"群聊流式是空白"措辞过强 | 按 §1.1 修正 |
| P2-9 | 调研没有覆盖 Rust 系 Matrix 服务端、OpenIM、CometChat | 按 §1.2 补充 |

---

## 8. 对上一轮对话中设计要点的逐条回应

| 上一轮的设计 | 结论 |
|---|---|
| Agent 一等身份 + 所有者 + `on_behalf_of` | **保留**，补充：`sender_type` 由服务端写入；委托权限 = Agent 权限 ∩ 被代表用户的权限 |
| 四种会话形态，消息模型相同，区别只在策略 | **保留** |
| 六种介入方式 | **保留**，按 §4.4 补充细节；耳语改走侧信道 |
| 过滤三个检查点 | **保留**，位置按 §4.5 修正 |
| 监控：面板、回放、审计、告警 | **保留**；单机版先做审计日志和基础指标 |
| 安全护栏在免费版 | **保留**，按 §4.3 补充因果链和具体参数 |
| 待审批消息不分配 seq，存草稿区 | **保留**，草稿区落到侧信道 |
| 介入点不拖慢人和人的消息 | **保留**，按 §4.5 说明为什么能做到 |
| 个人版与企业版的分层 | **保留**；补充：企业版的"工具审批"只有托管工具网关时才是强制的 |
| `tenant_id` 从第一天起 | **保留**，补充租户配额（P1-11） |

---

## 9. 建议的实施路线

### 9.1 第一阶段：单机版（已附实现，见 `server/`）

目标：用最小的代价验证 v0.2 消息模型和 AI 介入模型，**不验证规模**。

| 范围 | 做 | 不做 |
|---|---|---|
| 部署 | 单二进制，嵌入式存储（SQLite WAL，`synchronous=FULL`） | 集群、Raft |
| 消息 | 单聊、群聊、会话事件日志、去重、ACK 后才算发送成功、拉取补齐、已读游标、编辑、撤回 | 大群模式、搜索、文件 |
| AI | Agent 身份与所有者、`trigger` 模式、流式状态机与看门狗、侧信道（耳语、仅某人可见）、审批草稿、打断、接管、撤回与修订、护栏（跳数、重复、速率、预算）、过滤钩子、审计 | MCP Server、A2A、token 计量 |
| 客户端 | Web 演示客户端、演示 Agent | 移动端 SDK |

**单机版验收**（全部要有自动化测试）：

1. 已 ACK 的消息在进程被杀后不丢；重试同一个 `client_msg_id` 返回同一个 seq。
2. 任意成员拉取主日志，seq 连续无空洞，包括存在耳语和待审批草稿的会话。
3. 编辑、撤回、流式结束都能被已同步过的客户端通过增量拉取发现。
4. Agent 在流式中途断开，消息在超时后变为 `failed`，并保留已生成的内容。
5. 两个 Agent 互相回复，在跳数上限或重复检测处被熔断，并产生系统事件。
6. 接管期间 Agent 发送被拒绝；交还后恢复。
7. 群聊中 `trigger=mention` 的 Agent 收不到没有 @ 它的消息。

### 9.2 第二阶段：协议冻结与 SDK

线协议规范（含版本协商）、TypeScript SDK、Rust SDK，AG-UI 映射，内置 MCP Server（按 2025-11-25 及之后的授权规范）。

### 9.3 第三阶段之前必须完成的决策（ADR）

| ADR | 问题 |
|---|---|
| ADR-1 | 自有协议，还是兼容 Matrix（或者提供 Matrix 桥接）？ |
| ADR-2 | 集群存储：openraft、raft-rs，还是外部存储？ |
| ADR-3 | E2EE 立场与 MLS 预留 |
| ADR-4 | 首要场景与首要用户（建议 AI 客服 / AI 助手接入） |
| ADR-5 | 托管云做不做、什么时候做（影响合规投入） |

---

## 10. 来源

- [Telegram Bot API：sendMessageDraft](https://core.telegram.org/bots/api#sendmessagedraft)、[Bot API 更新日志](https://core.telegram.org/bots/api-changelog)
- [Ably AI Transport：Message per response](https://ably.com/docs/ai-transport/token-streaming/message-per-response)、[Ably AI 聊天机器人计费示例](https://ably.com/docs/platform/pricing/examples/ai-chatbot.md)
- [Stream Chat 定价](https://getstream.io/chat/pricing/)
- [Mattermost 定价](https://mattermost.com/pricing/)、[PricingSaaS：Mattermost 2026 年第 21 周变化](https://pricingsaas.com/companies/mattermost/diffs/2026W21)
- [Slack：MCP 与实时搜索 API GA](https://slack.com/blog/news/mcp-real-time-search-api-now-available)
- [Tuwunel](https://github.com/matrix-construct/tuwunel)、[Continuwuity](https://continuwuity.org/introduction)、[Stoat（维基百科）](https://en.wikipedia.org/wiki/Stoat_(software))
- [MSC4471：Streaming ephemeral event updates](https://github.com/matrix-org/matrix-spec-proposals/pull/4471)、[The Register：Matrix 在政府 IT 中的增长](https://www.theregister.com/2026/02/09/matrix_element_secure_chat/)
- [OpenIM 文档](https://docs.openim.io/guides/introduction)、[CometChat AI Agents in Chat](https://www.producthunt.com/products/cometchat/launches/ai-agents-in-chat)
- [AG-UI 架构](https://docs.ag-ui.com/concepts/architecture)
- [MCP 授权规范 2025-11-25](https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization)、[MCP 一周年：2025 年 11 月规范](https://blog.modelcontextprotocol.io/posts/2025-11-25-first-mcp-anniversary/)
- [Linux Foundation：A2A 超过 150 家组织](https://linuxfoundation.org/press/a2a-protocol-surpasses-150-organizations-lands-in-major-cloud-platforms-and-sees-enterprise-production-use-in-first-year)、[Google：A2A 一周年](https://opensource.googleblog.com/2026/04/a-year-of-open-collaboration-celebrating-the-anniversary-of-a2a.html)
- [Gibson Dunn：AI Act Omnibus 协议](https://www.gibsondunn.com/eu-ai-act-omnibus-agreement-postponed-high-risk-deadlines-and-other-key-changes/)、[Usercentrics：Omnibus 与第 50 条](https://usercentrics.com/knowledge-hub/eu-ai-act-high-risk-delay-article-50-transparency-consent/)
- [Conduktor：GDPR 与 Kafka 删除权](https://conduktor.io/blog/gdpr-kafka-right-to-erasure)
- [IETF：RCS 采用 MLS](https://www.ietf.org/blog/rcs-adopts-mls/)、[MLS（维基百科）](https://en.wikipedia.org/wiki/Messaging_Layer_Security)
- [Apple 开发者论坛：Guideline 1.2](https://developer.apple.com/forums/thread/807358)
- [Bristows：英国与欧盟在线安全制度比较](https://www.bristows.com/app/uploads/2025/04/Comparing-the-UK-and-EU-online-safety-regimes.pdf)
- [CSA：OWASP 2026 LLM Top 10](https://labs.cloudsecurityalliance.org/research/csa-research-note-owasp-genai-top10-2026-agent-control-stand/)
- [openraft FAQ](https://docs.rs/crate/openraft/0.9.25/source/src/docs/faq/faq.md)
- [Discord 如何存储万亿条消息（Simon Willison 摘要）](https://simonwillison.net/2023/Mar/8/how-discord-stores-trillions-of-messages)
