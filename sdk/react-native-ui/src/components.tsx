// React Native components (docs/en/design/developer-experience.md): the same names, props, texts,
// replaceable parts and layout rules as @agentimhq/react, drawn with
// native views. The AI mark is drawn by the list around each message,
// outside what can be replaced (EU AI Act art. 50).

import { createContext, memo, useCallback, useContext, useEffect, useMemo, useRef, useState, type ComponentType, type ReactNode } from 'react';
import { ActivityIndicator, FlatList, Pressable, ScrollView, StyleSheet, Switch, Text, TextInput, View, type ListRenderItem, type NativeScrollEvent, type NativeSyntheticEvent, type ViewStyle } from 'react-native';
import type { Message } from '@agentimhq/client-web/view';
import {
  AgentIMProvider,
  avatarColor,
  defaultStrings,
  ended,
  formatShort,
  formatTime,
  initials,
  interleave,
  layout,
  unreadIndex,
  useClient,
  useConnection,
  useConversations,
  useMessages,
  useSend,
  useTyping,
  type Client,
  type Item,
  type Notice,
  type Strings,
  type When,
  fieldError,
  formatAgo,
  formatValue,
  inboxId,
  inboxKind,
  interactiveBlock,
  tone,
  useCardAnswers,
  useInbox,
  useSteps,
  type CardAnswer,
  type InboxItem,
  type InboxOutcome,
  type InboxState,
  type Step,
} from '@agentimhq/react/hooks';
import { AgentIMThemeProvider, useTheme, type Theme } from './theme';
import type { Colors } from './tokens';
import { FileMessage, ImageMessage } from './files';

/** What a message component gets: replace `Message` in `components` to draw messages your way. */
export interface MessageProps {
  /** The message to show. */
  message: Message;
  /** The conversation (to read the message's file). */
  convId: string;
  /** Whether I sent it (shown on the right). */
  mine: boolean;
  /** Whether it starts a run of messages from the same sender (shows the name and avatar). */
  first: boolean;
  /** Whether it ends a run of messages from the same sender (shows the time). */
  last: boolean;
  /** The formatted time to show, when it shows one. */
  time?: string;
  /** The interface texts in the current language. */
  strings: Strings;
  /** Sends a failed message again. */
  onRetry(): void;
  /** Drops a failed message. */
  onDiscard(): void;
  /** Rates an agent's message. */
  onFeedback(rating: 'up' | 'down'): void;
  /** Answers a card block: a button `action` or form `values`. */
  onRespond(blockId: string, r: { action?: string; values?: Record<string, unknown> }): void;
  /** Edits my message (when it can still be edited). */
  onEdit?(text: string): void;
  /** Recalls my message for everyone. */
  onRecall?(): void;
  /** Stops an agent's streaming reply. */
  onStop?(): void;
  /** Adds (`on`) or removes my reaction (docs/en/design/messaging.md). */
  onReact?(key: string, on: boolean): void;
  /** Set when the app shows threads: opens this message's thread. */
  onOpenThread?(): void;
}

/**
 * What a conversation list row gets: replace `ConversationItem` in `components` to draw rows your
 * way.
 */
export interface ConversationItemProps {
  /** The conversation (title, last message, unread count, members…). */
  conversation: any;
  /** Whether it is the open conversation. */
  selected: boolean;
  /** Opens it. */
  onSelect(): void;
  /** The interface texts in the current language. */
  strings: Strings;
  /** The formatted time of the last message. */
  time?: string;
  /** My user id (for "You: …" previews and direct-conversation titles). */
  me?: string;
}

/** The parts you can replace, passed as `components` to `AgentIM`. */
export interface Components {
  /** Draws one message. */
  Message: ComponentType<MessageProps>;
  /** Draws one conversation list row. */
  ConversationItem: ComponentType<ConversationItemProps>;
  /** Draws an avatar from the id, name and kind (human or agent). */
  Avatar: ComponentType<{ id?: string; name?: string; kind?: string; size?: number }>;
  /** Draws an empty state (no conversations, no messages). */
  Empty: ComponentType<{ text: string; hint?: string }>;
  /** Draws an agent's steps next to its reply (docs/en/design/chat-components.md). */
  Steps?: ComponentType<StepsProps>;
  /** Your own card blocks, by type (`x-…`, docs/en/design/chat-components.md). */
  cardBlocks?: Record<string, ComponentType<CardBlockProps>>;
  /** Your own form fields, by type: `x-…`, or a built-in one drawn your way (a native date picker for `date`). */
  formFields?: Record<string, ComponentType<FormFieldProps>>;
}

/** What your own card block gets. */
export interface CardBlockProps {
  /** The block as sent (`type`, `id`, `data`, `fallback`). */
  block: any;
  /** Whether it can't be answered (any more) by this person. */
  done: boolean;
  /** Answers it: an `action` or `values` (blocks with an `id` only). */
  respond(r: { action?: string; values?: Record<string, unknown> }): void;
  /** The interface texts. */
  strings: Strings;
  /** The viewer's locale, time zone and clock. */
  when: When;
}

/** What your own form field gets. */
export interface FormFieldProps {
  /** The field as sent (`id`, `label`, `type`, `config`, …). */
  field: any;
  /** Its value now. */
  value: unknown;
  /** Sets its value. */
  onChange(value: unknown): void;
  /** Whether it can't be changed (sent, or not for this person). */
  disabled: boolean;
  /** The message when its value is not valid. */
  error?: string;
  /** The interface texts. */
  strings: Strings;
}

/** What a replacement `Steps` gets. */
export interface StepsProps {
  /** The steps, in order. */
  steps: Step[];
  /** The interface texts. */
  strings: Strings;
}

interface Ui {
  strings: Strings;
  components: Components;
  me?: string;
  when: () => When;
}

const UiCtx = createContext<Ui | null>(null);

function useUi(): Ui {
  return useContext(UiCtx) ?? { strings: defaultStrings, components: defaultComponents, when: () => ({}) };
}

/** Styles for the theme, made once per theme. */
function useStyles(): ReturnType<typeof makeStyles> {
  const t = useTheme();
  return useMemo(() => makeStyles(t), [t]);
}

/**
 * The client, the texts, the replaceable parts, who "I" am, how dates are
 * shown and the theme (`scheme`: light, dark or the system's; `colors`
 * overrides any token).
 */
export function AgentIM({
  client,
  me,
  strings,
  components,
  locale,
  timeZone,
  now,
  scheme,
  colors,
  children,
}: {
  client: Client;
  me?: string;
  strings?: Partial<Strings>;
  components?: Partial<Components>;
  locale?: string;
  timeZone?: string;
  now?: () => number;
  scheme?: 'light' | 'dark' | 'system';
  colors?: Partial<Colors>;
  children?: ReactNode;
}) {
  const ui: Ui = {
    me,
    strings: { ...defaultStrings, ...strings },
    components: { ...defaultComponents, ...components },
    when: () => ({ locale, timeZone, now: now?.() }),
  };
  return (
    <AgentIMProvider client={client}>
      <AgentIMThemeProvider scheme={scheme} colors={colors}>
        <UiCtx.Provider value={ui}>{children}</UiCtx.Provider>
      </AgentIMThemeProvider>
    </AgentIMProvider>
  );
}

function DefaultAvatar({ id, name, kind, size = 32 }: { id?: string; name?: string; kind?: string; size?: number }) {
  const t = useTheme();
  const bg = kind === 'agent' ? t.colors.ai : t.avatar[avatarColor(id ?? name, t.avatar.length)];
  return (
    <View style={{ width: size, height: size, borderRadius: size / 2, backgroundColor: bg, alignItems: 'center', justifyContent: 'center' }} accessible={false} importantForAccessibility="no-hide-descendants">
      <Text style={{ color: '#ffffff', fontWeight: '600', fontSize: Math.round(size * 0.4) }} allowFontScaling={false}>
        {initials(name)}
      </Text>
    </View>
  );
}

/**
 * The AI mark (EU AI Act art. 50): drawn next to every agent's name and message, also under a
 * replaced Message component.
 */
export function AIBadge() {
  const s = useStyles();
  const { strings } = useUi();
  return (
    <View style={s.ai} accessible accessibilityRole="image" accessibilityLabel={strings.aiTitle}>
      <Text style={s.aiText} importantForAccessibility="no">
        ✦ {strings.ai}
      </Text>
    </View>
  );
}

/**
 * A line saying what the connection is doing (offline, reconnecting, waiting for network); nothing
 * while connected.
 */
export function ConnectionBanner() {
  const c = useConnection();
  const s = useStyles();
  const { strings } = useUi();
  const text = strings.connection[c.state] ?? c.state;
  if (!text) return null;
  const bad = c.state === 'failed' || c.state === 'closed';
  return (
    <View style={[s.banner, bad && s.bannerBad]} accessibilityRole="alert" accessibilityLiveRegion="polite" testID={`banner-${c.state}`}>
      <Text style={[s.bannerText, bad && s.bannerTextBad]}>{text}</Text>
    </View>
  );
}

function preview(c: any, strings: Strings, me?: string): string {
  const m = c.last_message;
  if (!m) return '';
  const text = m.state === 'recalled' ? strings.recalled : m.content || '';
  return m.mine || (m.sender_id && m.sender_id === me) ? `${strings.you}: ${text}` : text;
}

function DefaultConversationItem({ conversation: c, selected, onSelect, strings, time, me }: ConversationItemProps) {
  const s = useStyles();
  const { components } = useUi();
  const unread = c.unread > 0;
  const title = c.title || c.conv_id;
  const last = preview(c, strings, me);
  return (
    <Pressable
      onPress={onSelect}
      style={({ pressed }) => [s.conv, selected && s.convSelected, pressed && s.pressed]}
      accessibilityRole="button"
      accessibilityState={{ selected }}
      accessibilityLabel={[title, unread ? `${c.unread} unread` : '', last, time].filter(Boolean).join(', ')}
    >
      <components.Avatar id={c.conv_id} name={title} size={48} />
      <View style={s.convBody}>
        <View style={s.convRow}>
          <Text style={s.convTitle} numberOfLines={1}>
            {title}
          </Text>
          {time ? <Text style={[s.convTime, unread && s.convTimeUnread]}>{time}</Text> : null}
        </View>
        <View style={s.convRow}>
          <Text style={[s.convLast, unread && s.convLastUnread]} numberOfLines={1}>
            {last}
          </Text>
          {unread ? (
            <View style={s.unread}>
              <Text style={s.unreadText}>{c.unread > 99 ? '99+' : c.unread}</Text>
            </View>
          ) : null}
        </View>
      </View>
    </Pressable>
  );
}

