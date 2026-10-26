// What happens in a conversation besides messages, as timeline notices
// (docs/12 §3.4): people joining and leaving, agent policy and state, the
// guardrail, cards answered; and what only some people see (whispers,
// "only you" messages, agent progress) from the side channel. Kept per
// conversation while the page is open.

import { useEffect, useMemo, useState, type ReactNode } from 'react';
import type { AgentIMClient } from '@agentimhq/client-web';
import type { Notice } from '@agentimhq/react/hooks';

type Names = (id: string | undefined) => string;

function eventText(ev: any, name: Names): { text: ReactNode; tone?: Notice['tone'] } | null {
  const d = ev.data ?? {};
  switch (ev.type) {
    case 'member.joined':
      return { text: d.kind === 'agent' ? `${d.name} (AI agent) joined · added by ${name(d.owner_id)}` : `${d.name} joined` };
    case 'members.joined': {
      const list: any[] = d.members ?? [];
      const shown = list.slice(0, 3).map((m) => m.name);
      const more = list.length > 3 ? ` and ${list.length - 3} more` : '';
      return { text: `${shown.join(', ')}${more} joined` };
    }
    case 'member.left':
      return { text: `${d.name || name(d.user_id)} left` };
    case 'member.removed':
      return { text: d.reason === 'destroyed' ? `${name(d.user_id)} was removed (agent deleted)` : `${name(d.user_id)} was removed` };
    case 'member.policy':
      return { text: `${name(d.user_id)} now replies to ${d.trigger === 'all' ? 'every message' : d.trigger === 'mention' ? 'mentions' : d.trigger === 'addressed' ? 'mentions and replies' : d.trigger}${d.approval ? ', with review before sending' : ''}` };
    case 'agent.state':
      if (d.state === 'taken_over' && d.handoff) return { text: `${name(d.from)} handed the conversation to ${name(d.by)}${d.note ? `: ${d.note}` : ''}` };
      if (d.handback) return { text: `${name(d.by)} handed the conversation back to ${name(d.agent_id)}${d.note ? `: ${d.note}` : ''}` };
      if (d.state === 'taken_over') return { text: `${name(d.by)} took over from ${name(d.agent_id)}` };
      if (d.state === 'tripped') return { text: `Guardrail stopped ${name(d.agent_id)} (${d.reason}: ${d.detail})`, tone: 'warning' };
      if (d.state === 'stopped') return { text: `${name(d.agent_id)} stopped (${d.reason}: ${d.detail}) until someone writes`, tone: 'warning' };
      return { text: `${name(d.agent_id)} is active again${d.reason ? ` (${d.reason})` : ''}` };
    case 'task.updated':
      return { text: `Task ${String(d.state).replace('_', ' ')}${d.assignee ? ` · assigned to ${name(d.assignee)}` : ''}${d.note ? `: ${d.note}` : ''}` };
    case 'card.response':
      return { text: `${name(d.by)} answered: ${d.action ?? JSON.stringify(d.values)}` };
    case 'msg.claimed':
      return { text: `${name(d.by)} picked up a request` };
    case 'msg.released':
      return { text: `${name(d.by)} let go of a request` };
    case 'floor.granted':
      return { text: `${name(d.by)} gave the floor to ${name(d.agent_id)}` };
    case 'floor.released':
      return { text: `${name(d.agent_id)} no longer has the floor` };
    case 'conv.updated':
      return d.title ? { text: `Renamed to “${d.title}”` } : null;
    default:
      return null;
  }
}

function sideNotice(ev: any, name: Names, me: string): Notice<ReactNode> | null {
  const d = ev.data ?? {};
  const at = (ev.anchor_seq ?? 0) + 0.5;
  const key = `s${ev.conv_id}:${ev.side_seq}`;
  if (ev.type === 'whisper')
    return {
      key,
      seq: at,
      tone: 'private',
      content: (
        <>
          <strong>{d.from === me ? `You whispered to ${name(d.to)}` : `${d.from_name} whispered to ${name(d.to)}`}</strong>
          <span className="whisper-note"> · Only you and {name(d.to)} can see this</span>
          <div className="whisper-text">{d.text}</div>
        </>
      ),
    };
  if (ev.type === 'ephemeral')
    return {
      key,
      seq: at,
      tone: 'private',
      content: (
        <>
          <strong>{d.from_name}</strong>
          <span className="whisper-note"> · Only visible to {d.to === me ? 'you' : name(d.to)}</span>
          <div className="whisper-text">{d.text}</div>
        </>
      ),
    };
  if (ev.type === 'progress') return { key, seq: at, content: `${name(d.agent_id)} is ${d.kind === 'thinking' ? 'thinking' : d.kind}: ${d.text}` };
  return null;
}

export interface Timeline {
  notices(convId: string): Notice<ReactNode>[];
  /** Side-channel notes for me that are not in the timeline (guardrail, drafts…). */
  revision: number;
}

/**
 * Collects notices for every conversation from the client's events; the
 * names come from the conversations' members.
 */
export function useTimeline(client: AgentIMClient, me: string, name: Names, onSide: (ev: any) => void): Timeline {
  const [byConv, setByConv] = useState<Record<string, Notice<ReactNode>[]>>({});
  const [revision, setRevision] = useState(0);
  useEffect(() => {
    const add = (conv: string, n: Notice<ReactNode>) =>
      setByConv((all) => {
        const list = all[conv] ?? [];
        if (list.some((x) => x.key === n.key)) return all;
        return { ...all, [conv]: [...list, n] };
      });
    const offEvent = client.on('event', (e) => {
      const ev = e.event ?? {};
      if (!ev.conv_id || String(ev.type).startsWith('msg.created') || ev.type === 'msg.updated') return;
      const t = eventText(ev, name);
      if (t) add(ev.conv_id, { key: `e${ev.seq}`, seq: ev.seq, content: t.text, tone: t.tone });
      setRevision((r) => r + 1);
    });
    const offSide = client.on('side', (e) => {
      const ev = e.event ?? {};
      const n = sideNotice(ev, name, me);
      if (n) add(ev.conv_id, n);
      onSide(ev);
      setRevision((r) => r + 1);
    });
    return () => {
      offEvent();
      offSide();
    };
  }, [client, me, name, onSide]);
  return useMemo(() => ({ notices: (c: string) => byConv[c] ?? [], revision }), [byConv, revision]);
}
