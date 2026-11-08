// Cards in the chat (docs/en/design/chat-components.md): text, buttons, forms, and data
// shown as facts, tables and lists; values formatted for the viewer's own
// locale. A card that was answered stays answered after a reload (the
// server's `card_answers`). Blocks and fields named `x-…` are yours:
// register a component for them in `components.cardBlocks` /
// `components.formFields`; unknown ones show their `fallback`.

import { useMemo, useState, type ComponentType, type FormEvent } from 'react';
import type { Message } from '@agentimhq/client-web';
import { fieldError, interactiveBlock, useCardAnswers, type CardAnswer } from './logic.js';
import type { Strings } from './strings.js';
import { formatTime, formatValue, tone, type When } from './format.js';

/** What your own card block gets (`components.cardBlocks['x-…']`). */
export interface CardBlockProps {
  /** The block as sent (`type`, `id`, `data`, `fallback`). */
  block: any;
  /** Whether it can't be answered (any more) by this person. */
  done: boolean;
  /** Answers it: an `action` or `values` (blocks with an `id` only). */
  respond(r: { action?: string; values?: Record<string, unknown> }): void;
  strings: Strings;
  when: When;
}

/** What your own form field gets (`components.formFields['x-…']`). */
export interface FormFieldProps {
  /** The field as sent (`id`, `label`, `type`, `config`, …). */
  field: any;
  value: unknown;
  onChange(value: unknown): void;
  disabled: boolean;
  /** The field's error message, when its value is not valid. */
  error?: string;
  strings: Strings;
}

/** The registries of your own blocks and fields. */
export interface CardParts {
  cardBlocks?: Record<string, ComponentType<CardBlockProps>>;
  formFields?: Record<string, ComponentType<FormFieldProps>>;
}

export function Card({
  m,
  mine,
  me,
  strings,
  when,
  parts,
  onRespond,
}: {
  m: Message;
  mine: boolean;
  me?: string;
  strings: Strings;
  when: When;
  parts: CardParts;
  onRespond(blockId: string, r: { action?: string; values?: Record<string, unknown> }): void;
}) {
  const p = m.payload ?? {};
  const blocks: any[] = Array.isArray(p.blocks) ? p.blocks : [];
  const interactive = blocks.some(interactiveBlock);
  const [answers, add] = useCardAnswers(m, interactive, me, mine);
  // For some people only: the others see it, read-only.
  const forOthers = Array.isArray(p.to) && me !== undefined && !p.to.includes(me);
  const closed = p.once === true && answers.length > 0;
  const mineAnswer = (id: string) => answers.find((a) => a.block_id === id && a.by === me);
  const done = (id: string) => mine || forOthers || closed || !!mineAnswer(id);
  const respond = (id: string) => (r: { action?: string; values?: Record<string, unknown> }) => {
    add({ block_id: id, by: me ?? '', action: r.action, values: r.values, at: Date.now() });
    onRespond(id, r);
  };
  const who = (a?: CardAnswer) => (a ? (a.by === me ? strings.inboxYou : a.by_name ?? '') : '');
  const last = answers[answers.length - 1];
  return (
    <div className="aim-card" data-answered={closed || undefined}>
      {p.title ? (
        <div className="aim-card-title" dir="auto">
          {p.title}
        </div>
      ) : null}
      {blocks.map((b, i) => {
        const key = b.id ?? `b${i}`;
        switch (b.type) {
          case 'text':
            return (
              <p key={key} className="aim-card-text" dir="auto">
                {b.text}
              </p>
            );
          case 'buttons': {
            const picked = mineAnswer(b.id)?.action ?? (closed && last?.block_id === b.id ? last.action : undefined);
            return (
              <div key={key} className="aim-card-buttons" role="group">
                {(b.options ?? []).map((x: any) => (
                  <button key={x.id} type="button" data-style={x.style} data-picked={picked === x.id || undefined} aria-pressed={picked ? picked === x.id : undefined} disabled={done(b.id)} onClick={() => respond(b.id)({ action: x.id })}>
                    {x.label}
                  </button>
                ))}
              </div>
            );
          }
          case 'form': {
            const a = mineAnswer(b.id) ?? (closed && last?.block_id === b.id ? last : undefined);
            if (a) return <FormSummary key={key} block={b} answer={a} who={who(a)} strings={strings} when={when} />;
            return <CardForm key={key} block={b} strings={strings} done={done(b.id)} parts={parts} onRespond={respond(b.id)} />;
          }
          case 'facts':
            return <Facts key={key} block={b} when={when} />;
          case 'table':
            return <Table key={key} block={b} when={when} />;
          case 'list':
            return <List key={key} block={b} done={done(b.id)} picked={mineAnswer(b.id)?.action ?? undefined} onPick={(id) => respond(b.id)({ action: id })} />;
          default: {
            if (typeof b.type === 'string' && b.type.startsWith('x-')) {
              const C = parts.cardBlocks?.[b.type];
              if (C) return <C key={key} block={b} done={b.id ? done(b.id) : true} respond={b.id ? respond(b.id) : () => {}} strings={strings} when={when} />;
              if (b.fallback && b.fallback !== 'drop')
                return (
                  <p key={key} className="aim-card-fallback" dir="auto">
                    {b.fallback}
                  </p>
                );
            }
            return null;
          }
        }
      })}
      {forOthers && !closed && interactive ? <div className="aim-card-status">{strings.cardWaiting}</div> : null}
      {closed && !blocks.some((b) => b.type === 'form') ? (
        <div className="aim-card-status" data-tone="success">
          {strings.cardAnswered}
          {last?.by ? ` · ${who(last)}` : ''}
        </div>
      ) : null}
    </div>
  );
}

