# 聊天组件

数据卡片、表单卡片、回复旁的步骤、审批卡片、审批待办、聊天组件本身和主题。这些让一个会办事的助手看起来是产品的一部分，而不是一个聊天机器人。

代码：服务端 `server/crates/agentim-core/src/logic/cards.rs`（卡片和回答的校验）、`hub/requests.rs`（`inbox`、`card_answers`）、`hub/assistants.rs`（`show_card`、`ask_form`、步骤）。Web `sdk/react/src/`（`logic.ts` 无界面的 hooks、`format.ts`、`cards.tsx`、`steps.tsx`、`inbox.tsx`、`components.tsx`、`styles.css`、`tokens.json`、`theme.css`）。React Native `sdk/react-native-ui/src/components.tsx`。Agent SDK 的 `ctx.showCard`、`ctx.askForm`、`ctx.steps`。

## 1. 用途

设计目标：
- **专业，没有"AI 味"**：中性的面板、产品自己的颜色、亮色和暗色。
- **不用配置就能用**：`<Chat>` 画卡片和步骤，`<Inbox>` 列出待办；内置助手默认就用它们。
- **容易扩展**：自己的卡片块、表单字段、步骤、待办项，或者用无界面的 hooks 自己画全部。
- **安全**：卡片是固定种类的块组成的声明式 JSON，从不执行脚本。服务端检查每张卡片和每个回答。

## 2. 功能点

| 编号 | 功能 | 行为 |
|---|---|---|
| UI-01 | 卡片 | `agentim.card` 类型的消息，最多 16 KB：`title`、1–20 个 `blocks`、可选的 `to`（只有这些人能回答）和 `once`（总共只收一个回答） |
| UI-02 | 数据块 | `text`；`facts`（标签和值）；`table`（≤ 8 列、≤ 50 行）；`list`（≤ 20 项，可以选） |
| UI-03 | 值的格式 | `format`：`text`、`number`、`currency:XXX`、`date`、`datetime`、`percent`，按看的人的语言、地区和时区显示。`tone`（`neutral`、`success`、`warning`、`danger`、`info`）给徽标上色 |
| UI-04 | 按钮和表单 | `buttons`（1–10 个选项）；`form`（1–10 个字段：`text`、`textarea`、`number`、`select`、`checkbox`、`date`、`time`、`email`、自定义），支持 `required`、`placeholder`、`hint`、`default`、`pattern`、`min`、`max` |
| UI-05 | 回答 | `respond {msg_id, block_id, action | values}`，检查方式和组件一样。每人每张一次；`once` 总共一次；发卡片的人不能回答；`to` 限定谁能答。回答是日志里的 `card.response` 事件，所以 Webhook 也能收到 |
| UI-06 | 答过的表单一直显示为已答 | `card_answers {msg_id}` 列出谁答了哪个块。值只给回答人和发卡片的人，所以刷新后显示"你提交的内容" |
| UI-07 | 自定义块和字段 | `x-<名字>` 类型的块，带 `data`（≤ 8 KB）和 `fallback`（文字或 `"drop"`）；`x-<名字>` 类型的字段，带 `config`。没有对应组件的客户端显示降级内容 |
| UI-08 | 步骤 | Agent 报 `plan` 和 `step` 类的 `progress`（`{id, state, title, detail}`）；普通的 `tool_call`/`tool_result` 也变成步骤。显示在所属回复的上方；有在跑、在等、失败的就展开，全部完成后收成一行"3 steps · 8s" |
| UI-09 | 审批卡片 | 带 `context.operation` 的请求显示做什么、参数、金额和规则，带批准和拒绝，然后显示结果 |
| UI-10 | 待办 | `<Inbox>`：跨会话列出等当前登录的人处理的一切，有分类标签和数量、搜索、列表和详情（手机上是两页）。操作、草稿（发出、改后发出、丢弃）、转人工（打开、交还）、提问。`<InboxBadge>` 用在导航上 |
| UI-11 | 聊天组件 | `<AgentIM>`（提供者）、`<Chat>`、会话列表、客服窗口：分组、日期、未读线、编辑和删除、图片和文件、AI 标识、从右到左 |
| UI-12 | 主题 | 设计 token（`tokens.json` → CSS 变量 `--aim-*` 和 React Native 的值）、亮色和暗色，测试检查对比度。文档站的实时主题预览生成 CSS |
| UI-13 | 扩展口子 | `components.cardBlocks`、`components.formFields`（也能重画内置类型）、`components.Steps`、`useSteps`、`useInbox`、`useCardAnswers`、`<Inbox extraItems renderItem types onOpen>`、`strings` |
| UI-14 | Agent 的工具 | 内置助手：`show_card`、`ask_form`、每次工具调用一个步骤。Agent SDK：`ctx.showCard`、`ctx.askForm`（等待回答）、`ctx.steps` |
| UI-15 | 话题、回应、置顶 | 消息下方的回应（自己的高亮，常用表情选择器）和话题根下方的"N 条回复"；只在话题里的回复不进消息列表；`Thread` 显示一个话题和它自己的输入框（"同时发到会话"）；`PinnedMessages` 显示最新的置顶和数量。React 和 React Native（回应和回复数）（[messaging](messaging.md)，MSG-19 至 MSG-21） |

