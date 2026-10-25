# 12 开发者体验

> 状态：📝 设计已写、待你确认（2026-10-18）。确认前不写代码。本文覆盖 [06 §3](06-plan.md) 的 P1–P6 和 D105；待定事项见 [06 §1.1](06-plan.md)。

目标是一条闭环：**发现 → 5 分钟跑起来 → 30 分钟嵌进自己的应用 → 用开发者控制台管理 → 付费**。验收标准见 [06 §0](06-plan.md)。

参照的做法：Stream / Sendbird（客户端 SDK + UI 组件 + 控制台，按月活计费）、Supabase / PocketBase（一个程序、内置控制台、一条命令跑起来）、Tailscale / Deno（`curl | sh` 和 PowerShell 安装、带签名和校验的发布）。

## 1. P1 安装和发布

**发布物**（打 `v*` 标签时由 CI 生成，挂在 GitHub Releases 上）

| 发布物 | 做法 |
|---|---|
| 预编译程序 | Linux x86_64 / aarch64（musl 静态链接）、macOS x86_64 / arm64、Windows x86_64；GitHub Actions 自己构建（不用 cargo-dist，见下），每个附 SHA-256 |
| 签名 | 用 Sigstore（cosign，无密钥，凭 GitHub 的身份签名）签发布物，并附 GitHub 构建来源证明；macOS 公证和 Windows Authenticode 需要付费证书（D106） |
| 一条命令安装 | `curl -fsSL https://…/install.sh \| sh`、`irm https://…/install.ps1 \| iex`：下载对应平台的程序、校验 SHA-256、放进用户目录的 `bin`，不需要 root |
| Docker 镜像 | `ghcr.io/<org>/agentim:<版本>` 和 `:latest`，amd64 + arm64，用现有的 `deploy/Dockerfile`；镜像签名和 SBOM |
| Helm Chart | 现有的 `deploy/helm/agentim` 以 OCI 包发布到 GHCR，版本和程序一致 |

**为什么不用 cargo-dist**（2026-10-25 调研）：dist（原 cargo-dist，0.33）还在维护，但它生成的安装脚本在机器上没有 `sha256sum` 时只提示一句就跳过校验（模板 `installer.sh.j2` 第 1628 行），macOS 默认只有 `shasum`，等于不校验；它还会接管并重新生成发布工作流。所以安装脚本自己写（校验不过就停，`sha256sum` 和 `shasum -a 256` 都支持），发布工作流也自己写。

