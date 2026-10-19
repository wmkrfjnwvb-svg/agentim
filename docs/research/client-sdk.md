# 客户端 SDK 调研和方案（2026-10-19，📝 待你批准）

调研范围：matrix-rust-sdk（读了源码和提交历史）、Telegram TDLib、Stream、Sendbird、腾讯云 IM、环信/Agora、网易云信、融云、PubNub、Twilio、Zulip、Mattermost、Rocket.Chat（读了能拿到的源码）；同步引擎 PowerSync、Replicache/Zero、Firestore、Linear、Electric、Realm、Couchbase Lite、Ditto；绑定工具 UniFFI、uniffi-bindgen-cs、uniffi-bindgen-react-native、flutter_rust_bridge、wasm-bindgen；开发者体验 Ably、Liveblocks、Firebase、Vercel AI SDK。GitHub 的 issue 搜索在本环境不可用，问题主要来自提交记录、更新日志和官方排障页。标"未核实"的是二手资料。

## 1. 结论（一页）

1. **核心用 Rust 写一次，各平台是薄封装**。做游戏、桌面、Unity 的 IM 厂商都是这样（TDLib、腾讯云 IM、网易云信 PC 都是一个原生核心 + C 接口）；Stream、Sendbird 每个平台各写一份，结果各平台行为不一致（缺口上限、离线队列时限、Unity 只有内存缓存都不一样）。PowerSync 2025 年把四套客户端逻辑并成一个 Rust 核心，理由和我们一样。
2. **核心不自己做网络（sans-IO）**：核心是确定性的状态机，输入"收到一帧 / 计时到了 / 存储返回了 / 应用调用了"，输出"发这帧 / 写这些行 / 通知界面这些变化"。matrix-rust-sdk 的 base 和 crypto、PowerSync 的 `powersync_control`、TDLib 的 JSON 接口都是这个思路。好处：同一个核心能跑在原生、浏览器、Unity WebGL；测试完全可控（假时钟、假网络）；没有异步运行时带来的死锁和 Drop 崩溃（matrix 反复踩的坑）。
3. **跨语言接口用一个 C ABI + 消息（命令进、事件出），不用 UniFFI**。理由：UniFFI 还是 0.x、经常破坏性升级，第三方生成器要锁版本；C# 生成器在 Unity IL2CPP 下不能直接用（回调没有 `MonoPInvokeCallback`、生成 C# 10 语法而 Unity 是 C# 9）；React Native、Dart 各要一个不同的工具。TDLib 的 `td_json_client` 被几十种语言直接接入、腾讯云 IM 的 Unity SDK 也是 C 接口传 JSON，证明这条路走得通。各语言的类型从一份协议描述（JSON Schema）生成，CI 检查一致。
4. **浏览器也用这个核心（wasm），由 TypeScript 提供 WebSocket、IndexedDB、WebCrypto**。先做一个测量：核心编成 wasm 压缩后超过 **300 KB**，就改为网页保留纯 TypeScript 并共用一致性测试（见 §6 D109）。参考：matrix 只把加密做成 wasm（压缩后 2.1 MB），但 2026 年在做整个 SDK 的 wasm 版（Aurora）；TDLib 的 wasm 版 28 MB、已停更；腾讯网页单独一套 JS。我们的核心没有端到端加密、没有 SQLite、没有异步运行时，预计小得多，但必须先量。
5. **SDK 自己负责一切同步细节**，开发者只拿"会话列表""消息列表""连接状态"三个可订阅对象。业内最常见的抱怨（重复消息、断线后漏消息、要自己补缺口、内存涨）都是 SDK 把这些推给了开发者。

## 2. 各家做法（要点）