/** The conversations, newest first, with a search field that filters by title and last message. */
export function ConversationList({ selected, onSelect, search = true }: { selected?: string; onSelect(convId: string): void; search?: boolean }) {
  const list = useConversations();
  const s = useStyles();
  const t = useTheme();
  const { strings, components, me, when } = useUi();
  const [q, setQ] = useState('');
  const w = when();
  const shown = useMemo(() => {
    const needle = q.trim().toLocaleLowerCase();
    if (!needle) return list;
    return list.filter((c) => `${c.title ?? ''}\n${c.last_message?.content ?? ''}`.toLocaleLowerCase().includes(needle));
  }, [list, q]);
  return (
    <View style={s.fill}>
      {search && list.length ? (
        <View style={s.search}>
          <View style={s.searchIcon} importantForAccessibility="no-hide-descendants">
            <View style={s.searchLens} />
            <View style={s.searchHandle} />
          </View>
          <TextInput value={q} onChangeText={setQ} placeholder={strings.search} placeholderTextColor={t.colors.muted} accessibilityLabel={strings.search} style={s.searchInput} returnKeyType="search" clearButtonMode="while-editing" autoCorrect={false} />
        </View>
      ) : null}
      {!list.length ? (
        <components.Empty text={strings.noConversations} />
      ) : !shown.length ? (
        <components.Empty text={strings.noMatches} />
      ) : (
        <FlatList
          data={shown}
          keyExtractor={(c) => c.conv_id}
          keyboardShouldPersistTaps="handled"
          accessibilityLabel={strings.conversations}
          renderItem={({ item: c }) => (
            <components.ConversationItem conversation={c} selected={c.conv_id === selected} onSelect={() => onSelect(c.conv_id)} strings={strings} me={me} time={c.last_message?.created_at ? formatShort(c.last_message.created_at, w) : undefined} />
          )}
        />
      )}
    </View>
  );
}

function Button({ label, onPress, kind, disabled }: { label: string; onPress(): void; kind?: string; disabled?: boolean }) {
  const s = useStyles();
  return (
    <Pressable
      onPress={onPress}
      disabled={disabled}
      accessibilityRole="button"
      accessibilityState={{ disabled: !!disabled }}
      style={({ pressed }) => [s.button, kind === 'primary' && s.buttonPrimary, kind === 'danger' && s.buttonDanger, disabled && s.disabled, pressed && s.pressed]}
    >
      <Text style={[s.buttonText, kind === 'primary' && s.buttonTextPrimary, kind === 'danger' && s.buttonTextDanger]}>{label}</Text>
    </Pressable>
  );
}

function CardForm({ block, strings, done, onRespond }: { block: any; strings: Strings; done: boolean; onRespond(r: { values: Record<string, unknown> }): void }) {
  const s = useStyles();
  const t = useTheme();
  const { components } = useUi();
  const fields: any[] = block.fields ?? [];
  const [values, setValues] = useState<Record<string, unknown>>(() => Object.fromEntries(fields.filter((f) => f.default !== undefined && f.default !== null).map((f) => [f.id, f.default])));
  const [tried, setTried] = useState(false);
  const set = (id: string, v: unknown) => setValues((x) => ({ ...x, [id]: v }));
  const errors: Record<string, string | undefined> = {};
  for (const f of fields) errors[f.id] = fieldError(f, values[f.id], strings);
  const submit = () => {
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
    <View style={s.cardForm}>
      {fields.map((f) => {
        const err = tried ? errors[f.id] : undefined;
        const v = values[f.id];
        const Custom = components.formFields?.[f.type];
        if (!Custom && typeof f.type === 'string' && f.type.startsWith('x-')) return null;
        let control: ReactNode;
        if (Custom) control = <Custom field={f} value={v} onChange={(x) => set(f.id, x)} disabled={done} error={err} strings={strings} />;
        else if (f.type === 'checkbox')
          control = <Switch value={!!v} disabled={done} onValueChange={(x) => set(f.id, x)} accessibilityLabel={f.label} trackColor={{ true: t.colors.accent, false: t.colors.controlBorder }} />;
        else if (f.type === 'select')
          control = (
            <View style={s.chips} accessibilityRole="radiogroup" accessibilityLabel={f.label}>
              {(f.options ?? []).map((o: string) => (
                <Pressable key={o} disabled={done} onPress={() => set(f.id, o)} accessibilityRole="radio" accessibilityState={{ checked: v === o, disabled: done }} style={[s.chip, v === o && s.chipOn]}>
                  <Text style={[s.chipText, v === o && s.chipTextOn]}>{o}</Text>
                </Pressable>
              ))}
            </View>
          );
        else
          control = (
            <TextInput
              value={v === undefined || v === null ? '' : String(v)}
              editable={!done}
              onChangeText={(x) => set(f.id, f.type === 'number' ? (x.trim() === '' ? undefined : Number(x)) : x)}
              keyboardType={f.type === 'number' ? 'numeric' : f.type === 'email' ? 'email-address' : f.type === 'date' || f.type === 'time' ? 'numbers-and-punctuation' : 'default'}
              autoCapitalize={f.type === 'email' ? 'none' : 'sentences'}
              multiline={f.type === 'textarea'}
              maxLength={f.type === 'text' || f.type === 'textarea' ? f.max : undefined}
              placeholder={f.placeholder ?? (f.type === 'date' ? 'YYYY-MM-DD' : f.type === 'time' ? 'HH:MM' : undefined)}
              accessibilityLabel={f.label}
              accessibilityHint={f.hint}
              style={[s.input, f.type === 'textarea' && s.inputArea, err ? s.inputBad : null]}
              placeholderTextColor={t.colors.muted}
            />
          );
        return (
          <View key={f.id} style={f.type === 'checkbox' ? s.checkRow : s.field}>
            <Text style={s.fieldLabel}>
              {f.label}
              {f.required ? <Text style={s.muted}> *</Text> : null}
            </Text>
            {control}
            {f.hint ? <Text style={s.fieldHint}>{f.hint}</Text> : null}
            {err ? (
              <Text style={s.fieldError} accessibilityRole="alert">
                {err}
              </Text>
            ) : null}
          </View>
        );
      })}
      <View style={s.alignStart}>
        <Button label={block.submit ?? strings.submit} kind="primary" disabled={done} onPress={submit} />
      </View>
    </View>
  );
}

function ToneText({ t: tn, children }: { t: unknown; children: ReactNode }) {
  const s = useStyles();
  const k = tone(tn);
  return (
    <View style={[s.tone, k === 'success' && s.toneSuccess, k === 'warning' && s.toneWarning, k === 'danger' && s.toneDanger, k === 'info' && s.toneInfo]}>
      <Text style={[s.toneText, k === 'success' && s.toneTextSuccess, k === 'warning' && s.toneTextWarning, k === 'danger' && s.toneTextDanger, k === 'info' && s.toneTextInfo]}>{children}</Text>
    </View>
  );
}

function Facts({ items, when }: { items: any[]; when: When }) {
  const s = useStyles();
  return (
    <View style={s.facts}>
      {items.map((f, i) => (
        <View key={i} style={s.fact}>
          <Text style={s.factLabel}>{f.label}</Text>
          <View style={s.factValue}>{f.tone ? <ToneText t={f.tone}>{formatValue(f.value, f.format, when)}</ToneText> : <Text style={s.text}>{formatValue(f.value, f.format, when)}</Text>}</View>
        </View>
      ))}
    </View>
  );
}

function Table({ block, when }: { block: any; when: When }) {
  const s = useStyles();
  const cols: any[] = block.columns ?? [];
  const end = (c: any) => c?.align === 'end' || (!c?.align && c?.format && c.format !== 'text' && c.format !== 'date' && c.format !== 'datetime');
  return (
    <View style={s.tableWrap}>
      {block.caption ? <Text style={s.tableCaption}>{block.caption}</Text> : null}
      <ScrollView horizontal showsHorizontalScrollIndicator={false}>
        <View>
          <View style={[s.tableRow, s.tableHead]}>
            {cols.map((c, i) => (
              <Text key={i} style={[s.tableCell, s.tableHeadText, end(c) && s.right]}>
                {c.label}
              </Text>
            ))}
          </View>
          {(block.rows ?? []).map((r: any[], i: number) => (
            <View key={i} style={[s.tableRow, i > 0 && s.tableRule]}>
              {r.map((v, j) => (
                <Text key={j} style={[s.tableCell, j === 0 && s.tableFirst, end(cols[j]) && s.right]}>
                  {formatValue(v, cols[j]?.format, when)}
                </Text>
              ))}
            </View>
          ))}
        </View>
      </ScrollView>
    </View>
  );
}

function DataList({ block, done, picked, onPick }: { block: any; done: boolean; picked?: string | null; onPick(id: string): void }) {
  const s = useStyles();
  return (
    <View style={s.dlist}>
      {(block.items ?? []).map((it: any, i: number) => {
        const body = (
          <>
            <View style={s.dlistMain}>
              <Text style={s.dlistTitle} numberOfLines={1}>
                {it.title}
              </Text>
              {it.subtitle ? (
                <Text style={s.muted} numberOfLines={1}>
                  {it.subtitle}
                </Text>
              ) : null}
            </View>
            {it.badge ? <ToneText t={it.badge.tone}>{it.badge.label}</ToneText> : null}
            {it.meta ? <Text style={s.muted}>{it.meta}</Text> : null}
          </>
        );
        return block.select ? (
          <Pressable key={it.id} disabled={done} onPress={() => onPick(it.id)} accessibilityRole="button" accessibilityState={{ disabled: done, selected: picked === it.id }} style={({ pressed }) => [s.dlistItem, i > 0 && s.tableRule, picked === it.id && s.dlistPicked, pressed && s.pressed]}>
            {body}
          </Pressable>
        ) : (
          <View key={it.id} style={[s.dlistItem, i > 0 && s.tableRule]}>
            {body}
          </View>
        );
      })}
    </View>
  );
}

