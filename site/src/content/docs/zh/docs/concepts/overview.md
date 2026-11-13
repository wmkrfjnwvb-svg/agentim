---
title: "AgentIM 怎么接入"
description: "四类调用方、各自能做什么，以及一条消息怎样从手机到 Agent 再回来。"
---

AgentIM 是你产品里的聊天部分。你的产品继续管理自己的账号、登录和业务数据；AgentIM 管理会话：谁在里面、说了什么、按什么顺序、谁看过。AI Agent 作为成员加入这些会话，和人坐在一起。

## 四类调用方，四个入口

| 调用方 | 是谁 | 入口 | 凭据 |
|---|---|---|---|
| **终端用户** | 使用你应用的人 | 客户端协议 `/ws`，通过[客户端 SDK](/zh/docs/client-sdks/) | 你的后端签发的客户端令牌（[设备和登录](/zh/docs/concepts/devices-and-login/)） |
| **Agent** | 作为成员接入的 AI 模型 | 用 [Agent SDK](/zh/docs/agent-sdks/) 连 `/ws`，或用 MCP 连 `/mcp` | Agent 密钥（`atk_…`） |
| **你的后端** | 你自己的服务器 | [开放 API](/zh/docs/reference/open-api/) `/v1`、[Webhook](/zh/docs/reference/webhooks/)、[发送前钩子](/zh/docs/guides/before-send/) | 带权限范围的 API 密钥（`ak_…`） |
| **运维** | 运行服务端的人，通常也是开发者 | `agentim` 命令行、配置文件、`/metrics`、[控制台](/zh/docs/operations/dashboard/) | 服务器登录、管理员账号或管理员密钥 |

这样划分是有意的：

- **客户端协议只做一个人聊天时会做的事。** 发送、编辑、撤回、已读、拉黑、举报，管理自己有权管理的会话，监督自己的 Agent（批准草稿、接管、停止）。
- **属于你产品的事都由你的后端做。** 创建、停用、导出和删除账号；让用户登录；决定用户在应用里能做什么（[权限](/zh/docs/concepts/permissions/)）；创建和下线 Agent；审核。
- **运维负责部署本身。** 数据库、集群、加密密钥、全局统计。API 密钥做不了这些，管理员密钥也不能冒充用户。

## 一条消息的路径

```text
 app ── send ──▶ /ws ──▶ writer for the conversation's database ──▶ COMMIT (fsync)
                                                                      │
       ◀── ack (seq 42) ─────────────────────────────────────────────┤
                                                                      ▼
                                          fan-out: every member's devices, agents, webhooks
```

1. **客户端 SDK 先存下消息再发送。** 消息立刻显示为 `sending`。即使应用关了或网络断了，消息仍在队列里，之后会恰好发出一次（[多端同步和离线](/zh/docs/concepts/sync-and-offline/)）。
2. **服务端检查业务规则。** 发送者是成员吗？有人拉黑了他吗？会话解散了吗？发送者的角色决定他能做什么。被拒绝的请求什么都不写：没有消息、没有事件、没有审计噪音。
3. **每个数据库一个写入者提交它。** 同时到达的消息在一个事务里一起提交（组提交），但每条命令有自己的保存点。消息拿到会话里的下一个序号（`seq`）。只有提交真正落盘后才回复客户端。
4. **事件扇出。** 发送者的其他设备、其他成员、Agent 和 Webhook 收到同一个事件、同一个 `seq`。慢的客户端只会丢掉自己的推送并被通知补齐，绝不会拖慢别人。
5. **消息和 Agent 有关时才唤醒它。** 单聊里每条消息都会唤醒；群里是 @ 它、回复它，或点名给它的消息（[Agent](/zh/docs/concepts/agents/)）。

## AgentIM 保证什么

| 保证 | 含义 |
|---|---|
| **不丢** | 已确认的消息能扛过崩溃（`kill -9`）和重启。有副本时，数据库故障切换也不丢（[集群](/zh/docs/operations/cluster/)）。 |
| **有序** | 每个成员看到同一个会话的同一串无空洞的序列。客户端绝不会先处理 43 再处理 42。 |
| **规则正确** | 权限、可见性、审批和去重都由服务端执行，对客户端、Agent 和开放 API 一视同仁。 |
| **稳定** | 过载时直接拒绝（`overloaded`、`rate_limited`），而不是无限排队。慢的或卡住的客户端伤不到别人。 |
| **可扩展** | 每个节点数万连接，群可以有数千人。多个节点共享数据库组成[集群](/zh/docs/operations/cluster/)。 |

这些都在 SQLite、PostgreSQL 和 MySQL 上、在 Linux 和 Windows 上测试，每次发布都跑（[我们怎么测试](/zh/docs/reliability/testing/)）。

## 数据存在哪里

- **数据库。** 单机用 SQLite；生产和集群用 PostgreSQL 或 MySQL（MariaDB）。一个部署可以按*单元*把会话分到多个数据库，比如把某个子公司的数据放在它自己的地区（[数据库](/zh/docs/operations/databases/)）。
- **文件。** 本地文件夹，或 S3 兼容的对象存储。用 S3 时，客户端直接上传和下载（[文件和图片](/zh/docs/concepts/files/)）。
- **静态加密**默认开启。消息内容用 AES-256-GCM 加密，每个会话一把密钥。删除会话就销毁它的密钥（[加密](/zh/docs/operations/encryption/)）。
