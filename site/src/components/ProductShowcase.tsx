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
  return { app, conversations, pins, cards, steps, inbox, title: t('Acme Support', 'Acme 客服'), subtitle: t('3 members · Nova (AI) and our team', '3 位成员 · Nova（AI）和客服团队'), cardsSub: t('Nova (AI) · orders and refunds', 'Nova（AI）· 订单和退款'), typing: 'u_jordan' };
}

function client(messages: any[], d: ReturnType<typeof data>, onSide?: (emit: (e: any) => void) => void, typing = false) {
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
      op === 'inbox' ? { items: d.inbox, more: false } : op === 'list_pins' ? { pins: d.pins, count: d.pins.length } : op === 'card_answers' ? { answers: [] } : {},
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
    { id: 'app', label: 'Messaging app', note: 'Conversations, threads, reactions, pins, members, typing, AI replies streaming in' },
    { id: 'agent', label: 'Agent at work', note: 'Cards, tables and steps from an agent; a refund waiting for a person' },
    { id: 'inbox', label: 'Approvals inbox', note: 'Every action an agent may not take alone, in one place for your team' },
  ],
  zh: [
    { id: 'app', label: '即时通讯应用', note: '会话列表、话题、表情回应、置顶、成员、输入状态、AI 流式回复' },
    { id: 'agent', label: 'Agent 办事', note: 'Agent 发出的卡片、表格和步骤；一笔退款正在等人审批' },
    { id: 'inbox', label: '审批待办', note: 'Agent 不能自己做主的事，集中到团队的待办里' },
  ],
};

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
          <span className="showcase-url">{tab === 'inbox' ? 'app.example.com/inbox' : 'app.example.com/messages'}</span>
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
              <div className="sc-single">
                <Chat convId="c_support" title={d.title} subtitle={d.cardsSub} />
              </div>
            </AgentIM>
          )}
          {tab === 'inbox' && (
            <AgentIM client={inboxClient} {...common}>
              <div className="sc-inbox">
                <Inbox onOpen={() => {}} />
              </div>
            </AgentIM>
          )}
        </div>
      </div>
      <p className="showcase-note">{current.note}</p>
    </div>
  );
}
