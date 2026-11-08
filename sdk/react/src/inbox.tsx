// The approval inbox (docs/en/design/chat-components.md): everything that waits for this person
// across conversations, in one place: operations an agent wants to run,
// agents' drafts, conversations handed over, questions. A list with tabs and
// search, and the selected item's details with its actions; on a narrow
// screen the list and the details are two pages. `useInbox` is the same
// without the drawing (merge it with your own to-dos); `InboxBadge` is the
// count for your navigation.

import { useEffect, useMemo, useState, type ReactNode } from 'react';
import { inboxId, inboxKind, useInbox, type Inbox, type InboxItem, type InboxOutcome } from './logic.js';
import type { Strings } from './strings.js';
import { formatAgo, formatTime, formatValue, type When } from './format.js';

/** The number of items waiting, for your navigation ("Inbox 3"). */
export function InboxCount({ strings, label = true }: { strings?: Partial<Strings>; label?: boolean }) {
  const { items } = useInbox();
  const n = items.length;
  return (
    <span className="aim-inbox-badge" data-empty={n === 0 || undefined}>
      {label ? <span>{strings?.inbox ?? 'Inbox'}</span> : null}
      {n ? <span className="aim-count" aria-label={String(n)}>{n > 99 ? '99+' : n}</span> : null}
    </span>
  );
}

/** What `renderItem` and a custom detail get. */
export interface InboxDetailProps {
  item: InboxItem;
  inbox: Inbox;
  strings: Strings;
  when: When;
  outcome?: InboxOutcome;
  onOpen?(convId: string): void;
}

const TABS = ['all', 'request', 'draft', 'handoff', 'question'] as const;

/** Icons for the kinds (line icons, 16px). */
function KindIcon({ kind }: { kind: string }) {
  const p = { width: 16, height: 16, viewBox: '0 0 24 24', fill: 'none', stroke: 'currentColor', strokeWidth: 1.8, strokeLinecap: 'round' as const, strokeLinejoin: 'round' as const, 'aria-hidden': true };
  if (kind === 'request')
    return (
      <svg {...p}>
        <path d="M12 3l7 3v6c0 4.5-3 7.5-7 9-4-1.5-7-4.5-7-9V6z" />
        <path d="M9 12l2 2 4-4" />
      </svg>
    );
  if (kind === 'draft')
    return (
      <svg {...p}>
        <path d="M4 20h4L19 9l-4-4L4 16z" />
        <path d="M14 6l4 4" />
      </svg>
    );
  if (kind === 'handoff')
    return (
      <svg {...p}>
        <circle cx="9" cy="8" r="3.5" />
        <path d="M3 20c.8-3.5 3.2-5.5 6-5.5s5.2 2 6 5.5" />
        <path d="M16 11h5m-2-2 2 2-2 2" />
      </svg>
    );
  return (
    <svg {...p}>
      <circle cx="12" cy="12" r="9" />
      <path d="M9.5 9.5a2.5 2.5 0 1 1 3.5 2.3c-.7.3-1 .9-1 1.7M12 17h.01" />
    </svg>
  );
}

/** A row's title and the line under it. */
function describe(i: InboxItem, strings: Strings, when: When): { title: string; sub: string; amount?: string } {
  const where = [i.conv_title, i.asker_name ? strings.inboxFor(i.asker_name) : null].filter(Boolean).join(' · ');
  if (i.type === 'request') {
    const op = i.context?.operation;
    if (op) {
      const amount = op.amount !== null && op.amount !== undefined ? formatValue(op.amount, typeof op.amount === 'number' ? 'number' : 'text', when) : undefined;
      return { title: op.description ? String(op.description).split(' (')[0] : op.name, sub: [op.name, where].filter(Boolean).join(' · '), amount };
    }
    return { title: String(i.title ?? '').split('\n')[0], sub: [i.agent_name, where].filter(Boolean).join(' · ') };
  }
  if (i.type === 'draft') return { title: String(i.content ?? '').slice(0, 140), sub: [strings.inboxDraftFrom(i.agent_name ?? ''), i.conv_title].filter(Boolean).join(' · ') };
  if (i.type === 'handoff') return { title: i.note || strings.inboxHandoffFrom(i.agent_name ?? ''), sub: [i.agent_name, i.conv_title].filter(Boolean).join(' · ') };
  return { title: String(i.title ?? i.type), sub: i.conv_title ?? '' };
}