## 3. 设计

### 3.1 服务端的卡片

- `logic/cards.rs` 在发送时检查卡片（`check_block`、`check_field`），在回答时检查每个回答（`valid_value`）：
  - 字段类型、长度、选项集合；
  - 日期是 `YYYY-MM-DD`、时间是 `HH:MM`，在 `min` 和 `max` 之间；
  - 邮箱；
  - `pattern` 编译时有大小上限，按整个回答匹配；
  - `default` 自己也必须是合法的回答。
- `to` 是 1–20 个用户 id 或名字，发送时解析。其他人回答返回 `forbidden`（`card.not_for_you`）。
- `card_answers` 读这张卡片序号之后的 `card.response` 事件，返回 `{block_id, action?, values?, by, at}`，不给别人的值。错误：`card.unknown`、`msg.unknown`。
- 撤回的卡片拒绝回答。

### 3.2 助手发的表单

- `ask_form {title, intro?, submit?, fields}` 发一张卡片：`to: [提问的人]`、`once: true`、`client_msg_id = ai-form-{回复}-{调用}`。
- 然后这次回复结束，告诉对方去填表单。
- 回答只唤醒助手一次（占住 `ai-f-{卡片}-{回答人}`），下一条回复以"Maya 填了表单……"和各个值开头。
- 表单的步骤通过 `msg.created` 事件里卡片的 `client_msg_id` 找到，标为完成。

### 3.3 步骤

- 数据约定：
  - `kind: "plan"`，`data: {steps: [{id, title}]}`；
  - `kind: "step"`，`data: {id, state, title?, detail?}`，state 是 `running`、`waiting`、`done`、`failed` 或 `skipped`。
  - `data` 里的其他字段保留，给自定义绘制用。
  - 带 `msg_id` 时步骤在那条回复上方；不带时在会话末尾。
- 组件在每个客户端的 `StepStore` 里把进度合成步骤，所以同一会话的所有视图共享。
- 内置助手的步骤标题用工具的摘要，不带路由和参数；等审批的步骤在决定前是 `waiting`。

### 3.4 格式化

`formatValue(value, format, locale, timeZone)` 用 `Intl`：
- 货币按看的人的习惯（美国 `$1,200.00`，德国 `1.200,00 $`）；
- `date` 是日历日，不受时区影响；
- `datetime` 按看的人的时区；
- `percent` 从小数算。

`formatAgo` 在待办里显示相对时间。

### 3.5 待办

- 数据来自服务端的 `inbox`，收到 `request.*`、`draft.*` 和转人工事件时刷新。
- `useInbox()` 提供 `items`、`counts`、`decide`、`answer`、`approveDraft`、`discardDraft`、`handBack`。
- 别人已经决定的项显示结果，不再显示操作。
- 操作的详情显示：
  - 金额（大字）；
  - 参数；
  - "为什么要审批"（规则，例如 `amount > 30`）；
  - 发起它的那条消息。

### 3.6 外观和主题

- **卡片**是中性面板（`data-kind="card"`），不是聊天气泡，带色调徽标。
- **token** 只有一份（`sdk/react/tokens.json`），Web 和 React Native 共用；`node scripts/tokens.mjs` 重新生成 `theme.css` 和 React Native 的值。
- **暗色**跟随 `prefers-color-scheme`，设了 `data-theme` 时以它为准。
- **对比度**：测试按 WCAG AA 检查文字和背景的组合，包括 `success-soft` 等色调背景。
- **主色和圆角**：产品用几个变量设置：`--aim-accent`、`--aim-accent-text`、`--aim-accent-soft`、`--aim-focus`、`--aim-radius-card`、`--aim-radius-control`、`--aim-radius-bubble`。

## 截图

由测试（`sdk/react/test/screens.mjs`、`examples/test/e2e.mjs`）对真实组件截取。