function Card({ m, mine, strings, onRespond }: { m: Message; mine: boolean; strings: Strings; onRespond: MessageProps['onRespond'] }) {
  const s = useStyles();
  const { me, when, components } = useUi();
  const w = when();
  const p = m.payload ?? {};
  const blocks: any[] = Array.isArray(p.blocks) ? p.blocks : [];
  const [answers, add] = useCardAnswers(m as any, blocks.some(interactiveBlock), me, mine);
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
    <View style={s.card}>
      {p.title ? <Text style={s.cardTitle}>{p.title}</Text> : null}
      {blocks.map((b, i) => {
        const key = b.id ?? `b${i}`;
        switch (b.type) {
          case 'text':
            return (
              <Text key={key} style={s.text}>
                {b.text}
              </Text>
            );
          case 'buttons': {
            const picked = mineAnswer(b.id)?.action ?? (closed && last?.block_id === b.id ? last.action : undefined);
            return (
              <View key={key} style={s.cardButtons}>
                {(b.options ?? []).map((x: any) => (
                  <Button key={x.id} label={x.label} kind={picked === x.id ? 'primary' : x.style} disabled={done(b.id)} onPress={() => respond(b.id)({ action: x.id })} />
                ))}
              </View>
            );
          }
          case 'form': {
            const a = mineAnswer(b.id) ?? (closed && last?.block_id === b.id ? last : undefined);
            if (a)
              return (
                <View key={key} style={s.cardForm}>
                  {a.values ? <Facts items={(b.fields ?? []).map((f: any) => ({ label: f.label, value: a.values?.[f.id] ?? null, format: f.type === 'date' ? 'date' : undefined }))} when={w} /> : null}
                  <Text style={s.cardStatusDone}>
                    {who(a) ? strings.answeredBy(who(a)) : strings.cardAnswered}
                    {a.at ? ` · ${formatTime(a.at, w)}` : ''}
                  </Text>
                </View>
              );
            return <CardForm key={key} block={b} strings={strings} done={done(b.id)} onRespond={respond(b.id)} />;
          }
          case 'facts':
            return <Facts key={key} items={b.items ?? []} when={w} />;
          case 'table':
            return <Table key={key} block={b} when={w} />;
          case 'list':
            return <DataList key={key} block={b} done={done(b.id)} picked={mineAnswer(b.id)?.action} onPick={(id) => respond(b.id)({ action: id })} />;
          default: {
            if (typeof b.type === 'string' && b.type.startsWith('x-')) {
              const C = components.cardBlocks?.[b.type];
              if (C) return <C key={key} block={b} done={b.id ? done(b.id) : true} respond={b.id ? respond(b.id) : () => {}} strings={strings} when={w} />;
              if (b.fallback && b.fallback !== 'drop')
                return (
                  <Text key={key} style={s.cardFallback}>
                    {b.fallback}
                  </Text>
                );
            }
            return null;
          }
        }
      })}
      {forOthers && !closed && blocks.some(interactiveBlock) ? <Text style={s.muted}>{strings.cardWaiting}</Text> : null}
      {closed && !blocks.some((b) => b.type === 'form') ? (
        <Text style={s.cardStatusDone}>
          {strings.cardAnswered}
          {last?.by ? ` · ${who(last)}` : ''}
        </Text>
      ) : null}
    </View>
  );
}

/** The default list of an agent's steps (docs/en/design/chat-components.md). */
export function DefaultSteps({ steps, strings }: StepsProps) {
  const s = useStyles();
  const t = useTheme();
  const running = steps.some((x) => x.state === 'running' || x.state === 'waiting' || x.state === 'pending');
  const failed = steps.some((x) => x.state === 'failed');
  const [open, setOpen] = useState(running || failed);
  useEffect(() => {
    if (running || failed) setOpen(true);
  }, [running, failed]);
  if (!steps.length) return null;
  const secs = Math.max(0, Math.round((Math.max(...steps.map((x) => x.at)) - Math.min(...steps.map((x) => x.at))) / 1000));
  const mark = (st: Step['state']) =>
    st === 'done' ? (
      <Text style={[s.stepMark, { color: t.colors.success }]}>✓</Text>
    ) : st === 'failed' ? (
      <Text style={[s.stepMark, { color: t.colors.danger }]}>✕</Text>
    ) : st === 'skipped' ? (
      <Text style={s.stepMark}>–</Text>
    ) : st === 'running' ? (
      <ActivityIndicator size="small" color={t.colors.accent} style={s.stepSpin} />
    ) : (
      <View style={[s.stepDot, st === 'waiting' && { backgroundColor: t.colors.warningText }]} />
    );
  return (
    <View style={s.steps}>
      {!running ? (
        <Pressable onPress={() => setOpen((x) => !x)} accessibilityRole="button" accessibilityState={{ expanded: open }} style={s.stepsToggle}>
          <Text style={s.muted}>
            {strings.stepsSummary(steps.length, secs)} {open ? '▴' : '▾'}
          </Text>
        </Pressable>
      ) : null}
      {open ? (
        <View style={s.stepsList} accessibilityLabel={strings.stepsTitle} accessibilityLiveRegion="polite">
          {steps.map((x) => (
            <View key={x.id} style={s.step} accessibilityLabel={`${x.title}, ${strings.stepStates[x.state]}`}>
              <View style={s.stepMarkBox}>{mark(x.state)}</View>
              <View style={s.flex}>
                <Text style={[s.stepTitle, (x.state === 'running' || x.state === 'waiting') && s.stepTitleOn]}>{x.title}</Text>
                {x.detail || x.state === 'waiting' || x.state === 'failed' ? (
                  <Text style={[s.stepDetail, x.state === 'waiting' && { color: t.colors.warningText }, x.state === 'failed' && { color: t.colors.danger }]}>{x.detail ?? strings.stepStates[x.state]}</Text>
                ) : null}
              </View>
            </View>
          ))}
        </View>
      ) : null}
    </View>
  );
}

function ReplySteps({ convId, msgId }: { convId: string; msgId?: string }) {
  const steps = useSteps(convId, msgId);
  const { components, strings } = useUi();
  if (!steps.length) return null;
  const S = components.Steps ?? DefaultSteps;
  return <S steps={steps} strings={strings} />;
}

/** What a custom inbox detail gets. */
export interface InboxDetailProps {
  /** The item (a request, a draft, a hand-off, or yours). */
  item: InboxItem;
  /** The inbox, to act: `decide`, `answer`, `approveDraft`, `discardDraft`, `handBack`. */
  inbox: InboxState;
  /** The interface texts. */
  strings: Strings;
  /** The viewer's locale, time zone and clock. */
  when: When;
  /** What happened to it here, once handled. */
  outcome?: InboxOutcome;
  /** Opens its conversation in your app. */
  onOpen?(convId: string): void;
}

function describeItem(i: InboxItem, strings: Strings, w: When): { title: string; sub: string; amount?: string } {
  const where = [i.conv_title, i.asker_name ? strings.inboxFor(i.asker_name) : null].filter(Boolean).join(' · ');
  if (i.type === 'request') {
    const op = i.context?.operation;
    if (op) return { title: op.description ? String(op.description).split(' (')[0] : op.name, sub: [op.name, where].filter(Boolean).join(' · '), amount: op.amount !== null && op.amount !== undefined ? formatValue(op.amount, typeof op.amount === 'number' ? 'number' : 'text', w) : undefined };
    return { title: String(i.title ?? '').split('\n')[0], sub: [i.agent_name, where].filter(Boolean).join(' · ') };
  }
  if (i.type === 'draft') return { title: String(i.content ?? '').slice(0, 140), sub: [strings.inboxDraftFrom(i.agent_name ?? ''), i.conv_title].filter(Boolean).join(' · ') };
  if (i.type === 'handoff') return { title: i.note || strings.inboxHandoffFrom(i.agent_name ?? ''), sub: [i.agent_name, i.conv_title].filter(Boolean).join(' · ') };
  return { title: String(i.title ?? i.type), sub: i.conv_title ?? '' };
}

function outcomeLabel(o: InboxOutcome, strings: Strings): string {
  const st = o.state === 'approved' ? strings.approved : o.state === 'rejected' ? strings.declined : o.state === 'expired' ? strings.approvalExpired : o.state === 'cancelled' ? strings.approvalCancelled : o.state === 'handed_back' ? strings.inboxHandBack : strings.cardAnswered;
  return o.by === 'me' ? strings.inboxDecidedBy(strings.inboxYou, st) : st;
}

