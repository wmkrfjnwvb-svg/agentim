// The chat components agents send (docs/en/design/chat-components.md), live:
// a data card, a form, an approval and buttons with steps, each drawn by
// our React components from the same JSON an agent sends, in the page's
// language.

import { useMemo } from 'react';
import { Observable } from '../../../sdk/client-web/src/view';
import { AgentIM, Chat } from '../../../sdk/react/src/index';
import '../../../sdk/react/theme.css';
import { zhStrings } from './ProductShowcase';

type Lang = 'en' | 'zh';
const NOW = Date.parse('2026-10-26T17:30:00Z');
const ME = 'u_maya';

class View extends Observable<any[]> {
  hasOlder = false;
  olderEnd = 'start';
  async loadOlder() {}
}

function tiles(lang: Lang) {
  const t = (en: string, zh: string) => (lang === 'zh' ? zh : en);
  const msg = (id: string, content: string, extra: any = {}) => ({
    key: id, msg_id: id, conv_id: 'c', sender_id: 'u_nova', sender_name: 'Nova', sender_type: 'agent', content,
    created_at: NOW - 60_000, created_seq: 1, order: 1, status: 'sent', state: 'committed', version: 1, ...extra,
  });
  const card = (id: string, payload: any) => msg(id, payload.title, { kind: 'agentim.card', payload });
  return [
    {
      id: 'data',
      title: t('Data cards', '数据卡片'),
      text: t('Facts and tables with money, dates and statuses formatted for each reader.', '事实和表格，金额、日期、状态按每位读者的语言和地区显示。'),
      api: 'ctx.showCard("Order 1002", [{ type: "facts", … }, { type: "table", … }])',
      messages: [card('d1', {
        title: t('Order 1002', '订单 1002'),
        blocks: [
          { type: 'facts', items: [{ label: t('Item', '商品'), value: t('Bluetooth speaker', '蓝牙音箱') }, { label: t('Total', '金额'), value: 120, format: 'currency:USD' }, { label: t('Status', '状态'), value: t('Delayed', '延误'), tone: 'warning' }, { label: t('Ordered', '下单'), value: '2026-10-19', format: 'date' }] },
          { type: 'table', columns: [{ label: t('Order', '订单') }, { label: t('Total', '金额'), format: 'currency:USD' }, { label: t('Status', '状态') }], rows: [['1001', 25, t('Delivered', '已送达')], ['1002', 120, t('Delayed', '延误')]] },
        ],
      })],
    },
    {
      id: 'form',
      title: t('Form cards', '表单卡片'),
      text: t('Agents collect details in a form; the server checks every answer.', 'Agent 用表单收集信息，每个答案都由服务端校验。'),
      api: 'await ctx.askForm("Refund order 1002", [{ id: "reason", type: "select", … }])',
      messages: [card('f1', {
        title: t('Refund order 1002', '退款：订单 1002'),
        to: [ME],
        once: true,
        blocks: [{
          type: 'form', id: 'form', submit: t('Request refund', '申请退款'),
          fields: [
            { id: 'reason', label: t('Reason', '原因'), type: 'select', options: [t('Arrived late', '送达太晚'), t('Damaged', '有损坏'), t('Changed my mind', '不想要了')], required: true, placeholder: t('Choose a reason', '选择原因') },
            { id: 'pickup', label: t('Pickup date', '取件日期'), type: 'date', min: '2026-10-27' },
          ],
        }],
      })],
    },
    {
      id: 'approval',
      title: t('Approval cards', '审批卡片'),
      text: t('An action that changes something waits for a person, with the values and the rule that asked.', '会改变数据的操作先等人批准，卡片上写明数值和触发的规则。'),
      api: 'approvals: [{ tool: "createRefund", amount_field: "amount", over: 30 }]',
      messages: [msg('a1', t('This refund needs a manager’s approval.', '这笔退款需要主管审批。'))],
      inbox: [{
        type: 'request', request_id: 'q1', kind: 'approve', conv_id: 'c', agent_name: 'Nova', asker_name: t('Maya Chen', '陈玛雅'), created_at: NOW - 60_000, expires_at: NOW + 3_000_000, title: t('Refund an order', '订单退款'),
        context: { operation: { name: 'createRefund', description: t('Refund an order (POST /refunds)', '订单退款（POST /refunds）'), fields: [{ name: 'order', value: '1002' }, { name: 'amount', value: 120 }], amount: 120, rule: { tool: 'createRefund', amount_field: 'amount', over: 30 } } },
      }],
    },
    {
      id: 'steps',
      title: t('Buttons and steps', '按钮和步骤'),
      text: t('Quick choices with buttons, and every step of the agent’s work as it happens.', '用按钮快速选择，Agent 办事的每一步实时可见。'),
      api: 'const s = await ctx.steps(["Look up the order", …]); await s.wait(2)',
      messages: [card('s1', {
        title: t('Change delivery address?', '修改收货地址？'),
        blocks: [{ type: 'buttons', id: 'b', options: [{ id: 'yes', label: t('Change address', '修改地址'), style: 'primary' }, { id: 'no', label: t('Keep it', '不改了') }] }],
      })],
      steps: [
        { id: 'a', title: t('Look up the order', '查询订单'), state: 'done' },
        { id: 'b', title: t('Check with UPS', '向 UPS 确认'), state: 'done' },
        { id: 'c', title: t('Change the address', '修改地址'), state: 'waiting', detail: t('Waiting for your choice', '等待你的选择') },
      ],
    },
  ];
}

