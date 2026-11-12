// The words of the homepage, pricing, cloud and benchmark pages, in English
// and Chinese. Every number is measured (README, server/bench, the scale
// workflow, docs/en/testing.md); nothing here is a target.

export type Lang = 'en' | 'zh';
export const GITHUB = 'https://github.com/wmkrfjnwvb-svg/agentim';
export const APP = 'https://app.agentim.co';

export const path = (lang: Lang, p: string) => (lang === 'zh' ? `/zh${p}` : p);

const en = {
  lang: 'en',
  htmlLang: 'en',
  nav: { product: 'Product', cloud: 'Cloud', pricing: 'Pricing', benchmarks: 'Benchmarks', docs: 'Docs', signIn: 'Sign in', start: 'Start free', other: '中文', otherLang: 'zh' as Lang },
  meta: {
    title: 'AgentIM — Messaging infrastructure for people and AI agents',
    description: 'Open-source IM backend with agents as first-class members: ordered, never lost, built to scale. SDKs and UI for every platform. AgentIM Cloud or your own servers.',
  },
  hero: {
    eyebrow: 'Open-source IM infrastructure',
    title: ['Messaging infrastructure', 'for people and AI agents'],
    lead: 'Ordered, never lost, built to scale. Chat, groups and threads for your users, with AI agents as first-class members: streaming replies, human approval and guardrails built in.',
    primary: 'Start free on Cloud',
    secondary: 'Self-host in one command',
    small: ['Free plan, no credit card', 'Unlimited monthly users', 'EU region', 'Apache-2.0'],
  },
  proof: {
    title: 'Measured, not promised',
    note: 'Every number comes from benchmarks in the repository. Run them on your own hardware.',
    link: 'See the benchmarks',
    items: [
      { v: '0', l: 'acknowledged messages lost after kill -9' },
      { v: '3 ms', l: 'median delivery with 10,000 connections' },
      { v: '100k/s', l: 'pushes in a 2,000-member group, 54 MB of memory' },
      { v: '2.3 s', l: 'for 20,000 connections to come back after a storm' },
      { v: '13.5 KB', l: 'of memory per open connection' },
      { v: '0.13 s', l: 'from a crash to serving again' },
    ],
  },
  guarantees: {
    eyebrow: 'Guarantees',
    title: 'The hard parts of messaging, done right',
    lead: 'What makes chat hard is not sending a message. It is never losing one, never showing two in the wrong order, and staying up while machines fail.',
    items: [
      { k: 'Ordered', t: 'One writer per conversation assigns a gap-free sequence. Every device shows the same order, and knows when something is missing.' },
      { k: 'Never lost', t: 'A message is acknowledged only after it is on disk. Kill the process at any moment: nothing acknowledged is gone.' },
      { k: 'Exactly once', t: 'Clients retry with the same message id; the server stores it once. Flaky networks never create duplicates.' },
      { k: 'Always on', t: 'In a cluster, conversations move between nodes with fencing, so a failed node is replaced in seconds without split brain.' },
    ],
    link: 'How the guarantees work',
  },
  arch: {
    eyebrow: 'Architecture',
    title: 'How a message travels',
    lead: 'A single Rust binary. Clients share one core, every conversation has one writer, and the event log is the source of truth.',
    steps: [
      { n: '1', t: 'Client core', d: 'One Rust core in every SDK: offline queue, retries, sync by position.' },
      { n: '2', t: 'Gateway', d: 'WebSocket and HTTP, auth, rate limits, permissions and rules.' },
      { n: '3', t: 'Conversation writer', d: 'One writer per conversation: sequence, idempotency, group commit.' },
      { n: '4', t: 'Event log', d: 'SQLite, PostgreSQL or MySQL. Acknowledged only after it is stored.' },
      { n: '5', t: 'Fan-out', d: 'Pushes to every device in order; agents, webhooks and the audit trail.' },
    ],
    link: 'Read the architecture',
  },
  features: {
    eyebrow: 'Everything chat needs',
    title: 'A complete IM, not a demo',
    lead: 'The features your users expect from a modern messenger, with the server rules behind them.',
    groups: [
      { t: 'Conversations', i: ['Direct messages', 'Groups and channels', 'Roles and permissions', 'Member lifecycle', 'Friends and blocking'] },
      { t: 'Messages', i: ['Text, files and images', 'Edit and recall', 'Threads', 'Reactions', 'Pinned messages', 'Mentions', 'Drafts'] },
      { t: 'Presence and sync', i: ['Read receipts', 'Typing', 'Unread counts', 'Every device in sync', 'Offline queue', 'History and search'] },
      { t: 'Trust and safety', i: ['Reports and moderation', 'Blocking', 'Audit trail', 'Rate limits', 'Retention', 'Encryption at rest'] },
    ],
  },
  ai: {
    eyebrow: 'Built for AI agents',
    title: 'Agents are members, people stay in control',
    lead: 'Not a chat SDK with a bot bolted on. An agent joins a conversation like a person, with the controls a business needs.',
    items: [
      { t: 'Streaming replies', d: 'Token by token to every device, with a Stop button for anyone in the conversation.', href: '/docs/concepts/streaming/' },
      { t: 'Human approval', d: 'Refunds, sends and tool calls wait for a person, with the reason and the rule that asked.', href: '/docs/concepts/approvals/' },
      { t: 'Guardrails', d: 'Loop breakers, budgets, data-loss and prompt-injection checks, enforced on the server.', href: '/docs/concepts/guardrails/' },
      { t: 'Hand-off', d: 'An agent hands a conversation to your team and takes it back, with the context.', href: '/docs/concepts/agents/' },
      { t: 'Cards, forms and steps', d: 'Agents send facts, tables, buttons and forms, and show the steps of their work.', href: '/docs/guides/chat-components/' },
      { t: 'Any model, any framework', d: 'A built-in MCP server, Node and Python agent SDKs, and webhooks.', href: '/docs/agent-sdks/' },
    ],
  },
  platforms: {
    eyebrow: 'SDKs and UI',
    title: 'One core, every platform',
    lead: 'The sync, retry and offline rules live in one Rust core, so your app behaves the same everywhere. Ready-made UI for web and mobile.',
    link: 'Browse the SDKs',
  },
  quality: {
    eyebrow: 'Quality',
    title: 'Tested like a database',
    lead: 'Messaging bugs are data bugs. We test AgentIM the way databases are tested, in public CI.',
    items: [
      { v: '500+', l: 'business tests, run once per database' },
      { v: '3 × 2', l: 'databases × operating systems on every release' },
      { v: '100%', l: 'line and branch coverage of the client core' },
      { v: '36', l: 'multi-node cluster tests with fault injection' },
    ],
    points: [
      'Races are replayed for many rounds, and each race test is proven able to fail by breaking the code it guards.',
      'After the suites, a verifier replays every event log and compares it with the stored state.',
      'Every refusal is a named rule in a public catalog, and every rule is triggered by a test.',
      'Real processes are killed with kill -9 mid-traffic; every acknowledged message must survive.',
    ],
    link: 'How we test',
  },
  deploy: {
    eyebrow: 'Cloud or self-hosted',
    title: 'Run it our way or yours',
    lead: 'The same open-source server in both. Start on Cloud and move to your servers, or the other way round: export and import keep everything.',
    cloud: { t: 'AgentIM Cloud', d: 'We run it for you in the EU. Sign up and send your first message in minutes.', points: ['Free plan, no credit card', 'Unlimited monthly users', 'Priced by peak connections and messages', 'Dashboard, keys, webhooks, usage'], cta: 'Start free', link: 'About the Cloud' },
    self: { t: 'Self-hosted', d: 'One static binary or a container. From a laptop with SQLite to a cluster on PostgreSQL or MySQL.', points: ['Every feature, free (Apache-2.0)', 'Your servers, your data', 'Docker, Helm and installers', 'Cluster with automatic failover'], cta: 'Install', link: 'Install guide' },
    install: 'curl -fsSL https://github.com/wmkrfjnwvb-svg/agentim/releases/latest/download/install.sh | sh',
  },
  compare: {
    eyebrow: 'Compare',
    title: 'How AgentIM differs',
    cols: ['AgentIM Cloud', 'AgentIM self-hosted', 'A typical closed chat API', 'Building it yourself'],
    rows: [
      ['Who runs it', 'We do', 'You do', 'The vendor', 'Your team'],
      ['Where your data lives', 'EU region', 'Your servers', 'The vendor’s cloud', 'Your servers'],
      ['Source code', 'Open (Apache-2.0)', 'Open (Apache-2.0)', 'Closed', 'Yours to write'],
      ['Pricing', 'Free plan; by peak connections and messages, unlimited users', 'Free; you pay for servers', 'Usually per monthly active user', 'Engineering time'],
      ['AI agents as members', 'Built in: streaming, approval, guardrails, MCP', 'Built in', 'Add-ons, or build it', 'Build it'],
      ['Delivery guarantees', 'Ordered, exactly once, durable', 'Ordered, exactly once, durable', 'Varies by vendor', 'Hard to get right'],
      ['Leaving', 'Export and run it yourself', 'Nothing to leave', 'Rebuild on another API', '—'],
      ['Time to the first message', 'Minutes', 'About five minutes', 'Minutes', 'Months'],
    ],
    note: 'The closed-API column describes the common model of hosted chat APIs, not one vendor.',
  },
  security: {
    eyebrow: 'Security and compliance',
    title: 'Ready for regulated markets',
    items: [
      { t: 'Encryption at rest', d: 'Messages, files and secrets encrypted with your key.' },
      { t: 'GDPR', d: 'Export and erasure per user, retention, data processing agreement.' },
      { t: 'EU AI Act', d: 'AI messages are labelled as AI on every client.' },
      { t: 'DSA and app stores', d: 'Reporting, moderation and blocking as stores require.' },
      { t: 'Signed releases', d: 'Sigstore signatures, build provenance and an SBOM.' },
      { t: 'Audit trail', d: 'Every admin and agent action recorded with a reason.' },
    ],
    link: 'Security and compliance',
  },
  cta: { title: 'Ship chat with AI agents this week', lead: 'Free on Cloud, or on your own servers in five minutes.', primary: 'Start free on Cloud', secondary: 'Read the docs' },
  footer: {
    tagline: 'Messaging infrastructure for people and AI agents.',
    privacy: 'This site uses no cookies, analytics or third-party scripts.',
    cols: [
      { t: 'Product', l: [['Cloud', '/cloud/'], ['Pricing', '/pricing/'], ['Benchmarks', '/benchmarks/'], ['Changelog', `${GITHUB}/blob/main/CHANGELOG.md`]] },
      { t: 'Developers', l: [['Quick start', '/docs/get-started/quickstart/'], ['Client SDKs', '/docs/client-sdks/'], ['Agent SDKs', '/docs/agent-sdks/'], ['API reference', '/docs/reference/']] },
      { t: 'Run it', l: [['Install', '/docs/operations/install/'], ['Cluster', '/docs/operations/cluster/'], ['Monitoring', '/docs/operations/monitoring/'], ['Go-live checklist', '/docs/guides/launch-checklist/']] },
      { t: 'Trust', l: [['Security', '/docs/security/'], ['GDPR', '/docs/security/gdpr/'], ['Report a vulnerability', '/docs/security/reporting/'], ['Site privacy', '/docs/security/site-privacy/']] },
      { t: 'Legal', l: [['Terms', '/legal/terms/'], ['Privacy', '/legal/privacy/'], ['DPA', '/legal/dpa/'], ['Subprocessors', '/legal/subprocessors/']] },
    ],
  },
};

