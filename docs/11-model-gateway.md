# 11 模型网关（Claude Code、Codex 接入）

> 状态：**设计，待你确认**（待确认项在 [06 §1.1](06-plan.md) 的 D55–D60）。确认前不写代码。
> 范围按你 2026-10-12 的要求：网关放进 MVP，**放进 AgentIM Lite**；协议只实现 Claude Code（Anthropic Messages）和 Codex（OpenAI Responses）两种。本文取代原来 [10 §6](10-agent-ecosystem.md) 的概要。

## 1. 结论

- **做"按协议原样透传"的网关，不做协议翻译。** Claude Code 的请求只转给 Anthropic Messages 兼容的上游，Codex 的请求只转给 OpenAI Responses 兼容的上游。请求体和响应流按字节转发，网关只"旁听"：从请求里读模型名，从流里读用量。业内翻译型代理的问题（工具调用序号错乱、思考签名失效、缓存失效、错误文字被改导致客户端判断错）都出在改写上；Anthropic 也明确不支持 Claude Code 接非 Claude 模型。
- **Agent 用自己的 AgentIM 密钥调用，厂商密钥只在服务端。** 一个 Claude Code 或 Codex 实例就是一个注册在 AgentIM 里的 Agent，有负责人；它的模型调用和它在 IM 里的身份、急停、销毁、审计是同一个东西。这是我们和 LiteLLM 这类独立网关的区别：**急停一按，正在烧钱的流当场断掉**。
- 难点不在"转发"，而在长连接流式转发下的**取消、重试、超时、背压、计量、限额**，以及**不能拖慢 IM 本身**。下面每一条都对应业内踩过的坑（§3）。
- 分四步做（§13），MVP 包括 G1–G3；Codex 的 WebSocket 传输、Bedrock / Vertex 上游放在 G4。

## 2. 看起来是透传，难在哪里

| 负担 | 具体情况 |
|---|---|
| 请求很大 | Claude Code 每次请求带完整上下文（最长 100 万 token），加上图片的 base64，单个请求体几 MB 到几十 MB（Anthropic 上限 32 MB）。一千个并发就是几十 GB，必须有全局内存上限 |
| 连接很长 | 一次回复可以持续几分钟；Codex 默认流空闲 5 分钟才算超时；中间的负载均衡器常常 60 秒空闲就断 |
| 并发很多 | 一个 Claude Code 会同时开多个子 Agent，每个都在流式请求；一个团队几十个 Agent 就是几百条长连接 |
| 取消 | 用户按 Esc、客户端崩溃，网关必须立刻断开上游，否则厂商继续生成、继续收钱 |
| 重试 | 客户端自己会重试（Claude Code 最多 10 次，Codex 请求 4 次、流 5 次）；网关再重试就是乘法，变成重试风暴。流已经开始输出后**任何人都不能重试**，否则用户看到重复的内容 |
| 错误 | 200 之后流中途也会出错；上游的错误文字客户端要拿来做判断（Claude Code 按错误文字决定去掉某个 beta 功能后重试），不能改 |
| 计量 | 用量只在流的开头（输入）和结尾（输出）出现；被取消的流拿不到最后的输出用量 |
| 缓存 | 提示词缓存按上游组织（工作区）生效；同一会话的请求如果被分到不同的厂商账号，缓存全部失效，费用成倍 |
| 有状态的引用 | Responses 的 `previous_response_id`、加密的推理内容、Anthropic 的思考签名，都只在同一个上游组织里有效 |
| 协议在变 | `anthropic-beta` 每月都有新值；Codex 2026 年 2 月删掉了 Chat Completions，只剩 Responses，又加了 WebSocket 传输和 `/responses/compact` |

## 3. 业内怎么做，学到什么

