// The product on the homepage: our own React components (sdk/react), live,
// with a fake client and a fixed clock, in three views: the messaging app
// (list, chat with threads, reactions and a pinned message, members), an
// agent at work (cards, a form, steps waiting for approval) and the approval
// inbox. English or Chinese data and interface text.

import { useEffect, useMemo, useState } from 'react';
import { Observable } from '../../../sdk/client-web/src/view';
import { AgentIM, Chat, ConversationList, Inbox, MemberList } from '../../../sdk/react/src/index';
import '../../../sdk/react/theme.css';

type Lang = 'en' | 'zh';
const NOW = Date.parse('2026-10-26T17:30:00Z');
const at = (iso: string) => Date.parse(iso);
const ME = 'u_maya';

class View extends Observable<any[]> {
  hasOlder = false;
  olderEnd = 'start';
  async loadOlder() {}
}

const zhStrings: Record<string, any> = {
  ai: 'AI',
  you: '你',
  search: '搜索',
  placeholder: '输入消息',
  send: '发送',
  stop: '停止',
  streaming: '正在输入…',
  failed: '未送达',
  retry: '重试',
  discard: '删除',
  sources: '来源',
  today: '今天',
  start: '会话从这里开始',
  empty: '还没有消息',
  loadOlder: '加载更早的消息',
  typing: (names: string[]) => `${names.join('、')} 正在输入…`,
  replies: (n: number) => `${n} 条回复`,
  replyInThread: '在话题中回复',
  pinned: '置顶',
  pinnedCount: (n: number) => `${n} 条置顶消息`,
  react: '添加表情',
  reactedBy: (key: string, n: number, me: boolean) => `${key} ${n}${me ? '，包括你' : ''}`,
  submit: '提交',
  stepsTitle: '步骤',
  stepsSummary: (n: number) => `${n} 个步骤`,
  stepStates: { pending: '未开始', running: '进行中', waiting: '等待中', done: '已完成', failed: '失败', skipped: '已跳过' },
  showSteps: '展开步骤',
  hideSteps: '收起步骤',
  approve: '批准',
  decline: '拒绝',
  inbox: '待办',
  inboxTabs: { all: '全部', request: '审批', draft: '草稿', handoff: '转人工', question: '提问', done: '已处理' },
  inboxSearch: '搜索待办',
  inboxSelect: '选择一项查看详情',
  inboxExpiresSoon: '即将过期',
  inboxFor: (name: string) => `为 ${name}`,
  inboxRule: '为什么需要审批',
  inboxTriggeredBy: '会话中的请求',
  inboxDraftFrom: (agent: string) => `${agent} 建议回复`,
  inboxHandoffFrom: (agent: string) => `${agent} 把会话转给了你`,
  inboxOpen: '打开会话',
  inboxWaiting: (ago: string) => `已等待 ${ago}`,
};