/** The default details of an inbox item, with its actions. */
export function DefaultInboxDetail({ item: i, inbox, strings, when: w, outcome, onOpen }: InboxDetailProps) {
  const s = useStyles();
  const t = useTheme();
  const [busy, setBusy] = useState(false);
  const [declining, setDeclining] = useState(false);
  const [reason, setReason] = useState('');
  const [text, setText] = useState<string>(i.content ?? '');
  const [answer, setAnswer] = useState('');
  const [err, setErr] = useState<string>();
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
  const d = describeItem(i, strings, w);
  const kind = inboxKind(i);
  const op = i.context?.operation;
  const asked = i.context?.tool_call?.msg;
  const open = onOpen ? <Button label={strings.inboxOpen} onPress={() => onOpen(i.conv_id)} /> : null;
  let body: ReactNode = null;
  let actions: ReactNode = null;
  if (kind === 'request') {
    body = (
      <>
        {op ? <View style={s.inboxBox}><Facts items={(op.fields ?? []).map((f: any) => ({ label: f.name, value: f.value, format: typeof f.value === 'number' ? 'number' : undefined }))} when={w} /></View> : <Text style={s.text}>{i.title}</Text>}
        {op?.rule?.amount_field ? <Text style={s.muted}>{`${strings.inboxRule}: ${op.rule.amount_field} > ${formatValue(op.rule.over, 'number', w)}`}</Text> : null}
        {asked?.content ? (
          <View style={s.inboxQuote}>
            <Text style={s.fieldHint}>{`${strings.inboxTriggeredBy}${asked.sender_name ? ` · ${asked.sender_name}` : ''}`}</Text>
            <Text style={s.text}>{asked.content}</Text>
          </View>
        ) : null}
      </>
    );
    actions = declining ? (
      <>
        <TextInput value={reason} onChangeText={setReason} placeholder={strings.inboxDeclineReason} accessibilityLabel={strings.inboxDeclineReason} placeholderTextColor={t.colors.muted} style={[s.input, s.flex]} />
        <Button label={strings.decline} kind="danger" disabled={busy} onPress={() => run(() => inbox.decide(i, 'reject', reason.trim() || undefined))} />
        <Button label={strings.cancel} disabled={busy} onPress={() => setDeclining(false)} />
      </>
    ) : (
      <>
        <Button label={strings.approve} kind="primary" disabled={busy} onPress={() => run(() => inbox.decide(i, 'approve'))} />
        <Button label={strings.decline} disabled={busy} onPress={() => setDeclining(true)} />
        {open}
      </>
    );
  } else if (kind === 'question') {
    actions =
      i.kind === 'choose' ? (
        <>
          {(i.options ?? []).map((o: string) => (
            <Button key={o} label={o} disabled={busy} onPress={() => run(() => inbox.answer(i, { choice: o }))} />
          ))}
          {open}
        </>
      ) : (
        <>
          <TextInput value={answer} onChangeText={setAnswer} multiline accessibilityLabel={strings.inboxAnswer} style={[s.input, s.inputArea, s.flex]} />
          <Button label={strings.inboxAnswer} kind="primary" disabled={busy || !answer.trim()} onPress={() => run(() => inbox.answer(i, { text: answer.trim() }))} />
        </>
      );
  } else if (kind === 'draft') {
    body = <TextInput value={text} onChangeText={setText} multiline accessibilityLabel={strings.inboxDraftFrom(i.agent_name ?? '')} style={[s.input, s.inputArea, { minHeight: 140 }]} />;
    actions = (
      <>
        <Button label={text === i.content ? strings.inboxSend : strings.inboxSendEdited} kind="primary" disabled={busy || !text.trim()} onPress={() => run(() => inbox.approveDraft(i, text))} />
        <Button label={strings.inboxDiscard} disabled={busy} onPress={() => run(() => inbox.discardDraft(i))} />
      </>
    );
  } else if (kind === 'handoff') {
    actions = (
      <>
        {open}
        <Button label={strings.inboxHandBack} disabled={busy} onPress={() => run(() => inbox.handBack(i))} />
      </>
    );
  }
  return (
    <View style={s.inboxDetail} accessibilityLabel={d.title}>
      <Text style={s.fieldHint}>{strings.inboxTabs[kind] ?? kind}</Text>
      <Text style={s.inboxH}>{d.title}</Text>
      <Text style={s.muted}>{d.sub}</Text>
      {d.amount ? <Text style={s.inboxAmountBig}>{d.amount}</Text> : null}
      {body}
      {outcome ? <Text style={outcome.state === 'approved' || outcome.state === 'answered' || outcome.state === 'chosen' ? s.cardStatusDone : s.muted}>{outcomeLabel(outcome, strings)}</Text> : <View style={s.inboxActions}>{actions}</View>}
      {err ? <Text style={s.fieldError}>{err}</Text> : null}
    </View>
  );
}

/**
 * The approval inbox (docs/en/design/chat-components.md): tabs, search, the list, and one item's
 * details with its actions (on a phone, one after the other).
 */
export function Inbox({ types, extraItems = [], renderItem, onOpen }: { types?: string[]; extraItems?: InboxItem[]; renderItem?(p: InboxDetailProps): ReactNode; onOpen?(convId: string): void }) {
  const s = useStyles();
  const t = useTheme();
  const { strings, when } = useUi();
  const w = when();
  const inbox = useInbox();
  const [tab, setTab] = useState('all');
  const [q, setQ] = useState('');
  const [selected, setSelected] = useState<string>();
  const [kept, setKept] = useState<Record<string, InboxItem>>({});
  const all = useMemo(() => {
    const list = [...inbox.items, ...extraItems].filter((i) => !types || types.includes(inboxKind(i)));
    const ids = new Set(list.map(inboxId));
    for (const [id, it] of Object.entries(kept)) if (!ids.has(id) && inbox.outcomes[id]) list.push(it);
    return list.sort((a, b) => (b.created_at ?? 0) - (a.created_at ?? 0));
  }, [inbox.items, extraItems, types, kept, inbox.outcomes]);
  const pending = (i: InboxItem) => !inbox.outcomes[inboxId(i)];
  const counts: Record<string, number> = { all: all.filter(pending).length };
  for (const i of all) if (pending(i)) counts[inboxKind(i)] = (counts[inboxKind(i)] ?? 0) + 1;
  const needle = q.trim().toLowerCase();
  const shown = all.filter((i) => (tab === 'all' || inboxKind(i) === tab) && (!needle || JSON.stringify([describeItem(i, strings, w), i.conv_title, i.agent_name, i.asker_name]).toLowerCase().includes(needle)));
  const current = all.find((i) => inboxId(i) === selected);
  if (current)
    return (
      <ScrollView style={s.fill} contentContainerStyle={s.inboxPage}>
        <Pressable onPress={() => setSelected(undefined)} accessibilityRole="button" style={s.inboxBack}>
          <Text style={s.inboxBackText}>{strings.inboxBack}</Text>
        </Pressable>
        {renderItem?.({ item: current, inbox, strings, when: w, outcome: inbox.outcomes[inboxId(current)], onOpen }) ?? <DefaultInboxDetail item={current} inbox={inbox} strings={strings} when={w} outcome={inbox.outcomes[inboxId(current)]} onOpen={onOpen} />}
      </ScrollView>
    );
  const tabs = ['all', 'request', 'draft', 'handoff', 'question'].filter((x) => x === 'all' || !types || types.includes(x));
  return (
    <View style={s.fill}>
      <View style={s.inboxTop}>
        <View style={s.chips} accessibilityRole="tablist">
          {tabs.map((x) => (
            <Pressable key={x} onPress={() => setTab(x)} accessibilityRole="tab" accessibilityState={{ selected: tab === x }} style={[s.chip, tab === x && s.chipOn]}>
              <Text style={[s.chipText, tab === x && s.chipTextOn]}>
                {strings.inboxTabs[x]}
                {counts[x] ? ` ${counts[x]}` : ''}
              </Text>
            </Pressable>
          ))}
        </View>
        <TextInput value={q} onChangeText={setQ} placeholder={strings.inboxSearch} accessibilityLabel={strings.inboxSearch} placeholderTextColor={t.colors.muted} style={s.input} />
      </View>
      {!shown.length ? (
        <DefaultEmpty text={needle ? strings.noMatches : strings.inboxEmpty} hint={needle ? undefined : strings.inboxEmptyHint} />
      ) : (
        <FlatList
          data={shown}
          keyExtractor={inboxId}
          renderItem={({ item: i }) => {
            const d = describeItem(i, strings, w);
            const o = inbox.outcomes[inboxId(i)];
            const soon = i.expires_at && i.expires_at - (w.now ?? Date.now()) < 10 * 60_000 && !o;
            return (
              <Pressable
                onPress={() => {
                  setKept((k) => ({ ...k, [inboxId(i)]: i }));
                  setSelected(inboxId(i));
                }}
                accessibilityRole="button"
                style={({ pressed }) => [s.inboxRow, pressed && s.pressed]}
              >
                <View style={s.flex}>
                  <Text style={[s.dlistTitle, o && { color: t.colors.muted }]} numberOfLines={1}>
                    {d.title}
                  </Text>
                  <Text style={s.muted} numberOfLines={1}>
                    {o ? outcomeLabel(o, strings) : d.sub}
                  </Text>
                </View>
                <View style={s.inboxSide}>
                  {d.amount ? <Text style={s.inboxAmount}>{d.amount}</Text> : null}
                  <Text style={[s.fieldHint, soon && { color: t.colors.warningText, fontWeight: '600' }]}>{soon ? strings.inboxExpiresSoon : formatAgo(i.created_at, w)}</Text>
                </View>
              </Pressable>
            );
          }}
        />
      )}
    </View>
  );
}

/** How many items wait in the inbox. */
export function InboxBadge() {
  const s = useStyles();
  const { items } = useInbox();
  return items.length ? (
    <View style={s.unread}>
      <Text style={s.unreadText}>{items.length > 99 ? '99+' : items.length}</Text>
    </View>
  ) : null;
}

/** Steps an agent reports without tying them to a message (docs/en/design/chat-components.md). */
export function ConversationSteps({ convId }: { convId: string }) {
  return <ReplySteps convId={convId} />;
}

function EditBox({ text, strings, onSave, onCancel }: { text: string; strings: Strings; onSave(t: string): void; onCancel(): void }) {
  const s = useStyles();
  const [value, setValue] = useState(text);
  const save = () => {
    const t = value.trim();
    if (t && t !== text) onSave(t);
    else onCancel();
  };
  return (
    <View style={s.edit}>
      <TextInput autoFocus multiline value={value} onChangeText={setValue} accessibilityLabel={strings.editing} style={[s.input, s.editInput]} />
      <View style={s.cardButtons}>
        <Button label={strings.cancel} onPress={onCancel} />
        <Button label={strings.save} kind="primary" onPress={save} disabled={!value.trim()} />
      </View>
    </View>
  );
}

function TextLink({ label, onPress, tone }: { label: string; onPress(): void; tone?: 'danger' }) {
  const s = useStyles();
  return (
    <Pressable onPress={onPress} hitSlop={12} accessibilityRole="button" style={({ pressed }) => [s.link, pressed && s.pressed]}>
      <Text style={[s.linkText, tone === 'danger' && s.linkDanger]}>{label}</Text>
    </Pressable>
  );
}