| 产品 | 做法 | 我们借鉴 / 避开 |
|---|---|---|
| **LiteLLM**（Python） | 统一成 OpenAI 格式再翻译到 100 多个厂商；虚拟密钥、团队预算、限流；有专门的 Claude Code 透传路由 | 借鉴：虚拟密钥 + 预算 + 按团队记账的模型。避开：2026-03 PyPI 1.82.7、1.82.8 被投毒（偷取凭据），网关正好是放满厂商密钥的地方；Python 网关在高并发长连接下内存和延迟都高 |
| **Bifrost**（Go） | 号称每请求附加约 11 µs；文档里给 Codex 的配置是 `supports_websockets = false`、走 HTTP/SSE | 借鉴：编译型语言 + 连接池；Codex 先走 HTTP/SSE |
| **Portkey、Kong AI Gateway** | 路由、回退、缓存、护栏；Kong 的高级功能要企业授权 | 回退只在第一个字节之前（§7） |
| **Envoy AI Gateway、agentgateway** | Kubernetes 原生，按 token 限流、按模型路由 | 按 token 限流（不只按请求数） |
| **Helicone**（Rust） | 观测为主：每次调用的耗时、首字节时间、用量、费用 | 每次调用一条用量记录，带首字节时间（§9） |
| **Cloudflare AI Gateway** | 托管、日志、缓存、重试 | 默认不存内容（§10），缓存不做（Agent 请求几乎不重复） |
| **CLIProxyAPI、claude-code-router、claude-code-proxy** | 把 Claude Code / Codex 的请求翻译到别家模型 | 不做翻译：它们的问题单里大量是工具调用序号、思考块、流事件顺序错乱、Codex 的 `item not found` |
| **Claude Code 官方网关协议** | 公开了网关需要满足的条件（§5） | 逐条照做，并用真实的 Claude Code 做端到端测试 |

流式代理的通用经验（多篇事后总结一致）：

1. 客户端断开要传到上游（否则继续计费）。
2. 第一个字节之后不重试；200 之后的错误要原样交给客户端。
3. 按字节转发，不解析再序列化（序列化会改变字段顺序、数字格式，破坏签名和缓存前缀）。
4. 关掉所有缓冲（应用、反向代理、压缩），空闲时发心跳。
5. 用量常常只在最后一个事件里，被取消的流要单独估算。
6. 设全局并发和内存上限，超出时尽早拒绝，而不是排队拖垮整个进程。

## 4. 结构

```
Claude Code ──ANTHROPIC_BASE_URL──▶ /gateway/anthropic/v1/messages ─┐
Codex       ──base_url───────────▶ /gateway/openai/v1/responses    ─┤
                                                                     ▼
            1 认证：AgentIM 密钥 → Agent（急停 / 销毁 / 已吊销 → 拒绝）
            2 准入：允许的上游和模型、并发上限、请求和 token 限流、预算（D53：只在开始前检查）
            3 选路：协议 → 上游池 → 会话粘性 → 厂商密钥
            4 转发：请求体原样，改认证头和少数头；流按字节回传，同时旁听用量
            5 记账：用量记录（批量写入），更新额度；Agent 的状态和负责人的提醒走 IM 侧信道
```

- 和 IM 在同一个程序里（Lite 也有），用同一个监听端口，路径前缀 `/gateway/`；可以在配置里整体关闭。
- 网关请求不经过写线程（单写者）：准入在内存里判断，用量记录攒批后异步写入控制库。网关再忙也不碰会话日志的写入路径；压测要证明这一点（§12）。
- 代码在 `agentim-core` 的 `gateway` 模块（Lite 和 AgentIM 都用）；HTTP 客户端用已有的 `reqwest`（rustls），不加新的大依赖。

## 5. Claude Code 协议（Anthropic Messages）

### 5.1 接口

| 接口 | 做法 |
|---|---|
| `POST {base}/v1/messages` | 主接口。**按路径匹配，忽略查询串**（Claude Code 会带 `?beta=true`） |
| `POST {base}/v1/messages/count_tokens` | 转给上游（不计费，只限流） |
| `GET {base}/v1/models` | 返回这个 Agent **被允许**的模型（不转发上游）。Claude Code 开 `CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1` 时用它填模型选择器：3 秒超时、不跟随跳转、只认 ID 里含 `claude` 或 `anthropic` 的模型 |
| `HEAD {base}`、`HEAD {base}/api/hello` | 返回 200（连通性探测） |

`{base}` = `https://im.example.com/gateway/anthropic`。

### 5.2 认证