function data(lang: Lang) {
  const t = (en: string, zh: string) => (lang === 'zh' ? zh : en);
  let seq = 0;
  const m = (sender: [string, string, string], content: string, iso: string, extra: any = {}) => ({
    key: `m${++seq}`,
    msg_id: `m${seq}`,
    conv_id: 'c_support',
    sender_id: sender[0],
    sender_name: sender[1],
    sender_type: sender[2],
    content,
    created_at: at(iso),
    created_seq: seq,
    order: seq,
    status: 'sent',
    state: 'committed',
    version: 1,
    ...extra,
  });
  const maya: [string, string, string] = [ME, t('Maya Chen', '陈玛雅'), 'human'];
  const jordan: [string, string, string] = ['u_jordan', t('Jordan Lee', '李乔丹'), 'human'];
  const nova: [string, string, string] = ['u_nova', 'Nova', 'agent'];

  const app = [
    m(maya, t('Hi! Where is my order #4821? It’s been a week.', '你好！我的订单 #4821 到哪了？已经一周了。'), '2026-10-26T16:50:00Z'),
    m(nova, t('Your order shipped this morning with UPS. It should arrive by Thursday, Oct 29.', '你的订单今天上午已由 UPS 发出，预计 10 月 29 日（周四）送达。'), '2026-10-26T16:50:20Z', {
      envelope: { sources: [{ title: t('Order #4821', '订单 #4821'), url: '#' }, { title: t('Shipping policy', '配送政策'), url: '#' }] },
      reactions: [{ key: '👍', count: 2, me: true, users: [ME, 'u_jordan'] }, { key: '🙏', count: 1, me: false, users: ['u_jordan'] }],
    }),
    m(maya, t('Great, thanks. Can I still change the delivery address?', '太好了，谢谢。还能改收货地址吗？'), '2026-10-26T16:58:00Z'),
    m(nova, t('Updating the address needs a quick confirmation.', '修改地址需要你确认一下。'), '2026-10-26T16:58:30Z', {
      kind: 'agentim.card',
      payload: {
        title: t('Change delivery address?', '修改收货地址？'),
        blocks: [
          { type: 'text', text: t('UPS allows one change before the package is out for delivery.', '包裹派送前，UPS 允许修改一次地址。') },
          { type: 'buttons', id: 'b1', options: [{ id: 'yes', label: t('Change address', '修改地址'), style: 'primary' }, { id: 'no', label: t('Keep it', '不改了') }] },
        ],
      },
    }),
    m(jordan, t('Hey Maya, Jordan from support. I’ve got this one — what’s the new address?', '你好玛雅，我是客服乔丹，这个我来处理。新地址是？'), '2026-10-26T17:20:00Z', {
      thread: { replies: 3, last_at: at('2026-10-26T17:22:00Z'), participants: ['u_jordan', 'u_nova'] },
      reactions: [{ key: '🙌', count: 1, me: true, users: [ME] }],
    }),
    m(maya, t('500 Market St, San Francisco, CA 94105, Apt 12B', '上海市静安区南京西路 500 号 12B 室'), '2026-10-26T17:24:00Z', {
      reactions: [{ key: '✅', count: 1, me: false, users: ['u_jordan'] }, { key: '👍', count: 2, me: false, users: ['u_jordan', 'u_nova'] }],
    }),
    m(nova, t('Updated. You’ll get a new tracking email shortly', '已更新，新的物流通知稍后会发给你'), '2026-10-26T17:25:00Z', {
      state: 'streaming',
      streamed: t('Updated. You’ll get a new tracking email shortly', '已更新，新的物流通知稍后会发给你'),
    }),
  ];
  const members = [
    { user_id: ME, name: maya[1], kind: 'human', role: 'member' },
    { user_id: 'u_jordan', name: jordan[1], kind: 'human', role: 'admin' },
    { user_id: 'u_nova', name: 'Nova', kind: 'agent', role: 'member' },
  ];
  const conversations = [
    { conv_id: 'c_support', title: t('Acme Support', 'Acme 客服'), unread: 0, last_message: app[app.length - 1], members },
    { conv_id: 'c_design', title: t('Design team', '设计组'), unread: 3, last_message: { content: t('New mocks are in Figma 🎨', '新设计稿已放到 Figma 🎨'), sender_id: 'u_jordan', created_at: at('2026-10-26T15:02:00Z') } },
    { conv_id: 'c_launch', title: t('Launch checklist', '上线清单'), unread: 0, last_message: { content: t('Ship it Thursday?', '周四上线？'), sender_id: ME, created_at: at('2026-10-25T19:00:00Z') } },
    { conv_id: 'c_ops', title: t('Ops agents', '运维 Agent 群'), unread: 12, last_message: { content: t('Deploy 2.4.1 finished on 3 nodes', '2.4.1 已在 3 个节点部署完成'), sender_id: 'u_bot', created_at: at('2026-10-25T13:00:00Z') } },
    { conv_id: 'c_nova', title: 'Nova', unread: 0, last_message: { content: t('Here’s your weekly summary.', '这是你的本周摘要。'), sender_id: 'u_nova', created_at: at('2026-10-22T13:00:00Z') } },
  ];
  const pins = [{ msg_id: 'm2', msg: { content: t('Refunds over $30 need a manager’s approval.', '超过 30 美元的退款需要主管审批。') } }, { msg_id: 'm9', msg: { content: '…' } }];

  const cards = [
    m(maya, t('Where is my order 1002? It should have arrived by now.', '我的订单 1002 怎么还没到？'), '2026-10-26T17:20:00Z'),
    m(nova, 'Order 1002', '2026-10-26T17:20:06Z', {
      msg_id: 'r1',
      kind: 'agentim.card',
      payload: {
        title: t('Order 1002', '订单 1002'),
        blocks: [
          { type: 'facts', items: [{ label: t('Item', '商品'), value: t('Bluetooth speaker', '蓝牙音箱') }, { label: t('Total', '金额'), value: 120, format: 'currency:USD' }, { label: t('Status', '状态'), value: t('Delayed', '延误'), tone: 'warning' }, { label: t('Ordered', '下单'), value: '2026-10-19', format: 'date' }] },
        ],
      },
    }),
    m(nova, t('Your speaker is delayed at the carrier. I can refund it, or send a replacement.', '你的音箱在承运商那里延误了。我可以帮你退款，或者补发一个。'), '2026-10-26T17:20:08Z', { msg_id: 'r2' }),
    m(maya, t('Please refund it.', '请退款。'), '2026-10-26T17:21:00Z'),
    m(nova, t('Your recent orders', '你最近的订单'), '2026-10-26T17:22:30Z', {
      msg_id: 'r3',
      kind: 'agentim.card',
      payload: { title: t('Your recent orders', '你最近的订单'), blocks: [{ type: 'table', columns: [{ label: t('Order', '订单') }, { label: t('Item', '商品') }, { label: t('Total', '金额'), format: 'currency:USD' }, { label: t('Status', '状态') }], rows: [['1001', t('Wireless headphones', '无线耳机'), 25, t('Delivered', '已送达')], ['1002', t('Bluetooth speaker', '蓝牙音箱'), 120, t('Refund pending', '退款中')], ['0997', t('USB-C cable', 'USB-C 线'), 12.5, t('Delivered', '已送达')]] }] },
    }),
    m(nova, t('The refund of $120 needs a manager’s approval. You’ll see the answer here.', '120 美元的退款需要主管审批，结果会在这里通知你。'), '2026-10-26T17:22:40Z', { msg_id: 'r4' }),
  ];
  const steps = {
    r1: [
      { id: 'a', title: t('Look up the order', '查询订单'), state: 'done' },
      { id: 'b', title: t('Check the carrier', '查询物流'), state: 'done' },
    ],
    r4: [
      { id: 'a', title: t('Check the refund rules', '检查退款规则'), state: 'done' },
      { id: 'b', title: t('Refund $120', '退款 120 美元'), state: 'waiting', detail: t('Waiting for approval from Sam', '等待 Sam 审批') },
      { id: 'c', title: t('Email the receipt', '发送收据'), state: 'pending' },
    ],
  };

  const inbox = [
    { type: 'request', request_id: 'q1', kind: 'approve', conv_id: 'c1', conv_title: maya[1], agent_name: 'Nova', asker_name: maya[1], created_at: NOW - 4 * 60_000, expires_at: NOW + 3_000_000, title: 'x', context: { operation: { name: 'createRefund', description: t('Refund an order (POST /refunds)', '订单退款（POST /refunds）'), connection: 'orders', fields: [{ name: 'order', value: '1002' }, { name: 'amount', value: 120 }, { name: 'reason', value: t('Arrived late', '送达太晚') }], amount: 120, rule: { tool: 'createRefund', amount_field: 'amount', over: 30 } }, tool_call: { msg: { content: t('Please refund it.', '请退款。'), sender_name: maya[1] } } } },
    { type: 'request', request_id: 'q2', kind: 'approve', conv_id: 'c2', conv_title: t('Leo Park', '朴利奥'), agent_name: 'Nova', asker_name: t('Leo Park', '朴利奥'), created_at: NOW - 22 * 60_000, expires_at: NOW + 6 * 60_000, title: 'x', context: { operation: { name: 'createRefund', description: t('Refund an order (POST /refunds)', '订单退款（POST /refunds）'), fields: [{ name: 'order', value: '0981' }, { name: 'amount', value: 64 }], amount: 64, rule: { tool: 'createRefund', amount_field: 'amount', over: 30 } } } },
    { type: 'draft', draft_id: 'd1', conv_id: 'c3', conv_title: t('Chris Wong', '黄克里斯'), agent_name: 'Aide', content: t('Selling now would lock in losses. Could we look at your plan together on Thursday at 10?', '现在卖出会锁定亏损。周四上午 10 点一起看看你的计划好吗？'), created_at: NOW - 9 * 60_000 },
    { type: 'handoff', conv_id: 'c4', agent_id: 'u_nova', agent_name: 'Nova', conv_title: t('Priya Shah', '普丽雅'), note: t('Wants to speak to a manager about a late delivery', '因配送延误想找主管'), created_at: NOW - 30 * 60_000 },
    { type: 'request', request_id: 'q3', kind: 'choose', options: [t('Replacement', '补发'), t('Store credit', '店铺积分')], conv_id: 'c5', conv_title: t('Sam Ortiz', '奥尔蒂斯'), agent_name: 'Nova', title: t('Offer a replacement or store credit for order 1010?', '订单 1010 给补发还是店铺积分？'), created_at: NOW - 2 * 3_600_000, expires_at: NOW + 3_600_000 },
  ];
  // Scenario A: the business systems behind the agent (the developer's own app).
  const desk = {
    customer: { name: maya[1], tier: t('Gold member since 2023', '2023 年起的金卡会员'), stats: [[t('Orders', '订单'), '14'], [t('Lifetime', '累计消费'), '$2,340'], [t('Refunds (6 mo)', '近 6 月退款'), '0']] },
    order: [[t('Order', '订单'), '1002'], [t('Item', '商品'), t('Bluetooth speaker', '蓝牙音箱')], [t('Amount', '金额'), '$120.00'], [t('Carrier', '承运商'), 'UPS 1Z999AA1…'], [t('Status', '状态'), t('Delayed 4 days', '延误 4 天')]],
    calls: [
      { at: '10:20:01', m: 'GET', path: '/orders/1002', sys: t('Orders API', '订单系统'), res: '200 · 84 ms', tone: 'ok', note: t('read-only, ran at once', '只读，直接执行') },
      { at: '10:20:03', m: 'GET', path: '/shipments/1Z999AA1', sys: 'UPS (MCP)', res: '200 · 212 ms', tone: 'ok', note: t('carrier confirms the delay', '承运商确认延误') },
      { at: '10:21:02', m: 'GET', path: '/policies/refunds', sys: t('Help center', '帮助中心'), res: '200 · 41 ms', tone: 'ok', note: t('refunds over $30 need a lead', '超过 30 美元需主管审批') },
      { at: '10:21:04', m: 'POST', path: '/refunds', sys: t('Payments API', '支付系统'), res: t('held', '已暂停'), tone: 'warn', note: t('changes money: waiting for Sam (support lead)', '涉及资金：等待主管 Sam 审批') },
    ],
    guards: [t('Personal data masked in logs', '日志中的个人信息已脱敏'), t('3 of 50 tool calls used', '已用 3/50 次工具调用'), t('No prompt injection found', '未发现提示词注入')],
    trace: 'tr_7f3a9c1e',
  };
  // The supervisor's console around the inbox.
  const ops = {
    kpis: [[t('Waiting for you', '待你处理'), '5'], [t('Median decision', '平均处理时长'), '2m 40s'], [t('Approved by rule today', '今日按规则自动通过'), '128'], [t('Handed to people', '转人工'), '3']],
    risk: { score: t('Low · 12/100', '低 · 12/100'), why: [t('No refunds in the last 6 months', '近 6 个月无退款'), t('Delay confirmed by UPS', 'UPS 已确认延误'), t('Amount within the order total', '金额未超过订单总额')] },
    policy: t('Refunds over $30 need a support lead. Under $30 the customer confirms and it runs.', '超过 30 美元的退款需要客服主管审批；30 美元以下顾客确认后直接执行。'),
    excerpt: [[maya[1], t('Where is my order 1002? It should have arrived by now.', '我的订单 1002 怎么还没到？')], ['Nova', t('Your speaker is delayed at the carrier. I can refund it, or send a replacement.', '你的音箱在承运商那里延误了。可以退款或补发。')], [maya[1], t('Please refund it.', '请退款。')]],
    audit: [['10:21:04', t('Nova asked to run createRefund ($120)', 'Nova 请求执行 createRefund（120 美元）')], ['10:21:04', t('Rule matched: amount > 30', '命中规则：金额 > 30')], ['10:21:05', t('Assigned to Sam (support lead)', '分配给 Sam（客服主管）')], ['10:21:05', t('Decide within 50 min, then it expires', '50 分钟内未处理将过期')]],
  };
  // Scenario D: a marketplace; the seller sees an assistant the buyer does not.
  const leo: [string, string, string] = ['u_leo', t('Leo (buyer)', 'Leo（买家）'), 'human'];
  const ana: [string, string, string] = [ME, t('Ana (seller)', 'Ana（卖家）'), 'human'];
  const market = [
    m(leo, t('Hi! Is the Sigma 35mm still available? Any haze or fungus?', '你好！Sigma 35mm 镜头还在吗？有没有起雾或发霉？'), '2026-10-26T17:12:00Z'),
    m(ana, t('Yes, still available. Let me check the inspection notes.', '还在的，我查一下检测记录。'), '2026-10-26T17:13:00Z'),
    m(leo, t('Great. Can it reach Berlin by Friday? I could also pay you by bank transfer directly.', '好的。周五前能寄到柏林吗？我也可以直接银行转账给你。'), '2026-10-26T17:15:00Z'),
  ];
  const copilot = {
    intent: t('Wants the lens by Friday in Berlin; asks about haze and fungus; offers to pay off the platform.', '希望周五前寄到柏林；关心起雾和发霉；提出绕过平台直接转账。'),
    facts: [[t('Listing', '商品'), 'L-883 · Sigma 35mm f/1.4 Art'], [t('Inspection', '检测'), t('Oct 20: grade A-, no haze, no fungus', '10 月 20 日：A- 级，无起雾、无霉斑')], [t('Shipping to DE', '寄往德国'), t('DHL Express, 2 days, €18', 'DHL 特快，2 天，18 欧元')]],
    draft: t('Yes! Inspected on Oct 20: grade A-, no haze or fungus. With DHL Express it arrives in 2 days, so Friday works. Please pay through the checkout so you’re covered by buyer protection.', '在的！10 月 20 日检测为 A- 级，无起雾、无霉斑。DHL 特快 2 天送达，周五前没问题。请通过平台下单付款，这样能享受买家保障。'),
    flag: t('Payment outside the platform was suggested. The draft keeps it on the platform (policy 4.2).', '对方提议在平台外付款。草稿已引导回平台内交易（规则 4.2）。'),
  };
  return { app, conversations, pins, cards, steps, inbox, desk, ops, market, copilot, title: t('Acme Support', 'Acme 客服'), subtitle: t('3 members · Nova (AI) and our team', '3 位成员 · Nova（AI）和客服团队'), cardsSub: t('Nova (AI) · orders and refunds', 'Nova（AI）· 订单和退款'), typing: 'u_jordan' };
}

