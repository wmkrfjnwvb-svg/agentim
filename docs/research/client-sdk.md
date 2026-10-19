# 客户端 SDK 调研（2026-10-19）

调研范围：matrix-rust-sdk（读了源码和提交历史）、Telegram TDLib、Stream、Sendbird、腾讯云 IM、环信/Agora、网易云信、融云、PubNub、Twilio、Zulip、Mattermost、Rocket.Chat（读了能拿到的源码）；同步引擎 PowerSync、Replicache/Zero、Firestore、Linear、Electric、Realm、Couchbase Lite、Ditto；绑定工具 UniFFI、uniffi-bindgen-cs、uniffi-bindgen-react-native、flutter_rust_bridge、wasm-bindgen；开发者体验 Ably、Liveblocks、Firebase、Vercel AI SDK。GitHub 的 issue 搜索在本环境不可用，问题主要来自提交记录、更新日志和官方排障页。标"未核实"的是二手资料。

## 1. 结论（一页）

1. **核心用 Rust 写一次，各平台是薄封装**。做游戏、桌面、Unity 的 IM 厂商都是这样（TDLib、腾讯云 IM、网易云信 PC 都是一个原生核心 + C 接口）；Stream、Sendbird 每个平台各写一份，结果各平台行为不一致（缺口上限、离线队列时限、Unity 只有内存缓存都不一样）。PowerSync 2025 年把四套客户端逻辑并成一个 Rust 核心，理由和我们一样。
2. **核心不自己做网络（sans-IO）**：核心是确定性的状态机，输入"收到一帧 / 计时到了 / 存储返回了 / 应用调用了"，输出"发这帧 / 写这些行 / 通知界面这些变化"。matrix-rust-sdk 的 base 和 crypto、PowerSync 的 `powersync_control`、TDLib 的 JSON 接口都是这个思路。好处：同一个核心能跑在原生、浏览器、Unity WebGL；测试完全可控（假时钟、假网络）；没有异步运行时带来的死锁和 Drop 崩溃（matrix 反复踩的坑）。
3. **跨语言接口用一个 C ABI + 消息（命令进、事件出），不用 UniFFI**。理由：UniFFI 还是 0.x、经常破坏性升级，第三方生成器要锁版本；C# 生成器在 Unity IL2CPP 下不能直接用（回调没有 `MonoPInvokeCallback`、生成 C# 10 语法而 Unity 是 C# 9）；React Native、Dart 各要一个不同的工具。TDLib 的 `td_json_client` 被几十种语言直接接入、腾讯云 IM 的 Unity SDK 也是 C 接口传 JSON，证明这条路走得通。各语言的类型从一份协议描述（JSON Schema）生成，CI 检查一致。
4. **浏览器也用这个核心（wasm），由 TypeScript 提供 WebSocket、IndexedDB、WebCrypto**（2026-10-19 已定）。参考：matrix 的加密 wasm 压缩后 2.1 MB，2026 年在做整个 SDK 的 wasm 版（Aurora）；TDLib 的 wasm 版 28 MB、已停更；腾讯网页单独一套 JS。我们的核心没有端到端加密、不带 SQLite 和异步运行时，体积目标压缩后 300 KB，探路时先量。
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

## 3. 补充调研（2026-10-19 第二轮）

**浏览器存储**：Chrome / Edge 每个站点最多可用磁盘的 60%；Firefox 默认 10%（同一主域合计最多 10 GiB），持久化后 50%；Safari 17 起浏览器里约 60%，内嵌 WebView 约 15%，**跨站 iframe 只有父页面配额的十分之一**，写满直接报 `QuotaExceededError`。默认是"尽力保存"：磁盘紧张时整站被清；Safari 开启防跟踪时 7 天没有用户交互就清掉脚本写入的所有存储（主屏幕网页应用除外）。`navigator.storage.persist()` 能请求持久化（Chrome、Safari 静默决定，Firefox 弹窗）。iframe 里的存储按嵌入的网站隔开。大的网页 IM（Element、WhatsApp Web、Telegram Web、Slack、Discord）都只把浏览器当缓存，完整历史在服务端。结论：浏览器不设人为上限，但必须能在任何时候被清空后重新同步。

**往回翻历史**：Zulip 用锚点（seq / 最新 / 第一条未读）+ 前后各多少条，一个接口覆盖所有翻页和跳转；Discord 前后各最多 100 条，没有读历史权限时返回空列表而不是报错；Matrix 的可见范围在每条事件发生时按当时的设置算、改设置不追溯，没有 `end` 就是到头了；Sendbird 是应用级开关（新成员能不能看加入前的消息），Telegram、Zulip 是按会话的设置；Slack 免费版只给 90 天并用 `is_limited` 告诉客户端。业内都不为成员读自己的会话写审计。