| 产品 | 共用核心 | 本地存储 | 离线队列 | 缺口太大时 |
|---|---|---|---|---|
| TDLib | C++，网页 wasm | SQLite + binlog | 有，持久化 | `differenceTooLong`：重置位置重新拉 |
| matrix-rust-sdk | Rust（UniFFI 给 Swift/Kotlin），网页只有加密 | SQLite / IndexedDB，按编号迁移 | 每个房间一个，持久化后再发，一次一个，依赖请求 | 插入 Gap 块，界面不跨缺口显示，往回翻时补 |
| Stream | 每个平台一套 | Core Data / Room / drift / op-sqlite；Unity 只有内存 | JS 有 PendingTask 表；iOS 每频道一个、12 小时后丢弃 | 超过 30 天或事件太多：清库或只刷新打开的频道 |
| Sendbird | 每个平台一套 | IndexedDB / MMKV，默认 256 MB | 失败的标为待发，重连自动重发 | 缺 300 条以上：通知应用重建集合 |
| 腾讯云 IM | C 核心，网页单独 JS | 本地库（未核实） | 未核实 | 未核实 |
| PowerSync | Rust（SQLite 扩展） | SQLite（网页 wa-sqlite + OPFS） | `ps_crud` 先进先出，队列不空时不前进检查点 | 检查点 |
| Firestore | 每个平台一套 | IndexedDB，100 MB LRU | 按用户分区的批次队列，**本地视图 = 服务端副本 + 覆盖层** | — |
| Replicache/Zero | — | IndexedDB | 每个客户端递增编号，服务端去重，**拉取后重放未确认的操作** | — |

## 3. 方案

### 3.1 分层

```
应用 / 界面组件（React、SwiftUI、Compose、Unity UI、Flutter）
  └─ 平台封装（每种语言一个，薄）：WebSocket、存储驱动、计时、网络和前后台、
     密钥、把事件转成本平台的可订阅对象、切到主线程
       └─ C ABI / wasm 接口：agentim_send(json)、事件回调(json)
            └─ agentim-client-core（Rust，sans-IO，单线程）
                 连接状态机 · 同步和缺口 · 离线队列 · 本地存储规则 · 会话和消息状态 · 差量输出
```

- **核心是单线程的执行者**：所有状态只有它一个人写（matrix 的死锁都来自多个组件互相回调、锁重入）。原生上它跑在自己的线程，网页上跑在主线程或 Worker。
- **存储**：核心发出"写这批行 / 读这一页"，原生封装用核心自带的 SQLite（`rusqlite` 静态编译进去，所有原生平台同一套 SQL，不用各平台的 SQLite 驱动），网页由 TypeScript 用 IndexedDB 实现同一组操作。表沿用已完成的存储第二版（会话、消息、离线队列、元数据，每个账号一个库，结构版本迁移，损坏重建，256 MB 上限按最久没打开的会话淘汰，内容 AES-GCM 加密）。**离线队列放在单独的小库文件里**：缓存库损坏重建时不丢没发出去的东西（Firestore、Notion 的教训）。
- **网络**：原生封装默认用核心旁边的 Rust WebSocket（tungstenite + rustls，原生平台共用）；网页、Unity WebGL 用浏览器的 WebSocket。核心只认"连上了 / 收到一帧 / 断了"。

### 3.2 同步

保留已经验证过的规则：每个会话按 seq 只应用连续的，跳号等待，确认不越过缺口，同一连接补不上就重连，定期校验位置。新增：
- **缺口太大**（服务端返回 `reset`）：丢掉这个会话的本地窗口，从最新一页开始显示，通知界面（Sendbird、Stream、TDLib 都这么做）。
- **往回翻**：本机缓存没有时向服务端要（D108，需要服务端给人开按页读历史）。没有它，新设备、缓存被淘汰后都看不到更早的消息——所有竞品都支持。

### 3.3 离线操作队列（主要参照 matrix 的 send queue + Firestore 的覆盖层）

- 每个会话一个队列，**先持久化再发**，同一时间只有一个在路上；不同会话并行。
- 幂等键：发送用 `client_msg_id`，其余操作用操作 ID；结果不明（断线）就用同一个键重发。
- **本地显示 = 服务端副本 + 队列里的操作叠上去**。消息表只存服务端确认过的内容；本地消息、待生效的编辑和撤回都从队列算出来。被拒绝时把操作删掉，显示自动回到服务端的样子，不需要另存"旧值"。
- 依赖还没发出去的消息的操作（编辑、撤回、回应）挂在它下面，它发出去后换成真实 ID 再发（matrix 的 dependent request）；没发出去就撤回 = 直接丢弃。
- 两类错误：可重试的（断线、超时、限流、过载）暂停这个会话的队列，网络恢复时继续；业务拒绝的标为失败。**失败的消息不挡住后面的**（matrix 会挡住、保证顺序；聊天应用的习惯是失败的那条显示重试按钮，后面照常发），见 D110。
- 在队列里超过 3 天还没发出的按失败处理（Stream iOS 是 12 小时）。
- 审批、请人决定、接管不排队（作用于正在运行的 Agent，几小时后才送到的决定可能已经不对）。