function Facts({ block, when }: { block: any; when: When }) {
  return (
    <dl className="aim-facts">
      {(block.items ?? []).map((f: any, i: number) => {
        const t = tone(f.tone);
        const v = formatValue(f.value, f.format, when);
        return (
          <div key={i} className="aim-fact">
            <dt dir="auto">{f.label}</dt>
            <dd dir="auto" data-format={f.format}>
              {f.tone ? (
                <span className="aim-tone" data-tone={t}>
                  {v}
                </span>
              ) : (
                v
              )}
            </dd>
          </div>
        );
      })}
    </dl>
  );
}

function Table({ block, when }: { block: any; when: When }) {
  const cols: any[] = block.columns ?? [];
  const align = (c: any) => c?.align ?? (c?.format && c.format !== 'text' && c.format !== 'date' && c.format !== 'datetime' ? 'end' : 'start');
  return (
    <div className="aim-table-wrap" role="region" aria-label={block.caption ?? undefined} tabIndex={0}>
      <table className="aim-table">
        {block.caption ? <caption>{block.caption}</caption> : null}
        <thead>
          <tr>
            {cols.map((c, i) => (
              <th key={i} scope="col" data-align={align(c)}>
                {c.label}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {(block.rows ?? []).map((r: any[], i: number) => (
            <tr key={i}>
              {r.map((v, j) =>
                j === 0 ? (
                  <th key={j} scope="row" data-align={align(cols[j])} dir="auto">
                    {formatValue(v, cols[j]?.format, when)}
                  </th>
                ) : (
                  <td key={j} data-align={align(cols[j])} dir="auto">
                    {formatValue(v, cols[j]?.format, when)}
                  </td>
                ),
              )}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function List({ block, done, picked, onPick }: { block: any; done: boolean; picked?: string; onPick(id: string): void }) {
  const row = (it: any) => (
    <>
      <span className="aim-dlist-main">
        <span className="aim-dlist-title" dir="auto">
          {it.title}
        </span>
        {it.subtitle ? (
          <span className="aim-dlist-sub" dir="auto">
            {it.subtitle}
          </span>
        ) : null}
      </span>
      {it.badge ? (
        <span className="aim-tone" data-tone={tone(it.badge.tone)}>
          {it.badge.label}
        </span>
      ) : null}
      {it.meta ? <span className="aim-dlist-meta">{it.meta}</span> : null}
    </>
  );
  return (
    <ul className="aim-dlist" data-select={block.select || undefined}>
      {(block.items ?? []).map((it: any) => (
        <li key={it.id}>
          {block.select ? (
            <button type="button" className="aim-dlist-item" disabled={done} aria-pressed={picked ? picked === it.id : undefined} data-picked={picked === it.id || undefined} onClick={() => onPick(it.id)}>
              {row(it)}
            </button>
          ) : (
            <div className="aim-dlist-item">{row(it)}</div>
          )}
        </li>
      ))}
    </ul>
  );
}

function CardForm({ block, strings, done, parts, onRespond }: { block: any; strings: Strings; done: boolean; parts: CardParts; onRespond(r: { values: Record<string, unknown> }): void }) {
  const fields: any[] = block.fields ?? [];
  const initial = useMemo(() => {
    const v: Record<string, unknown> = {};
    for (const f of fields) if (f.default !== undefined && f.default !== null) v[f.id] = f.default;
    return v;
  }, [block]);
  const [values, setValues] = useState<Record<string, unknown>>(initial);
  const [tried, setTried] = useState(false);
  const errors: Record<string, string | undefined> = {};
  for (const f of fields) errors[f.id] = fieldError(f, values[f.id], strings);
  const set = (id: string, v: unknown) => setValues((x) => ({ ...x, [id]: v }));
  const submit = (e: FormEvent<HTMLFormElement>) => {
    e.preventDefault();
    setTried(true);
    if (fields.some((f) => errors[f.id])) return;
    const out: Record<string, unknown> = {};
    for (const f of fields) {
      const v = values[f.id];
      if (v !== undefined && v !== '') out[f.id] = v;
      else if (f.type === 'checkbox') out[f.id] = false;
    }
    onRespond({ values: out });
  };
  return (
    <form className="aim-card-form" onSubmit={submit} noValidate>
      {fields.map((f) => {
        const id = `aim-f-${block.id}-${f.id}`;
        const err = tried ? errors[f.id] : undefined;
        const describedBy = [f.hint ? `${id}-hint` : '', err ? `${id}-err` : ''].filter(Boolean).join(' ') || undefined;
        const common = { id, name: f.id, disabled: done, 'aria-invalid': err ? true : undefined, 'aria-describedby': describedBy, 'aria-required': f.required || undefined };
        const v = values[f.id];
        let control;
        // Yours first (an `x-…` type, or your own control for a built-in one).
        if (parts.formFields?.[f.type] || (typeof f.type === 'string' && f.type.startsWith('x-'))) {
          const C = parts.formFields?.[f.type];
          control = C ? <C field={f} value={v} onChange={(x) => set(f.id, x)} disabled={done} error={err} strings={strings} /> : null;
          if (!C) return null;
        } else if (f.type === 'select') {
          control = (
            <select {...common} value={(v as string) ?? ''} onChange={(e) => set(f.id, e.target.value || undefined)}>
              <option value="">{f.placeholder ?? ''}</option>
              {(f.options ?? []).map((o: string) => (
                <option key={o} value={o}>
                  {o}
                </option>
              ))}
            </select>
          );
        } else if (f.type === 'checkbox') {
          control = <input {...common} type="checkbox" checked={!!v} onChange={(e) => set(f.id, e.target.checked)} />;
        } else if (f.type === 'textarea') {
          control = <textarea {...common} rows={3} placeholder={f.placeholder} maxLength={f.max} value={(v as string) ?? ''} onChange={(e) => set(f.id, e.target.value)} />;
        } else {
          const type = f.type === 'number' ? 'number' : f.type === 'date' ? 'date' : f.type === 'time' ? 'time' : f.type === 'email' ? 'email' : 'text';
          control = (
            <input
              {...common}
              type={type}
              placeholder={f.placeholder}
              maxLength={f.type === 'text' ? f.max : undefined}
              min={f.type === 'number' || f.type === 'date' || f.type === 'time' ? f.min : undefined}
              max={f.type === 'number' || f.type === 'date' || f.type === 'time' ? f.max : undefined}
              value={v === undefined || v === null ? '' : String(v)}
              onChange={(e) => set(f.id, f.type === 'number' ? (e.target.value === '' ? undefined : Number(e.target.value)) : e.target.value)}
            />
          );
        }
        return (
          <div key={f.id} className="aim-field" data-type={f.type} data-invalid={err ? true : undefined}>
            <label htmlFor={id}>
              {f.label}
              {f.required ? <span className="aim-field-req" aria-hidden="true"> *</span> : null}
            </label>
            {control}
            {f.hint ? (
              <span id={`${id}-hint`} className="aim-field-hint">
                {f.hint}
              </span>
            ) : null}
            {err ? (
              <span id={`${id}-err`} className="aim-field-error" role="alert">
                {err}
              </span>
            ) : null}
          </div>
        );
      })}
      <div className="aim-card-actions">
        <button type="submit" data-style="primary" disabled={done}>
          {block.submit ?? strings.submit}
        </button>
      </div>
    </form>
  );
}

function FormSummary({ block, answer, who, strings, when }: { block: any; answer: CardAnswer; who: string; strings: Strings; when: When }) {
  const fields: any[] = block.fields ?? [];
  const v = answer.values;
  const shown = (f: any) => {
    const x = v?.[f.id];
    if (x === undefined || x === null || x === '') return strings.emptyValue;
    if (f.type === 'date') return formatValue(x, 'date', when);
    if (f.type === 'checkbox') return x ? '✓' : strings.emptyValue;
    return typeof x === 'object' ? JSON.stringify(x) : String(x);
  };
  return (
    <div className="aim-form-summary">
      {v ? (
        <dl className="aim-facts">
          {fields.map((f) => (
            <div key={f.id} className="aim-fact">
              <dt dir="auto">{f.label}</dt>
              <dd dir="auto">{shown(f)}</dd>
            </div>
          ))}
        </dl>
      ) : null}
      <div className="aim-card-status" data-tone="success">
        {who ? strings.answeredBy(who) : strings.cardAnswered}
        {answer.at ? ` · ${formatTime(answer.at, when)}` : ''}
      </div>
    </div>
  );
}