type Copy = typeof en;

const zh: Copy = {
  lang: 'zh',
  htmlLang: 'zh-CN',
  nav: { product: '产品', cloud: '云服务', pricing: '价格', benchmarks: '性能', docs: '文档', signIn: '登录', start: '免费开始', other: 'English', otherLang: 'en' },
  meta: {
    title: 'AgentIM — 为人和 AI Agent 打造的即时通讯基础设施',
    description: '开源的 IM 后端，AI Agent 是一等成员：消息有序、不丢、可扩展。全平台 SDK 和界面组件。可用 AgentIM 云服务，也可以部署在自己的服务器上。',
  },
  hero: {
    eyebrow: '开源的即时通讯基础设施',
    title: ['即时通讯基础设施', '为人，也为 AI Agent'],
    lead: '消息有序、不丢、可扩展。为你的用户提供单聊、群聊和话题，AI Agent 作为一等成员加入：流式回复、人工审批、安全护栏，全部内置。',
    primary: '免费使用云服务',
    secondary: '一条命令自己部署',
    small: ['免费版无需绑卡', '不限月活用户', '欧盟数据区域', 'Apache-2.0 开源'],
  },
  proof: {
    title: '实测数据，不是承诺',
    note: '每个数字都来自仓库里的基准测试，你可以在自己的机器上复现。',
    link: '查看性能测试',
    items: [
      { v: '0', l: 'kill -9 后丢失的已确认消息' },
      { v: '3 ms', l: '1 万连接时的投递延迟中位数' },
      { v: '10 万/秒', l: '2,000 人大群的推送量，内存仅 54 MB' },
      { v: '2.3 秒', l: '2 万连接断开后全部恢复' },
      { v: '13.5 KB', l: '每个在线连接占用的内存' },
      { v: '0.13 秒', l: '从崩溃到重新提供服务' },
    ],
  },
  guarantees: {
    eyebrow: '可靠性保证',
    title: '把即时通讯最难的部分做对',
    lead: '即时通讯难的不是发出一条消息，而是一条都不丢、顺序永远不乱、机器出故障时服务照常。',
    items: [
      { k: '有序', t: '每个会话只有一个写入者，分配连续无空洞的序号。所有设备显示同样的顺序，缺了哪条也能发现。' },
      { k: '不丢', t: '消息写入磁盘后才确认。任何时刻杀掉进程，已确认的消息都在。' },
      { k: '恰好一次', t: '客户端用同一个消息 ID 重试，服务端只存一份。网络再不稳定也不会出现重复消息。' },
      { k: '持续可用', t: '集群里会话在节点间带栅栏交接，故障节点几秒内被接替，不会出现双主。' },
    ],
    link: '了解这些保证如何实现',
  },
  arch: {
    eyebrow: '架构',
    title: '一条消息怎么走完全程',
    lead: '一个 Rust 二进制文件。所有客户端共用一个核心，每个会话一个写入者，事件日志是唯一的事实来源。',
    steps: [
      { n: '1', t: '客户端核心', d: '每个 SDK 共用的 Rust 核心：离线队列、重试、按位置同步。' },
      { n: '2', t: '网关', d: 'WebSocket 和 HTTP，鉴权、限流、权限和业务规则。' },
      { n: '3', t: '会话写入者', d: '每个会话一个写入者：序号、幂等、批量提交。' },
      { n: '4', t: '事件日志', d: 'SQLite、PostgreSQL 或 MySQL。存好之后才确认。' },
      { n: '5', t: '扇出', d: '按顺序推送到每台设备，以及 Agent、Webhook 和审计记录。' },
    ],
    link: '阅读架构文档',
  },
  features: {
    eyebrow: '功能完整',
    title: '一套完整的即时通讯，不是演示',
    lead: '现代聊天软件该有的功能都有，背后都有服务端规则保障。',
    groups: [
      { t: '会话', i: ['单聊', '群聊和频道', '角色和权限', '成员进出管理', '好友和拉黑'] },
      { t: '消息', i: ['文字、文件、图片', '编辑和撤回', '话题', '表情回应', '置顶', '@ 提及', '草稿'] },
      { t: '状态和同步', i: ['已读回执', '输入状态', '未读计数', '多端同步', '离线队列', '历史和搜索'] },
      { t: '安全治理', i: ['举报和审核', '拉黑', '审计记录', '限流', '保留期', '静态加密'] },
    ],
  },
  ai: {
    eyebrow: '为 AI Agent 设计',
    title: 'Agent 是成员，人始终掌控',
    lead: '不是在聊天 SDK 上外挂一个机器人。Agent 像人一样加入会话，同时具备企业需要的管控能力。',
    items: [
      { t: '流式回复', d: '逐字推送到每台设备，会话里任何人都可以点"停止"。', href: '/docs/concepts/streaming/' },
      { t: '人工审批', d: '退款、发送、工具调用先等人批准，附带原因和触发的规则。', href: '/docs/concepts/approvals/' },
      { t: '安全护栏', d: '防死循环、预算、数据泄露和提示词注入检查，由服务端强制执行。', href: '/docs/concepts/guardrails/' },
      { t: '转人工', d: 'Agent 把会话连同上下文转给你的团队，处理完再交回。', href: '/docs/concepts/agents/' },
      { t: '卡片、表单和步骤', d: 'Agent 能发数据、表格、按钮和表单，并展示办事的每一步。', href: '/docs/guides/chat-components/' },
      { t: '任意模型和框架', d: '内置 MCP 服务，Node 和 Python Agent SDK，以及 Webhook。', href: '/docs/agent-sdks/' },
    ],
  },
  platforms: {
    eyebrow: 'SDK 和界面组件',
    title: '一个核心，覆盖所有平台',
    lead: '同步、重试、离线规则都在同一个 Rust 核心里，你的应用在每个平台上行为一致。Web 和移动端都有现成的界面组件。',
    link: '浏览 SDK',
  },
  quality: {
    eyebrow: '质量',
    title: '像测试数据库一样测试',
    lead: '消息的 bug 就是数据的 bug。我们用测试数据库的方式测试 AgentIM，全部在公开的 CI 里运行。',
    items: [
      { v: '500+', l: '业务测试，每种数据库各跑一遍' },
      { v: '3 × 2', l: '三种数据库 × 两种操作系统，每次发布都跑' },
      { v: '100%', l: '客户端核心的行覆盖和分支覆盖' },
      { v: '36', l: '带故障注入的多节点集群测试' },
    ],
    points: [
      '并发竞争场景反复跑很多轮；每个竞争测试都做过变异检查：故意改坏被保护的代码，确认测试能抓到。',
      '测试跑完后，校验器回放每个事件日志，和存储的状态逐一比对。',
      '每一次拒绝都是公开规则目录里有名字的一条规则，每条规则都有测试触发。',
      '在真实流量中用 kill -9 杀掉进程，每条已确认的消息都必须还在。',
    ],
    link: '我们怎么测试',
  },
  deploy: {
    eyebrow: '云服务或自己部署',
    title: '托管或自建，随你选择',
    lead: '两种方式运行的是同一套开源服务端。可以先用云服务再迁到自己的服务器，也可以反过来：导出导入，数据完整保留。',
    cloud: { t: 'AgentIM 云服务', d: '我们在欧盟替你运行。注册后几分钟就能发出第一条消息。', points: ['免费版无需绑卡', '不限月活用户', '按并发连接峰值和消息量计费', '控制台、密钥、Webhook、用量'], cta: '免费开始', link: '了解云服务' },
    self: { t: '自己部署', d: '一个静态二进制文件或一个容器。从笔记本上的 SQLite，到 PostgreSQL 或 MySQL 上的集群。', points: ['全部功能免费（Apache-2.0）', '你的服务器，你的数据', 'Docker、Helm 和安装脚本', '集群自动故障切换'], cta: '安装', link: '安装指南' },
    install: 'curl -fsSL https://github.com/wmkrfjnwvb-svg/agentim/releases/latest/download/install.sh | sh',
  },
  compare: {
    eyebrow: '对比',
    title: 'AgentIM 有什么不同',
    cols: ['AgentIM 云服务', 'AgentIM 自己部署', '常见的闭源聊天 API', '完全自己开发'],
    rows: [
      ['谁来运维', '我们', '你', '供应商', '你的团队'],
      ['数据在哪里', '欧盟区域', '你的服务器', '供应商的云', '你的服务器'],
      ['源代码', '开源（Apache-2.0）', '开源（Apache-2.0）', '闭源', '自己写'],
      ['计费方式', '有免费版；按并发连接峰值和消息量，不限用户数', '免费；只付服务器费用', '通常按月活用户数', '研发人力'],
      ['AI Agent 作为成员', '内置：流式、审批、护栏、MCP', '内置', '附加功能，或自己开发', '自己开发'],
      ['消息投递保证', '有序、恰好一次、持久化', '有序、恰好一次、持久化', '因供应商而异', '很难做对'],
      ['想要离开时', '导出后自己部署', '无需迁移', '换一套 API 重写', '—'],
      ['发出第一条消息', '几分钟', '约五分钟', '几分钟', '几个月'],
    ],
    note: '"常见的闭源聊天 API"描述的是托管聊天 API 的通常模式，不特指某一家。',
  },
  security: {
    eyebrow: '安全与合规',
    title: '为受监管的市场准备好了',
    items: [
      { t: '静态加密', d: '消息、文件和密钥用你的主密钥加密。' },
      { t: 'GDPR', d: '按用户导出和删除数据、保留期、数据处理协议。' },
      { t: '欧盟 AI 法案', d: 'AI 发出的消息在每个客户端都标明是 AI。' },
      { t: 'DSA 和应用商店', d: '按应用商店要求提供举报、审核和拉黑。' },
      { t: '签名发布', d: 'Sigstore 签名、构建来源证明和 SBOM。' },
      { t: '审计记录', d: '每个管理员和 Agent 的操作都记录在案，附带原因。' },
    ],
    link: '安全与合规',
  },
  cta: { title: '这周就上线带 AI Agent 的聊天', lead: '云服务免费开始，或五分钟部署到自己的服务器。', primary: '免费使用云服务', secondary: '阅读文档' },
  footer: {
    tagline: '为人和 AI Agent 打造的即时通讯基础设施。',
    privacy: '本站不使用 Cookie、统计分析或第三方脚本。',
    cols: [
      { t: '产品', l: [['云服务', '/zh/cloud/'], ['价格', '/zh/pricing/'], ['性能', '/zh/benchmarks/'], ['更新日志', `${GITHUB}/blob/main/CHANGELOG.md`]] },
      { t: '开发者', l: [['快速开始', '/docs/get-started/quickstart/'], ['客户端 SDK', '/docs/client-sdks/'], ['Agent SDK', '/docs/agent-sdks/'], ['API 参考', '/docs/reference/']] },
      { t: '运维', l: [['安装', '/docs/operations/install/'], ['集群', '/docs/operations/cluster/'], ['监控', '/docs/operations/monitoring/'], ['上线检查表', '/docs/guides/launch-checklist/']] },
      { t: '信任', l: [['安全', '/docs/security/'], ['GDPR', '/docs/security/gdpr/'], ['报告漏洞', '/docs/security/reporting/'], ['本站隐私', '/docs/security/site-privacy/']] },
      { t: '法律', l: [['服务条款', '/legal/terms/'], ['隐私政策', '/legal/privacy/'], ['数据处理协议', '/legal/dpa/'], ['子处理方', '/legal/subprocessors/']] },
    ],
  },
};

export const copy: Record<Lang, Copy> = { en, zh };
export const PLATFORMS = ['Web', 'React', 'React Native', 'iOS · Swift', 'Android · Kotlin', 'Flutter', 'Unity', '.NET', 'Node', 'Python', 'Go', 'MCP'];