Claude Code 用 `Authorization: Bearer`（`ANTHROPIC_AUTH_TOKEN`）或 `x-api-key`（`ANTHROPIC_API_KEY`）带凭据，两种都收，值是 Agent 密钥。转发前**删除**这两个头，换成上游的 `x-api-key`。也支持 Claude Code 的 `apiKeyHelper`（定期换密钥）：就是普通的 Agent 密钥轮换（[10 §5.1](10-agent-ecosystem.md)）。

### 5.3 头

| 头 | 做法 |
|---|---|
| `anthropic-version`、`anthropic-beta` | **原样转发**，不做白名单（新 beta 值每月都有，白名单会让新版 Claude Code 出错）。管理员可以配置禁止的 beta 值（例如需要额外付费的功能） |
| `x-claude-code-session-id`、`x-claude-code-agent-id`、`x-claude-code-parent-agent-id` | 转发，同时记进用量记录（按会话汇总、子 Agent 归到父 Agent）；会话 ID 也用于会话粘性（§7） |
| `CLAUDE_CODE_GATEWAY_HINT_HEADERS=1` 时的提示头（请求类型、Agent 类型、压缩、上下文已压缩、上一轮工具耗时、prompt ID） | 记进用量记录，可以按"主循环 / 子 Agent / 压缩"分别统计费用 |
| `x-agentim-conv-id`、`x-agentim-msg-id`（自定义） | 通过 Claude Code 的 `ANTHROPIC_CUSTOM_HEADERS` 设置；网关核对这个 Agent 确实是该会话成员后记进用量记录，串起"哪条消息 → 哪次模型调用 → 花了多少"；不转发上游 |
| 逐跳头（`connection`、`transfer-encoding` 等）、`cookie`、`x-forwarded-*` | 删除 |
| 响应：`content-type: text/event-stream`、`request-id`、`retry-after`、`x-should-retry`、`anthropic-ratelimit-*` | 原样回传（Claude Code 靠它们决定是否重试、等多久）；网关自己拒绝时 `retry-after` 用整数秒 |
| 响应：`set-cookie` 等 | 删除 |
| 响应加 `x-accel-buffering: no`、`cache-control: no-cache` | 让 nginx 等反向代理不缓冲 |

### 5.4 请求体

- **不改写**：`system` 数组（第一块是 Claude Code 的归属标记）、`cache_control`、`thinking`（含签名）、工具定义原样转发。beta 头和对应的请求体字段总是成对出现，只改一边会出错，所以都不改。
- 网关只读：`model`（准入、选路、计价）、`stream`、`max_tokens`（预算检查时估算上限）。读用一次不复制的解析，读完丢掉，转发的仍是原始字节。
- 模型不在允许列表里：返回 Anthropic 格式的 `403 permission_error`，文字里写允许哪些模型。
- 不支持通过网关转发个人订阅（Claude Pro / Max）的 OAuth 令牌（D56）。

### 5.5 错误格式

上游的错误**原样回传**（状态码、头、正文）。网关自己的拒绝用 Anthropic 的格式，业务码放在文字开头，便于客户端和人都看懂：

| 业务码 | HTTP | `error.type` | 说明 |
|---|---|---|---|
| 认证失败 | 401 | `authentication_error` | 密钥无效、已吊销 |
| `forbidden` | 403 | `permission_error` | 模型或上游不允许；Agent 已急停或销毁；负责人被封禁 |
| 预算用完 | 403 | `permission_error` | 用 403 而不是 429：Claude Code 对 429 会反复重试，预算用完重试没用。文字写清楚哪一级预算、什么时候重置 |
| `rate_limited` | 429 | `rate_limit_error` | 带 `retry-after`（整数秒） |
| `bad_request` | 400 | `invalid_request_error` | 请求体超过上限、不是 JSON、缺 `model` |
| 网关过载 | 529 | `overloaded_error` | 全局并发或内存上限；Claude Code 会退避重试 |
| 上游连不上 | 502 | `api_error` | 第一个字节之前；已经换过上游仍失败 |

## 6. Codex 协议（OpenAI Responses）

### 6.1 接口

| 接口 | 做法 |
|---|---|
| `POST {base}/responses` | 主接口，SSE 流式 |
| `POST {base}/responses/compact` | 远程压缩，转发上游；按普通调用计量 |
| `GET {base}/models` | 返回允许的模型 |
| `GET {base}/responses`（WebSocket 升级） | G1–G3 不支持：返回 426，Codex 握手失败后本次会话改用 HTTP/SSE。分发的配置里直接写 `supports_websockets = false`，不走这一步。G4 再做 WebSocket（§13） |