### 3.4 给界面的状态

- 三个可订阅对象：`conversations`（会话列表）、`messages(convId)`（消息列表，`loadOlder()`、`hasOlder`、流式回复）、`connection`。
- 核心输出**差量**（追加、插入、替换、删除、重置），订阅时先给一次"重置"（完整的当前内容）再给后续差量——matrix #3535 修过的就是订阅时漏更新、顺序错。
- 订阅返回一个句柄，不再需要时释放（matrix 的 `TaskHandle`；Sendbird 的 `dispose()`；内存泄漏是 Sendbird 论坛的常见问题）。
- 各平台的接法：React `useSyncExternalStore`（快照不变就返回同一个对象，Zero 的做法）；Swift `@MainActor @Observable` + `AsyncSequence`；Kotlin `StateFlow`；Flutter `Stream`；Unity C# 事件，**保证在主线程触发**（腾讯、Stream、PubNub 的 Unity SDK 都这样做）。
- 方法完成时状态已经更新（Stream Swift 新状态层的保证）。

### 3.5 连接和鉴权

- 状态：`connecting`、`connected`、`disconnected`（短暂，自动重试）、`suspended`（超过 2 分钟没连上，降低重试频率）、`failed`（令牌被拒且换不到新的，停止重试，要应用处理）、`closed`。参照 Ably。
- 界面横幅用"连接丢失超过 5 秒"才通知（Liveblocks），避免一闪一闪。
- `tokenProvider` 返回"禁止"时停止重试（Liveblocks、Ably 的做法），防止刷爆开发者后端。
- 收不到任何帧超过两个心跳周期判死重连；网页后台标签页计时器被节流，切回前台立即检查（Supabase 的已知问题）。

### 3.6 浏览器多标签页

一个账号同一时间只有一个"主标签页"持有连接和写库（Web Locks 选主，关掉后另一个接替），其他标签页通过 BroadcastChannel 收变化、把操作交给主标签页。Firestore、Notion、PowerSync 都是这个结构；不这样做会出现两个标签页同时写坏库（Notion 遇到过）。

### 3.7 平台和发布

| 平台 | 封装 | 发布 |
|---|---|---|
| 网页、Node、Electron | TypeScript + wasm 核心 | npm |
| React Native | TypeScript 封装 + JSI 调原生核心 | npm |
| iOS、macOS | Swift | SwiftPM（XCFramework） |
| Android | Kotlin | Maven Central（4 种 ABI，16 KB 页对齐） |
| .NET、Unity（Android、iOS、Windows、macOS） | C#，C ABI，回调带 `MonoPInvokeCallback`，转到主线程 | NuGet、Unity 包（OpenUPM） |
| Unity WebGL | C# 通过 `.jslib` 调网页版 SDK（腾讯的做法） | 同 Unity 包 |
| Flutter | Dart FFI 调 C ABI；网页用 wasm 版 | pub.dev |
| 游戏主机 | 先不做（工具链要签保密协议，Rust 没有官方目标） | — |

体积目标：原生每个平台每种架构的核心库压缩后不超过 3 MB（`opt-level="z"`、LTO、strip）；wasm 压缩后不超过 300 KB。CI 每次构建记录体积，超了就失败。

### 3.8 错误、测试、版本

- 错误：服务端的错误码（`forbidden`、`not_found`、`conflict`、`bad_request`、`blocked`、`rate_limited`）加客户端的（`disconnected`、`timeout`、`storage`、`closed`），每个带 `retryable`、`retryAfterMs`、文档链接；各语言的错误枚举留"未知"分支，新版本加码不会让旧应用崩。
- 给开发者的测试工具：假网络、可注入的时钟；`agentim` 单个程序就是本地模拟器（Firebase 模拟器那样，我们天然有）。
- 我们自己的测试：**一套协议一致性场景**（现在 TS SDK 的 46 项改写成与语言无关的场景描述），每个封装都要全部跑过；核心用假时钟、假网络做确定性测试和竞争测试；Linux、Windows、macOS、iOS 模拟器、Android 模拟器、三种浏览器都在 CI 里跑。
- 版本：语义化版本；旧的主版本至少维护 12 个月；服务端能告诉太旧的 SDK "请升级"（Ably 80024）。

