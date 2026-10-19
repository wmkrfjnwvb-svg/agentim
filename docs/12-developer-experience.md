# 12 开发者体验

> 状态：📝 设计已写、待你确认（2026-10-18）。确认前不写代码。本文覆盖 [06 §3](06-plan.md) 的 P1–P6 和 D105；待定事项见 [06 §1.1](06-plan.md)。

目标是一条闭环：**发现 → 5 分钟跑起来 → 30 分钟嵌进自己的应用 → 用开发者控制台管理 → 付费**。验收标准见 [06 §0](06-plan.md)。

参照的做法：Stream / Sendbird（客户端 SDK + UI 组件 + 控制台，按月活计费）、Supabase / PocketBase（一个程序、内置控制台、一条命令跑起来）、Tailscale / Deno（`curl | sh` 和 PowerShell 安装、带签名和校验的发布）。

## 1. P1 安装和发布

**发布物**（打 `v*` 标签时由 CI 生成，挂在 GitHub Releases 上）

| 发布物 | 做法 |
|---|---|
| 预编译程序 | Linux x86_64 / aarch64（musl 静态链接）、macOS x86_64 / arm64、Windows x86_64；用 cargo-dist 生成，每个附 SHA-256 |
| 签名 | 用 Sigstore（cosign，无密钥，凭 GitHub 的身份签名）签发布物，并附 GitHub 构建来源证明；macOS 公证和 Windows Authenticode 需要付费证书（D106） |
| 一条命令安装 | `curl -fsSL https://…/install.sh \| sh`、`irm https://…/install.ps1 \| iex`：下载对应平台的程序、校验 SHA-256、放进用户目录的 `bin`，不需要 root |
| Docker 镜像 | `ghcr.io/<org>/agentim:<版本>` 和 `:latest`，amd64 + arm64，用现有的 `deploy/Dockerfile`；镜像签名和 SBOM |
| Helm Chart | 现有的 `deploy/helm/agentim` 以 OCI 包发布到 GHCR，版本和程序一致 |

**版本和更新日志**：语义化版本，1.0 之前次版本号可以有不兼容改动，但协议靠能力协商兼容相邻版本（[11 §7](11-cluster.md)）。`CHANGELOG.md` 按 Keep a Changelog 的格式，每个改动在提交时写进"未发布"一节，发布时归档。

**依赖扫描**：CI 加 `cargo-deny`（许可证、已知漏洞、来源）和 `npm audit`；Dependabot 管 Cargo、npm 和 Actions；镜像用 Trivy 扫描。有高危漏洞时发布任务失败。

**第一次运行要顺**（5 分钟目标的关键）：现在没有配置时会因为没有主密钥拒绝启动。改为：
- `agentim` 不带参数：在当前目录建 `agentim.db`，并生成主密钥文件 `agentim.key`（权限 0600），日志里醒目地提示备份它；已有密钥文件就用它。
- 启动后打印本机地址和控制台的一次性初始化链接（第一次打开时设置管理员密码和 TOTP，见 §4）。
- `agentim --demo` 保持现状（演示用户和演示 Agent）。

**快速开始**（README 改成这样，`cargo run` 移到"从源码构建"）：

```bash
curl -fsSL https://…/install.sh | sh      # 或 docker run -p 8080:8080 ghcr.io/<org>/agentim
agentim                                    # 打开 http://127.0.0.1:8080
```

**测试**：发布流程在每个 PR 上空跑一次（构建全部目标、不上传）；安装脚本在 Linux、macOS、Windows 的 CI 机器上真实安装并启动，检查 `/readyz` 和发第一条消息；第一次运行的密钥生成测试覆盖：没有密钥文件、已有密钥文件、密钥文件权限过宽（拒绝并说明）、目录不可写。

## 2. 客户端 SDK

### 2.1 TypeScript（P2，第 2 步）

包名 `@agentim/client`，纯 TypeScript，无运行时依赖，浏览器、React Native、Electron、Node 都能用。它是给**人用的应用**的 SDK（Agent 继续用 `sdk/agent-node`、`sdk/agent-python`）。目标：开发者只管画界面，**连接、同步、离线、重试、缓存、排序、去重全部交给 SDK**，而且在弱网、切后台、多标签页、重装、换账号时都不出错。

参照：Sendbird（本地缓存有容量上限、按最久没打开的会话淘汰、可加密，重连后检查缺口、缺口太大时整段重建）、Stream（离线时操作先在本地生效、排队联网后补发，状态用可订阅的 store，React 用 `useSyncExternalStore` 接）、matrix-js-sdk（IndexedDB 存储，多标签页要协调）。