`{base}` = `https://im.example.com/gateway/openai/v1`。

### 6.2 认证和头

Codex 用 `Authorization: Bearer <env_key 的值>`。配置里 `requires_openai_auth = false`（不用 ChatGPT 登录）。转发前换成上游的 `Authorization`（Azure OpenAI 用 `api-key` 头，按上游配置）。`OpenAI-Beta`、`session_id`、`originator`、`version` 等 Codex 发的头原样转发；会话标识用于会话粘性（实现时以抓包为准）。自定义的 `x-agentim-conv-id` 用 Codex 的 `http_headers` / `env_http_headers` 设置。

### 6.3 有状态的部分（Codex 代理最常见的坑）

| 情况 | 做法 |
|---|---|
| `previous_response_id` | 只在创建它的那个上游组织里有效。网关记住"响应 ID → 上游密钥"（内存 LRU，24 小时），带这个字段的请求必须发到同一个密钥；找不到对应关系时按会话粘性，再不行原样转发让上游报错（不自己编错误） |
| `store: false` + 加密的推理内容（`reasoning.encrypted_content`） | Codex 默认这样用（零数据保留）。加密内容只在同一个组织里能解开，所以同一个池里的密钥**必须属于同一个上游组织**，或者按会话粘住（§7） |
| `/responses/compact` 之后的请求 | 不带 `previous_response_id`，发完整对话；网关不需要特殊处理，只要不改写 |
| 输入项 ID（`item_id`） | 原样转发；改写或去重会导致上游 `item not found` |

### 6.4 错误格式

上游错误原样回传。网关自己的拒绝用 OpenAI 格式 `{"error":{"message","type","code"}}`：认证 401；`forbidden` 403；预算用完 429 + `code: "insufficient_quota"`（Codex 把它当作不可重试的额度错误）；`rate_limited` 429 + `retry-after`；`bad_request` 400；过载 503；上游连不上 502。每种拒绝在 G2 里用真实的 Codex 验证它的实际行为（会不会重试、提示什么）。

## 7. 选路、粘性、回退

- **上游池**：管理员配置若干上游（类型 `anthropic` 或 `openai-responses`、地址、区域标签、密钥列表、每个密钥的上限），以及模型到上游池的映射。厂商密钥用主密钥加密存放（和静态加密同一套），日志和错误里永远不出现。
- **会话粘性**：同一个 Claude Code / Codex 会话的请求固定发到同一个密钥（按会话 ID 一致性哈希），提示词缓存才能命中、思考签名和加密推理才有效。只有这个密钥不可用时才换。
- **密钥之间的负载**：按上游返回的限额头（`anthropic-ratelimit-*`、`x-ratelimit-*`）记录每个密钥的剩余额度，新会话分给剩余最多的；收到 429 的密钥在 `retry-after` 内不再分新会话。
- **回退只在第一个字节之前**：连接失败、上游 5xx / 529 / 429 时，同一个池里换一个密钥**最多再试一次**，然后把上游的错误原样交给客户端，由客户端按自己的策略重试（避免重试相乘）。第一个字节之后不重试、不换上游。
- **跨上游回退**（例如 Anthropic → Bedrock）默认关闭，只能在同一模型家族之间由管理员显式打开（会丢缓存）。

## 8. 流式转发

