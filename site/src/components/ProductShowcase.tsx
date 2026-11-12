// The product on the homepage: our own React components (sdk/react), live,
// with a fake client and a fixed clock, in three views: the messaging app
// (list, chat with threads, reactions and a pinned message, members), an
// agent at work (cards, a form, steps waiting for approval) and the approval
// inbox. English or Chinese data and interface text.

import { useEffect, useMemo, useState } from 'react';
import { Observable } from '../../../sdk/client-web/src/view';
import { AgentIM, Chat, ConversationList, Inbox, MemberList, Thread } from '../../../sdk/react/src/index';
import '../../../sdk/react/theme.css';
import '../styles/showcase.css';

type Lang = 'en' | 'zh';
const NOW = Date.parse('2026-10-26T17:30:00Z');
const at = (iso: string) => Date.parse(iso);
const ME = 'u_maya';

class View extends Observable<any[]> {
  hasOlder = false;
  olderEnd = 'start';
  async loadOlder() {}
}

export const zhStrings: Record<string, any> = {
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
  thread: '话题',
  threadPlaceholder: '在话题中回复…',
  alsoInConversation: '同时发到会话',
  close: '关闭',
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
  aiTitle: 'AI 助手',
  sending: '发送中…',
  sent: '已发送',
  edit: '编辑',
  editing: '编辑消息',
  save: '保存',
  cancel: '取消',
  recalled: '这条消息已删除',
  cancelled: '已停止',
  streamFailed: '没能完成',
  edited: '已编辑',
  loading: '加载中…',
  retention: '更早的消息已不再保留',
  emptyHint: '打个招呼开始聊天吧。',
  noConversations: '还没有会话',
  noMatches: '没有匹配的结果',
  newMessages: '新消息',
  jumpToLatest: (n: number) => (n ? `${n} 条新消息` : '回到最新'),
  pin: '置顶',
  unpin: '取消置顶',
  helpful: '有帮助',
  notHelpful: '没帮助',
  attach: '添加文件',
  image: '图片',
  viewOriginal: '查看原图',
  download: '下载',
  uploading: (pct: number) => `上传中… ${pct}%`,
  fileUnavailable: '文件已不可用',
  dropFiles: '松开即可发送文件',
  fileTypes: { pdf: 'PDF', doc: '文档', sheet: '表格', slides: '演示文稿', archive: '压缩包', audio: '音频', video: '视频', code: '代码', image: '图片', other: '文件' } as Record<string, string>,
  connection: { closed: '离线', connecting: '连接中…', connected: '', disconnected: '重新连接中…', suspended: '等待网络…', failed: '无法连接' } as Record<string, string>,
  messages: '消息',
  conversations: '会话',
  members: '成员',
  roles: { owner: '群主', admin: '管理员', member: '' } as Record<string, string>,
  handedOff: '正在为你转接人工',
  handedBack: 'AI 助手回来了',
  approvalTitle: '需要审批',
  approvalFor: 'AI 助手想要',
  approved: '已批准',
  declined: '已拒绝',
  approvalExpired: '已过期',
  approvalCancelled: '已取消',
  approvalAmount: '金额',
  cardWaiting: '等待回答',
  cardAnswered: '已回答',
  answeredBy: (name: string) => `${name} 已提交`,
  fieldRequired: '必填',
  fieldInvalid: '请检查这个值',
  emptyValue: '—',
  inboxEmpty: '都处理完了',
  inboxEmptyHint: '需要你处理的审批、草稿和转人工会显示在这里。',
  inboxDeclineReason: '原因（可选）',
  inboxHandBack: '交还给 AI',
  inboxSend: '发送',
  inboxSendEdited: '发送修改后的内容',
  inboxDiscard: '丢弃',
  inboxAnswer: '回答',
  inboxBack: '返回',
  inboxDecidedBy: (name: string, state: string) => `${name}${state}`,
  inboxYou: '你',
  ended: (reason: string, by?: string) => (reason === 'removed' ? (by ? `${by} 把你移出了会话` : '你已被移出这个会话') : reason === 'dissolved' ? (by ? `${by} 关闭了这个会话` : '这个会话已关闭') : '你已离开这个会话'),
  endedComposer: (reason: string) => (reason === 'dissolved' ? '这个会话已关闭，你仍可以查看历史消息。' : '你已不是成员，无法发送消息。'),
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
      msg_id: 't1',
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
  const priya: [string, string, string] = ['u_priya', t('Priya Shah', '普丽雅'), 'human'];
  const bot: [string, string, string] = ['u_bot', t('Deploy bot', '部署机器人'), 'agent'];
  const c = (conv: string) => (sender: [string, string, string], content: string, iso: string, extra: any = {}) => m(sender, content, iso, { conv_id: conv, ...extra });
  const dz = c('c_design');
  const design = [
    dz(priya, t('New checkout mocks are in Figma 🎨 Two options for the order page.', '新的结算页设计稿已放到 Figma 🎨 订单页有两个方案。'), '2026-10-26T14:40:00Z', { reactions: [{ key: '🔥', count: 3, me: true, users: [ME, 'u_jordan', 'u_nova'] }] }),
    dz(priya, t('A keeps the summary on the right. B puts it on top, which is easier on phones.', 'A 方案把摘要放在右边；B 方案放在顶部，手机上更好看。'), '2026-10-26T14:41:00Z'),
    dz(jordan, t('B reads better on small screens.', 'B 在小屏上更清楚。'), '2026-10-26T14:55:00Z', { reactions: [{ key: '👍', count: 2, me: false, users: ['u_priya', 'u_nova'] }] }),
    dz(nova, t('Here’s the vote so far', '目前的投票结果'), '2026-10-26T14:58:00Z', {
      msg_id: 'd_vote', kind: 'agentim.card',
      payload: { title: t('Order page: A or B?', '订单页：A 还是 B？'), blocks: [{ type: 'facts', items: [{ label: t('Option A', '方案 A'), value: t('1 vote', '1 票') }, { label: t('Option B', '方案 B'), value: t('4 votes', '4 票'), tone: 'success' }] }, { type: 'buttons', id: 'v', options: [{ id: 'a', label: t('Vote A', '投 A') }, { id: 'b', label: t('Vote B', '投 B'), style: 'primary' }] }] },
    }),
    dz(maya, t('Let’s go with B. Final mocks by Friday?', '就选 B。周五前出终稿？'), '2026-10-26T15:00:00Z', { msg_id: 'd_due' }),
    dz(priya, t('On it 👌', '没问题 👌'), '2026-10-26T15:02:00Z', { reactions: [{ key: '🙏', count: 1, me: true, users: [ME] }] }),
  ];
  const ln = c('c_launch');
  const launch = [
    ln(jordan, t('Release 2.5 checklist 👇', '2.5 版本上线清单 👇'), '2026-10-25T17:30:00Z'),
    ln(nova, t('Release 2.5', '2.5 版本'), '2026-10-25T17:31:00Z', {
      msg_id: 'l_list', kind: 'agentim.card',
      payload: { title: t('Release 2.5', '2.5 版本'), blocks: [{ type: 'table', columns: [{ label: t('Task', '任务') }, { label: t('Owner', '负责人') }, { label: t('Status', '状态') }], rows: [[t('Release notes', '发布说明'), 'Jordan', t('Done', '完成')], [t('Migration test', '迁移测试'), 'Nova', t('Done', '完成')], [t('App Store review', 'App Store 审核'), 'Maya', t('Waiting', '等待中')]] }] },
    }),
    ln(maya, t('Ship it Thursday?', '周四上线？'), '2026-10-25T19:00:00Z'),
    ln(jordan, t('Thursday works. I’ll post the notes.', '周四可以，我来发上线说明。'), '2026-10-25T19:04:00Z', { reactions: [{ key: '🚀', count: 3, me: true, users: [ME, 'u_nova', 'u_priya'] }] }),
  ];
  const op = c('c_ops');
  const ops_ = [
    op(bot, t('Deploy 2.4.1', '部署 2.4.1'), '2026-10-25T12:52:00Z', {
      msg_id: 'o_deploy', kind: 'agentim.card',
      payload: { title: t('Deploy 2.4.1', '部署 2.4.1'), blocks: [{ type: 'facts', items: [{ label: t('Nodes', '节点'), value: '3 / 3', tone: 'success' }, { label: t('Duration', '耗时'), value: '4m 12s' }, { label: t('Messages lost', '丢失消息'), value: '0', tone: 'success' }] }] },
    }),
    op(bot, t('Deploy 2.4.1 finished on 3 nodes', '2.4.1 已在 3 个节点部署完成'), '2026-10-25T13:00:00Z'),
    op(nova, t('p95 delivery latency is back to 38 ms after the rolling restart.', '滚动重启后，P95 投递延迟回到 38 毫秒。'), '2026-10-25T13:02:00Z', { reactions: [{ key: '🙌', count: 2, me: false, users: ['u_jordan', 'u_priya'] }] }),
  ];
  const nv = c('c_nova');
  const novaDm = [
    nv(maya, t('Can you summarize this week?', '帮我总结一下这周？'), '2026-10-22T12:59:00Z'),
    nv(nova, t('Your week', '你的一周'), '2026-10-22T13:00:00Z', {
      msg_id: 'n_week', kind: 'agentim.card',
      payload: { title: t('Support this week', '本周客服'), blocks: [{ type: 'facts', items: [{ label: t('Conversations', '会话'), value: '1,284' }, { label: t('Answered by Nova', 'Nova 解决'), value: '82%', tone: 'success' }, { label: t('Handed to the team', '转人工'), value: '41' }, { label: t('Satisfaction', '满意度'), value: '4.7 / 5' }] }] },
    }),
    nv(nova, t('Here’s your weekly summary.', '这是你的本周摘要。'), '2026-10-22T13:00:10Z', { envelope: { sources: [{ title: t('Support analytics', '客服数据'), url: '#' }] } }),
  ];
  const team = [
    { user_id: ME, name: maya[1], kind: 'human', role: 'owner' },
    { user_id: 'u_priya', name: priya[1], kind: 'human', role: 'member' },
    { user_id: 'u_jordan', name: jordan[1], kind: 'human', role: 'admin' },
    { user_id: 'u_nova', name: 'Nova', kind: 'agent', role: 'member' },
  ];
  const conversations = [
    { conv_id: 'c_support', title: t('Acme Support', 'Acme 客服'), unread: 0, last_message: app[app.length - 1], members },
    { conv_id: 'c_design', title: t('Design team', '设计组'), unread: 3, last_message: design[design.length - 1], members: team },
    { conv_id: 'c_launch', title: t('Launch checklist', '上线清单'), unread: 0, last_message: launch[launch.length - 1], members: team },
    { conv_id: 'c_ops', title: t('Ops agents', '运维 Agent 群'), unread: 12, last_message: ops_[ops_.length - 1], members: [team[0], team[2], { user_id: 'u_bot', name: bot[1], kind: 'agent', role: 'member' }, team[3]] },
    { conv_id: 'c_nova', title: 'Nova', unread: 0, last_message: novaDm[novaDm.length - 1], members: [team[0], team[3]] },
  ];
  const convs: Record<string, { messages: any[]; title: string; subtitle: string; pins: any[] }> = {
    c_support: { messages: app, title: t('Acme Support', 'Acme 客服'), subtitle: t('3 members · Nova (AI) and our team', '3 位成员 · Nova（AI）和客服团队'), pins: [{ msg_id: 'm2', msg: { content: t('Order #4821 ships with UPS, arriving Thursday, Oct 29', '订单 #4821 由 UPS 发货，10 月 29 日（周四）送达') } }] },
    c_design: { messages: design, title: t('Design team', '设计组'), subtitle: t('4 members · Figma reviews', '4 位成员 · 设计评审'), pins: [{ msg_id: 'd_due', msg: { content: t('Go with B. Final mocks by Friday', '选 B 方案，周五前出终稿') } }] },
    c_launch: { messages: launch, title: t('Launch checklist', '上线清单'), subtitle: t('4 members · release 2.5', '4 位成员 · 2.5 版本'), pins: [{ msg_id: 'l_list', msg: { content: t('Release 2.5 checklist', '2.5 版本上线清单') } }] },
    c_ops: { messages: ops_, title: t('Ops agents', '运维 Agent 群'), subtitle: t('2 people, 2 agents', '2 人，2 个 Agent'), pins: [] },
    c_nova: { messages: novaDm, title: 'Nova', subtitle: t('AI assistant', 'AI 助手'), pins: [] },
  };
  const threadRoot = app.find((x) => x.msg_id === 't1');
  const threadReplies = [
    m(nova, t('The package is still at the UPS depot, so the address can change once.', '包裹还在 UPS 仓库，可以修改一次地址。'), '2026-10-26T17:21:00Z', { thread_id: 't1' }),
    m(jordan, t('Changed it. UPS confirmed the new address for Thursday.', '已改好，UPS 确认周四按新地址送达。'), '2026-10-26T17:24:30Z', { thread_id: 't1' }),
    m(maya, t('Perfect, thank you both! 🙏', '太好了，谢谢你们！🙏'), '2026-10-26T17:25:10Z', { thread_id: 't1' }),
  ];
  const agentPins = [{ msg_id: 'r1', msg: { content: t('Order 1002 · Bluetooth speaker, $120, delayed at UPS', '订单 1002 · 蓝牙音箱，120 美元，UPS 延误') } }];

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
  return { app, conversations, convs, threadRoot, threadReplies, agentPins, cards, steps, inbox, desk, ops, market, copilot, title: t('Acme Support', 'Acme 客服'), subtitle: t('3 members · Nova (AI) and our team', '3 位成员 · Nova（AI）和客服团队'), cardsSub: t('Nova (AI) · orders and refunds', 'Nova（AI）· 订单和退款'), typing: 'u_jordan' };
}