function client(messages: any[], d: ReturnType<typeof data>, onSide?: (emit: (e: any) => void) => void, typing = false, pins = true) {
  const view = new View(messages);
  const listeners = new Map<string, Set<(e: any) => void>>();
  const noop = async () => ({});
  const c: any = {
    connection: new Observable<any>({ state: 'connected' }),
    conversations: new Observable<any[]>(d.conversations),
    messages: () => view,
    send: noop,
    edit: noop,
    recall: noop,
    retry: noop,
    discard: noop,
    feedback: noop,
    respond: noop,
    read: noop,
    call: async (op: string) =>
      op === 'inbox' ? { items: d.inbox, more: false } : op === 'list_pins' ? (pins ? { pins: d.pins, count: d.pins.length } : { pins: [], count: 0 }) : op === 'card_answers' ? { answers: [] } : {},
    // Typing and steps are sent once something listens (a view mounts
    // when its tab opens, after this client was made).
    on: (t: string, f: (e: any) => void) => {
      let s = listeners.get(t);
      if (!s) listeners.set(t, (s = new Set()));
      s.add(f);
      if (t === 'signal' && typing) setTimeout(() => f({ conv_id: 'c_support', kind: 'typing', from: d.typing }), 60);
      if (t === 'side' && onSide && !sent) {
        sent = true;
        setTimeout(() => onSide((e) => listeners.get('side')?.forEach((g) => g(e))), 60);
      }
      return () => s!.delete(f);
    },
  };
  let sent = false;
  return c;
}