| 方面 | 做法 |
|---|---|
| 转发 | 上游每到一块就写给客户端，不攒；不压缩；HTTP/1.1 分块或 HTTP/2，都可以 |
| 旁听 | 一个增量 SSE 解析器只看事件名和用量字段：Anthropic 的 `message_start`（输入、缓存写、缓存读 token）、`message_delta`（输出 token、停止原因）；Responses 的 `response.completed` / `response.incomplete` / `response.failed`（`usage`：输入、缓存、输出、推理 token）。解析器出错只影响计量，不影响转发 |
| 心跳 | Anthropic 上游自己发 `ping`，原样转发。Responses 上游没有心跳：上游空闲超过 15 秒时，网关发一行 SSE 注释（`: keepalive`），防止中间的负载均衡器断开；注释行所有 SSE 解析器都会忽略（G2 用真实 Codex 验证） |
| 客户端断开 | 立即丢弃上游请求（断开连接），厂商停止生成。用量按已收到的部分记录：输入用量在开头已经拿到；输出用量用收到的文本估算并标记"估算" |
| 上游中途断开 | 上游如果已经发了错误事件，原样转发；如果连接直接断了，网关补一个该协议的错误事件（Anthropic：`event: error`，`overloaded_error` 或 `api_error`；Responses：`response.failed`）再结束，让客户端知道不是正常结束。不重试 |
| 非流式请求 | 正文原样回传，从正文里读用量 |
| 超时 | 连接 10 秒；等响应头 10 分钟（大上下文的首字节可能很慢）；两块数据之间 5 分钟；整体不设上限（长回复是正常的）。都可配置 |
| 反向代理 | 文档里给出 nginx / Caddy 配置：关缓冲、读超时 ≥ 10 分钟、请求体上限 32 MB |

## 9. 准入、限额、计量

### 9.1 准入（请求开始前，全部在内存里）

按顺序，第一个不满足就拒绝，**被拒绝的请求不写用量记录**（只加指标计数）：

1. Agent 存在、未吊销、未急停、未销毁；负责人未被封禁。
2. 上游和模型在允许列表里（Agent、负责人、部署三级取交集；区域白名单，例如只允许欧盟区域的上游）。
3. 并发：每个 Agent（默认 16，Claude Code 会并行开子 Agent）、每个负责人（默认 64）、全局（默认 2,000）。
4. 内存：所有在途请求体的总字节数上限（默认 1 GB）；超出返回过载，不排队。单个请求体上限 32 MB。
5. 限流：每分钟请求数、每分钟 token 数（令牌桶；token 按请求体大小估算，结束后按实际用量校正）。
6. 预算：日 / 月费用的软上限（提醒）和硬上限（拒绝）；按 D53，只在开始前检查，进行中的请求不掐断，超出部分照记，下一次拒绝。

### 9.2 计量

| 方面 | 做法 |
|---|---|
| 记录 | 每次调用一条只追加的用量记录：时间、Agent、负责人、协议、上游、密钥编号、模型、各类 token（输入、输出、缓存写、缓存读、推理）、费用、状态（完成 / 被取消 / 上游错误 / 中途断开）、首字节时间、总耗时、Claude Code 的会话和子 Agent ID、关联的会话和消息（如果带了） |
| 写入 | 内存攒批，每秒或每 500 条写一次控制库，不经过会话日志的写线程。进程崩溃时最多丢最后一秒的记录（模型调用不属于"消息不丢"的保证范围；厂商账单可以对账）；正常停服务会先写完 |
| 费用 | 可版本化的价格表（配置文件，按模型、按 token 类型；缓存读写、长上下文分档单独计价）；记录里存当时的价格版本 |
| 额度计数 | 内存里按 Agent / 负责人维护当日、当月的累计，启动时从用量记录汇总恢复；集群时各节点各自检查，允许的超出量 = 在途请求数（和 D53 一致） |
| 在 IM 里可见 | Agent 的 `whoami` / 管理接口返回已用和剩余额度；到软上限时给负责人发一条侧信道提醒（不进会话日志）；负责人和管理员能按 Agent、模型、会话查询用量 |
| 导出 | CSV / JSON，带 Prometheus 指标（请求数、token 数、费用、首字节时间、在途数、拒绝数，按上游和模型分） |

## 10. 内容、审计、合规