function client(byConv: Record<string, any[]>, d: ReturnType<typeof data>, onSide?: (emit: (e: any) => void) => void, typing = false, pins: Record<string, any[]> = {}) {
  const views = new Map(Object.entries(byConv).map(([k, v]) => [k, new View(v)]));
  const empty = new View([]);
  const listeners = new Map<string, Set<(e: any) => void>>();
  const noop = async () => ({});
  const conversations = new Observable<any[]>(d.conversations);
  const c: any = {
    connection: new Observable<any>({ state: 'connected' }),
    conversations,
    messages: (conv: string) => views.get(conv) ?? empty,
    send: noop,
    edit: noop,
    recall: noop,
    retry: noop,
    discard: noop,
    feedback: noop,
    respond: noop,
    // Opening a conversation reads it: its unread count goes away.
    read: async (conv: string) => {
      const list = conversations.get();
      if (list.some((x) => x.conv_id === conv && x.unread)) conversations.set(list.map((x) => (x.conv_id === conv ? { ...x, unread: 0 } : x)));
      return {};
    },
    call: async (op: string, args: any = {}) => {
      if (op === 'inbox') return { items: d.inbox, more: false };
      if (op === 'pull_thread') return { root: d.threadRoot, replies: d.threadReplies };
      if (op === 'list_pins') {
        const p = pins[args.conv_id] ?? [];
        return { pins: p, count: p.length };
      }
      if (op === 'card_answers') return { answers: [] };
      return {};
    },
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
    { id: 'app', label: 'Messaging app', url: 'app.example.com/messages', note: 'Conversations, a thread open beside the chat, reactions and likes, a pinned message, typing, AI replies streaming in' },
    { id: 'agent', label: 'AI support agent', url: 'desk.acme.com/conversations/1002', note: 'The customer’s chat on the left; on the right, what the agent did in your systems: every API call, the rule that held the refund, the guardrails' },
    { id: 'inbox', label: 'Approvals desk', url: 'desk.acme.com/approvals', note: 'Your team decides what agents may not do alone, with the customer, the risk, the policy and the audit trail in view' },
    { id: 'market', label: 'Marketplace copilot', url: 'market.example.com/inbox', note: 'A buyer and a seller; only the seller sees the assistant: the buyer’s intent, facts from your systems, a drafted reply and a policy warning' },
  ],
  zh: [
    { id: 'app', label: '即时通讯应用', url: 'app.example.com/messages', note: '会话列表、在旁边打开的话题、表情回应和点赞、置顶消息、输入状态、AI 流式回复' },
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

/** The homepage's four product views; `only` shows one of them alone (a docs page's demo). */
export default function ProductShowcase({ lang = 'en', only }: { lang?: Lang; only?: 'app' | 'agent' | 'inbox' | 'market' }) {
  const [tab, setTab] = useState<string>(only ?? 'app');
  // The examples speak the page's language.
  const demo = lang;
  const [conv, setConv] = useState('c_support');
  const d = useMemo(() => data(demo), [demo]);
  const strings = demo === 'zh' ? zhStrings : undefined;
  const locale = demo === 'zh' ? 'zh-CN' : 'en-US';
  const tz = 'America/Los_Angeles';
  const appClient = useMemo(() => client(Object.fromEntries(Object.entries(d.convs).map(([k, v]) => [k, v.messages])), d, undefined, true, Object.fromEntries(Object.entries(d.convs).map(([k, v]) => [k, v.pins]))), [d]);
  const agentClient = useMemo(
    () =>
      client({ c_support: d.cards }, d, (emit) => {
        const t0 = at('2026-10-26T17:20:01Z');
        d.steps.r1.forEach((s, i) => emit({ event: { type: 'progress', conv_id: 'c_support', created_at: t0 + i * 4000, data: { kind: 'step', text: s.title, msg_id: 'r1', data: s } } }));
        const t1 = at('2026-10-26T17:22:31Z');
        emit({ event: { type: 'progress', conv_id: 'c_support', created_at: t1, data: { kind: 'plan', text: 'plan', msg_id: 'r4', data: { steps: d.steps.r4.map(({ id, title }) => ({ id, title })) } } } });
        d.steps.r4.filter((s) => s.state !== 'pending').forEach((s, i) => emit({ event: { type: 'progress', conv_id: 'c_support', created_at: t1 + i * 2000, data: { kind: 'step', text: s.title, msg_id: 'r4', data: s } } }));
      }, false, { c_support: d.agentPins }),
    [d],
  );
  const inboxClient = useMemo(() => client({}, d), [d]);
  const marketClient = useMemo(() => client({ c_support: d.market }, d), [d]);
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
    <div className={only ? 'showcase compact not-content' : 'showcase'}>
      {only ? null : (
      <div className="showcase-tabs" role="tablist" aria-label={lang === 'zh' ? '产品界面' : 'Product views'}>
        {list.map((x) => (
          <button key={x.id} role="tab" aria-selected={tab === x.id} className={tab === x.id ? 'on' : ''} onClick={() => setTab(x.id)}>
            {x.label}
          </button>
        ))}
      </div>
      )}
      <div className="showcase-window">
        <div className="showcase-bar">
          <span className="lights" aria-hidden="true"><i /><i /><i /></span>
          <span className="showcase-url">{current.url}</span>
        </div>
        <div className="showcase-body" role="tabpanel" aria-label={current.label}>
          {tab === 'app' && (
            <AgentIM client={appClient} {...common}>
              <div className="sc-app">
                <aside className="sc-side">
                  <div className="sc-head">{demo === 'zh' ? '消息' : 'Messages'}</div>
                  <ConversationList selected={conv} onSelect={setConv} />
                </aside>
                <div className="sc-main">
                  <Chat key={conv} convId={conv} title={d.convs[conv].title} subtitle={d.convs[conv].subtitle} />
                </div>
                {conv === 'c_support' ? (
                  <aside className="sc-info sc-thread">
                    <Thread convId="c_support" threadId="t1" />
                  </aside>
                ) : (
                  <aside className="sc-info">
                    <div className="sc-head">{demo === 'zh' ? '成员' : 'Members'}</div>
                    <MemberList convId={conv} />
                  </aside>
                )}
              </div>
            </AgentIM>
          )}
          {tab === 'agent' && (
            <AgentIM client={agentClient} {...common}>
              <div className="sc-biz">
                <div className="sc-main">
                  <Chat convId="c_support" title={d.title} subtitle={d.cardsSub} />
                </div>
                <DeskPanel d={d} lang={demo} />
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
                  <OpsPanel d={d} lang={demo} />
                </div>
              </div>
            </AgentIM>
          )}
          {tab === 'market' && (
            <AgentIM client={marketClient} {...common}>
              <div className="sc-biz">
                <div className="sc-main">
                  <Chat convId="c_support" title="Sigma 35mm f/1.4 Art · €640" subtitle={L(demo, 'Leo (buyer) and Ana (seller)', 'Leo（买家）和 Ana（卖家）')} />
                </div>
                <CopilotPanel d={d} lang={demo} />
              </div>
            </AgentIM>
          )}
        </div>
      </div>
      {only ? null : <p className="showcase-note">{current.note}</p>}
    </div>
  );
}