const tabs = {
  en: [
    { id: 'app', label: 'Messaging app', url: 'app.example.com/messages', note: 'Conversations, threads, reactions, pins, members, typing, AI replies streaming in' },
    { id: 'agent', label: 'AI support agent', url: 'desk.acme.com/conversations/1002', note: 'The customer’s chat on the left; on the right, what the agent did in your systems: every API call, the rule that held the refund, the guardrails' },
    { id: 'inbox', label: 'Approvals desk', url: 'desk.acme.com/approvals', note: 'Your team decides what agents may not do alone, with the customer, the risk, the policy and the audit trail in view' },
    { id: 'market', label: 'Marketplace copilot', url: 'market.example.com/inbox', note: 'A buyer and a seller; only the seller sees the assistant: the buyer’s intent, facts from your systems, a drafted reply and a policy warning' },
  ],
  zh: [
    { id: 'app', label: '即时通讯应用', url: 'app.example.com/messages', note: '会话列表、话题、表情回应、置顶、成员、输入状态、AI 流式回复' },
    { id: 'agent', label: 'AI 售后助手', url: 'desk.acme.com/conversations/1002', note: '左边是顾客的售后聊天；右边是 Agent 在你的系统里做了什么：每一次接口调用、拦下退款的规则、安全护栏' },
    { id: 'inbox', label: '审批中心', url: 'desk.acme.com/approvals', note: 'Agent 不能自己做主的事由团队决定，顾客、风险、规则和审计记录一目了然' },
    { id: 'market', label: '平台 AI 协助', url: 'market.example.com/inbox', note: '买家和卖家在聊天，只有卖家能看到 AI 助手：买家意图、你系统里的数据、拟好的回复和违规提醒' },
  ],
};