### 3.9 已知的坑（照着检查）

Drop 在错误的线程上崩溃、任务互相持有导致泄漏、锁重入死锁（matrix 多次）；panic 穿过 C ABI 会让应用崩溃（边界上必须 `catch_unwind`）；调试版在设备上崩溃（matrix #4009，用 opt-level 1 的调试配置）；Android 16 KB 页；iOS 符号表（dSYM）要随包发布；IL2CPP 下泛型回调被裁掉；回调线程不是主线程。

## 4. 现有 TypeScript SDK 怎么办

它的规则和测试是核心的规格：同步顺序、缺口、发送去重、存储第二版的表和规则都搬进 Rust 核心；TypeScript 留下 WebSocket、IndexedDB、WebCrypto、多标签页和可订阅对象这一层。46 项测试改写成一致性场景。

## 5. 分步（每步做完、测完、提交，再做下一步）

| 步 | 内容 |
|---|---|
| S0 | 探路（一周内）：最小核心编成 wasm 量体积；Unity IL2CPP 在 iOS 上收回调；Android 16 KB 页。结果写回本文，决定 D109 |
| S1 | Rust 核心：连接状态机、同步、离线队列、存储规则、差量输出；原生 SQLite；一致性场景全部跑过（Linux、Windows、macOS） |
| S2 | 网页封装（wasm + TypeScript）：IndexedDB、加密、多标签页；Playwright 三种浏览器 |
| S3 | Swift、Kotlin 封装和发布 |
| S4 | C#（.NET、Unity，含 WebGL 桥） |
| S5 | Flutter、React Native |
| S6 | React 界面组件和客服窗口示例（P3） |

## 6. 需要你决定

| # | 事项 | 我的建议 |
|---|---|---|
| D109 | 浏览器用不用 Rust 核心 | 用（wasm），S0 量出来压缩后超过 300 KB 就网页保留纯 TS |
| D108 | 人能不能向服务端按页要历史 | 开（新设备、缓存淘汰后才能往回翻） |
| D110 | 失败的消息挡不挡后面的 | 不挡（显示重试按钮，后面照常发） |
| D111 | 跨语言接口 | 一个 C ABI + 消息，类型从协议描述生成；不用 UniFFI |
| D112 | 游戏主机 | 先不做 |

## 7. 来源

matrix-rust-sdk：ARCHITECTURE.md、`crates/matrix-sdk/src/send_queue/`、`event_cache/`、`linked_chunk`、issue #3058、#3535、#4009，element-web#28632，FOSDEM 2025/2026 演讲。TDLib：core.telegram.org/tdlib/docs、`UpdatesManager.cpp`、`example/web/tdweb`。Stream：stream-chat-js `offline-support/`、stream-chat-swift `MessageSender.swift`、`OfflineRequestsRepository.swift`、stream-chat-android `SyncManager.kt`、stream-chat-unity `WebsocketClient.cs`。Sendbird：v4 local caching、MessageCollection、error codes 文档。腾讯云 IM：TencentCloud/chat-sdk-unity `NativeSDK.cs`、`TencentIMSDK.cs`。PowerSync：client architecture、consistency、“SQLite persistence on the web”（2026-05）、powersync-sqlite-core。Replicache：how it works、push spec。Firestore：firebase-js-sdk PR #9241、`indexeddb_persistence.ts`。Notion：wasm SQLite 博客。UniFFI 0.32.2 文档；NordSecurity/uniffi-bindgen-cs（issue #178）；jhugman/uniffi-bindgen-react-native；flutter_rust_bridge 2.13；Unity WebGL native plugins 文档；LiveKit Unity SDK。Ably connection states、error codes、deprecation policy；Liveblocks React API；Vercel AI SDK useChat；Supabase 后台标签页断线排障页。