function outcomeText(o: InboxOutcome, strings: Strings): string {
  const state =
    o.state === 'approved' ? strings.approved : o.state === 'rejected' ? strings.declined : o.state === 'expired' ? strings.approvalExpired : o.state === 'cancelled' ? strings.approvalCancelled : o.state === 'handed_back' ? strings.inboxHandBack : strings.cardAnswered;
  return o.by === 'me' ? strings.inboxDecidedBy(strings.inboxYou, state) : state;
}

/** The default details of an item: what it is and its actions. */
export function DefaultInboxDetail({ item: i, inbox, strings, when, outcome, onOpen }: InboxDetailProps) {
  const [busy, setBusy] = useState(false);
  const [reason, setReason] = useState('');
  const [declining, setDeclining] = useState(false);
  const [text, setText] = useState<string>(i.content ?? '');
  const [answer, setAnswer] = useState('');
  const [err, setErr] = useState<string>();
  useEffect(() => {
    setDeclining(false);
    setReason('');
    setText(i.content ?? '');
    setErr(undefined);
  }, [inboxId(i)]);
  const run = async (f: () => Promise<unknown>) => {
    setBusy(true);
    setErr(undefined);
    try {
      await f();
    } catch (e: any) {
      setErr(e?.message ?? String(e));
    } finally {
      setBusy(false);
    }
  };
  const d = describe(i, strings, when);
  const kind = inboxKind(i);
  const op = i.context?.operation;
  const asked = i.context?.tool_call?.msg;
  const open = onOpen ? (
    <button type="button" className="aim-btn" onClick={() => onOpen(i.conv_id)}>
      {strings.inboxOpen}
    </button>
  ) : null;
  let body: ReactNode;
  let actions: ReactNode = null;
  if (kind === 'request') {
    body = (
      <>
        {op ? (
          <dl className="aim-facts aim-inbox-fields">
            {(op.fields ?? []).map((f: any) => (
              <div key={f.name} className="aim-fact" data-key={op.rule?.amount_field === f.name || undefined}>
                <dt>{f.name}</dt>
                <dd dir="auto">{formatValue(f.value, typeof f.value === 'number' ? 'number' : 'text', when)}</dd>
              </div>
            ))}
          </dl>
        ) : (
          <p className="aim-inbox-text" dir="auto">
            {i.title}
          </p>
        )}
        {op?.rule ? (
          <p className="aim-inbox-note">
            <span className="aim-inbox-label">{strings.inboxRule}</span>
            {op.rule.amount_field && op.rule.over !== undefined ? ` ${op.rule.amount_field} > ${formatValue(op.rule.over, 'number', when)}` : ` ${op.name}`}
          </p>
        ) : null}
        {asked?.content ? (
          <blockquote className="aim-inbox-quote">
            <span className="aim-inbox-label">
              {strings.inboxTriggeredBy}
              {asked.sender_name ? ` · ${asked.sender_name}` : ''}
            </span>
            <span dir="auto">{asked.content}</span>
          </blockquote>
        ) : null}
      </>
    );
    actions = declining ? (
      <>
        <input className="aim-input" aria-label={strings.inboxDeclineReason} placeholder={strings.inboxDeclineReason} value={reason} onChange={(e) => setReason(e.target.value)} autoFocus />
        <button type="button" className="aim-btn" data-style="danger" disabled={busy} onClick={() => run(() => inbox.decide(i, 'reject', reason.trim() || undefined))}>
          {strings.decline}
        </button>
        <button type="button" className="aim-btn" disabled={busy} onClick={() => setDeclining(false)}>
          {strings.cancel}
        </button>
      </>
    ) : (
      <>
        <button type="button" className="aim-btn" data-style="primary" disabled={busy} onClick={() => run(() => inbox.decide(i, 'approve'))}>
          {strings.approve}
        </button>
        <button type="button" className="aim-btn" disabled={busy} onClick={() => setDeclining(true)}>
          {strings.decline}
        </button>
        {open}
      </>
    );
  } else if (kind === 'question') {
    // The title is the question; the body only adds what was asked with it.
    body = i.context?.text ? (
      <p className="aim-inbox-text" dir="auto">
        {String(i.context.text)}
      </p>
    ) : null;
    actions =
      i.kind === 'choose' ? (
        <>
          {(i.options ?? []).map((o: string) => (
            <button key={o} type="button" className="aim-btn" disabled={busy} onClick={() => run(() => inbox.answer(i, { choice: o }))}>
              {o}
            </button>
          ))}
          {open}
        </>
      ) : (
        <>
          <textarea className="aim-input" rows={3} aria-label={strings.inboxAnswer} value={answer} onChange={(e) => setAnswer(e.target.value)} />
          <button type="button" className="aim-btn" data-style="primary" disabled={busy || !answer.trim()} onClick={() => run(() => inbox.answer(i, { text: answer.trim() }))}>
            {strings.inboxAnswer}
          </button>
          {open}
        </>
      );
  } else if (kind === 'draft') {
    body = <textarea className="aim-input aim-inbox-draft" rows={6} aria-label={strings.inboxDraftFrom(i.agent_name ?? '')} dir="auto" value={text} onChange={(e) => setText(e.target.value)} />;
    actions = (
      <>
        <button type="button" className="aim-btn" data-style="primary" disabled={busy || !text.trim()} onClick={() => run(() => inbox.approveDraft(i, text))}>
          {text === i.content ? strings.inboxSend : strings.inboxSendEdited}
        </button>
        <button type="button" className="aim-btn" disabled={busy} onClick={() => run(() => inbox.discardDraft(i))}>
          {strings.inboxDiscard}
        </button>
        {open}
      </>
    );
  } else if (kind === 'handoff') {
    body = (
      <p className="aim-inbox-text" dir="auto">
        {i.note || strings.inboxHandoffFrom(i.agent_name ?? '')}
      </p>
    );
    actions = (
      <>
        {open}
        <button type="button" className="aim-btn" disabled={busy} onClick={() => run(() => inbox.handBack(i))}>
          {strings.inboxHandBack}
        </button>
      </>
    );
  }
  return (
    <article className="aim-inbox-detail" aria-label={d.title}>
      <header>
        <span className="aim-inbox-kind" data-kind={kind}>
          <KindIcon kind={kind} />
          {strings.inboxTabs[kind] ?? kind}
        </span>
        {/* A draft's text is below, to edit: the heading says whose it is. */}
        <h2 dir="auto">{kind === 'draft' ? strings.inboxDraftFrom(i.agent_name ?? '') : d.title}</h2>
        <p className="aim-inbox-sub">
          {kind === 'draft' ? i.conv_title ?? '' : d.sub}
          {i.created_at ? ` · ${formatTime(i.created_at, when)}` : ''}
        </p>
        {d.amount ? <p className="aim-inbox-amount">{d.amount}</p> : null}
      </header>
      <div className="aim-inbox-body">{body}</div>
      {outcome ? (
        <div className="aim-card-status" data-tone={outcome.state === 'approved' || outcome.state === 'answered' || outcome.state === 'chosen' ? 'success' : undefined} role="status">
          {outcomeText(outcome, strings)}
        </div>
      ) : (
        <div className="aim-inbox-actions">{actions}</div>
      )}
      {err ? (
        <p className="aim-field-error" role="alert">
          {err}
        </p>
      ) : null}
    </article>
  );
}

