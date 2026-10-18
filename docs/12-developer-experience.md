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

包名 `@agentim/client`，纯 TypeScript，无运行时依赖；同时支持浏览器、React Native 和 Node 18+（测试用）。它是给**人用的应用**的 SDK；现有的 Agent SDK（`sdk/agent-node`、`sdk/agent-python`）继续服务 Agent，两者共用协议，以后可以共用传输层。

**结构**

| 模块 | 做什么 |
|---|---|
| 连接 | WebSocket、心跳、断线后指数退避加随机抖动重连；服务端让客户端离开（1012）时立即换连接 |
| 鉴权 | `tokenProvider: () => Promise<string>`：令牌由开发者自己的后端通过开放 API 签发；过期前自动刷新，401 时再要一次 |
| 握手 | 协议版本和能力协商；不认识的事件逐条跳过 |
| 同步 | 每个会话按 seq 记位置；推送出现缺口时补拉；`resync` 时按服务端给的原因重置；每 30 秒用 `heads` 校验位置（和 Agent SDK 一致） |
| 发送队列 | 每条消息带 `client_msg_id`；`unavailable`、`overloaded`、断线时用同一个 id 重试，最多 2 分钟后标为失败（D34）；状态：发送中、已发送、失败 |
| 本地缓存 | 存储接口 + 两个实现：内存、IndexedDB（浏览器）；React Native 由开发者传入存储适配器（例如 MMKV），默认内存 |
| 流式消息 | 把增量拼成完整消息，暴露"正在生成"状态；完成、失败、被打断都能看到 |
| AI 交互 | 审批（批准、修改、拒绝）、请人决定（回答、选择）、卡片回传、反馈、转人工 |
| 在线和输入中 | 订阅和发送，按会话节流 |

**类型**：协议类型不手写。服务端从 Rust 的协议定义导出 JSON Schema，SDK 的 TypeScript 类型由它生成；CI 检查两边一致，协议改了没重新生成就失败。

**测试**
- 协议一致性：SDK 对着真实的 `agentim` 程序跑一组场景，和服务端的场景测试同一套（发送和去重、断线时正在发送、推送缺口补拉、resync、流式、审批、请人决定、拉黑和移出后的行为），三种数据库、Linux 和 Windows。
- 故障：测试里用一个 WebSocket 代理制造断线、延迟、丢帧，检查不丢、不重、不乱。
- 浏览器：Playwright 在 Chromium、Firefox、WebKit 里跑 IndexedDB 缓存和重连。
- 发布到 npm 时带来源证明（`npm publish --provenance`）。

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

## 6. P6 托管云（待 D102）

只写方向，D102 确认后再写详细设计。
- 注册后自动创建一个应用（用现有的 `tenant_id` 隔离）、拿到密钥，控制台复用 P4，按应用划分。
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