**分层**

| 层 | 内容 |
|---|---|
| 平台适配 | WebSocket、存储驱动、网络状态（在线/离线）、前后台、加密原语、时钟；浏览器、React Native、Node 各一套默认实现，都可替换 |
| 连接 | 状态机：`connecting` → `connected` → `reconnecting` → `offline`（系统报告没网时不白白重试）→ `closed`；指数退避加随机抖动；**收不到任何帧超过 2 个心跳周期就判定连接已死、主动重连**（D99 那种"连接看着在、其实收不到"的情况）；切回前台、网络恢复时立即重连；关闭码：1012 立即换节点重连，`logged_out` / 令牌被吊销停止重连并报告 |
| 鉴权 | `tokenProvider`：开发者后端用开放 API 签发令牌；连接被拒或收到 `unauthenticated` 时再要一次新令牌，连续失败时退避，不会刷爆开发者的后端 |
| 同步 | 已实现的规则：按 seq 只应用连续的事件，跳号的等待，确认不越过缺口，同一连接补不上就重连，每 30 秒校验位置；**落后太多**（服务端 `reset`）时整段重建这个会话的本地记录并通知界面 |
| 离线操作队列 | 见下面的“离线操作队列”。审批、请人决定、接管不排队：它们作用于正在运行的 Agent，几小时后才送到的决定可能已经不对，没连上时立即报 `disconnected` |
| 本地存储 | 设备上 SQLite、浏览器 IndexedDB、测试用内存；表：会话（位置、列表信息、最后打开时间）、消息、离线队列、元数据（结构版本、账号）；**结构版本和迁移**：升级时按版本迁移，迁移失败或库损坏时删除重建、从服务端重新同步（离线队列单独保留）；**每个账号一个库** |
| 加密 | 本地内容默认加密（可关闭）：AES-256-GCM，每个账号一把数据密钥；浏览器用不可导出的 WebCrypto 密钥存在 IndexedDB；React Native 由开发者提供钥匙串/Keystore 里的密钥（`keyProvider`）；也可以传自己的 `encrypt` / `decrypt`（和 Sendbird 一样）。只加密内容字段，用于查询的 ID、seq、时间不加密 |
| 容量 | 默认上限 256 MB（可配置，最少 32 MB）；超出时按会话最后打开时间淘汰最旧会话的消息；离线队列、会话位置永远不淘汰；被淘汰的会话再打开时只显示同步以后的新消息 |
| 状态 | 框架无关的可订阅状态：`client.conversations`（会话列表：按最后活动排序，带未读数、最后一条、免打扰、正在输入）、`client.messages(convId)`（某个会话的消息、加载更早、正在生成的流式回复）、`client.connection`；每个都有 `get()` 和 `subscribe(fn)`，只在自己那部分变化时通知；React 组件（P3）用 `useSyncExternalStore` 直接接 |
| 多标签页 | 每个标签页自己的连接（Sendbird、Stream 都是这样）；同一个库的写入用 Web Locks 串行，位置只往前走，两个标签页同时写不会互相覆盖；一个标签页发出的消息，另一个标签页通过同步拿到 |
| 诊断 | 日志级别和日志回调；连接、同步、队列的计数；`client.debugState()` 导出当前状态便于报问题 |
| 错误 | `AgentIMError {code, message, retryable}`，`code` 和服务端一致，另加客户端的 `disconnected`、`timeout`、`storage`、`unauthenticated` |

**离线操作队列**（步骤 B；参照 Stream 的离线操作队列、Sendbird 的自动重发）

| 操作 | 本地立即生效 | 排队时合并 | 重试时怎么不重复 | 被业务拒绝 |
|---|---|---|---|---|
| 发送 | 本地消息 `sending` | — | `client_msg_id`（服务端去重） | 标为 `failed`，留在队列里，可 `retry` / `discard` |
| 编辑 | 改内容，`version` 不变，标 `pending` | 编辑还没发出的消息：直接改那条发送；同一条消息连续编辑：只留最后一次 | 带 `base_version`（看到的版本）；结果不明后重试被拒为 `conflict` 时先同步，消息已是这次的内容就算成功 | 回滚内容，报 `operation` 事件 |
| 撤回 | 标为 `recalled`、清空内容 | 撤回还没发出的消息：直接删掉那条发送（等于丢弃）；之前排队的编辑一起删掉 | 重试被拒为 `conflict`（已撤回）就算成功 | 回滚 |
| 已读 | — | 每个会话只留一个，取最大 seq | 本来就幂等（只往前） | 丢弃 |
| 反馈 | — | 同一条消息只留最后一次 | 本来就幂等（覆盖） | 丢弃，报事件 |
| 卡片回应 | — | — | 结果不明后重试被拒为“你已经回应过”就算成功 | 丢弃，报事件 |