function DefaultMessage({ message: m, convId, mine, first, last, time, strings, onRetry, onDiscard, onFeedback, onRespond, onEdit, onRecall, onStop, onReact, onOpenThread }: MessageProps) {
  const s = useStyles();
  const [editing, setEditing] = useState(false);
  // A long press shows what I can do with my message (iMessage, WhatsApp).
  const [actions, setActions] = useState(false);
  if (m.state === 'recalled')
    return (
      <View style={[s.bubble, s.recalled]}>
        <Text style={s.recalledText}>{strings.recalled}</Text>
      </View>
    );
  const streaming = m.state === 'streaming';
  const text = streaming ? (m.streamed ?? m.content) : m.content;
  const sources: any[] = m.envelope?.sources ?? [];
  const failed = m.status === 'failed';
  const edited = m.state === 'committed' && m.version > 1;
  const notes = [streaming && strings.streaming, m.state === 'cancelled' && strings.cancelled, m.state === 'failed' && strings.streamFailed, edited && strings.edited].filter(Boolean) as string[];
  const feedback = m.sender_type === 'agent' && !mine && !streaming && m.status === 'sent';
  const agent = m.sender_type === 'agent';
  const image = !!(m.payload?.file_id || m.payload?.local_source || m.payload?.local_thumb);
  if (editing && onEdit)
    return (
      <EditBox
        text={m.content}
        strings={strings}
        onCancel={() => setEditing(false)}
        onSave={(t) => {
          setEditing(false);
          onEdit(t);
        }}
      />
    );
  const corners: ViewStyle = mine
    ? { borderTopRightRadius: first ? 18 : 6, borderBottomRightRadius: last ? 18 : 6 }
    : { borderTopLeftRadius: first ? 18 : 6, borderBottomLeftRadius: last ? 18 : 6 };
  const isCard = m.kind === 'agentim.card';
  return (
    <View style={mine ? s.alignEnd : s.alignStart}>
      {agent && m.msg_id ? <ReplySteps convId={convId} msgId={m.msg_id} /> : null}
      <Pressable
        onLongPress={onEdit || onRecall ? () => setActions((a) => !a) : undefined}
        accessibilityHint={onEdit || onRecall ? 'Long press for actions' : undefined}
        accessibilityState={{ busy: streaming }}
        testID={`bubble-${m.key}`}
        style={[s.bubble, corners, mine ? s.bubbleMine : agent ? s.bubbleAi : null, m.status === 'sending' && s.sending, failed && s.failedBubble, m.kind === 'image' && image && s.bubbleImage, isCard && s.bubbleCard]}
      >
        {m.kind === 'agentim.card' ? (
          <Card m={m} mine={mine} strings={strings} onRespond={onRespond} />
        ) : m.kind === 'image' && image ? (
          <ImageMessage convId={convId} message={m} strings={strings} mine={mine} />
        ) : (m.kind === 'file' || m.kind === 'image') && m.payload ? (
          <FileMessage convId={convId} message={m} strings={strings} mine={mine} />
        ) : (
          <Text style={[s.text, mine && s.textMine]} selectable>
            {text}
            {streaming ? <Text style={s.cursor}> ▍</Text> : null}
          </Text>
        )}
        {sources.length ? (
          <View style={s.sources}>
            <Text style={s.sourcesLabel}>{strings.sources.toUpperCase()}</Text>
            {sources.map((x: any) => (
              <Text key={x.url} style={s.source} accessibilityRole="link">
                {x.title} ↗
              </Text>
            ))}
          </View>
        ) : null}
      </Pressable>
      {(m as any).reactions?.length && m.state !== 'recalled' ? (
        <View style={[s.reactions, mine && s.metaMine]}>
          {((m as any).reactions as any[]).map((r) => (
            <Pressable
              key={r.key}
              style={[s.reaction, r.me && s.reactionMine]}
              accessibilityRole="button"
              accessibilityState={{ selected: !!r.me }}
              accessibilityLabel={strings.reactedBy(r.key, r.count, !!r.me)}
              disabled={!onReact}
              onPress={() => onReact?.(r.key, !r.me)}
            >
              <Text style={s.reactionText}>
                {r.key} {r.count}
              </Text>
            </Pressable>
          ))}
        </View>
      ) : null}
      {(m as any).thread?.replies ? (
        <Pressable accessibilityRole="button" disabled={!onOpenThread} onPress={onOpenThread} style={mine && s.metaMine}>
          <Text style={s.threadLink}>{strings.replies((m as any).thread.replies)}</Text>
        </Pressable>
      ) : null}
      {last || failed || m.status === 'sending' || notes.length || feedback || onStop || actions ? (
        <View style={[s.meta, mine && s.metaMine]}>
          {notes.map((n) => (
            <Text key={n} style={s.metaText}>
              {n}
            </Text>
          ))}
          {last && time && !failed ? <Text style={s.metaText}>{time}</Text> : null}
          {mine && m.status === 'sending' ? <Text style={s.metaText}>{strings.sending}</Text> : null}
          {mine && last && m.status === 'sent' ? (
            <Text style={s.sent} accessibilityLabel={strings.sent}>
              ✓
            </Text>
          ) : null}
          {failed ? (
            <View style={s.failed} accessibilityRole="alert">
              <Text style={s.failedText}>{strings.failed}</Text>
              <TextLink label={strings.retry} onPress={onRetry} tone="danger" />
              <TextLink label={strings.discard} onPress={onDiscard} tone="danger" />
            </View>
          ) : null}
          {onStop ? (
            <Pressable onPress={onStop} hitSlop={10} accessibilityRole="button" style={({ pressed }) => [s.stop, pressed && s.pressed]}>
              <Text style={s.stopText}>■ {strings.stop}</Text>
            </Pressable>
          ) : null}
          {actions && onEdit ? <TextLink label={strings.edit} onPress={() => { setActions(false); setEditing(true); }} /> : null}
          {actions && onRecall ? <TextLink label={strings.discard} onPress={() => { setActions(false); onRecall(); }} tone="danger" /> : null}
          {feedback ? (
            <View style={s.feedback}>
              <Pressable onPress={() => onFeedback('up')} hitSlop={10} accessibilityRole="button" accessibilityLabel={strings.helpful} style={s.thumb}>
                <Text style={s.thumbText}>👍</Text>
              </Pressable>
              <Pressable onPress={() => onFeedback('down')} hitSlop={10} accessibilityRole="button" accessibilityLabel={strings.notHelpful} style={s.thumb}>
                <Text style={s.thumbText}>👎</Text>
              </Pressable>
            </View>
          ) : null}
        </View>
      ) : null}
    </View>
  );
}

function DefaultEmpty({ text, hint }: { text: string; hint?: string }) {
  const s = useStyles();
  return (
    <View style={s.empty}>
      <Text style={s.emptyText}>{text}</Text>
      {hint ? <Text style={s.emptyHint}>{hint}</Text> : null}
    </View>
  );
}

export const defaultComponents: Components = { Message: DefaultMessage, ConversationItem: DefaultConversationItem, Avatar: DefaultAvatar, Empty: DefaultEmpty };

interface RowProps {
  item: Item<Message, ReactNode>;
  convId: string;
  onOpenThread?(threadId: string): void;
}

/** One cell of the list: a notice, or a message with its day line, name, avatar and AI mark. */
const Cell = memo(function Cell({ item, convId, onOpenThread }: RowProps) {
  const client = useClient();
  const s = useStyles();
  const { strings, components, me, when } = useUi();
  if (item.notice) {
    const tone = item.notice.tone;
    return (
      <View>
        {item.day ? <Day label={item.day} /> : null}
        <View style={[s.notice, tone === 'warning' && s.noticeWarning, tone === 'private' && s.noticePrivate]} accessibilityRole="text" testID={`notice-${item.notice.key}`}>
          {typeof item.notice.content === 'string' ? <Text style={[s.noticeText, tone && tone !== 'info' && s.noticeTextWarning]}>{item.notice.content}</Text> : item.notice.content}
        </View>
      </View>
    );
  }
  const { message: m, day, unread, first, last } = item.row;
  const mine = m.mine || m.sender_id === me || m.key.startsWith('local:');
  const agent = m.sender_type === 'agent';
  const sent = mine && m.status === 'sent' && m.msg_id && !m.key.startsWith('local:');
  const target = { msgId: m.msg_id };
  return (
    <View testID={`row-${m.key}`}>
      {day ? <Day label={day} /> : null}
      {unread ? <Day label={strings.newMessages} tone="danger" /> : null}
      <View style={[s.row, first && s.rowFirst, mine && s.rowMine]}>
        {!mine ? <View style={s.gutter}>{first ? <components.Avatar id={m.sender_id} name={m.sender_name} kind={m.sender_type} /> : null}</View> : null}
        <View style={[s.col, mine && s.colMine]}>
          {first && !mine ? (
            <View style={s.sender}>
              <Text style={s.senderText}>{m.sender_name}</Text>
              {agent ? <AIBadge /> : null}
            </View>
          ) : null}
          <components.Message
            message={m}
            convId={convId}
            mine={mine}
            first={first}
            last={last}
            time={last && m.created_at ? formatTime(m.created_at, when()) : undefined}
            strings={strings}
            onRetry={() => m.client_msg_id && void client.retry(convId, m.client_msg_id)}
            onDiscard={() => m.client_msg_id && void client.discard(convId, m.client_msg_id)}
            onFeedback={(r) => void client.feedback(convId, m.msg_id, r).catch(() => {})}
            onRespond={(b, r) => void client.respond(convId, m.msg_id, b, r).catch(() => {})}
            onEdit={sent && m.state === 'committed' && (m.kind ?? 'text') === 'text' ? (t) => void client.edit(convId, target, t).catch(() => {}) : undefined}
            onRecall={sent && m.state !== 'streaming' ? () => void client.recall(convId, target).catch(() => {}) : undefined}
            onStop={m.state === 'streaming' && agent ? () => void client.call('stream_cancel', { msg_id: m.msg_id }).catch(() => {}) : undefined}
            onReact={m.msg_id && !m.key.startsWith('local:') ? (k, on) => void client.call(on ? 'react' : 'unreact', { conv_id: convId, msg_id: m.msg_id, key: k }).catch(() => {}) : undefined}
            onOpenThread={onOpenThread && m.msg_id ? () => onOpenThread(m.msg_id) : undefined}
          />
        </View>
      </View>
    </View>
  );
});