const L = (lang: Lang, en: string, zh: string) => (lang === 'zh' ? zh : en);

function DeskPanel({ d, lang }: { d: ReturnType<typeof data>; lang: Lang }) {
  const x = d.desk;
  return (
    <aside className="biz">
      <div className="biz-title"><span className="biz-dot" />{L(lang, 'Acme order desk', 'Acme 订单后台')}<span className="biz-tag">{L(lang, 'your systems', '你的系统')}</span></div>
      <section className="biz-card">
        <div className="biz-row"><b>{x.customer.name}</b><span className="biz-pill gold">{x.customer.tier}</span></div>
        <div className="biz-stats">{x.customer.stats.map(([k, v]) => <div key={k}><span>{k}</span><b>{v}</b></div>)}</div>
      </section>
      <section className="biz-card">
        <h4>{L(lang, 'Order', '订单')}</h4>
        <dl className="biz-dl">{x.order.map(([k, v]) => <div key={k}><dt>{k}</dt><dd>{v}</dd></div>)}</dl>
      </section>
      <section className="biz-card">
        <h4>{L(lang, 'Agent activity', 'Agent 操作记录')}</h4>
        <ol className="biz-calls">
          {x.calls.map((c) => (
            <li key={c.at + c.path} className={c.tone}>
              <div className="biz-call"><code><b>{c.m}</b> {c.path}</code><span className={'biz-res ' + c.tone}>{c.res}</span></div>
              <div className="biz-sub">{c.at} · {c.sys} · {c.note}</div>
            </li>
          ))}
        </ol>
      </section>
      <section className="biz-card">
        <h4>{L(lang, 'Guardrails', '安全护栏')}</h4>
        <ul className="biz-ok">{x.guards.map((g) => <li key={g}>{g}</li>)}</ul>
        <div className="biz-sub mono">trace {x.trace} · {L(lang, 'every step recorded', '每一步都有记录')}</div>
      </section>
    </aside>
  );
}