function client(tile: ReturnType<typeof tiles>[number]) {
  const view = new View(tile.messages);
  const listeners = new Map<string, Set<(e: any) => void>>();
  const noop = async () => ({});
  let sent = false;
  return {
    connection: new Observable<any>({ state: 'connected' }),
    conversations: new Observable<any[]>([{ conv_id: 'c', title: '', unread: 0, members: [] }]),
    messages: () => view,
    send: noop, edit: noop, recall: noop, retry: noop, discard: noop, feedback: noop, respond: noop, read: noop,
    call: async (op: string) => (op === 'inbox' ? { items: (tile as any).inbox ?? [], more: false } : op === 'card_answers' ? { answers: [] } : op === 'list_pins' ? { pins: [], count: 0 } : {}),
    on: (type: string, f: (e: any) => void) => {
      let set = listeners.get(type);
      if (!set) listeners.set(type, (set = new Set()));
      set.add(f);
      const steps = (tile as any).steps as any[] | undefined;
      if (type === 'side' && steps && !sent) {
        sent = true;
        setTimeout(() => {
          const emit = (e: any) => listeners.get('side')?.forEach((g) => g(e));
          emit({ event: { type: 'progress', conv_id: 'c', created_at: NOW - 50_000, data: { kind: 'plan', text: 'plan', msg_id: 's1', data: { steps: steps.map(({ id, title }) => ({ id, title })) } } } });
          steps.forEach((s, i) => emit({ event: { type: 'progress', conv_id: 'c', created_at: NOW - 50_000 + i * 3000, data: { kind: 'step', text: s.title, msg_id: 's1', data: s } } }));
        }, 60);
      }
      return () => set!.delete(f);
    },
  } as any;
}

export default function CardsGallery({ lang = 'en' }: { lang?: Lang }) {
  // The examples speak the page's language.
  const demo = lang;
  const list = useMemo(() => tiles(demo).map((x) => ({ ...x, client: client(x) })), [demo]);
  const common: any = { me: ME, locale: demo === 'zh' ? 'zh-CN' : 'en-US', timeZone: 'America/Los_Angeles', now: () => NOW, strings: demo === 'zh' ? zhStrings : undefined };
  return (
    <div className="cg">
      <div className="cg-grid">
        {list.map((x) => (
          <figure key={x.id + demo} className="cg-tile">
            <div className="cg-chat">
              <AgentIM client={x.client} {...common}>
                <Chat convId="c" />
              </AgentIM>
            </div>
            <figcaption>
              <h3>{x.title}</h3>
              <p>{x.text}</p>
              <code>{x.api}</code>
            </figcaption>
          </figure>
        ))}
      </div>
    </div>
  );
}