- **默认不记录提示词和回复的内容**，只记元数据（§9.2）。日志里不出现请求体、响应体、密钥。
- 可选的内容记录（排查问题用）：按部署或按 Agent 打开，用这个 Agent 的数据密钥加密（销毁 Agent 时一起加密销毁），默认保留 7 天、最长 30 天，属于 GDPR 的访问和删除范围（D59）。
- 审计：网关配置的修改（上游、密钥、价格、额度、允许列表）写审计；单次调用不写审计（用量记录就是记录）。
- GDPR：用量记录里有个人数据（谁、何时、哪个会话），删除用户时把用量记录匿名化，只保留汇总；Agent 的注册信息里写明它用哪个上游、哪个区域、是否零数据保留，作为第 30 条的处理记录。
- EU AI Act：网关不改变 AI 标记规则；Claude Code / Codex 若同时通过 MCP 在 IM 里发消息，照常标为 AI。
- **手机审批 Claude Code 的操作不经过网关**：工具执行和权限确认发生在开发者的机器上，网关只看到模型请求，看不到"要不要执行这条命令"。手机审批走 Claude Code 的钩子（权限请求时调用 AgentIM 的"请人决定"，人在手机上批准后钩子返回允许或拒绝），Codex 走它的审批 / 通知机制。这部分放在审批的设计里（[06](06-plan.md) 的审批一项），网关只负责把它们关联到同一个 Agent。

## 11. 安全

- 上游地址只来自管理员配置，请求里不能指定（防 SSRF）；不跟随上游的跳转。
- 厂商密钥用主密钥加密存放；内存里用完清零；错误信息里不出现。
- 网关的认证和 MCP、WebSocket 用同一个密钥校验路径；未知密钥的数据库查询受现有的每秒一次限制。
- **人的令牌不能调网关**（`403`），只有 Agent 密钥可以：模型调用必须归属到一个有负责人的 Agent。
- 依赖：不引入 Python；`cargo deny` 照常；网关可以在配置里整体关闭。

## 12. 测试计划（按 CLAUDE.md）

用一个进程内的**假上游**（axum 写的假 Anthropic 和假 Responses 服务，可以编排：正常流、慢流、中途错误事件、中途断开、429 带 `retry-after`、529、超大请求体、不发心跳、只发头不发体）测网关；再用**真实的 Claude Code 和 Codex** 对着网关 + 假上游跑端到端（CI 里 `npm i -g` 安装，假上游不需要真实密钥）；带真实厂商密钥的测试只在手动或夜间任务里跑。

| 逻辑节点 | 测什么 |
|---|---|
| 转发正确性 | 客户端收到的字节和上游发出的完全相同（流式和非流式）；请求体到上游时一个字节不变；查询串被忽略；头的删除、保留、替换逐个核对 |
| 认证 | 两种认证头；无效、吊销、人的令牌、急停、销毁的 Agent；`apiKeyHelper` 式轮换（新旧密钥并发） |
| 准入边界 | 并发上限正好到和超出 1；内存上限；32 MB 正好和超出；模型和区域允许列表的三级交集；预算正好用完；限流令牌桶 |
| 错误 | 上游错误原样回传（逐字节）；网关自己的每种拒绝的状态码、格式、`retry-after`；**被拒绝的请求没有用量记录** |
| 流式 | 无缓冲（假上游发一块，客户端在 100 ms 内收到）；心跳注释；客户端断开后假上游在 1 秒内看到断开；上游中途断开时客户端收到协议错误事件；第一个字节之后不重试 |
| 选路 | 会话粘性；`previous_response_id` 发回原密钥；429 的密钥暂停；第一个字节之前最多换一次 |
| 计量 | 两种协议的各类 token 一个不差；取消的流记输入和估算的输出；价格版本；重启后额度从记录恢复 |
| 和 IM 的联动 | 急停让在途的流在 1 秒内断开、之后的请求被拒；销毁同上；负责人收到软上限提醒；`x-agentim-conv-id` 不是成员时不关联；`whoami` 的额度 |
| 并发竞争 | 多个请求同时冲预算硬上限：超出量 ≤ 在途数；急停和新请求竞争：急停之后开始的请求一个都不放行；多轮重复，**变异检查**（去掉锁或检查后，测试必须失败） |
| 三种数据库、两种系统 | 用量记录、配置、额度恢复在 SQLite / PostgreSQL / MySQL 上跑；Linux 和 Windows CI |
| 压测 | 1,000 条并发慢流（每条 5 分钟）：内存有界、无泄漏；**同时测 IM 消息的 p99 延迟不变差**（网关不能拖慢 IM）；结果写进 README |
| 真实客户端 | Claude Code：普通对话、工具调用、子 Agent、`/compact`、模型发现、Esc 取消；Codex：普通对话、工具调用、`/compact`、WebSocket 握手失败后回到 HTTP、取消 |