function OpsPanel({ d, lang }: { d: ReturnType<typeof data>; lang: Lang }) {
  const x = d.ops;
  return (
    <aside className="biz">
      <section className="biz-card">
        <h4>{L(lang, 'Risk', '风险评估')}</h4>
        <div className="biz-row"><b className="ok-text">{x.risk.score}</b></div>
        <ul className="biz-ok">{x.risk.why.map((w) => <li key={w}>{w}</li>)}</ul>
      </section>
      <section className="biz-card">
        <h4>{L(lang, 'Policy', '规则')}</h4>
        <p className="biz-p">{x.policy}</p>
      </section>
      <section className="biz-card">
        <h4>{L(lang, 'Conversation', '会话摘录')}</h4>
        <ul className="biz-quote">{x.excerpt.map(([who, text], i) => <li key={i}><b>{who}</b> {text}</li>)}</ul>
      </section>
      <section className="biz-card">
        <h4>{L(lang, 'Audit trail', '审计记录')}</h4>
        <ol className="biz-audit">{x.audit.map(([at, text], i) => <li key={i}><span className="mono">{at}</span>{text}</li>)}</ol>
      </section>
    </aside>
  );
}

function CopilotPanel({ d, lang }: { d: ReturnType<typeof data>; lang: Lang }) {
  const x = d.copilot;
  return (
    <aside className="biz">
      <div className="biz-title"><span className="biz-dot ai" />{L(lang, 'AI assistant', 'AI 助手')}<span className="biz-tag">{L(lang, 'only you see this', '仅你可见')}</span></div>
      <section className="biz-card">
        <h4>{L(lang, 'What the buyer wants', '买家想要什么')}</h4>
        <p className="biz-p">{x.intent}</p>
      </section>
      <section className="biz-card">
        <h4>{L(lang, 'From your systems', '来自你的系统')}</h4>
        <dl className="biz-dl">{x.facts.map(([k, v]) => <div key={k}><dt>{k}</dt><dd>{v}</dd></div>)}</dl>
      </section>
      <section className="biz-card warn">
        <h4>{L(lang, 'Policy', '平台规则')}</h4>
        <p className="biz-p">{x.flag}</p>
      </section>
      <section className="biz-card draft">
        <h4>{L(lang, 'Suggested reply', '建议回复')} <span className="biz-tag">AI</span></h4>
        <p className="biz-p">{x.draft}</p>
        <div className="biz-actions"><button type="button" className="primary">{L(lang, 'Send', '发送')}</button><button type="button">{L(lang, 'Edit', '编辑')}</button></div>
      </section>
    </aside>
  );
}

