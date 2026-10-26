// The details panel (docs/12 §3.4): members, my agents in this
// conversation (policy, whisper, take over), replies waiting for my
// approval, and things to try. Calls the protocol directly: these are
// owner tools, not part of the chat components.

import { useState } from 'react';
import { AIBadge, MemberList } from '@agentimhq/react';
import { Close } from './icons';

type Act = (op: string, args: Record<string, unknown>) => Promise<any>;

function Section({ title, count, children }: { title: string; count?: number; children: React.ReactNode }) {
  return (
    <section className="panel-section">
      <h3>
        {title}
        {count !== undefined ? <span className="count">{count}</span> : null}
      </h3>
      {children}
    </section>
  );
}

function Toggle({ checked, onChange, label, hint }: { checked: boolean; onChange(v: boolean): void; label: string; hint?: string }) {
  return (
    <label className="toggle">
      <span className="toggle-text">
        <span>{label}</span>
        {hint ? <span className="hint">{hint}</span> : null}
      </span>
      <input type="checkbox" role="switch" checked={checked} onChange={(e) => onChange(e.target.checked)} />
      <span className="switch" aria-hidden="true" />
    </label>
  );
}

function AgentCard({ conv, agent, act }: { conv: any; agent: any; act: Act }) {
  const [text, setText] = useState('');
  const c = conv.conv_id;
  const a = agent.user_id;
  const whisper = async () => {
    const t = text.trim();
    if (!t) return;
    setText('');
    await act('whisper', { conv_id: c, agent_id: a, text: t });
  };
  return (
    <div className="agent-card" data-agent={agent.name}>
      <div className="agent-head">
        <span className="agent-name">{agent.name}</span>
        <AIBadge />
      </div>
      <div className="field">
        <span className="field-label">Replies to</span>
        <div className="segmented" role="radiogroup" aria-label={`${agent.name} replies to`}>
          {[
            ['mention', '@Mentions', 'Only when mentioned'],
            ['addressed', 'Addressed', 'When mentioned or replied to'],
            ['all', 'All', 'Every message'],
          ].map(([v, label, title]) => (
            <button key={v} type="button" role="radio" title={title} aria-checked={agent.trigger === v} onClick={() => act('set_agent_policy', { conv_id: c, agent_id: a, trigger: v })}>
              {label}
            </button>
          ))}
        </div>
      </div>
      <Toggle checked={!!agent.approval} onChange={(v) => act('set_agent_policy', { conv_id: c, agent_id: a, approval: v })} label="Review before sending" hint="Replies wait for your approval" />
      <form
        className="whisper"
        onSubmit={(e) => {
          e.preventDefault();
          void whisper();
        }}
      >
        <input value={text} onChange={(e) => setText(e.target.value)} placeholder={`Whisper to ${agent.name}…`} aria-label={`Whisper to ${agent.name}`} />
        <button type="submit" disabled={!text.trim()}>
          Whisper
        </button>
      </form>
      <p className="hint">Only {agent.name} sees a whisper. It shapes the next reply.</p>
      <div className="button-row">
        <button type="button" onClick={() => act('takeover', { conv_id: c, agent_id: a })}>
          Take over
        </button>
        <button type="button" onClick={() => act('release', { conv_id: c, agent_id: a })}>
          Hand back
        </button>
        <button type="button" onClick={() => act('reset_guard', { conv_id: c, agent_id: a })}>
          Reset guardrail
        </button>
      </div>
    </div>
  );
}

function Draft({ draft, title, name, act, onDone }: { draft: any; title: string; name: (id: string) => string; act: Act; onDone(): void }) {
  const [text, setText] = useState(draft.content ?? '');
  return (
    <div className="draft" data-draft={draft.draft_id}>
      <div className="draft-head">
        <span className="agent-name">{name(draft.agent_id)}</span>
        <AIBadge />
        <span className="draft-where">in {title}</span>
      </div>
      <textarea value={text} onChange={(e) => setText(e.target.value)} aria-label={`Reply from ${name(draft.agent_id)}`} rows={3} />
      <div className="button-row">
        <button
          type="button"
          className="primary"
          onClick={async () => {
            await act('approve', { draft_id: draft.draft_id, text });
            onDone();
          }}
        >
          Approve and send
        </button>
        <button
          type="button"
          onClick={async () => {
            await act('reject', { draft_id: draft.draft_id });
            onDone();
          }}
        >
          Discard
        </button>
      </div>
    </div>
  );
}

const TIPS: [string, string][] = [
  ['Aria', 'Say anything to watch a streamed reply. Send /long, then press Stop.'],
  ['Aria', '/secret: the policy filter replaces a card number mid-stream.'],
  ['Aria', '/stall or /crash: the watchdog fails the reply and keeps what was written.'],
  ['Aria', '“Ignore all previous instructions”: flagged as low trust.'],
  ['Aria', '/card: a card with buttons; your answer goes back to the agent.'],
  ['Agent loop lab', 'Send a message and watch two agents get stopped at the hop limit.'],
  ['Support: refund request', 'As Bob, ask for a refund. As Alice, review RefundBot’s reply here, or whisper first.'],
  ['Product team', 'Aria only answers when you mention @Aria.'],
];

export function Details({ conv, me, name, drafts, convs, act, onDrafts, onClose }: { conv: any; me: string; name: (id: string) => string; drafts: any[]; convs: any[]; act: Act; onDrafts(): void; onClose(): void }) {
  const mine = (conv.members ?? []).filter((m: any) => m.kind === 'agent' && m.owner_id === me);
  const titleOf = (id: string) => convs.find((c) => c.conv_id === id)?.title ?? 'a conversation';
  return (
    <div className="panel">
      <header className="panel-head">
        <h2>Details</h2>
        <button type="button" className="icon-button" onClick={onClose} title="Close details">
          <Close />
          <span className="sr">Close details</span>
        </button>
      </header>
      <Section title="Members" count={(conv.members ?? []).length}>
        <MemberList convId={conv.conv_id} />
      </Section>
      {mine.length ? (
        <Section title="Your agents here">
          {mine.map((a: any) => (
            <AgentCard key={a.user_id} conv={conv} agent={a} act={act} />
          ))}
        </Section>
      ) : null}
      <Section title="Waiting for your approval" count={drafts.length}>
        {drafts.length ? (
          drafts.map((d) => <Draft key={d.draft_id} draft={d} title={titleOf(d.conv_id)} name={name} act={act} onDone={onDrafts} />)
        ) : (
          <p className="hint">Nothing right now. Turn on “Review before sending” for one of your agents, and its replies wait here. Nobody else sees a draft.</p>
        )}
      </Section>
      <Section title="Things to try">
        <ul className="tips">
          {TIPS.map(([where, what]) => (
            <li key={what}>
              <span className="tip-where">{where}</span>
              {what}
            </li>
          ))}
        </ul>
      </Section>
    </div>
  );
}