**做成的样子**（2026-10-25，`.github/workflows/release.yml`）：
- 打 `v*` 标签时发布；PR 上改到 `server/`、`deploy/`、发布流程或 `CHANGELOG.md` 时整套空跑（构建、安装测试、镜像测试、Chart 检查），只是不上传。程序、Chart 的 `version`/`appVersion`、标签、`CHANGELOG.md` 里的小节必须一致，否则一开始就失败（`deploy/release/version.sh`）；发布说明取 `CHANGELOG.md` 那一节。
- 五个目标：`x86_64`/`aarch64-unknown-linux-musl`（cargo-zigbuild 静态链接）、`x86_64`/`aarch64-apple-darwin`、`x86_64-pc-windows-msvc`；固定 Rust 版本、不用构建缓存、`--locked`；先构建控制台再嵌进程序。文件名不带版本（`agentim-<目标>.tar.gz`、Windows 是 `.zip`），每个旁边一个 `.sha256`，另有汇总的 `SHA256SUMS`，这样安装脚本总能用 `releases/latest/download/`。
- 安装测试在 Linux、macOS、Windows 上真实运行：从本机起的下载地址安装、`--version`、在空目录里不带参数启动、`/readyz`、用开放 API 发第一条消息并读回（`deploy/smoke/first_message.py`）、再装一次（升级）、改坏的压缩包不装；macOS 上另用 Rosetta 跑 Intel 版。
- 镜像用 Dockerfile 的 `release` 阶段装上面构建好的静态程序（和下载的是同一个程序，arm64 不用在 QEMU 里编译）；推送前 amd64 和 arm64 各起一次（arm64 在 QEMU 里）检查 `/readyz` 和版本；推送带 SBOM 和构建来源证明，用户和组都是 10001，再用 cosign 签名。
- Chart 指向同一版本的镜像（`image.tag` 为空时取 `appVersion`），`helm lint`、渲染检查后以 OCI 推到 `ghcr.io/<owner>/charts/agentim` 并签名。
- 发布物（压缩包、安装脚本、`SHA256SUMS`）每个都有 Sigstore 签名（`.sigstore.json`）和 GitHub 构建来源证明。验证：`gh attestation verify agentim-x86_64-unknown-linux-musl.tar.gz --repo <owner>/agentim`，或 `cosign verify-blob --bundle <文件>.sigstore.json --certificate-identity https://github.com/<owner>/agentim/.github/workflows/release.yml@refs/tags/<版本> --certificate-oidc-issuer https://token.actions.githubusercontent.com <文件>`；镜像 `cosign verify ghcr.io/<owner>/agentim:<版本>`（同样的身份参数）。
- 会发布的工作流里第三方 Action 都固定到提交，这是 GitHub 安全加固指南的建议（[Security hardening for GitHub Actions](https://docs.github.com/en/actions/security-for-github-actions/security-guides/security-hardening-for-github-actions#using-third-party-actions)）；标签可以被改写，2025 年 3 月 tj-actions/changed-files 就是这样被投毒的（CVE-2025-30066）。版本按"发布满两周"挑选。
- 安装脚本 `deploy/install/install.sh`、`install.ps1`：只装到用户目录（`~/.agentim/bin`），Windows 加进用户的 PATH（`AGENTIM_NO_MODIFY_PATH=1` 不加），Unix 只提示怎么加；`AGENTIM_VERSION` 装指定版本；没有 `sha256sum` 也没有 `shasum` 时不装；替换程序时一步换掉（运行中的也不受影响）；Windows 出错用 `throw` 而不是 `exit`，免得 `iex` 关掉用户的窗口。

**版本和更新日志**：语义化版本，1.0 之前次版本号可以有不兼容改动，但协议靠能力协商兼容相邻版本（[11 §7](11-cluster.md)）。`CHANGELOG.md` 按 Keep a Changelog 的格式，每个改动在提交时写进"未发布"一节，发布时归档。

**依赖扫描**（✅ 2026-10-25，`.github/workflows/deps.yml`，每次改依赖时、每周一次、每次发布前都跑）：`cargo-deny`（根目录的 `deny.toml`，两个 Rust 工作区：RustSec 已知漏洞和不安全、只允许宽松许可证、只从 crates.io 来；自己的 crate 都标了 `publish = false`）和 `npm audit --audit-level=high`（五个有锁文件的包，连构建工具一起）；Dependabot 管 Cargo、npm、Actions 和 Docker，新版本发布满 14 天才提（`cooldown`，安全更新不等）。镜像扫描用 Grype（`--only-fixed --fail-on high`，有修复的高危才拦），不用设计里写的 Trivy：Trivy 的 GitHub Action 和程序在 2026 年 3 月被攻破，trivy-action 76 个版本标签里 75 个被改成偷 CI 密钥的代码，三周内第二次（[Wiz](https://wiz.io/blog/trivy-compromised-teampcp-supply-chain-attack)、[Snyk](https://snyk.io/es/articles/trivy-github-actions-supply-chain-compromise/)）；Grype 也不用 Action，直接下载固定版本并核对 SHA-256。第一次跑 `cargo-deny` 就查出服务端依赖里 rustls、rustls-webpki、tokio、ctutils、webpki-root-certs 的 8 个公告（7 个漏洞、1 个不安全），已升级到修复版本（安全修复不等两周）。

**第一次运行要顺**（5 分钟目标的关键；✅ 2026-10-25）：
- `agentim` 不带参数：在当前目录建 `agentim.db`，并在旁边生成主密钥文件 `agentim.db.key`（权限 0600）；启动输出里和地址放在一起提示"已生成主密钥、和数据库一起备份，丢了就读不出加密的内容"。已有密钥文件就用它。
- 密钥文件的权限：自动生成的那个只能本人读，别人能读（组或其他人有任何权限）就拒绝启动，并说明 `chmod 600`（OpenSSH 和 PostgreSQL 对私钥也这样）；用 `encryption.key_file` 指定的文件（多是挂进来的 Secret）只在所有人可读时警告，组可读不算（Kubernetes 用 `fsGroup` 加 `defaultMode: 0440`，Helm Chart 已改成这样）。目录不可写时说明可以换目录、`--db` 或 `encryption.key_file`，不留下半成品。
- 启动后打印本机地址和控制台的一次性初始化链接（第一次打开时设置管理员密码和 TOTP，见 §4）。
- `agentim --demo` 保持现状（演示用户和演示 Agent）。

**快速开始**（README 改成这样，`cargo run` 移到"从源码构建"）：

```bash
curl -fsSL https://…/install.sh | sh      # 或 docker run -p 8080:8080 ghcr.io/<org>/agentim
agentim                                    # 打开 http://127.0.0.1:8080
```

**测试**：发布流程在每个 PR 上空跑一次（构建全部目标、不上传）；安装脚本在 Linux、macOS、Windows 的 CI 机器上真实安装并启动，检查 `/readyz` 和发第一条消息；第一次运行的密钥生成测试覆盖：没有密钥文件、已有密钥文件、密钥文件权限过宽（拒绝并说明）、目录不可写。

## 2. 客户端 SDK

> 状态：🔧 设计 2026-10-20 批准（D104），S1–S7 已实现到本环境能做到的程度（2026-10-24），每一步的结果和与设计的差别见 §2.14。调研和来源见 [research/client-sdk.md](research/client-sdk.md)。旧的 TypeScript SDK（`sdk/client-ts`）的规则和测试已搬进 Rust 核心，2026-10-25 删除（D122，git 历史里还在）。

代码：核心和原生运行层 `sdk/client-rust`（`core`、`native`、`wasm`），浏览器 `sdk/client-web`，React `sdk/react`，React Native `sdk/client-react-native`，Swift `sdk/client-swift`，Kotlin `sdk/client-kotlin`，C# 和 Unity `sdk/client-dotnet`，Flutter 和 Dart `sdk/client-flutter`。

### 2.1 目标

开发者只管画界面：**连接、重连、同步、补缺口、离线、发送重试、缓存、排序、去重、历史、多设备、多标签页全部交给 SDK**，弱网、VPN、切网络、切后台、服务端重启或过载、存储不够、库损坏、换账号时都不出错，出错时告诉应用发生了什么、该怎么办。

客户端对核心保证（[01 §3](01-product.md)）负责的部分：

| 保证 | 客户端怎么守住 |
|---|---|
| G1 稳定 | 任何网络、服务端、存储故障都不让应用崩溃、不卡死在"连接中"；核心边界上捕获 panic；重试有预算、有抖动，服务端重启或过载时不会被所有客户端一起压垮 |
| G2 不丢消息 | 用户发的东西**先写进本地队列再发**，服务端确认后才删；收到的事件**先写进本地再确认**；本地库坏了、被浏览器清掉、空间满了，都从服务端重新同步，队列单独保存 |
| G3 有序 | 每个会话按 seq 只应用连续的事件；同一台设备在一个会话里同一时间只有一个操作在路上，按入队顺序提交 |
| G4 业务正确 | 规则只在服务端判断；客户端先在本地显示，被拒绝时退回并报告服务端的错误码 |
| G5 高并发 | 客户端不制造流量尖峰：重连有抖动和退避，离线队列补发限速，同步分批，后台少发心跳 |
| G6 集群 | 节点下线、滚动升级、切主时按服务端给的关闭原因换节点重连，不需要应用处理 |

### 2.2 分层

```
应用 / 界面组件（React、SwiftUI、Compose、Unity UI、Flutter）
  └─ 平台封装（每种语言一个，薄）：WebSocket、存储驱动、网络和前后台信号、
     计时、密钥、把核心的事件变成本平台的可订阅对象、切到主线程
       └─ 接口：C ABI（原生）/ wasm（浏览器）：发命令（JSON），收事件（JSON 回调）
            └─ agentim-client-core（Rust，单线程，不直接做 IO）
                 连接状态机 · 网络和存储探测的判断 · 同步和缺口 · 历史 · 离线队列
                 · 本地存储规则 · 会话和消息状态 · 差量输出 · 诊断
```

- **核心不直接做 IO**（业内叫 sans-IO）：输入"应用调用了 / 收到一帧 / 连接断了 / 计时到了 / 存储返回了 / 网络变了 / 切到前台了"，输出"发这帧 / 开连接 / 写这批行 / 读这一页 / 定个时 / 通知界面这些变化"。同一个核心跑在原生、浏览器、Unity WebGL；测试时时钟、网络、存储全是假的、可控的。
- **核心是单线程的**：所有状态只有它写，没有锁，不会死锁（matrix-rust-sdk 反复踩的坑）；原生上跑在自己的线程，浏览器上跑在 Worker（见 §2.10）。
- **接口是消息**（命令进、事件出），各语言的类型从一份协议描述（JSON Schema）生成，CI 检查一致；TDLib、腾讯云 IM 都是这样，几十种语言都接得上。不用 UniFFI（D111，见调研）。
- **原生平台的 IO 由核心旁边的 Rust 运行层提供**（WebSocket、SQLite、计时），所有原生平台共用，平台封装只补本平台才有的：网络状态、前后台、系统代理、密钥存储。浏览器和 Unity WebGL 由 TypeScript 提供 WebSocket、IndexedDB、WebCrypto。

### 2.3 连接状态

| 状态 | 含义 | 发送 |
|---|---|---|
| `connecting` | 正在连（第一次，或网络恢复后） | 进队列 |
| `connected` | 已连上并完成同步握手 | 立即发 |
| `disconnected` | 短暂断开，自动重试中 | 进队列 |
| `suspended` | 超过 2 分钟没连上（或系统说没网），降低重试频率 | 进队列 |
| `failed` | 不能自动恢复：令牌被拒且拿不到新的、被下线、SDK 太旧、协议错误。**停止重试**，带原因，等应用处理 | 进队列（等应用处理后继续） |
| `closed` | 应用调用了 `close()` | 进队列（下次连接继续） |

界面横幅建议用 `connection.lost`：断开超过 5 秒才报（Liveblocks 的做法），避免一闪一闪。状态带 `reason`（下表的类别）和下次重试时间。

### 2.4 网络探测和故障分类

**探测信号**（平台封装提供，核心判断）：

| 信号 | 来源 | 用法 |
|---|---|---|
| 系统网络状态 | iOS/macOS `NWPathMonitor`（满足/不满足、接口类型、是否昂贵/受限）；Android `ConnectivityManager` 默认网络回调（`VALIDATED`、`CAPTIVE_PORTAL`、`TRANSPORT_VPN`）；Windows `NetworkListManager`；Linux netlink；浏览器 `online`/`offline`（只当提示，不可靠） | 没网：立即断开，停止重试计时，等网络回来；网络变了（换了接口、开关 VPN、换 Wi-Fi）：立即检查连接 |
| 连接活性 | 应用层心跳：没有其他流量时每 25 秒发一个 `ping`，**按墙上时间**算超时（浏览器后台标签页的计时器会被节流）；超过 `2 × 心跳间隔 + 5 秒` 没收到任何帧就判死 | 判死后主动关闭、立即重连一次，失败再按退避 |
| 前后台 | iOS/Android 生命周期、浏览器 `visibilitychange`（Chrome 新版还要听 `resume`）、Unity `OnApplicationPause` | 回到前台：立即发一次 `ping`（5 秒内没回就判死重连）并同步 |
| 连通性探测 | 连接失败而系统说有网时，向服务端的 `/healthz` 发一个 HTTP 请求（可以配置成开发者自己的探测地址） | 分辨"网络能上但连不上我们"（服务端问题、VPN 或防火墙挡了）和"其实没网"（强制门户、只有局域网） |
| 往返时间 | 每次 `ping` 测往返时间 | 弱网（往返时间长、经常超时）时调大请求超时、调小同步批次 |

**故障分类和反应**（参照 Ably、gRPC、AWS、Telegram 的做法，具体数字见 §2.5）：

| 类别 | 怎么判断 | 反应 |
|---|---|---|
| 没网 | 系统报告不满足；或探测也失败且系统报告了强制门户 / 没有验证过的网络 | 进 `suspended`（原因 `offline` 或 `captive_portal`），**不重试**，等系统报告网络回来再立即连 |
| 连接僵死 | 心跳超时，系统说有网 | 关闭、立即重连一次，之后按退避 |
| 切网络 | 默认网络换了（Wi-Fi ↔ 蜂窝、开关 VPN、有线 ↔ 无线） | 旧连接可能已经不通：立即在新网络上重连，退避清零 |
| 服务端不可达 | 连不上 / TLS 失败，但探测显示网络能上别处 | 退避重试；连续失败 2 分钟进 `suspended`；原因 `server_unreachable`（可能是服务端宕机，也可能是 VPN、代理、防火墙挡了，诊断信息里写明） |
| 服务端重启、节点下线 | 关闭原因 `server_restart`（1012）或下线通知 | 换节点重连，**先随机等 0–5 秒**（服务端给了等待时间就用它），避免所有客户端同时涌回；退避不增长 |
| 服务端过载、限流 | `overloaded`、`rate_limited`、`unavailable`、`storage_unavailable`，或握手被拒带 `retry_after_ms` | 至少等服务端给的时间；退避从 1 秒起（比普通故障长）；消耗重试预算（§2.5） |
| 令牌过期 | 握手返回 `unauthorized` | 立即向 `tokenProvider` 要一次新令牌重连；再失败按退避；`tokenProvider` 自己报错时按服务端不可达处理 |
| 令牌被拒 | `tokenProvider` 返回"禁止"；关闭原因 `logged_out` | `failed`（原因 `unauthenticated` / `logged_out`），停止重试 |
| SDK 太旧 | 关闭原因 `client_too_old` | `failed`（原因 `upgrade_required`） |
| 协议错误 | 收到解析不了的帧、服务端报 `bad_request` 于握手 | `failed`（原因 `protocol`），上报诊断；不循环重试 |

#### 2.4.1 VPN、多网络、代理、受限网络

参照 Chromium（网络变化时把连接池里的连接标为过期）、Tailscale（判断"大的链路变化"后重新绑定）、Android 官方文档（新的默认网络出现后，旧网络上的连接"过一阵会被强制断开"）、Apple TN3151、Slack、Socket.IO、SignalR、Telegram 的做法。

- **网络代次**：平台封装把所有网络变化（默认网络换了、接口或地址变了、路由变了、VPN 开关、DNS 换了）汇成一个递增的"网络代次"交给核心。只靠"断网"事件不够：分流 VPN 开关时我们的服务器地址可能改走另一条路，系统却不报断网。
- **代次变了**：在现有连接上立即发一个 `ping`（3 秒超时）；没回、或本机地址 / 接口变了，就关掉重连，不等退避；从确认过的位置继续，不丢不重（同步本来就按位置续上）。100 次 / 分钟的频繁抖动只触发一次重连（合并 1 秒内的变化）。
- **VPN 判断**：Android `TRANSPORT_VPN`；Apple 看 `utun` 接口；Windows 路由和接口变化通知。只用于诊断和"可能是 VPN 挡了"的提示，不改变行为。
- **多网络**：只用系统的默认网络（Wi-Fi 和蜂窝同时在时由系统选）；Apple 上收到"有更好的路径"时重连到新路径。**不**自己绑定到非默认网络（会走计费流量、费电）。
- **IPv4 / IPv6**：连接时 IPv6 和 IPv4 竞速（Happy Eyeballs，RFC 8305，间隔 250 毫秒）；只用域名连、用系统的解析器（这样 VPN 的分流 DNS、私有 DNS、iOS 的 NAT64 才生效）；网络代次变了清掉进程内的 DNS 缓存。CI 加一个只有 IPv6 的测试环境（App Store 要求支持 IPv6-only / NAT64 网络）。
- **代理**：遵守系统代理（环境变量、Windows 注册表和 PAC、macOS SystemConfiguration、Android 默认代理），通过 HTTP CONNECT 建隧道再走 WebSocket（Rust 的 WebSocket 库本身不支持代理，要自己建隧道）；PAC 文件由平台解析；也可以由应用传入代理设置。
- **WebSocket 被挡**（公司代理去掉了 Upgrade、只放行普通 HTTPS）：先只做到**明确报出来**——握手阶段失败时诊断里分开记录 DNS、TCP、TLS、升级、第一帧各自的耗时和结果，原因报 `websocket_blocked`。HTTPS 长轮询备用通道（Socket.IO、SignalR、Telegram 都有）需要服务端新增一条通道，放到以后（D114）。
- **TLS 被中间人检查**（公司代理换了证书）：默认信任系统证书库，所以企业环境照常能用；证书固定（pinning）可选、默认关，开了以后被换证书时报 `tls_intercepted`，不当成普通网络错误重试。
- **强制门户**（酒店、机场 Wi-Fi 要先登录）：Android 看 `CAPTIVE_PORTAL` / 没有 `VALIDATED`，其他平台看探测结果；状态 `suspended`，原因 `captive_portal`，不反复重连，等网络被验证后立即连。
- **弱网和省流量**：自己测往返时间（`ping` 的平滑往返时间和波动），请求超时 = `max(10 秒, 平滑往返 + 4 × 波动)`，最多 30 秒；往返时间长或丢包时同步批次和同时在路上的请求数减半。Apple 低数据模式（`isConstrained`）、计费网络（`isExpensive`）、Android 计费网络、浏览器 `saveData`：推迟预取和历史预加载，用户主动的操作照常。
- **心跳**：前台 25 秒；蜂窝网络和后台（还没被系统挂起时）60 秒，省电（Signal 的做法）。
- **移动端后台**：iOS 切后台几秒后进程就被挂起，Android 的 Doze 会断网，所以连接只在前台保持；后台送达靠系统推送（APNs / FCM，[05 §6](05-operations.md)），回到前台立即同步。
- **自检**：`client.diagnose()` 依次测 DNS、TCP、TLS、WebSocket 升级、第一帧、往返时间，返回每一步的结果，给应用做"网络检测"页面（Slack 有类似的连接测试页）。

### 2.5 重连和发送策略

**重连**
- 退避：全抖动（`等待 = 随机(0, min(上限, 基数 × 2^次数))`），基数 500 毫秒，上限 30 秒；过载类故障基数 1 秒。
- `disconnected` 超过 2 分钟进 `suspended`，之后每 60 秒（±20%）试一次。
- 网络恢复、切网络、回到前台：立即试，退避清零。连上并完成同步后退避清零。
- 每次连接尝试 10 秒超时（含握手和欢迎帧）。
- 同一时间只有一次连接尝试；状态变化都在核心里串行处理（Stream Swift 卡在"断开中"就是状态变化并发引起的）。

**请求**
- 每个请求 10 秒超时（弱网时按往返时间放大，最多 30 秒）。
- 两类请求：
  - **排队的修改**（发送、编辑、撤回、已读、反馈、卡片回应）：进离线队列（§2.7），断线、超时都不丢，结果不明时用同一个幂等键重发。
  - **只能在线的请求**（审批、请人决定、接管，以及所有查询）：没连上立即报 `disconnected`；超时报 `timeout` 并标明"结果不明"，由应用决定是否重试（服务端对重复的决定返回 `conflict`）。
- **重试预算**（gRPC 的令牌桶）：每个客户端 10 个令牌，每次因服务端故障重试扣 1，每次成功加 0.1，剩一半以下就不再立即重试，而是暂停离线队列，等下一次连接成功或 60 秒后再试。防止服务端出问题时客户端越重试越多。
- **补发限速**：重连后离线队列每个会话一次一个、全局最多 8 个同时在路上，不会一次把几百条同时发出去（Socket.IO 的教训）。

### 2.6 同步

沿用已经验证的规则（[08](08-sync-and-client.md)）：每个会话按 seq 只应用连续的，跳号等待，确认不越过缺口，同一连接补不上就重连，定期校验位置。新增：
- **落后太多**（服务端 `reset`）：这个会话的本地记录从重置点重新开始（旧的保留为一个"孤岛"，见 §2.8），通知界面。
- **重连恢复**：只靠位置——服务端按设备记位置，重连后从确认过的位置继续，没有"会话恢复期限"。

### 2.7 离线操作队列

- 每个会话一个队列，**先持久化再发**，同一时间只有一个在路上；不同会话并行（受 §2.5 的全局上限）。
- **本地显示 = 服务端副本 + 队列里的操作叠上去**（Firestore 的覆盖层）：消息表只存服务端确认过的内容；被拒绝时删掉那个操作，显示自动回到服务端的样子。

| 操作 | 本地立即生效 | 排队时合并 | 结果不明后重发怎么不重复 | 被业务拒绝 |
|---|---|---|---|---|
| 发送 | 本地消息 `sending` | — | `client_msg_id`（服务端去重） | 标为 `failed`，可 `retry` / `discard` |
| 编辑 | 显示新内容，标 `pending` | 还没发出的消息：直接改那条发送；同一条连续编辑：只留最后一次 | 带 `base_version`；重发被拒为 `conflict` 时先同步，已是这次的内容就算成功 | 退回，报事件 |
| 撤回 | 显示已撤回 | 还没发出的消息：直接丢弃那条发送和它的编辑 | 重发被拒为"已撤回"就算成功 | 退回 |
| 已读 | 未读数立即变 | 每个会话只留一个，取最大 seq | 只往前，本来幂等 | 丢弃 |
| 反馈 | 立即显示 | 同一条消息只留最后一次 | 覆盖，本来幂等 | 退回，报事件 |
| 卡片回应 | 显示已提交 | — | 重发被拒为"你已经回应过"就算成功 | 退回，报事件 |

- 依赖还没发出的消息的操作挂在它下面，它发出去后换成真实 ID 再发（matrix 的做法）；它最终失败，挂在下面的一起失败。
- 被业务拒绝的不挡后面的；暂时性故障时后面的等着（保证顺序，[08 §4.1](08-sync-and-client.md) S5、S6）。
- **什么时候算失败**（D113，替代 D34）：**只有服务端按业务规则拒绝才失败**（`forbidden`、`not_found`、`blocked`、`conflict`、`bad_request`、`policy_blocked` 等）。断网、服务端不可达、过载、限流都**一直重试**，频率按 §2.5（退避、2 分钟后降到每分钟一次、重试预算、服务端给的等待时间），界面显示"等待网络"或"发送中"，不会因为时间到了变成失败。用户可以随时 `discard` 取消。手动重试失败的消息仍用原来的 `client_msg_id`（服务端去重保留 7 天，超过 7 天的生成新 ID，[08 §4.1](08-sync-and-client.md) S4）。
- 应用能看到整个队列（`client.pending()`）和每个操作的结果（`operation` 事件）。

### 2.8 历史消息

服务端设计见 [08 §2.4](08-sync-and-client.md)（D108：人能按页向服务端要历史，有配置开关）。客户端：
- **先少后多，所有平台一样**（浏览器、手机、桌面、Unity；新设备、重装、本地被清掉都一样）：本地没有这个会话的记录时，服务端先给会话列表，再给每个会话最近 30 条（[08 §2.4](08-sync-and-client.md) `max_messages`，后端可配置），启动快、流量小；用户往上翻时才按页（默认 50 条）向服务端要更早的；**拉下来的和之后收到的都留在本地、默认不删**，下次往上翻先读本地，本地没有才问服务端（Stream、Sendbird、Matrix 的滑动同步都是这样：列表带最近几条，打开、上翻再取）。
- 本地按会话记"连续区段"（seq 从哪到哪是完整的）。同步只延长最新的区段；往回翻、跳到某条消息会产生"孤岛"，中间的缺口往回翻时补上（matrix 的 Gap 块）。
- `loadOlder()`：先读本地；本地这一段到头了（区段的起点之前是缺口）就向服务端要一页，写进本地再显示。服务端说"没有更早的了"（会话开头、可见起点、保留期、开关关了）时 `hasOlder` 变 false，并给出原因，界面可以显示"更早的消息不可见"。
- `jumpTo(seq)` / `jumpToFirstUnread()`：取这条前后各一页，形成孤岛，上下都能继续翻。
- 去重按 seq；服务端返回的是消息的**当前**状态（编辑过的是新内容，撤回的是撤回标记）；之后的编辑、撤回照常经同步到达，更新本地已有的消息。
- 离线时只能翻本地有的。

### 2.9 本地存储和存储探测

- **默认不限大小**，聊天记录一直保留（`maxCacheBytes` 可选，开发者想限就限）。服务端始终是准的：任何时候本地被清掉，都能同步回来、往回翻回来。
- **表**：会话（位置、列表信息、最后打开时间、连续区段）、消息、元数据（结构版本、账号）；**离线队列单独一个小库**，缓存库损坏重建时不丢没发出去的东西。每个账号一套。结构版本按编号迁移，迁移失败删除缓存库重建。
- **加密**：内容 AES-256-GCM（ID、seq、时间不加密，用于查询）；浏览器用不可导出的 WebCrypto 密钥，原生用钥匙串 / Keystore 里的密钥，也可以传自己的加解密函数。
- **存储状态**对应用可见：`client.storage` = `{ kind, persisted, usage, quota, state }`，`state` 是 `ok` / `low` / `full` / `memory_only`，变化时发 `storage` 事件。

**存储失败的处理**（空间不够见 §2.9.1）：

| 情况 | 怎么发现 | 处理 |
|---|---|---|
| 写满（磁盘满、浏览器配额用完） | SQLite `SQLITE_FULL`；IndexedDB `QuotaExceededError` | 先保证离线队列能写（单独的小库）；暂停缓存新的历史页；`state = full`，报事件；空间回来后自动恢复。开发者设了上限或浏览器配额紧张时按最久没打开的会话淘汰消息内容（位置和队列不淘汰） |
| 库损坏 | `SQLITE_CORRUPT` / `SQLITE_NOTADB`；IndexedDB 打不开 | 删掉缓存库重建、从服务端同步；队列库能读的行尽量救回，救不回的报 `operation` 失败事件 |
| 没有写权限、只读、存储不可用（隐私模式） | 打开或写入失败 | 进 `memory_only`，照常工作，关闭后不保留；报事件 |
| 被浏览器清掉（Safari 7 天规则、低空间时整站清除、用户清除） | 启动时发现账号标记没了 | 当作新设备重新同步；报 `storage` 事件（`reset: 'cleared'`） |
| 换账号 | 账号标记不一致 | 清掉上一个账号的缓存和队列 |
| 密钥丢了或换了 | 解不开 | 解不开的行当作没有，从服务端重新取 |

#### 2.9.1 空间不够

参照 Telegram（`optimizeStorage` 只删文件、从不删消息，"所有内容都还在云端"）、Signal（保留期和每个会话条数由用户选）、WhatsApp（按大小列出文件、按会话看占用）、matrix-rust-sdk（媒体缓存 400 MB 上限、60 天没用就删，事件缓存可从服务端重建）、SQLite 官方文档。

**分三层存，空间紧张时从下往上让**：

| 层 | 内容 | 放在哪 | 空间不够时 |
|---|---|---|---|
| 必需 | 账号、密钥、同步位置、离线队列、草稿 | 小库文件；iOS `Application Support`、Android `filesDir`、桌面应用数据目录、浏览器单独的 IndexedDB 库（请求持久化） | **永远不删**；预留空间保证它能写 |
| 历史 | 消息 | 主库文件，位置同上 | 默认不删（聊天记录一直保留）；只有在开发者或用户设了保留策略、或必需层写不进去时，才删最久没打开的会话的消息内容——服务端还能取回来，并且告诉应用 |
| 缓存 | 附件、缩略图、链接预览、头像、搜索索引 | iOS `Caches`、Android `cacheDir`（系统空间不够时自己会清）、浏览器 Cache Storage；不进 iCloud 备份 | 按最久没用先删；有上限（默认 500 MB，可改） |

**探测**：
- 启动、回到前台、每次大批写入之前查剩余空间：iOS 区分"重要用途"和"可选用途"两个值（可选用途的先用完）；Android `getAllocatableBytes`（会算上可清理的缓存）和 `getCacheQuotaBytes`；Windows `GetDiskFreeSpaceExW`（按配额算）；Linux/macOS `statvfs` 的 `f_bavail`；浏览器 `navigator.storage.estimate()`。
- 不只靠查询：**每次写入都检查错误**（SQLite `SQLITE_FULL`、`ENOSPC`、IndexedDB `QuotaExceededError`），因为查询的结果只是估计（浏览器还会故意加噪声）。

**分级反应**：

| 状态 | 条件 | SDK 做什么 |
|---|---|---|
| `ok` | 空间足够 | 正常 |
| `low` | 可选用途的空间低于 500 MB（或浏览器已用超过配额的 80%） | 停止预取、自动下载附件、建搜索索引、预加载历史；报 `storage` 事件，应用可以提示用户 |
| `full` | 写入失败，或重要用途的空间低于 50 MB | 停止缓存新的历史页（照常显示，只是不落盘）；清缓存层；**必需层照常写**（用预留空间）；收到的事件**不确认**（[08 §4.2](08-sync-and-client.md) R6），服务端会再发；发送照常（离线队列在必需层） |
| 恢复 | 空间回来 | 自动回到 `ok`，补上落下的写入 |

**SQLite 的具体做法**（为空间不够预先设好）：
- 建库时就打开 `auto_vacuum = INCREMENTAL`（建表之后再开要整库重写）；删数据后用 `incremental_vacuum` 一点点把空间还给系统——`VACUUM` 要两倍空间，空间不够时用不了。
- `journal_size_limit = 64 MB`，定期 `wal_checkpoint(TRUNCATE)`，防止 WAL 文件一直变大；`temp_store = MEMORY`（临时文件可能在更小的分区上，导致明明有空间却报满）。
- 必需层库旁边放一个 20 MB 的**预留文件**：空间满时先删掉它，保证队列和位置写得进去、清理操作本身也能执行（删除和清理也需要日志空间）；空间恢复后重新建。
- `SQLITE_FULL` 后**明确执行 `ROLLBACK`**（SQLite 有时只撤销一条语句、有时整个事务），并且**绝不在写入失败后确认事件或前进位置**——磁盘满本身不会损坏库，损坏都是程序把失败的写入当成成功。

**给应用的接口**：`client.storage.usage()` 按会话和类型（消息、附件、缓存）列出占用；`client.storage.clearCache()`；`client.storage.setRetention({ days?, perConversation? })`（默认不限，Signal 那样的选项，开发者自己决定是否给用户）；`client.storage.trim(convId)`。附件按内容哈希存成文件，库里只存元数据。

浏览器专门的：登录后请求 `navigator.storage.persist()`（可关闭），结果写进 `storage.persisted`；嵌在别人网站里的 iframe（客服窗口）存储按嵌入的网站隔开、Safari 只有十分之一配额，所以那里只当缓存，历史靠服务端。

### 2.10 浏览器多标签页

同一个账号同一时间只有一个"主标签页"：核心跑在它里面（现在在页面主线程，没放进 Worker，见 §2.14），持有连接、写库；其他标签页通过 BroadcastChannel 拿状态变化、把操作交给主标签页。用 Web Locks 选主，主标签页关闭后锁释放，另一个立即接替。Firestore、Notion、PowerSync 都是这个结构，不这样做会出现两个标签页同时写坏库。接替的标签页接着做旧主标签页在做的事（在连就接着连）；交接的空档里发出的命令不丢：可以重复执行的（发送、已读、查询等；发送的 `client_msg_id` 在标签页里生成，重复仍是一条）由新主标签页重新执行，其他的返回 `disconnected` 让调用方决定。

### 2.11 给应用的接口

- **三个可订阅对象**：`conversations`（会话列表：按最后活动排序，未读数、@我、最后一条、免打扰、正在输入）、`messages(convId)`（`loadOlder()`、`hasOlder`、`jumpTo()`、流式回复、每条的 `sending` / `failed` / `pending`）、`connection`。
- 核心输出**差量**（追加、插入、替换、删除、重置）；**订阅时先给一次完整内容，再给之后的差量**（matrix #3535 修的就是订阅时漏更新）；订阅返回句柄，释放后停止。
- **方法完成时状态已经更新**（`await send()` 返回时消息已经在列表里）。
- 各平台的接法：React `useSyncExternalStore`（快照不变就返回同一个对象）；Swift `@MainActor @Observable` + `AsyncSequence`；Kotlin `StateFlow`；Flutter `Stream`；C# 事件和 `IAsyncEnumerable`，**Unity 保证在主线程触发**。
- **事件**：`connection`、`storage`、`operation`（队列里每个操作的结果）、`reset`（会话被重置）、`side`（审批、请人决定等侧信道）、`signal`（正在输入等）。
- **错误**：`AgentIMError { code, message, retryable, retryAfterMs, docsUrl }`；`code` 用服务端的（`forbidden`、`not_found`、`conflict`、`bad_request`、`blocked`、`rate_limited`……）加客户端的（`disconnected`、`timeout`、`storage`、`closed`、`unauthenticated`、`upgrade_required`）；各语言的枚举都留"未知"分支。
- **诊断**：日志级别和回调；`client.debugState()` 导出连接、同步位置、队列、存储状态和最近的故障，便于开发者报问题。

### 2.12 平台和发布

| 平台 | 封装 | 发布 |
|---|---|---|
| 浏览器、Node、Electron | TypeScript + wasm 核心 | npm |
| React Native | TypeScript + JSI 调原生核心 | npm |
| iOS、macOS | Swift | SwiftPM（XCFramework，含 dSYM） |
| Android | Kotlin | Maven Central（4 种 ABI，16 KB 页对齐） |
| .NET、Unity（Android、iOS、Windows、macOS） | C#；回调带 `MonoPInvokeCallback`，转到主线程 | NuGet、OpenUPM |
| Unity WebGL | C# 通过 `.jslib` 调浏览器版 | 同 Unity 包 |
| Flutter | Dart FFI；网页用 wasm 版 | pub.dev |
| 游戏主机 | 先不做（D112） | — |

体积：原生每个平台每种架构压缩后不超过 3 MB，wasm 压缩后不超过 300 KB；CI 记录体积，超了失败。版本：语义化版本，旧的主版本至少维护 12 个月；服务端可以告诉太旧的 SDK 升级（`client_too_old`）。

#### 2.12.1 打包和发布（A9 / D123，2026-10-25 设计）

所有客户端包用同一个版本号（`sdk/client-rust` 工作区的版本），打 `sdk-v*` 标签时发布；改到 `sdk/` 的推送和 PR 上整套构建、测试、打包，只是不上传（`.github/workflows/sdk-packages.yml`）。能用 OIDC 的注册表都不存长期密钥：npm（[trusted publishers](https://docs.npmjs.com/trusted-publishers)，带 provenance）、NuGet（[trusted publishing](https://learn.microsoft.com/nuget/nuget-org/trusted-publishing)，`NuGet/login` 换一小时的临时密钥）、pub.dev（[automated publishing](https://dart.dev/go/automated-publishing)，按标签）。这三个都要先在注册表网站上登记仓库和工作流文件名，npm 还要求包先存在（第一次手动发布）。

| 包 | 内容 | 测试 | 发布 |
|---|---|---|---|
| 原生库 | `libagentim_client`：Linux x64/arm64（glibc 2.28，cargo-zigbuild）、macOS x64/arm64、Windows x64；每个附 SHA-256 | 各平台跑 C ABI 测试 | 作为 NuGet、Unity 包的内容，也挂在 GitHub Release 上 |
| XCFramework | 静态库：iOS 真机 arm64、iOS 模拟器 arm64+x86_64、macOS arm64+x86_64，带头文件和 module map，压缩包附 SwiftPM 的 checksum | macOS 上用它跑 Swift 一致性场景（W1–W9），并为 iOS 模拟器编译 | SwiftPM 要求 `Package.swift` 在仓库根目录，所以发到单独的 `agentim-swift` 仓库（D125）：`sdk/client-swift/release-package.sh` 生成它的内容（Swift 源码 + `binaryTarget` 指向 Release 上的压缩包和 checksum），CI 用 `swift package dump-package` 检查；推送到那个仓库要等负责人建仓库和令牌 |
| NuGet `AgentIM.Client` | net8.0 和 netstandard2.1 的程序集 + `runtimes/<rid>/native/` 下的原生库 | Linux、macOS、Windows 上用本地源装进一个新项目、连真实服务端发一条消息 | trusted publishing |
| npm（浏览器、React） | `npm pack` 的内容和 `files` 一致、不含源码以外的东西 | 已有的单元和端到端测试 | trusted publishing + provenance；作用域见 D124 |
| npm `@agentimhq/client-react-native` | 纯 C++ TurboModule（`install()` 装上 JSI 绑定），自动链接：Android 用 `android/CMakeLists.txt`（`react-native.config.cjs` 的 `cxxModule*` 三项），iOS 用 podspec；原生核心放在包里：Android 四种 ABI 的 `.so`（16 KB 页，CMake 导入库，应用打包时带上），iOS 动态 XCFramework（带隐私清单）。不放静态 XCFramework：压缩后 70 MB，动态的三份 dylib 不到 5 MB | 新建一个 0.87.1 应用、从 tarball 安装：Android 打 release APK 并检查核心和模块在里面，iOS 打模拟器 release 包、在模拟器里对着真实服务端两人收发 | 同上 |
| Unity `com.agentim.client` | 和 NuGet 同一份 C#，加各平台插件和导入设置（`.meta`），WebGL 用 `.jslib` | 包结构检查；编辑器里的测试要 Unity 许可（D123） | OpenUPM 从 Git 标签取 |
| Android `com.agentim:client` | AAR（四种 ABI、16 KB 页）+ POM + 源码和 javadoc 包，各带 md5、sha1，按 Central Portal 的上传格式打成一个包（`android/maven-bundle.sh`，CI 每次生成并检查）；有负责人的密钥时加 GPG 签名 | 模拟器上的仪器测试（后续） | Maven Central，组名等负责人验证命名空间（D126） |
| Flutter、Dart `agentim_client` | Dart FFI（`@Native`）+ 构建钩子 `hook/build.dart` + `hook/prebuilt.json`（12 个预编译库的文件名和 SHA-256：Android 四种 ABI、iOS 真机和模拟器、Linux、macOS、Windows）；钩子按目标平台下载对应的库并核对，不对就构建失败；仓库里没有 `prebuilt.json`，钩子用 cargo 编（D127） | Dart VM 上的一致性场景；按发布的样子（仓库外的副本、从 `file://` 下载）在 Linux、macOS、Windows 上跑同一场景，改错 SHA-256 构建失败；Flutter Linux 应用在 Xvfb 里对着真实服务端 | pub.dev automated publishing；预编译库挂在 `sdk-v<版本>` Release 上 |

第一步（本次）先做不依赖账号和决定的部分：原生库、XCFramework（含 macOS 上的 Swift 场景和 iOS 模拟器编译）、NuGet 包和它在三种系统上的安装测试、npm 包内容检查。第二步已做 Flutter 和 Dart 的预编译库和构建钩子。

**发布**（`.github/workflows/sdk-release.yml`，打 `sdk-v<版本>` 标签）：`sdk/version.sh` 先查所有包的版本号一致、和标签相同（每次 CI 也查）；然后依赖检查、整套构建和测试（复用 `sdk-packages.yml`）；再把所有文件放上 GitHub Release：Dart 的 12 个预编译库（先核对和包里记录的 SHA-256 一致）、XCFramework、NuGet、npm 包、AAR、`agentim-swift` 的内容，加 `SHA256SUMS`、每个文件的 Sigstore 签名和构建来源证明。之后才发各注册表，每个注册表要负责人先在那边设好（变量或密钥，见工作流开头的说明），没设的跳过、包只在 Release 上：npm（trusted publishing + provenance，先发 client-web）、NuGet（`NuGet/login` 换临时密钥）、pub.dev（automated publishing；`dart pub publish --dry-run` 每次 CI 都跑，必须 0 个警告）、`agentim-swift` 仓库（提交并打版本标签）、Maven Central（签名后上传 bundle）。

还剩 Unity 插件导入设置、Android 模拟器里跑场景（AAR、Flutter、React Native）。

### 2.13 测试和覆盖率

**覆盖率 100%**：核心（Rust）用 `cargo llvm-cov` 统计行和分支，低于 100% CI 失败；各语言封装用各自的工具（TypeScript c8、Swift xccov、Kotlin Kover、C# coverlet、Dart coverage），同样 100%。确实测不到的行（比如平台不可能返回的错误分支）必须加标注并在 [05 §5](05-operations.md) 列出理由。覆盖率只是底线，下面这些才是主体：

| 测试 | 内容 |
|---|---|
| 核心确定性测试 | 假时钟、假网络、假存储，逐条覆盖 §2.3–§2.11 的每一行规则和每个边界（空、最大、未知 ID、重复、自己对自己、离线、刚好超时） |
| 故障注入 | 网络：丢帧、延迟、乱序、半开连接、连接中途断、握手超时、切网络、强制门户、VPN 开关；服务端：重启、过载、限流、令牌过期和被拒、太旧、协议错误；存储：写满、损坏、只读、慢、被清掉、密钥丢失 |
| 随机模拟 | 每次 CI 跑几千个随机种子，随机组合上面的故障和用户操作，检查不变量：确认过的消息不丢、每条只出现一次、顺序和服务端一致、队列最终清空或明确失败、终态之后不再变化；失败时打印种子可以复现 |
| 变异测试 | `cargo mutants` 跑核心，守护竞争和顺序的测试必须能抓到变异 |
| 协议一致性 | 一套与语言无关的场景（现在 TypeScript SDK 的 46 项改写过来，再加上本设计新增的），对着真实的 `agentim` 跑，每个封装都必须全部通过 |
| 平台 | Linux、Windows、macOS、iOS 模拟器、Android 模拟器、Chromium / Firefox / WebKit（Playwright）、Unity 批处理模式；SQLite、IndexedDB 两种存储；服务端三种数据库 |

### 2.14 分步

| 步 | 内容 | 状态（2026-10-24） |
|---|---|---|
| S0 | 探路：最小核心编成 wasm 量体积；Unity IL2CPP 在 iOS 上收回调；Android 16 KB 页；结果写回本节 | 🔧 wasm 核心 450 KB、压缩后 **146 KB**（预算 300 KB，CI 超了失败）；Linux x86_64 原生库压缩后 1.6 MB（预算 3 MB）；Android 四种 ABI 压缩后 1.4–1.7 MB，每个段都按 16 KB 对齐（CI 检查）。IL2CPP 在 iOS 上收回调没有验证：本环境没有 macOS 和 Unity |
| S1 | 服务端：人的历史读取（D108）、应用层 `ping`、欢迎帧带心跳间隔、过载和下线带 `retry_after_ms`（[08 §2.8、§2.9](08-sync-and-client.md)） | ✅ |
| S2 | Rust 核心 + 原生运行层（SQLite、WebSocket）+ 核心测试和一致性场景 | ✅ 核心不做 IO，§2.3–§2.9 的规则都在里面；原生运行层：tokio 单线程、rustls、SQLite（缓存库和队列库分开、20 MB 预留文件）、C ABI（`include/agentim.h`，每个入口捕获 panic） |
| S3 | 浏览器封装：wasm、IndexedDB、加密、多标签页 | ✅ `@agentimhq/client-web`：IndexedDB（AES-GCM，密钥不可导出）、打不开时用内存；Web Locks + BroadcastChannel 一个账号一条连接，主标签页关闭后接替；Node 也能用 |
| S4 | Swift、Kotlin | 🔧 封装写完，在 Linux 上对着真实服务端跑通一致性场景（Swift 6.2；Kotlin 通过 JNI 在 JVM 上）。Android 库（AAR）能构建：NDK r28c 编四种 ABI 的核心和 JNI 桥（16 KB 页），Kotlin 对着 `android.jar` 编译，CI 检查对齐和 JNI 入口；没在模拟器或真机上跑过（本环境没有 KVM）。XCFramework 没做（需要 macOS） |
| S5 | C#（.NET、Unity，含 WebGL 桥） | 🔧 .NET（net8.0、netstandard2.1）跑通一致性场景；Unity 包（主线程、暂停和恢复、网络）写了但没在 Unity 里跑过；WebGL 桥：`AgentIM.jslib` 在浏览器里实现和 C ABI 同名的函数（底下是 `@agentimhq/client-web`），C# 代码各平台一样，测试按 emscripten 的方式加载它、模拟 C# 一侧，对着真实服务端跑；C# 的 WebGL 编译路径在 CI 编译 |
| S6 | Flutter、React Native | 🔧 Dart FFI 在 Dart VM 上跑通一致性场景；Flutter 通过 `agentim_client` 的构建钩子拿到核心（发布的包下载核对过的预编译库，仓库里用 cargo 编）：Linux 桌面应用在虚拟显示里对着真实服务端跑通，Android、iOS 的预编译库在 CI 里构建，但没在设备或模拟器上跑过；React Native：JSI 绑定能对着 0.87.1 的头文件编译，TypeScript 部分用假绑定测试，没在设备上跑过 |
| S7 | React 界面组件和客服窗口示例（§3） | ✅ `@agentimhq/react`：hooks 和组件（含成员列表、转人工提示），每种状态的组件测试，客服窗口在 Chromium 里对着真实服务端和真实 Agent 跑通 |

**每个封装都跑的一致性场景**（W1–W9，对着真实 `agentim`）：两边连上；发送返回时消息已在列表里、之后变成已发送、对方收到、会话进列表（含表情）；编辑和撤回到达对方；被业务拒绝的发送显示失败并报告、删除后消失；在线请求返回服务端的错误码、未知命令 `bad_request`；重启后队列先从缓存显示、连上后按顺序发出且只发一次、队列清空；断网后不是已连接、离线发的消息恢复后只发一次；令牌被拒后停在 `failed`（`unauthenticated`）；关闭后命令返回 `closed`、关两次没问题。

**和设计的差别**（待定的写进 [06 §1.1](06-plan.md)）：
- 浏览器的核心跑在主标签页的主线程，不在 Worker 里（§2.2、§2.10）。核心很小、每次处理很快，先这样；要放进 Worker 时接口不用变。
- 各语言的类型是手写的，没有从 JSON Schema 生成、也没有 CI 一致性检查（§2.2）；靠每个封装都跑同一套一致性场景兜底。
- 覆盖率（§2.13）：核心行覆盖 98.4%，CI 门槛 98%，测不到的行和理由在 [05 §5](05-operations.md)；各语言封装没跑覆盖率工具。没跑 `cargo mutants`，守护顺序和确认的测试做了手工变异验证（[05 §5](05-operations.md)）。
- 平台测试只有 Linux（原生、JVM、Swift、Dart、.NET、Flutter 桌面应用）和 Chromium，Android 只到构建和检查产物；没有 macOS、Windows 上的封装、iOS 和 Android 模拟器、Firefox 和 WebKit、Unity 里跑。
- 一致性场景：TypeScript SDK 的 46 项没有全部改写；核心的规则测试（35 项）、细节测试（18 项）和随机模拟覆盖了规则，原生运行层 6 项、浏览器 4 + 1 项、每个封装 W1–W9 对着真实服务端。
- 发布（npm、SwiftPM、Maven Central、NuGet、OpenUPM、pub.dev）都没做，归到 A9。

## 3. P3 React 界面组件

包名 `@agentimhq/react`，建在 §2.11 的可订阅对象上。

> 状态：🔧 2026-10-24 实现了第一版（`sdk/react`），和下面设计的差别：没做 `usePresence`（服务端没有在线状态）和 `useApproval`（审批用卡片和 `respond`，组件里已有）；消息列表没做真正的虚拟滚动，用 `content-visibility: auto` 让浏览器跳过屏幕外的行；截图对比没做（端到端测试会保存亮色、暗色、从右到左的截图，供人看）；Web 演示端还没改用这套组件。

- **两层**：一层 hooks（`useConversations`、`useMessages`、`useSend`、`useTyping`、`usePresence`、`useApproval`），开发者完全自己画界面也能用；一层组件，建在 hooks 上。
- **组件**：会话列表、消息列表（虚拟滚动；流式 AI 消息、AI 标记、卡片、来源、反馈）、输入框、审批卡片、转人工提示、成员列表、正在输入。
- **AI 标记不可关闭**（D36、EU AI Act 第 50 条）：按 `sender_type` 显示，由消息列表画在可替换的消息组件外面，换掉消息组件也去不掉。
- **换主题和替换组件**：样式全部用 CSS 变量，附一套默认主题（亮色、暗色）；不依赖任何 CSS 框架。每个组件可以通过 `components` 属性换成自己的实现（Stream 的做法），文档里给出完整例子。
- **无障碍和多语言**：键盘操作、读屏标签，按 WCAG 2.1 AA（欧盟无障碍法的要求）；界面文字可以整体替换，支持从右到左的语言。
- **Web 演示端**改用 P2 + P3 重写，作为组件的活样例。
- **测试**：Vitest + Testing Library 测每个组件的状态（空、加载、失败、流式中、被打断）；Playwright 对真实服务端跑端到端；截图对比防止样式退化。

## 4. P4 开发者控制台和管理 API（A5，✅ 2026-10-21）

**内置在 `agentim` 程序里**（D103）：React 单页应用（TypeScript + Vite，不依赖 CSS 框架，源码在 `server/console`），构建后由 `build.rs` 嵌进 Rust 程序，在 `/console` 提供；单机到集群都是同一个程序、同一个端口。`cargo build` 时如果没有构建好的前端（`server/console/dist`），就嵌一个"请从发布版安装或先构建前端"的占位页，Rust 的构建不依赖 Node；CI 的浏览器测试和 Docker 镜像先构建前端。页面带 CSP（只允许同源脚本和样式）、`X-Frame-Options: DENY`、`Referrer-Policy: no-referrer`（初始化链接里有令牌）。

**两块，各自的权限**（[13](13-roles-and-entry-points.md) 问题 10，D116）：

| 块 | 给谁 | 内容 | 角色 |
|---|---|---|---|
| **应用** `/admin/v1/app/…` | 开发者 | 概览（人数、Agent 数、会话数、在线、待处理举报、密钥数）、开放 API 密钥、签名密钥、用户（搜索、资料、设备和下线、停用、客户端权限）、会话（元数据；看内容要填原因并写审计）、Agent（建、急停和恢复、换密钥、转交、销毁）、审核队列、Webhook（只读）、审计日志、应用设置（客户端权限的默认值） | `owner`、`developer`；`moderator` 只有概览、用户和会话的查看、审核队列 |
| **部署** `/admin/v1/deploy/…` | 部署者 | 运行状态（各数据库的类型、从库模式、静态加密、写线程、连接数、版本）、集群节点（状态、下线一个节点，单机时返回 `bad_request`）、管理员账号和管理密钥（只有 `owner`） | `owner`、`operator` |

托管云上客户只拿到"应用"那块。控制台按角色只显示能用的页面。

**管理员账号**：存在控制库 `admins` 表（名字、角色、停用时间），密码和 TOTP 沿用原来内置账号的代码（Argon2id、连续失败锁定 `auth.max_failures`/`lock_minutes`、每个验证码只能用一次、并发哈希有上限），**只给管理员用**。

- **初始化**：还没有管理员时，启动时打印一次性链接 `…/console/setup?token=…`（随机令牌只在内存里存哈希，只能从本机地址用，用一次就失效；重启后换新的）。用它建第一个 `owner`。在容器里运行时浏览器不在本机：用 `docker exec` 在容器里调 `POST /admin/v1/setup`，或者用本机命令 `agentim admin-key create --role owner` 拿到管理密钥再 `agentim admin admin-create`（Grafana 用默认密码、GitLab 把初始密码写进文件，我们不留默认密码）。
- **登录**：`POST /admin/v1/login {name, password, totp?}`，成功后设会话 Cookie（`HttpOnly`、`SameSite=Strict`，HTTPS 或 `behind_proxy` 时加 `Secure`；会话存在 `admin_sessions`，只存哈希，12 小时不用失效、最长 7 天，后台每 10 秒清理），同时返回 CSRF 令牌；所有修改请求都要带 `X-CSRF-Token`。带 Cookie 的请求只接受同源（`Origin` 等于本服务器），客户端 App 的 `allowed_origins` 不算，免得 App 网站上的脚本能操作控制台。名字或密码错统一返回 `invalid_credentials`，缺验证码返回 `totp_required`，锁定返回 `rate_limited`。`POST /admin/v1/logout`。自己的密码（要当前密码，改完所有会话退出）、TOTP 开关在 `…/me`。
- **最后一个 owner**：不能被降级或停用（`conflict`），在控制库写线程里检查，两个 owner 同时互相停用也只成功一个。停用一个管理员立即结束他的会话；`owner` 重设别人的密码也结束那人的会话。
- **命令行用的管理密钥**：`adk…` 开头，带角色，只存哈希（`admin_keys`），在控制台的"管理员"页或本机命令 `agentim admin-key create --role owner` 创建。`agentim admin --url … --key …`（或环境变量 `AGENTIM_ADMIN_URL`、`AGENTIM_ADMIN_KEY`）调同一套管理 API：`overview`、`users`、`user`、`permissions`、`deactivate`、`reactivate`、`agents`、`agent-create`、`agent-key`、`agent-stop`、`agent-resume`、`agent-transfer`、`agent-destroy`、`conversations`、`reports`、`resolve`、`audit`、`api-keys`、`api-key-create`、`api-key-revoke`、`signing-keys`、`signing-key-create`、`signing-key-revoke`、`status`、`cluster`、`drain`、`admins`、`admin-create`（密码从标准输入读）。管理密钥没有密码和 TOTP，`…/me/password` 和 `…/me/totp:*` 对它返回 `bad_request`。
- 原来直接改库的 `agentim agent create/transfer/key/list/export/import` 已删除；`api-key`、`signing-key`、`admin-key`、`token sign` 这些本机命令保留，给第一次部署时还没有管理员的情况。

**规则**：管理 API 的每个修改都走和客户端、开放 API 同一套业务逻辑和错误码，写审计（`admin.<动作>`，操作者是 `admin:<名字>` 或 `admin:key:<密钥名>`）；拒绝的请求不留痕迹。查看会话内容（`GET /admin/v1/app/conversations/{id}/messages?reason=…`）必须带 `reason`（1–500 字节），每次都写审计（GDPR 的目的限制）；会话列表和详情只有元数据。列表按 id 分页（`after`、`before`，各数据库按字节序比较），搜索里的 `%`、`_` 按普通字符处理。集群里从任一节点调用结果一致：会话、密钥、设置都在控制库，改了设置会通知其他进程重新读。

**应用设置**：客户端权限的默认值存在控制库（`app_settings`，按 `tenant_id`），控制台里修改立即生效、所有节点一致、重启后保持；配置文件里的 `[clients.allow]` 只是没改过时的初始值。其他应用级设置（Webhook、发送前拦截、审核时限、Agent 上限）这一版仍在配置文件里，控制台只读显示。改了默认值不主动推送给在线的人（人数可能很多），App 在下次 `hello` 时拿到；单个人的权限变化照旧推 `user.updated`。

**测试**（[05 §5](05-operations.md)）：`tests/admin_api.rs` 在三种数据库上逐个接口测角色、CSRF 和同源、会话过期和清理、初始化令牌只能用一次且只能从本机用、登录锁定和 TOTP、边界、错误码、审计、拒绝不留痕、重启、第二个进程读到同样结果，以及并发（同一个初始化链接只建出一个 owner、两个 owner 互相停用只成功一个、同名管理员只建一个；两处都做过变异检查）；`agentim/tests/cli.rs` 跑真实进程的 `agentim admin`；`ci/console_smoke.mjs` 用 Playwright 跑控制台的主要流程（初始化、建密钥、看会话内容并留下审计、处理举报、改客户端权限、看部署状态、`moderator` 只看到自己的页面）。

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