export default function ProductShowcase({ lang = 'en' }: { lang?: Lang }) {
  const [tab, setTab] = useState('app');
  const d = useMemo(() => data(lang), [lang]);
  const strings = lang === 'zh' ? zhStrings : undefined;
  const locale = lang === 'zh' ? 'zh-CN' : 'en-US';
  const tz = 'America/Los_Angeles';
  const appClient = useMemo(() => client(d.app, d, undefined, true), [d]);
  const agentClient = useMemo(
    () =>
      client(d.cards, d, (emit) => {
        const t0 = at('2026-10-26T17:20:01Z');
        d.steps.r1.forEach((s, i) => emit({ event: { type: 'progress', conv_id: 'c_support', created_at: t0 + i * 4000, data: { kind: 'step', text: s.title, msg_id: 'r1', data: s } } }));
        const t1 = at('2026-10-26T17:22:31Z');
        emit({ event: { type: 'progress', conv_id: 'c_support', created_at: t1, data: { kind: 'plan', text: 'plan', msg_id: 'r4', data: { steps: d.steps.r4.map(({ id, title }) => ({ id, title })) } } } });
        d.steps.r4.filter((s) => s.state !== 'pending').forEach((s, i) => emit({ event: { type: 'progress', conv_id: 'c_support', created_at: t1 + i * 2000, data: { kind: 'step', text: s.title, msg_id: 'r4', data: s } } }));
      }),
    [d],
  );
  const inboxClient = useMemo(() => client([], d), [d]);
  const marketClient = useMemo(() => client(d.market, d, undefined, false, false), [d]);
  useEffect(() => {
    if (tab !== 'inbox') return;
    let n = 0;
    const pick = () => {
      const b = document.querySelector<HTMLButtonElement>('.showcase .aim-inbox-row');
      if (b) b.click();
      else if (n++ < 40) setTimeout(pick, 30);
    };
    setTimeout(pick, 60);
  }, [tab]);
  const list = tabs[lang];
  const current = list.find((x) => x.id === tab)!;
  const common = { me: ME, locale, timeZone: tz, now: () => NOW, strings } as any;
  return (
    <div className="showcase">
      <div className="showcase-tabs" role="tablist" aria-label={lang === 'zh' ? '产品界面' : 'Product views'}>
        {list.map((x) => (
          <button key={x.id} role="tab" aria-selected={tab === x.id} className={tab === x.id ? 'on' : ''} onClick={() => setTab(x.id)}>
            {x.label}
          </button>
        ))}
      </div>
      <div className="showcase-window">
        <div className="showcase-bar" aria-hidden="true">
          <span className="lights"><i /><i /><i /></span>
          <span className="showcase-url">{current.url}</span>
        </div>
        <div className="showcase-body" role="tabpanel" aria-label={current.label}>
          {tab === 'app' && (
            <AgentIM client={appClient} {...common}>
              <div className="sc-app">
                <aside className="sc-side">
                  <div className="sc-head">{lang === 'zh' ? '消息' : 'Messages'}</div>
                  <ConversationList selected="c_support" onSelect={() => {}} />
                </aside>
                <div className="sc-main">
                  <Chat convId="c_support" title={d.title} subtitle={d.subtitle} />
                </div>
                <aside className="sc-info">
                  <div className="sc-head">{lang === 'zh' ? '成员' : 'Members'}</div>
                  <MemberList convId="c_support" />
                </aside>
              </div>
            </AgentIM>
          )}
          {tab === 'agent' && (
            <AgentIM client={agentClient} {...common}>
              <div className="sc-biz">
                <div className="sc-main">
                  <Chat convId="c_support" title={d.title} subtitle={d.cardsSub} />
                </div>
                <DeskPanel d={d} lang={lang} />
              </div>
            </AgentIM>
          )}
          {tab === 'inbox' && (
            <AgentIM client={inboxClient} {...common}>
              <div className="sc-ops">
                <div className="sc-kpis">{d.ops.kpis.map(([k, v]) => <div key={k}><span>{k}</span><b>{v}</b></div>)}</div>
                <div className="sc-biz">
                  <div className="sc-inbox">
                    <Inbox onOpen={() => {}} />
                  </div>
                  <OpsPanel d={d} lang={lang} />
                </div>
              </div>
            </AgentIM>
          )}
          {tab === 'market' && (
            <AgentIM client={marketClient} {...common}>
              <div className="sc-biz">
                <div className="sc-main">
                  <Chat convId="c_support" title="Sigma 35mm f/1.4 Art · €640" subtitle={L(lang, 'Leo (buyer) and Ana (seller)', 'Leo（买家）和 Ana（卖家）')} />
                </div>
                <CopilotPanel d={d} lang={lang} />
              </div>
            </AgentIM>
          )}
        </div>
      </div>
      <p className="showcase-note">{current.note}</p>
    </div>
  );
}