function Day({ label, tone }: { label: string; tone?: 'danger' }) {
  const s = useStyles();
  return (
    <View style={s.day} accessibilityRole="header">
      <View style={[s.dayLine, tone && s.dayLineDanger]} />
      <Text style={[s.dayText, tone && s.dayTextDanger]}>{label}</Text>
      <View style={[s.dayLine, tone && s.dayLineDanger]} />
    </View>
  );
}

const NEAR_BOTTOM = 48;

/**
 * The messages in an inverted list (newest at the bottom, scroll up for
 * older ones), in runs per sender with day
 * separators, notices and "New messages"; a button brings you back down.
 */
export function MessageList({ convId, notices = [], onOpenThread }: { convId: string; notices?: Notice<ReactNode>[]; onOpenThread?(threadId: string): void }) {
  const client = useClient();
  const s = useStyles();
  const { strings, components, me, when } = useUi();
  const { messages: everything, hasOlder, olderEnd, loadingOlder, loadOlder } = useMessages(convId);
  // A reply only in a thread is shown in its thread, not here (docs/en/design/messaging.md).
  const messages = useMemo(() => everything.filter((m: any) => !(typeof m.thread_id === 'string' && m.thread_shown !== true)), [everything]);
  const conv = useConversations().find((c) => c.conv_id === convId);
  const list = useRef<FlatList<Item<Message, ReactNode>>>(null);
  const [atBottom, setAtBottom] = useState(true);
  const seen = useRef(0);
  const divider = useRef<{ conv: string; count: number; key?: string }>({ conv: '', count: 0 });
  if (divider.current.conv !== convId) divider.current = { conv: convId, count: conv?.unread ?? 0 };
  if (divider.current.count > 0 && divider.current.key === undefined && messages.length) {
    const i = unreadIndex(messages, divider.current.count, me);
    divider.current.key = i === undefined ? '' : messages[i].key;
  }
  const unreadFrom = divider.current.key ? messages.findIndex((m) => m.key === divider.current.key) : -1;
  // No longer in it: the last line says so (L4).
  const end = ended(conv, me);
  const endText = end ? strings.ended(end.reason, end.by) : undefined;
  // Inverted: the newest first.
  const items = useMemo(() => {
    const all = endText ? [...notices, { key: 'ended', seq: Number.MAX_SAFE_INTEGER, content: endText, tone: 'info' as const }] : notices;
    return interleave(layout(messages, when(), unreadFrom >= 0 ? unreadFrom : undefined), all).reverse();
  }, [messages, notices, unreadFrom, when, endText]);
  const isMine = (m: Message) => m.mine || m.sender_id === me || m.key.startsWith('local:');

  useEffect(() => {
    const focus = (c: string | null) => void client.focus?.(c).catch(() => {});
    focus(convId);
    return () => focus(null);
  }, [client, convId]);

  useEffect(() => {
    const last = messages.at(-1);
    if (last?.created_seq) client.read(convId, last.created_seq).catch(() => {});
  }, [client, convId, messages]);

  // When I send, go down to it.
  const lastKey = useRef<string | undefined>(undefined);
  useEffect(() => {
    const newest = messages.at(-1);
    if (newest && newest.key !== lastKey.current && lastKey.current !== undefined && isMine(newest) && !atBottom) {
      list.current?.scrollToOffset({ offset: 0, animated: true });
    }
    lastKey.current = newest?.key;
    if (atBottom) seen.current = messages.length;
  });

  const onScroll = useCallback(
    (e: NativeSyntheticEvent<NativeScrollEvent>) => {
      const at = e.nativeEvent.contentOffset.y < NEAR_BOTTOM;
      if (at) seen.current = messages.length;
      setAtBottom((b) => (b === at ? b : at));
    },
    [messages.length],
  );

  const renderItem: ListRenderItem<Item<Message, ReactNode>> = useCallback(({ item }) => <Cell item={item} convId={convId} onOpenThread={onOpenThread} />, [convId, onOpenThread]);
  const below = atBottom ? 0 : messages.slice(seen.current).filter((m) => !isMine(m)).length;
  // Inverted, the footer is at the top.
  const top = hasOlder ? (
    <View style={s.top}>{loadingOlder ? <ActivityIndicator /> : <TextLink label={strings.loadOlder} onPress={() => void loadOlder()} />}</View>
  ) : messages.length ? (
    <View style={s.top}>
      <Text style={s.topText}>{olderEnd === 'retention' ? strings.retention : strings.start}</Text>
    </View>
  ) : null;

  return (
    <View style={s.list}>
      {!messages.length && !notices.length ? (
        <components.Empty text={strings.empty} hint={strings.emptyHint} />
      ) : (
        <FlatList
          ref={list}
          inverted
          data={items}
          keyExtractor={(it) => (it.notice ? `n:${it.notice.key}` : it.row!.message.key)}
          renderItem={renderItem}
          ListFooterComponent={top ?? undefined}
          onEndReached={() => {
            if (hasOlder && !loadingOlder) void loadOlder();
          }}
          onEndReachedThreshold={0.5}
          onScroll={onScroll}
          scrollEventThrottle={100}
          keyboardDismissMode="interactive"
          keyboardShouldPersistTaps="handled"
          contentContainerStyle={s.listContent}
          accessibilityLabel={strings.messages}
          maintainVisibleContentPosition={{ minIndexForVisible: 0 }}
        />
      )}
      {!atBottom ? (
        <Pressable onPress={() => list.current?.scrollToOffset({ offset: 0, animated: true })} style={({ pressed }) => [s.jump, below > 0 && s.jumpNew, pressed && s.pressed]} accessibilityRole="button">
          <Text style={[s.jumpText, below > 0 && s.jumpTextNew]}>↓ {strings.jumpToLatest(below)}</Text>
        </Pressable>
      ) : null}
    </View>
  );
}

/** Who is typing, by name (from the members). */
export function TypingIndicator({ convId, names }: { convId: string; names?: (ids: string[]) => string[] }) {
  const { typing } = useTyping(convId);
  const s = useStyles();
  const { strings } = useUi();
  const members: any[] = useConversations().find((c) => c.conv_id === convId)?.members ?? [];
  if (!typing.length) return <View style={s.typing} />;
  const shown = names ? names(typing) : typing.map((id) => members.find((m) => m.user_id === id)?.name ?? id);
  return (
    <View style={s.typing} accessibilityLiveRegion="polite">
      <Text style={s.typingText}>••• {strings.typing(shown)}</Text>
    </View>
  );
}

const MAX_COMPOSER_LINES = 6;

/** A growing text field and a round send button (44 pt). */
/**
 * `onAttach`: the app's own picker (expo-document-picker, react-native-image-picker, …)
 * returns the paths to send; without it there is no attach button.
 */
export function Composer({ convId, onAttach }: { convId: string; onAttach?: () => Promise<string[] | undefined | null> }) {
  const client = useClient();
  const { send } = useSend(convId);
  const attach = async () => {
    const paths = (await onAttach?.().catch(() => null)) ?? [];
    for (const p of paths) void (client as any).sendFile?.(convId, p).catch(() => {});
  };
  const { notify } = useTyping(convId);
  const s = useStyles();
  const t = useTheme();
  const { strings, me } = useUi();
  const [text, setText] = useState('');
  const end = ended(
    useConversations().find((c) => c.conv_id === convId),
    me,
  );
  const can = !!text.trim();
  const submit = () => {
    const v = text.trim();
    if (!v) return;
    setText('');
    void send(v);
  };
  if (end)
    return (
      <View style={s.composer} accessibilityRole="text" accessibilityLiveRegion="polite">
        <View style={s.composerEnded}>
          <Text style={s.noticeText}>{strings.endedComposer(end.reason)}</Text>
        </View>
      </View>
    );
  return (
    <View style={s.composer}>
      <View style={s.composerBox}>
        {onAttach ? (
          <Pressable onPress={() => void attach()} accessibilityRole="button" accessibilityLabel={strings.attach} hitSlop={8} style={({ pressed }) => [s.attach, pressed && s.pressed]} testID="attach">
            <Text style={s.attachText}>＋</Text>
          </Pressable>
        ) : null}
        <TextInput
          value={text}
          onChangeText={(v) => {
            setText(v);
            if (v) notify();
          }}
          placeholder={strings.placeholder}
          placeholderTextColor={t.colors.muted}
          accessibilityLabel={strings.placeholder}
          multiline
          numberOfLines={1}
          style={[s.composerInput, { maxHeight: MAX_COMPOSER_LINES * 22 }]}
        />
        <Pressable onPress={submit} disabled={!can} accessibilityRole="button" accessibilityLabel={strings.send} accessibilityState={{ disabled: !can }} style={({ pressed }) => [s.send, !can && s.sendOff, pressed && s.pressed]} hitSlop={6}>
          <Text style={s.sendText}>↑</Text>
        </Pressable>
      </View>
    </View>
  );
}

/** The conversation's members; agents carry the AI mark. */
export function MemberList({ convId }: { convId: string }) {
  const conv = useConversations().find((c) => c.conv_id === convId);
  const s = useStyles();
  const { strings, components } = useUi();
  const members: any[] = conv?.members ?? [];
  return (
    <View accessibilityLabel={strings.members}>
      {members.map((m) => (
        <View key={m.user_id} style={s.member} testID={`member-${m.user_id}`}>
          <components.Avatar id={m.user_id} name={m.name} kind={m.kind} size={36} />
          <Text style={s.memberName} numberOfLines={1}>
            {m.name}
          </Text>
          {m.kind === 'agent' ? <AIBadge /> : null}
          {strings.roles[m.role] ? (
            <View style={s.role}>
              <Text style={s.roleText}>{strings.roles[m.role]}</Text>
            </View>
          ) : null}
        </View>
      ))}
    </View>
  );
}