| | 亮色 | 暗色或手机 |
|---|---|---|
| 数据卡片、表单、步骤（UI-01–UI-08） | <img src="../../images/components/cards.png" width="320" alt="订单表格、退款表单和步骤"> | <img src="../../images/components/cards-dark.png" width="320" alt="暗色"> |
| 审批待办（UI-10） | <img src="../../images/components/inbox.png" width="320" alt="待办里有一笔退款等批准"> | <img src="../../images/components/inbox-dark.png" width="320" alt="暗色的待办"> |
| 手机上的待办：列表和详情 | <img src="../../images/components/inbox-phone.png" width="160" alt="手机上的待办列表"> | <img src="../../images/components/inbox-phone-detail.png" width="160" alt="手机上的待办详情"> |
| 聊天里的审批卡片（UI-09） | <img src="../../images/examples/aftersales-customer.png" width="320" alt="客户自己确认的退款"> | <img src="../../images/examples/aftersales-desk.png" width="320" alt="经理的待办和会话"> |
| 聊天（UI-11） | <img src="../../images/components/web-chat-light.png" width="320" alt="Web 聊天"> | <img src="../../images/components/web-chat-dark.png" width="320" alt="暗色的 Web 聊天"> |
| 从右到左、客服窗口 | <img src="../../images/components/web-chat-rtl.png" width="320" alt="从右到左的语言"> | <img src="../../images/components/web-support.png" width="320" alt="客服窗口"> |
| 文件、图片和查看器 | <img src="../../images/components/web-files.png" width="320" alt="图片和文件消息"> | <img src="../../images/components/web-viewer-phone.png" width="160" alt="手机上的图片查看器"> |
| 离线和发送中的状态 | <img src="../../images/components/web-offline.png" width="200" alt="离线时等待发送的消息"> | <img src="../../images/components/web-phone.png" width="160" alt="手机上的聊天"> |
| React Native | <img src="../../images/components/rn-light.png" width="320" alt="React Native 的列表和聊天"> | <img src="../../images/components/rn-dark.png" width="320" alt="React Native 暗色"> |
| 主题预览（UI-12） | <img src="../../images/components/theming.png" width="200" alt="主色、圆角、亮暗和生成的 CSS"> | <img src="../../images/components/theming-dark.png" width="200" alt="暗色的主题预览"> |

## 4. 接口

- 协议：`kind: "agentim.card"` 的 `send`、`respond`、`card_answers`、`progress`、`inbox`、`decide`、`approve`、`reject`、`handback`。
- Web：`@agentimhq/react` 导出 `AgentIM`、`Chat`、`Inbox`、`InboxBadge`、`ConversationSteps`、`Card`、hooks、`formatValue`。
- 无界面：`@agentimhq/react/headless`。
- React Native：`@agentimhq/react-native-ui`，组件相同。
- 给开发者的指南在文档站："Chat components" 和 "Theme the components"。

## 5. 规则和错误

规则前缀 `card.*`（[目录](../../en/business-rules.md)）：
- `bad_request`（`card.input`）：卡片或回答不合法；
- `forbidden`（`card.not_for_you`）：`to` 以外的人；
- `conflict`：已经答过，或 `once` 的卡片已经有人答了；
- `bad_request`：卡片已撤回。

被拒绝的回答什么都不写。

## 6. 测试

| 功能点 | 测试 |
|---|---|
| UI-01–UI-07 (server) | `server/crates/agentim-core/tests/ai.rs`: `data_and_form_cards`, `cards_take_responses`, `race_once_cards_take_one_response`, `card_refusals_leave_no_trace`（三种数据库） |
| UI-08, UI-14 (server) | `assistant_tools.rs`: `cards_forms_and_steps`; `ai.rs`: `progress_goes_on_the_side`; `mcp.rs`: `agents_use_cards_progress_sources_and_handoff` |
| UI-02–UI-07 (web) | `sdk/react/test/chat-components.test.tsx`: "Data cards" (facts, tables, lists, tones, formats, picking a row, another locale), "Form cards" (checks like the server, read-only for others, answered after a reload), "Your own blocks and fields" |
| UI-08 (web) | `chat-components.test.tsx`: "Steps" (open while working, folded once done, per reply; drawn your way or with `useSteps`); `format.test.ts`: `foldStep` |
| UI-09, UI-10 (web) | `chat-components.test.tsx`: "Inbox" (counts and filters; approve, decline with a reason, edit a draft, hand back, answer; decided by someone else; empty; own items and details) |
| UI-03 | `sdk/react/test/format.test.ts`: `formatValue` in several locales and time zones |
| UI-11, UI-12 | `sdk/react/test/components.test.tsx` (components); `sdk/react/test/format.test.ts` and `scripts/tokens.mjs` (contrast of every token pair); `sdk/react/test/e2e.mjs` (against a real server); `sdk/react/test/screens.mjs` (screenshots in light, dark, phone, RTL compared with baselines in CI) |
| React Native | `sdk/react-native-ui/test/chat-components.test.tsx`: data cards, form cards, own blocks and fields, steps, inbox and badge; `components.test.tsx` |
| UI-14 (SDKs) | `sdk/agent-node/e2e.mjs`, `sdk/agent-python/e2e.py`: `showCard`, `askForm` with answers, `steps` |
| UI-15 | `sdk/react/test/social.test.tsx`: all 7 tests; `sdk/react-native-ui/test/components.test.tsx`: `keeps thread-only replies out, shows the reply count and toggles reactions` |
| End to end | `examples/test/e2e.mjs`: the after-sales example (order card, refund form, steps, approvals in the chat and in the inbox, hand-off) and the advisor example (drafts in the inbox) |


## 7. 限制和下一步

- 没做：聊天里的文档预览、完整的客服工作台组件（售后示例里有一个小的）、MCP Apps。