- **持久化**：本地变化和队列项在同一次写入里（崩溃后不会只有一半）；重启后先读队列，联网后继续。
- **顺序**：每个会话一个发送循环，按入队顺序一次一个；不同会话并行。依赖还没发出的消息的操作（发送还在路上时的编辑）等它发出后换成真实的消息 ID 再发。
- **重试**：可重试的错误（断线、超时、限流、过载、节点切换）退避重试（250 ms 起，最长 30 s）；离线时不重试，联网后立即继续。
- **时限**：在队列里超过 `queueTtlMs`（默认 3 天）还没成功的，按业务拒绝处理，错误码 `timeout`。
- **返回值**：每个方法返回的 Promise 在服务端接受时完成、被拒时以服务端的错误码失败；客户端先关闭时以 `closed` 失败（操作仍在队列里，下次连接继续）。`client.pending()` 列出队列，`operation` 事件报告每个操作的结果。
- **换账号、退出登录**：队列和缓存一起清空。

**类型**：协议类型以后从服务端导出的 JSON Schema 生成，CI 检查两边一致。

**分步**（每步都有完整测试、Linux 和 Windows 上跑，做完一步提交一步）

| 步 | 内容 | 状态 |
|---|---|---|
| A | 存储第二版：结构版本和迁移、离线队列表、元数据、每个账号一个库、损坏重建；内容加密；容量上限和淘汰；三种存储同一套测试 | ✅ 2026-10-19 |
| B | 离线操作队列：所有修改类操作、持久化、按会话顺序补发、合并、回滚、重启后继续 | 进行中 |
| C | 可订阅状态：会话列表、消息列表（加载更早）、连接状态 | ⏳ |
| D | 连接加固：网络和前后台、收不到帧判死、令牌刷新和退避、关闭码、多标签页 | ⏳ |
| E | 浏览器真机测试（Playwright：Chromium、Firefox、WebKit）、文档和 API 参考、npm 发布配置 | ⏳ |

已完成：连接和重连、按 seq 同步和缺口处理、发送重试不重复、流式、AI 交互；存储第二版（A）。测试 46 项（真实服务端 10 项、脚本化 3 项、存储 33 项）。往回翻只能翻本机缓存里有的（服务端不给人读历史，[08 §2.4](08-sync-and-client.md)），被淘汰的会话历史在本机就没有了，见 D108。

**测试**
- 协议一致性：对着真实的 `agentim` 程序跑（发送和去重、断线时正在发送、离线补齐、缺口、流式、拒绝、输入中，以及每一步新增的行为），Linux 和 Windows。
- 精确竞争：用脚本化的假服务端复现竞争（同步结果越过排队中的推送等），并做变异检查。
- 存储：三种存储跑同一套测试，加上迁移、损坏、加密、淘汰。
- 浏览器：Playwright 在三种浏览器里跑 IndexedDB、加密、多标签页、重连。

### 2.2 其他平台（第 4 步，D104 待确认）

开发者要的是**每个平台都有、用起来一样、不出错**（游戏用 C# / Unity，手机 App 用 Swift、Kotlin、Flutter）。每种语言各写一遍同步、重试、缓存，既慢又难保证一致，所以除 Web 外共用一个 Rust 核心：

| 层 | 内容 |
|---|---|
| Rust 核心 | 连接和重连、握手、按 seq 同步和补缺口、发送队列和 `client_msg_id` 重试、本地 SQLite 缓存（按值加密）、流式消息拼接、审批和请人决定；行为和 §2.1 完全一致 |
| Swift、Kotlin、Python | 用 UniFFI 生成（Mozilla 在 Firefox 里用、matrix-rust-sdk 也用这一套） |
| C#（Unity、.NET） | 核心导出 C 接口，C# 封装由工具生成（csbindgen 或 uniffi-bindgen-cs）；Unity 打包 iOS、Android、Windows、macOS、Linux 的原生库，发布到 NuGet 和 Unity 的包管理（OpenUPM）；Unity 的 WebGL 平台改用 §2.1 的 TypeScript SDK |
| Flutter | flutter_rust_bridge |