**重连和发送**：Ably 断开 2 分钟后进 `suspended`、降低重试频率，令牌被拒（403）进 `failed` 不再重试；gRPC 退避基数 1 秒、乘 1.6、上限 120 秒，并用令牌桶限制重试（失败扣 1、成功加 0.1，剩一半以下不再重试）；AWS 用全抖动，限流类错误基数比普通错误大；Google SRE 建议重试最多 3 次、只在被拒的那一层重试；Telegram `FLOOD_WAIT_X` 要等 X 秒。网络恢复、切到前台时各家都立即重连、退避清零。心跳：Stream 25 秒、Phoenix 30 秒、Signal 前台 30 秒后台 60 秒；浏览器后台标签页的计时器被节流（最慢一分钟一次），要按墙上时间判断。教训：没有抖动导致重启后的连接风暴（Discord 事故）；状态变化并发导致卡在"连接中"（Stream Swift）；离线时攒下的发送一次性涌出（Socket.IO）。

**VPN 和多网络**：Android 新的默认网络出现后，旧网络上的连接"过一阵会被强制断开"；分流 VPN 可能让我们的服务器改走另一条路而系统不报断网，所以要监听路由和地址变化，而不只是断网；Chromium 在网络变化时把连接池标为过期，Tailscale 判断大的链路变化后重新绑定。Apple TN3151：自己用 BSD 套接字就要自己做 Happy Eyeballs、用系统解析器、在只有 IPv6 的网络上测试（App Store 要求）。Rust 的 WebSocket 库不支持代理，要自己用 HTTP CONNECT 建隧道。Slack 要求公司代理放行它的 WebSocket 域名并提供连接测试页；Socket.IO、SignalR、Telegram 有 HTTPS 长轮询备用。

**空间不够**：Telegram 只删文件、从不删消息（"所有内容都还在云端"），Keep Media 按私聊、群、频道分别设；Signal 让用户选消息保留期和每个会话的条数；WhatsApp 按大小列出文件；matrix-rust-sdk 媒体缓存 400 MB、60 天没用就删。iOS 把库放 `Application Support`、媒体放 `Caches`；Android 库放 `filesDir`、媒体放 `cacheDir`（系统空间不够时会清）。SQLite：磁盘满报 `SQLITE_FULL`，要明确 `ROLLBACK`；`VACUUM` 要两倍空间，所以建库时就开 `auto_vacuum = INCREMENTAL`；WAL 要设上限并定期截断；磁盘满本身不损坏库，损坏都来自程序把失败的写入当成功。

设计见 [12 §2](../12-developer-experience.md) 和 [08 §2.8–§2.9](../08-sync-and-client.md)。

## 4. 来源

matrix-rust-sdk：ARCHITECTURE.md、`crates/matrix-sdk/src/send_queue/`、`event_cache/`、`linked_chunk`、issue #3058、#3535、#4009，element-web#28632，FOSDEM 2025/2026 演讲。TDLib：core.telegram.org/tdlib/docs、`UpdatesManager.cpp`、`example/web/tdweb`。Stream：stream-chat-js `offline-support/`、stream-chat-swift `MessageSender.swift`、`OfflineRequestsRepository.swift`、stream-chat-android `SyncManager.kt`、stream-chat-unity `WebsocketClient.cs`。Sendbird：v4 local caching、MessageCollection、error codes 文档。腾讯云 IM：TencentCloud/chat-sdk-unity `NativeSDK.cs`、`TencentIMSDK.cs`。PowerSync：client architecture、consistency、“SQLite persistence on the web”（2026-05）、powersync-sqlite-core。Replicache：how it works、push spec。Firestore：firebase-js-sdk PR #9241、`indexeddb_persistence.ts`。Notion：wasm SQLite 博客。UniFFI 0.32.2 文档；NordSecurity/uniffi-bindgen-cs（issue #178）；jhugman/uniffi-bindgen-react-native；flutter_rust_bridge 2.13；Unity WebGL native plugins 文档；LiveKit Unity SDK。Ably connection states、error codes、deprecation policy；Liveblocks React API；Vercel AI SDK useChat；Supabase 后台标签页断线排障页。

第二轮：MDN 存储配额和清除规则、WebKit 博客（存储策略更新、完整的第三方 Cookie 拦截）、web.dev 持久化存储；Zulip `GET /messages`、Discord 消息接口、Matrix `/messages` 和 history_visibility、Sendbird 聊天历史设置、Slack `conversations.history`、XMPP XEP-0313；Ably 规范、gRPC 退避和重试提案 A6、AWS 重试行为、Google SRE《处理过载》、Telegram 错误和传输文档、Stream 和 Phoenix 源码、Signal 的连接健康监控、Chrome 计时器节流；Android 读取网络状态、Apple TN3151 和 WWDC19 712、Chromium 网络变化、Tailscale netmon、Slack 网络要求、Socket.IO 工作原理、Signal 代理；Apple 和 Android 存储位置文档、SQLite 结果码 / 事务 / WAL / VACUUM 文档、TDLib `optimizeStorage`、Signal 存储管理、matrix-sdk `MediaRetentionPolicy`。