/**
 * The inbox: tabs (All, Approvals, Drafts, Hand-offs, Questions) with
 * counts, a search, the list and the selected item. `types` limits the
 * kinds; `extraItems` adds yours (any `type`, drawn by `renderItem`);
 * `onOpen` opens a conversation in your app.
 */
export function InboxPanel({
  strings,
  when,
  types,
  extraItems = [],
  renderItem,
  onOpen,
}: {
  strings: Strings;
  when: When;
  types?: string[];
  extraItems?: InboxItem[];
  renderItem?(props: InboxDetailProps): ReactNode;
  onOpen?(convId: string): void;
}) {
  const inbox = useInbox();
  const [tab, setTab] = useState<string>('all');
  const [q, setQ] = useState('');
  const [selected, setSelected] = useState<string>();
  // What was handled here stays until the next refresh replaces it.
  const [kept, setKept] = useState<Record<string, InboxItem>>({});
  const all = useMemo(() => {
    const list = [...inbox.items, ...extraItems].filter((i) => !types || types.includes(inboxKind(i)));
    const ids = new Set(list.map(inboxId));
    for (const [id, it] of Object.entries(kept)) if (!ids.has(id) && inbox.outcomes[id]) list.push(it);
    // Newest first, as mail and approval apps list them.
    return list.sort((a, b) => (b.created_at ?? 0) - (a.created_at ?? 0));
  }, [inbox.items, extraItems, types, kept, inbox.outcomes]);
  useEffect(() => {
    const sel = all.find((i) => inboxId(i) === selected);
    if (sel) setKept((k) => (k[inboxId(sel)] ? k : { ...k, [inboxId(sel)]: sel }));
  }, [selected, all]);
  const pending = (i: InboxItem) => !inbox.outcomes[inboxId(i)];
  const counts: Record<string, number> = { all: all.filter(pending).length };
  for (const i of all) if (pending(i)) counts[inboxKind(i)] = (counts[inboxKind(i)] ?? 0) + 1;
  const needle = q.trim().toLowerCase();
  const shown = all.filter((i) => (tab === 'all' || inboxKind(i) === tab) && (!needle || JSON.stringify([describe(i, strings, when), i.conv_title, i.agent_name, i.asker_name]).toLowerCase().includes(needle)));
  const current = all.find((i) => inboxId(i) === selected);
  const tabs = TABS.filter((t) => t === 'all' || !types || types.includes(t));
  const now = when.now ?? Date.now();
  return (
    <section className="aim-inbox" data-detail={current ? true : undefined} aria-label={strings.inbox}>
      <div className="aim-inbox-list">
        <div className="aim-inbox-top">
          <div className="aim-tabs" role="tablist" aria-label={strings.inbox}>
            {tabs.map((t) => (
              <button key={t} type="button" role="tab" aria-selected={tab === t} className="aim-tab" onClick={() => setTab(t)}>
                {strings.inboxTabs[t]}
                {counts[t] ? <span className="aim-count">{counts[t]}</span> : null}
              </button>
            ))}
          </div>
          <input className="aim-input aim-inbox-search" type="search" aria-label={strings.inboxSearch} placeholder={strings.inboxSearch} value={q} onChange={(e) => setQ(e.target.value)} />
        </div>
        {inbox.loading ? (
          <div className="aim-skeleton" aria-hidden="true" />
        ) : !shown.length ? (
          <div className="aim-empty">
            <p className="aim-empty-text">{needle ? strings.noMatches : strings.inboxEmpty}</p>
            {needle ? null : <p className="aim-empty-hint">{strings.inboxEmptyHint}</p>}
          </div>
        ) : (
          <ul className="aim-inbox-rows" role="listbox" aria-label={strings.inboxTabs[tab]}>
            {shown.map((i) => {
              const id = inboxId(i);
              const d = describe(i, strings, when);
              const o = inbox.outcomes[id];
              const soon = i.expires_at && i.expires_at - now < 10 * 60_000 && !o;
              return (
                <li key={id} role="option" aria-selected={id === selected}>
                  <button type="button" className="aim-inbox-row" data-selected={id === selected || undefined} data-done={o ? true : undefined} onClick={() => setSelected(id)}>
                    <span className="aim-inbox-kind" data-kind={inboxKind(i)}>
                      <KindIcon kind={inboxKind(i)} />
                    </span>
                    <span className="aim-inbox-main">
                      <span className="aim-inbox-title" dir="auto">
                        {d.title}
                      </span>
                      <span className="aim-inbox-sub" dir="auto">
                        {o ? outcomeText(o, strings) : d.sub}
                      </span>
                    </span>
                    <span className="aim-inbox-side">
                      {d.amount ? <span className="aim-inbox-amount">{d.amount}</span> : null}
                      <span className="aim-inbox-time" data-soon={soon || undefined}>
                        {soon ? strings.inboxExpiresSoon : formatAgo(i.created_at, when)}
                      </span>
                    </span>
                  </button>
                </li>
              );
            })}
          </ul>
        )}
      </div>
      <div className="aim-inbox-pane">
        {current ? (
          <>
            <button type="button" className="aim-inbox-back" onClick={() => setSelected(undefined)}>
              {strings.inboxBack}
            </button>
            {renderItem?.({ item: current, inbox, strings, when, outcome: inbox.outcomes[inboxId(current)], onOpen }) ?? (
              <DefaultInboxDetail item={current} inbox={inbox} strings={strings} when={when} outcome={inbox.outcomes[inboxId(current)]} onOpen={onOpen} />
            )}
          </>
        ) : (
          <div className="aim-empty">
            <p className="aim-empty-hint">{strings.inboxSelect}</p>
          </div>
        )}
      </div>
    </section>
  );
}