- **一致性**：TypeScript 和 Rust 核心跑同一套协议一致性场景（§2.1 的测试），任何一边行为不同 CI 就失败。
- **稳定**：语义化版本；协议靠能力协商兼容，旧 SDK 连新服务端不会坏；每种语言都有"未知事件"分支。
- **好用**：每种语言的接口按该语言的习惯（Swift 的 async、Kotlin 的协程和 Flow、C# 的 async/await 和事件、Unity 在主线程回调）；每个平台一个能直接运行的示例项目和 5 分钟快速开始。
- **测试**：各语言的封装在对应平台的 CI 机器上真实跑（iOS 模拟器、Android 模拟器、Windows、macOS、Linux、Unity 批处理模式）。

## 3. P3 React 界面组件

包名 `@agentim/react`，依赖 P2。

- **两层**：一层 hooks（`useConversations`、`useMessages`、`useSend`、`useTyping`、`usePresence`、`useApproval`），开发者完全自己画界面也能用；一层组件，建在 hooks 上。
- **组件**：会话列表、消息列表（虚拟滚动；流式 AI 消息、AI 标记、卡片、来源、反馈）、输入框、审批卡片、转人工提示、成员列表、正在输入。
- **AI 标记不可关闭**（D36、EU AI Act 第 50 条）：消息组件按 `sender_type` 显示，替换组件时文档写明这一条义务。
- **换主题和替换组件**：样式全部用 CSS 变量，附一套默认主题（亮色、暗色）；不依赖任何 CSS 框架。每个组件可以通过 `components` 属性换成自己的实现（Stream 的做法），文档里给出完整例子。
- **无障碍和多语言**：键盘操作、读屏标签，按 WCAG 2.1 AA（欧盟无障碍法的要求）；界面文字可以整体替换，支持从右到左的语言。
- **Web 演示端**改用 P2 + P3 重写，作为组件的活样例。
- **测试**：Vitest + Testing Library 测每个组件的状态（空、加载、失败、流式中、被打断）；Playwright 对真实服务端跑端到端；截图对比防止样式退化。

## 4. P4 开发者控制台

**内置在 `agentim` 程序里**（D103，2026-10-18 已确认：用 Rust，和服务端同一个二进制）：React 单页应用，构建后嵌进 Rust 程序，在 `/console` 提供。不单独做 Go 程序，单机到集群都是同一个程序、同一个端口（也可以配置成单独的管理地址，只在内网开放）。`cargo build` 时如果没有构建好的前端，就嵌一个"请从发布版安装或先构建前端"的占位页，Rust 的构建不依赖 Node；发布版和 Docker 镜像总是带完整的控制台。

**分两次做**（[06 §3](06-plan.md)）：第 3 步和托管云一起先做登录、API 密钥和用量；第 7 步补全其余模块和 `agentim admin`。

**管理 API `/admin/v1`**：控制台和 `agentim admin …` 命令（缩小后的 D63）都只调这一套 API。每个修改都走服务端的业务逻辑（和客户端、开放 API 同一套规则和错误码），写审计；拒绝的请求不留痕迹（CLAUDE.md）。

**登录**：管理员账号密码 + TOTP。第一次运行时日志里打印一次性初始化链接（只在本机地址可用，用一次失效）。会话 Cookie 设 `HttpOnly`、`SameSite=Strict`，修改请求带 CSRF 令牌；登录限流和锁定沿用现有账号逻辑。`agentim admin` 用单独的管理密钥（带权限范围，只存哈希）。

**模块**

| 模块 | 内容 |
|---|---|
| 概览 | 连接数、消息量、确认延迟、各数据库和集群节点的健康状态 |
| API 密钥 | 开放 API 密钥的创建、权限范围、IP 限制、过期、吊销（现有的 `api-key` 命令改为走这里） |
| 用户和设备 | 搜索、查看、停用、下线设备、重置密码 |
| 会话 | 元数据（成员、消息数、最后活动）；消息查看器：**查看内容要填原因并写审计**（GDPR 的目的限制），默认只看元数据 |
| Agent | 列表、负责人、凭据、急停、用量 |
| Webhook | 配置、投递日志、失败重发 |
| 审核队列 | 用户的举报：查看被举报的消息、处理（删除、停用发送者、驳回），处理结果写审计 |
| 设置 | 静态加密状态、数据库和从库模式、集群节点、版本 |

**测试**：管理 API 按 CLAUDE.md 的要求逐个接口测权限、边界、错误码、审计、拒绝不留痕，三种数据库；集群下从任一节点调用结果一致；Playwright 跑控制台的主要流程（初始化、登录、建密钥、查看会话并留下审计、处理举报）。