覆盖情况写进 [05 §5](05-operations.md)。

## 13. 分步

| 步骤 | 内容 | 版本 |
|---|---|---|
| G1 | 网关框架、Agent 密钥认证、准入（并发、内存、允许列表、限流）、Claude Code 协议全套（§5）、流式转发（§8）、计量和预算（§9）、急停联动、`agent create` 输出 Claude Code 配置 | MVP，Lite |
| G2 | Codex 协议（§6，HTTP/SSE）、`/responses/compact`、`previous_response_id` 粘性、`agent create` 输出 Codex 配置 | MVP，Lite |
| G3 | 上游池（多密钥、按限额头分配、会话粘性、一次回退）、用量查询和导出、软上限提醒、压测和真实客户端端到端 | MVP，Lite |
| G4 | Codex WebSocket 传输；Bedrock、Vertex、Azure 上游（欧盟区域的 Claude 只能走这两家）；可选的内容记录和 DLP；服务端托管 Agent（[10 §6](10-agent-ecosystem.md)） | MVP 之后（D58） |

## 14. 分发给 Agent 的配置

`agentim agent create --name my-claude --owner alice --url https://im.example.com` 会多打印下面两段（密钥只显示一次）。

Claude Code（`~/.claude/settings.json` 或项目的 `.claude/settings.json`；企业可以用托管设置统一下发，并用 `allowedProviders: ["customEndpoint"]` 禁止绕过网关）：

```json
{
  "env": {
    "ANTHROPIC_BASE_URL": "https://im.example.com/gateway/anthropic",
    "ANTHROPIC_AUTH_TOKEN": "<Agent 密钥>",
    "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY": "1",
    "CLAUDE_CODE_GATEWAY_HINT_HEADERS": "1"
  }
}
```

Codex（`~/.codex/config.toml`；密钥放环境变量 `AGENTIM_AGENT_KEY`）：

```toml
model_provider = "agentim"

[model_providers.agentim]
name = "AgentIM"
base_url = "https://im.example.com/gateway/openai/v1"
env_key = "AGENTIM_AGENT_KEY"
wire_api = "responses"
requires_openai_auth = false
supports_websockets = false
stream_idle_timeout_ms = 300000
```

服务端配置（`agentim.toml`，草案）：

```toml
[gateway]
enabled = true
max_body_bytes = 33554432        # 32 MB
max_inflight_body_bytes = 1073741824
max_concurrent = 2000
per_agent_concurrent = 16
per_owner_concurrent = 64

[[gateway.upstreams]]
id = "anthropic"
kind = "anthropic"               # 或 "openai-responses"
base_url = "https://api.anthropic.com"
region = "us"
keys = ["enc:..."]               # 用 `agentim gateway key add` 写入，加密存放
models = ["claude-*"]

[gateway.limits.default]         # 每个 Agent 的默认值，负责人和 Agent 可以单独设
requests_per_minute = 120
tokens_per_minute = 2000000
daily_cost_soft = 20.0
daily_cost_hard = 50.0
```

## 15. 参考资料

- Claude Code 网关：<https://code.claude.com/docs/en/llm-gateway>、网关协议 <https://code.claude.com/docs/en/llm-gateway-protocol>
- Codex 配置：<https://learn.chatgpt.com/docs/config-file/config-reference>；问题单：openai/codex #35739、#46470、#45096、#28503、#18960、#19821（WebSocket 回退、压缩、`store` 和 `previous_response_id`）
- 代理实现：CLIProxyAPI #2596、thezillo/codex-proxy PR #8、headroom #121、preloop #1455；Bifrost 的 Codex 配置 <https://docs.getbifrost.ai/cli-agents/codex-cli>；LiteLLM 的 Claude Code 接入 <https://docs.litellm.ai/docs/proxy/client_setup/claude_code>
- LiteLLM 供应链事件（PyPI 1.82.7、1.82.8，2026-03）
- 流式代理经验：dev.to《Streaming SSE Proxying for LLM APIs: The Hard Parts》；SSE 背压相关文章；arXiv 2607.17525（FailureAtlas）
- 网关对比：getmaxim.ai、dev.to 上的 2026 年 AI 网关对比文章