/** Says when an agent hands the conversation to a person, and when it is back. */
export function HandoffNotice({ convId }: { convId: string }) {
  const client = useClient();
  const s = useStyles();
  const { strings } = useUi();
  const [notice, setNotice] = useState<{ text: string; note?: string } | null>(null);
  useEffect(() => {
    setNotice(null);
    return client.on('event', (e) => {
      const ev = e.event ?? {};
      if (ev.conv_id !== convId || ev.type !== 'agent.state') return;
      const d = ev.data ?? {};
      if (d.state === 'taken_over' && d.handoff) setNotice({ text: strings.handedOff, note: d.note });
      else if (d.state === 'active' && d.handback) setNotice({ text: strings.handedBack, note: d.note });
    });
  }, [client, convId, strings]);
  if (!notice) return null;
  return (
    <View style={s.handoff} accessibilityRole="alert">
      <Text style={s.handoffText}>{notice.text}</Text>
      {notice.note ? <Text style={s.handoffNote}>{notice.note}</Text> : null}
    </View>
  );
}

/** A whole chat screen: header, status, messages, typing, composer. Wrap it in the app's keyboard handling. */
export function Chat({ convId, title, subtitle, notices, onAttach }: { convId: string; title?: string; subtitle?: string; notices?: Notice<ReactNode>[]; onAttach?: () => Promise<string[] | undefined | null> }) {
  const s = useStyles();
  return (
    <View style={s.chat}>
      {title ? (
        <View style={s.header} accessibilityRole="header">
          <Text style={s.headerTitle} numberOfLines={1}>
            {title}
          </Text>
          {subtitle ? (
            <Text style={s.headerSub} numberOfLines={1}>
              {subtitle}
            </Text>
          ) : null}
        </View>
      ) : null}
      <ConnectionBanner />
      <HandoffNotice convId={convId} />
      <MessageList convId={convId} notices={notices} />
      <ConversationSteps convId={convId} />
      <TypingIndicator convId={convId} />
      <Composer convId={convId} onAttach={onAttach} />
    </View>
  );
}