## 5. P5 文档站和示例应用

**文档站**：静态站（Astro Starlight），发布在 GitHub Pages，跟版本走。
- 快速开始：Next.js、React Native（Expo）、Node 后端（签发令牌、接 Webhook）。
- API 参考：从 `/v1/openapi.json` 生成；管理 API 同样生成。
- SDK 参考：从 TypeScript 源码生成（TypeDoc）。
- 概念：会话和 seq、同步、Agent 成员、审批和请人决定、集群部署。

**三个示例应用**（放在仓库的 `examples/`，每个一条命令跑起来：`docker compose up` 或 `npm run dev`）

| 示例 | 内容 |
|---|---|
| AI 客服窗口 | SaaS 页面里嵌一个聊天窗口；Agent 回答，答不了或用户要求时转人工，客服在另一个页面接手 |
| 人和 Agent 的团队群聊 | 群里有人和 Agent；Agent 的草稿要人审批后才发出 |
| 多 Agent 协作房间 | 几个 Agent 在任务频道里分工、抢单、汇报，人随时介入 |

- 默认用一个**假的模型**（脚本化回复），不需要任何 API 密钥就能跑；设了环境变量就换成真实模型。
- 每个示例附三种 Agent 框架的模板：Claude Agent SDK、OpenAI Agents SDK、LangGraph，都通过现有的 Agent SDK 接入。
- **测试**：CI 里每个示例用假模型启动，Playwright 跑完主要流程；另有一个脚本计时"安装 → 发出第一条消息"和"→ 第一个 Agent 回复"，超过验收标准就失败。

## 6. P6 托管云（先不做，2026-10-19）

做托管云已确认、并提前到 [06 §3](06-plan.md) 第 3 步；价格待 D102。这里只写方向，详细设计在第 3 步开始前写。
- 注册后自动创建一个应用（用现有的 `tenant_id` 隔离）、拿到密钥；控制台复用 P4，按应用划分，第 3 步先做其中的密钥和用量部分。
- 免费额度；超出后用 Stripe 按用量计费，计费单位是**合并后的消息**：一条流式 AI 消息不管多少个增量只算一条；Agent 不按月活计费。
- 计费、注册、多区域（欧盟、美国，满足数据驻留）放在单独的控制面服务里，不进开源的 `agentim` 程序。

## 7. D105 合并 Lite 和 AgentIM

现在两个程序的差别只有三处：`agentim-lite` 拒绝多个数据库的配置；握手和 MCP 里报的产品名不同；测试多跑一遍 Lite。合并后：

| 地方 | 改法 |
|---|---|
| 程序 | 删掉 `crates/agentim-lite`，工作区只剩 `agentim` 一个程序 |
| 代码 | 删掉 `edition` 模块、`HubConfig.edition`、`Edition::check`（多库限制按 D101 去掉）；`cli::run_with` 不再带产品参数 |
| 握手 | `server_info` 变为 `{"name": "agentim", "product": "AgentIM", "version": …}`，去掉 `edition` 字段；`capabilities` 照旧。我们自己的 SDK 都不读 `edition`（已检查 `sdk/`），1.0 之前直接去掉 |
| MCP | `serverInfo.name` 固定为 `agentim` |
| 配置 | 没有 Lite 专用的配置项，不用改；原来用 `agentim-lite` 的部署把程序换成 `agentim`，数据库和配置照用 |
| 测试 | 删掉 `AGENTIM_TEST_EDITION` 和 `common::edition()`；`tests/editions.rs` 删掉 Lite 相关的用例，保留"握手报出产品名和版本"并改名移到 `protocol.rs`；其余测试里设置 `Edition::Full` 的地方直接去掉 |
| CI | 删掉 Lite 那一遍测试（Linux 和 Windows 各一遍）和 `agentim-lite` 的 SDK 冒烟、Agent 端到端两步 |
| 脚本和部署 | `sdk/smoke.sh`、`sdk/agent_e2e.sh` 的用法说明、`deploy/Dockerfile`（不再构建 `agentim-lite`） |
| 文档 | 05 §5 里 Lite 的测试行、05 §7 的 `agentim-lite --demo`、11 里"像 Lite 再跑一遍"的说法 |

**不损失覆盖**：Lite 是 AgentIM 的子集，第二遍测试只多验证了"拒绝多库"和产品名，两者都随这次改动删除或保留在 `protocol.rs` 里；其余用例在合并后照常跑一遍。合并做完，CI 每次推送少两个任务的时间。