function makeStyles(t: Theme) {
  const c = t.colors;
  const f = t.font;
  return StyleSheet.create({
    fill: { flex: 1, backgroundColor: c.bg },
    chat: { flex: 1, backgroundColor: c.bg },
    pressed: { opacity: 0.7 },
    disabled: { opacity: 0.45 },
    alignStart: { alignItems: 'flex-start' },
    alignEnd: { alignItems: 'flex-end' },
    header: { paddingHorizontal: 16, paddingVertical: 10, borderBottomWidth: StyleSheet.hairlineWidth, borderBottomColor: c.border, backgroundColor: c.bg },
    headerTitle: { fontSize: 17, fontWeight: '600', color: c.fg, textAlign: 'center' },
    headerSub: { fontSize: f.sizeTiny, color: c.muted, textAlign: 'center' },
    banner: { paddingVertical: 6, paddingHorizontal: 12, backgroundColor: c.warningSoft, alignItems: 'center' },
    bannerBad: { backgroundColor: c.dangerSoft },
    bannerText: { fontSize: f.sizeSmall, color: c.warningText, fontWeight: '500' },
    bannerTextBad: { color: c.danger },
    handoff: { marginHorizontal: 16, marginTop: 8, padding: 10, borderRadius: t.radius.card, backgroundColor: c.accentSoft },
    handoffText: { fontSize: f.sizeSmall, fontWeight: '600', color: c.accentText },
    handoffNote: { fontSize: f.sizeSmall, color: c.accentText },

    list: { flex: 1, backgroundColor: c.bg },
    listContent: { paddingHorizontal: 12, paddingVertical: 12 },
    top: { alignItems: 'center', paddingVertical: 12 },
    topText: { fontSize: f.sizeSmall, color: c.muted },
    day: { flexDirection: 'row', alignItems: 'center', marginTop: 16, marginBottom: 8, gap: 12 },
    dayLine: { flex: 1, height: StyleSheet.hairlineWidth, backgroundColor: c.border },
    dayLineDanger: { backgroundColor: c.danger, opacity: 0.4 },
    dayText: { fontSize: f.sizeTiny + 1, fontWeight: '600', color: c.muted },
    dayTextDanger: { color: c.danger },
    notice: { alignSelf: 'center', maxWidth: '90%', marginTop: 12, marginBottom: 4, paddingHorizontal: 12, paddingVertical: 5, borderRadius: 999, backgroundColor: c.surface },
    noticeWarning: { backgroundColor: c.warningSoft },
    noticePrivate: { alignSelf: 'stretch', maxWidth: '100%', borderRadius: t.radius.card, backgroundColor: c.warningSoft, borderWidth: 1, borderStyle: 'dashed', borderColor: c.warningText },
    noticeText: { fontSize: f.sizeSmall, color: c.muted, textAlign: 'center' },
    noticeTextWarning: { color: c.warningText },

    row: { flexDirection: 'row', alignItems: 'flex-start', marginTop: 2, gap: 8 },
    rowFirst: { marginTop: 12 },
    rowMine: { justifyContent: 'flex-end' },
    gutter: { width: 32 },
    col: { maxWidth: '78%', alignItems: 'flex-start' },
    colMine: { alignItems: 'flex-end' },
    sender: { flexDirection: 'row', alignItems: 'center', gap: 6, marginLeft: 4, marginBottom: 4 },
    senderText: { fontSize: f.sizeSmall - 1, fontWeight: '600', color: c.muted },
    ai: { flexDirection: 'row', alignItems: 'center', paddingHorizontal: 6, paddingVertical: 1, borderRadius: 999, backgroundColor: c.aiSoft },
    aiText: { fontSize: f.sizeTiny - 1, fontWeight: '700', letterSpacing: 0.4, color: c.aiText },

    bubble: { paddingHorizontal: 14, paddingVertical: 9, borderRadius: t.radius.bubble, backgroundColor: c.bubble },
    bubbleMine: { backgroundColor: c.bubbleMine },
    bubbleImage: { padding: 3 },
    attach: { width: 36, height: 36, borderRadius: 18, alignItems: 'center', justifyContent: 'center', alignSelf: 'flex-end' },
    attachText: { fontSize: 22, color: c.muted, lineHeight: 26 },
    bubbleAi: { backgroundColor: c.bubbleAi },
    sending: { opacity: 0.7 },
    failedBubble: { borderWidth: 2, borderColor: c.danger },
    text: { fontSize: f.size, lineHeight: Math.round(f.size * f.line), color: c.bubbleFg },
    textMine: { color: c.bubbleMineFg },
    cursor: { color: c.aiText },
    recalled: { backgroundColor: 'transparent', borderWidth: 1, borderStyle: 'dashed', borderColor: c.border },
    recalledText: { fontSize: f.sizeSmall, fontStyle: 'italic', color: c.muted },
    reactions: { flexDirection: 'row', flexWrap: 'wrap', gap: 4, marginTop: 4 },
    reaction: { paddingHorizontal: 8, minHeight: 26, justifyContent: 'center', borderRadius: 999, borderWidth: StyleSheet.hairlineWidth, borderColor: c.border },
    reactionMine: { borderColor: c.accent },
    reactionText: { fontSize: f.sizeSmall, color: c.fg },
    threadLink: { marginTop: 4, fontSize: f.sizeSmall, fontWeight: '600', color: c.accent },
    sources: { marginTop: 8, paddingTop: 8, borderTopWidth: StyleSheet.hairlineWidth, borderTopColor: c.border, gap: 4 },
    sourcesLabel: { fontSize: f.sizeTiny - 1, fontWeight: '600', letterSpacing: 0.6, color: c.muted },
    source: { fontSize: f.sizeSmall, color: c.accentText },
    meta: { flexDirection: 'row', flexWrap: 'wrap', alignItems: 'center', gap: 8, marginTop: 4, marginHorizontal: 4 },
    metaMine: { justifyContent: 'flex-end' },
    metaText: { fontSize: f.sizeTiny, color: c.muted },
    sent: { fontSize: f.sizeTiny, color: c.accentText, fontWeight: '700' },
    failed: { flexDirection: 'row', alignItems: 'center', gap: 8 },
    failedText: { fontSize: f.sizeTiny, color: c.danger, fontWeight: '600' },
    link: { paddingVertical: 2 },
    linkText: { fontSize: f.sizeTiny, color: c.muted, fontWeight: '600' },
    linkDanger: { color: c.danger, textDecorationLine: 'underline' },
    stop: { paddingHorizontal: 10, paddingVertical: 3, borderRadius: 999, borderWidth: 1, borderColor: c.controlBorder },
    stopText: { fontSize: f.sizeTiny, color: c.fg, fontWeight: '600' },
    feedback: { flexDirection: 'row', gap: 4 },
    thumb: { paddingHorizontal: 4 },
    thumbText: { fontSize: f.sizeSmall },

    card: { gap: 8, minWidth: 220 },
    cardTitle: { fontSize: f.size, fontWeight: '600', color: c.fg },
    cardButtons: { flexDirection: 'row', flexWrap: 'wrap', gap: 8 },
    cardForm: { gap: 12 },
    fieldLabel: { fontSize: f.sizeSmall, fontWeight: '500', color: c.fg, marginBottom: 4 },
    checkRow: { flexDirection: 'row', alignItems: 'center', justifyContent: 'space-between' },
    chips: { flexDirection: 'row', flexWrap: 'wrap', gap: 6 },
    chip: { minHeight: 32, justifyContent: 'center', paddingHorizontal: 12, borderRadius: 999, borderWidth: 1, borderColor: c.controlBorder },
    chipOn: { backgroundColor: c.accent, borderColor: c.accent },
    chipText: { fontSize: f.sizeSmall, color: c.fg },
    chipTextOn: { color: c.accentFg, fontWeight: '600' },
    input: { minHeight: t.target, paddingHorizontal: 12, borderRadius: t.radius.control, borderWidth: 1, borderColor: c.controlBorder, backgroundColor: c.bg, color: c.fg, fontSize: f.size },
    button: { minHeight: t.target, justifyContent: 'center', paddingHorizontal: 16, borderRadius: t.radius.control, borderWidth: 1, borderColor: c.controlBorder, backgroundColor: c.bg },
    buttonPrimary: { backgroundColor: c.accent, borderColor: c.accent },
    buttonDanger: { borderColor: c.danger },
    buttonText: { fontSize: f.size - 1, fontWeight: '600', color: c.fg, textAlign: 'center' },
    buttonTextPrimary: { color: c.accentFg },
    buttonTextDanger: { color: c.danger },
    flex: { flex: 1, minWidth: 0 },
    muted: { fontSize: f.sizeSmall, color: c.muted },
    right: { textAlign: 'right' },
    field: { gap: 0 },
    inputArea: { minHeight: 88, paddingTop: 10, textAlignVertical: 'top' },
    inputBad: { borderColor: c.danger },
    fieldHint: { fontSize: f.sizeTiny, color: c.muted, marginTop: 4 },
    fieldError: { fontSize: f.sizeTiny, color: c.danger, marginTop: 4 },
    cardStatusDone: { fontSize: f.sizeSmall, color: c.success },
    cardFallback: { fontSize: f.sizeSmall, color: c.muted, fontStyle: 'italic' },
    tone: { alignSelf: 'flex-start', paddingHorizontal: 8, paddingVertical: 1, borderRadius: 999, borderWidth: 1, borderColor: c.border, backgroundColor: c.surface },
    toneSuccess: { backgroundColor: c.successSoft, borderColor: c.successSoft },
    toneWarning: { backgroundColor: c.warningSoft, borderColor: c.warningSoft },
    toneDanger: { backgroundColor: c.dangerSoft, borderColor: c.dangerSoft },
    toneInfo: { backgroundColor: c.accentSoft, borderColor: c.accentSoft },
    toneText: { fontSize: f.sizeTiny, fontWeight: '600', color: c.muted },
    toneTextSuccess: { color: c.success },
    toneTextWarning: { color: c.warningText },
    toneTextDanger: { color: c.danger },
    toneTextInfo: { color: c.accentText },
    facts: { gap: 6 },
    fact: { flexDirection: 'row', gap: 12 },
    factLabel: { width: 96, fontSize: f.sizeSmall, color: c.muted, paddingTop: 1 },
    factValue: { flex: 1, minWidth: 0 },
    tableWrap: { borderWidth: StyleSheet.hairlineWidth, borderColor: c.border, borderRadius: t.radius.control, overflow: 'hidden', backgroundColor: c.bg },
    tableCaption: { fontWeight: '600', color: c.fg, paddingHorizontal: 12, paddingTop: 8 },
    tableRow: { flexDirection: 'row' },
    tableHead: { backgroundColor: c.surface },
    tableHeadText: { fontSize: f.sizeTiny, fontWeight: '600', color: c.muted },
    tableRule: { borderTopWidth: StyleSheet.hairlineWidth, borderTopColor: c.border },
    tableCell: { minWidth: 88, paddingHorizontal: 12, paddingVertical: 7, fontSize: f.sizeSmall, color: c.fg },
    tableFirst: { fontWeight: '500' },
    dlist: { borderWidth: StyleSheet.hairlineWidth, borderColor: c.border, borderRadius: t.radius.control, overflow: 'hidden', backgroundColor: c.bg },
    dlistItem: { flexDirection: 'row', alignItems: 'center', gap: 12, paddingHorizontal: 12, paddingVertical: 9, minHeight: t.target },
    dlistPicked: { backgroundColor: c.accentSoft },
    dlistMain: { flex: 1, minWidth: 0 },
    dlistTitle: { fontSize: f.size, fontWeight: '500', color: c.fg },
    steps: { marginBottom: 4, maxWidth: 520 },
    stepsToggle: { alignSelf: 'flex-start', paddingVertical: 2 },
    stepsList: { marginTop: 4, gap: 2, borderLeftWidth: StyleSheet.hairlineWidth, borderLeftColor: c.border, paddingLeft: 12 },
    step: { flexDirection: 'row', gap: 8, paddingVertical: 2 },
    stepMarkBox: { width: 16, height: 18, alignItems: 'center', justifyContent: 'center' },
    stepMark: { fontSize: 12, fontWeight: '700', color: c.muted },
    stepSpin: { transform: [{ scale: 0.6 }] },
    stepDot: { width: 6, height: 6, borderRadius: 3, backgroundColor: c.controlBorder },
    stepTitle: { fontSize: f.sizeSmall, color: c.muted },
    stepTitleOn: { color: c.fg },
    stepDetail: { fontSize: f.sizeTiny, color: c.muted },
    bubbleCard: { backgroundColor: c.bg, borderWidth: StyleSheet.hairlineWidth, borderColor: c.border, borderRadius: t.radius.card, paddingHorizontal: 14, paddingVertical: 12, maxWidth: '100%' },
    inboxTop: { padding: 12, gap: 8, borderBottomWidth: StyleSheet.hairlineWidth, borderBottomColor: c.border },
    inboxRow: { flexDirection: 'row', alignItems: 'center', gap: 12, paddingHorizontal: 16, paddingVertical: 12, borderBottomWidth: StyleSheet.hairlineWidth, borderBottomColor: c.border, minHeight: t.target },
    inboxSide: { alignItems: 'flex-end', gap: 2 },
    inboxAmount: { fontSize: f.size, fontWeight: '600', color: c.fg },
    inboxAmountBig: { fontSize: 22, fontWeight: '600', color: c.fg },
    inboxPage: { padding: 16, gap: 8 },
    inboxBack: { alignSelf: 'flex-start', paddingVertical: 4 },
    inboxBackText: { fontSize: f.size, color: c.accentText, fontWeight: '600' },
    inboxDetail: { gap: 12 },
    inboxH: { fontSize: 18, fontWeight: '600', color: c.fg },
    inboxBox: { borderWidth: StyleSheet.hairlineWidth, borderColor: c.border, borderRadius: t.radius.control, padding: 12 },
    inboxQuote: { borderLeftWidth: 2, borderLeftColor: c.border, paddingLeft: 10, gap: 2 },
    inboxActions: { flexDirection: 'row', flexWrap: 'wrap', gap: 8, alignItems: 'center' },
    edit: { gap: 8, width: 280, maxWidth: '100%' },
    editInput: { minHeight: 72, paddingVertical: 10, textAlignVertical: 'top' },

    jump: { position: 'absolute', right: 16, bottom: 12, minHeight: 36, justifyContent: 'center', paddingHorizontal: 14, borderRadius: 999, backgroundColor: c.bg, borderWidth: StyleSheet.hairlineWidth, borderColor: c.border, shadowColor: '#000', shadowOpacity: 0.15, shadowRadius: 10, shadowOffset: { width: 0, height: 4 }, elevation: 4 },
    jumpNew: { backgroundColor: c.accent, borderColor: c.accent },
    jumpText: { fontSize: f.sizeSmall, fontWeight: '600', color: c.fg },
    jumpTextNew: { color: c.accentFg },
    empty: { flex: 1, alignItems: 'center', justifyContent: 'center', padding: 24 },
    emptyText: { fontSize: f.size, fontWeight: '600', color: c.fg },
    emptyHint: { marginTop: 4, fontSize: f.sizeSmall, color: c.muted, textAlign: 'center' },

    typing: { minHeight: 22, paddingHorizontal: 16, justifyContent: 'center', backgroundColor: c.bg },
    typingText: { fontSize: f.sizeSmall - 1, color: c.muted },
    composer: { paddingHorizontal: 12, paddingTop: 6, paddingBottom: 10, backgroundColor: c.bg },
    composerEnded: { paddingHorizontal: 16, paddingVertical: 12, borderRadius: t.radius.card, backgroundColor: c.surface },
    composerBox: { flexDirection: 'row', alignItems: 'flex-end', gap: 8, paddingLeft: 16, paddingRight: 4, paddingVertical: 4, borderRadius: 24, borderWidth: 1, borderColor: c.controlBorder, backgroundColor: c.bg },
    composerInput: { flex: 1, minHeight: 36, paddingTop: 8, paddingBottom: 8, fontSize: f.size, color: c.fg },
    send: { width: 36, height: 36, borderRadius: 18, alignItems: 'center', justifyContent: 'center', backgroundColor: c.accent },
    sendOff: { opacity: 0.35 },
    sendText: { color: c.accentFg, fontSize: 20, fontWeight: '700', lineHeight: 22 },

    search: { flexDirection: 'row', alignItems: 'center', gap: 8, marginHorizontal: 16, marginVertical: 8, paddingHorizontal: 12, borderRadius: 10, backgroundColor: c.surface },
    searchIcon: { width: 16, height: 16 },
    searchLens: { position: 'absolute', top: 1, left: 1, width: 11, height: 11, borderRadius: 6, borderWidth: 2, borderColor: c.muted },
    searchHandle: { position: 'absolute', top: 11, left: 10, width: 6, height: 2, borderRadius: 1, backgroundColor: c.muted, transform: [{ rotate: '45deg' }] },
    searchInput: { flex: 1, minHeight: 38, fontSize: f.size, color: c.fg },
    conv: { flexDirection: 'row', alignItems: 'center', gap: 12, paddingHorizontal: 16, paddingVertical: 10, minHeight: 72 },
    convSelected: { backgroundColor: c.accentSoft },
    convBody: { flex: 1, minWidth: 0, gap: 2, borderBottomWidth: StyleSheet.hairlineWidth, borderBottomColor: c.border, paddingBottom: 10 },
    convRow: { flexDirection: 'row', alignItems: 'center', gap: 8 },
    convTitle: { flex: 1, fontSize: f.size, fontWeight: '600', color: c.fg },
    convTime: { fontSize: f.sizeTiny + 1, color: c.muted },
    convTimeUnread: { color: c.accentText, fontWeight: '600' },
    convLast: { flex: 1, fontSize: f.sizeSmall + 1, color: c.muted },
    convLastUnread: { color: c.fg },
    unread: { minWidth: 22, height: 22, paddingHorizontal: 6, borderRadius: 11, backgroundColor: c.accent, alignItems: 'center', justifyContent: 'center' },
    unreadText: { color: c.accentFg, fontSize: f.sizeTiny, fontWeight: '700' },

    member: { flexDirection: 'row', alignItems: 'center', gap: 12, paddingHorizontal: 16, minHeight: t.target + 8 },
    memberName: { flex: 1, fontSize: f.size, fontWeight: '500', color: c.fg },
    role: { paddingHorizontal: 8, paddingVertical: 1, borderRadius: 999, borderWidth: 1, borderColor: c.border },
    roleText: { fontSize: f.sizeTiny - 1, fontWeight: '600', color: c.muted },
  });
}
