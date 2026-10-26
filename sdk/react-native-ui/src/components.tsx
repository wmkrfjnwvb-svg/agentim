// React Native components (docs/12 §3.3): the same names, props, texts,
// replaceable parts and layout rules as @agentimhq/react, drawn with
// native views. The AI mark is drawn by the list around each message,
// outside what can be replaced (D36, EU AI Act art. 50).

import { createContext, memo, useCallback, useContext, useEffect, useMemo, useRef, useState, type ComponentType, type ReactNode } from 'react';
import { ActivityIndicator, FlatList, Pressable, StyleSheet, Switch, Text, TextInput, View, type ListRenderItem, type NativeScrollEvent, type NativeSyntheticEvent, type ViewStyle } from 'react-native';
import type { Message } from '@agentimhq/client-web/view';
import {
  AgentIMProvider,
  avatarColor,
  defaultStrings,
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
} from '@agentimhq/react/hooks';
import { AgentIMThemeProvider, useTheme, type Theme } from './theme';
import type { Colors } from './tokens';

export interface MessageProps {
  message: Message;
  mine: boolean;
  first: boolean;
  last: boolean;
  time?: string;
  strings: Strings;
  onRetry(): void;
  onDiscard(): void;
  onFeedback(rating: 'up' | 'down'): void;
  onRespond(blockId: string, r: { action?: string; values?: Record<string, unknown> }): void;
  onEdit?(text: string): void;
  onRecall?(): void;
  onStop?(): void;
}

export interface ConversationItemProps {
  conversation: any;
  selected: boolean;
  onSelect(): void;
  strings: Strings;
  time?: string;
  me?: string;
}

export interface Components {
  Message: ComponentType<MessageProps>;
  ConversationItem: ComponentType<ConversationItemProps>;
  Avatar: ComponentType<{ id?: string; name?: string; kind?: string; size?: number }>;
  Empty: ComponentType<{ text: string; hint?: string }>;
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
  return m.sender_id && m.sender_id === me ? `${strings.you}: ${text}` : text;
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

function CardForm({ block, strings, done, onRespond }: { block: any; strings: Strings; done: boolean; onRespond: MessageProps['onRespond'] }) {
  const s = useStyles();
  const t = useTheme();
  const fields: any[] = block.fields ?? [];
  const [values, setValues] = useState<Record<string, unknown>>(() => Object.fromEntries(fields.map((f) => [f.id, f.type === 'checkbox' ? false : f.type === 'select' ? (f.options?.[0] ?? '') : ''])));
  const set = (id: string, v: unknown) => setValues((x) => ({ ...x, [id]: v }));
  return (
    <View style={s.cardForm}>
      {fields.map((f) => (
        <View key={f.id} style={f.type === 'checkbox' ? s.checkRow : undefined}>
          <Text style={s.fieldLabel}>{f.label}</Text>
          {f.type === 'checkbox' ? (
            <Switch value={!!values[f.id]} onValueChange={(v) => set(f.id, v)} accessibilityLabel={f.label} trackColor={{ true: t.colors.success, false: t.colors.controlBorder }} />
          ) : f.type === 'select' ? (
            <View style={s.chips} accessibilityRole="radiogroup" accessibilityLabel={f.label}>
              {(f.options ?? []).map((o: string) => (
                <Pressable key={o} onPress={() => set(f.id, o)} accessibilityRole="radio" accessibilityState={{ checked: values[f.id] === o }} style={[s.chip, values[f.id] === o && s.chipOn]}>
                  <Text style={[s.chipText, values[f.id] === o && s.chipTextOn]}>{o}</Text>
                </Pressable>
              ))}
            </View>
          ) : (
            <TextInput
              value={String(values[f.id] ?? '')}
              onChangeText={(v) => set(f.id, v)}
              keyboardType={f.type === 'number' ? 'numeric' : 'default'}
              maxLength={f.max}
              accessibilityLabel={f.label}
              style={s.input}
              placeholderTextColor={t.colors.muted}
            />
          )}
        </View>
      ))}
      <Button
        label={block.submit ?? strings.submit}
        kind="primary"
        disabled={done}
        onPress={() => onRespond(block.id, { values: Object.fromEntries(fields.map((f) => [f.id, f.type === 'number' ? Number(values[f.id]) : values[f.id]])) })}
      />
    </View>
  );
}

function Card({ m, mine, strings, onRespond }: { m: Message; mine: boolean; strings: Strings; onRespond: MessageProps['onRespond'] }) {
  const s = useStyles();
  const p = m.payload ?? {};
  const [answered, setAnswered] = useState<ReadonlySet<string>>(new Set());
  const done = (id: string) => mine || answered.has(id);
  const answer: MessageProps['onRespond'] = (b, r) => {
    setAnswered((a) => new Set(a).add(b));
    onRespond(b, r);
  };
  return (
    <View style={s.card}>
      {p.title ? <Text style={s.cardTitle}>{p.title}</Text> : null}
      {(p.blocks ?? []).map((b: any, i: number) => {
        if (b.type === 'text')
          return (
            <Text key={i} style={s.text}>
              {b.text}
            </Text>
          );
        if (b.type === 'buttons')
          return (
            <View key={b.id} style={s.cardButtons}>
              {(b.options ?? []).map((x: any) => (
                <Button key={x.id} label={x.label} kind={x.style} disabled={done(b.id)} onPress={() => answer(b.id, { action: x.id })} />
              ))}
            </View>
          );
        if (b.type === 'form') return <CardForm key={b.id} block={b} strings={strings} done={done(b.id)} onRespond={answer} />;
        return null;
      })}
    </View>
  );
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

function DefaultMessage({ message: m, mine, first, last, time, strings, onRetry, onDiscard, onFeedback, onRespond, onEdit, onRecall, onStop }: MessageProps) {
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
  return (
    <View style={mine ? s.alignEnd : s.alignStart}>
      <Pressable
        onLongPress={onEdit || onRecall ? () => setActions((a) => !a) : undefined}
        accessibilityHint={onEdit || onRecall ? 'Long press for actions' : undefined}
        accessibilityState={{ busy: streaming }}
        testID={`bubble-${m.key}`}
        style={[s.bubble, corners, mine ? s.bubbleMine : agent ? s.bubbleAi : null, m.status === 'sending' && s.sending, failed && s.failedBubble]}
      >
        {m.kind === 'agentim.card' ? (
          <Card m={m} mine={mine} strings={strings} onRespond={onRespond} />
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
}

/** One cell of the list: a notice, or a message with its day line, name, avatar and AI mark. */
const Cell = memo(function Cell({ item, convId }: RowProps) {
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
  const mine = m.sender_id === me || m.key.startsWith('local:');
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
 * older ones; Stream Chat does the same), in runs per sender with day
 * separators, notices and "New messages"; a button brings you back down.
 */
export function MessageList({ convId, notices = [] }: { convId: string; notices?: Notice<ReactNode>[] }) {
  const client = useClient();
  const s = useStyles();
  const { strings, components, me, when } = useUi();
  const { messages, hasOlder, olderEnd, loadingOlder, loadOlder } = useMessages(convId);
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
  // Inverted: the newest first.
  const items = useMemo(() => interleave(layout(messages, when(), unreadFrom >= 0 ? unreadFrom : undefined), notices).reverse(), [messages, notices, unreadFrom, when]);
  const isMine = (m: Message) => m.sender_id === me || m.key.startsWith('local:');

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

  const renderItem: ListRenderItem<Item<Message, ReactNode>> = useCallback(({ item }) => <Cell item={item} convId={convId} />, [convId]);
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
export function Composer({ convId }: { convId: string }) {
  const { send } = useSend(convId);
  const { notify } = useTyping(convId);
  const s = useStyles();
  const t = useTheme();
  const { strings } = useUi();
  const [text, setText] = useState('');
  const can = !!text.trim();
  const submit = () => {
    const v = text.trim();
    if (!v) return;
    setText('');
    void send(v);
  };
  return (
    <View style={s.composer}>
      <View style={s.composerBox}>
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
export function Chat({ convId, title, subtitle, notices }: { convId: string; title?: string; subtitle?: string; notices?: Notice<ReactNode>[] }) {
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
      <TypingIndicator convId={convId} />
      <Composer convId={convId} />
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
    bubbleAi: { backgroundColor: c.bubbleAi },
    sending: { opacity: 0.7 },
    failedBubble: { borderWidth: 2, borderColor: c.danger },
    text: { fontSize: f.size, lineHeight: Math.round(f.size * f.line), color: c.bubbleFg },
    textMine: { color: c.bubbleMineFg },
    cursor: { color: c.aiText },
    recalled: { backgroundColor: 'transparent', borderWidth: 1, borderStyle: 'dashed', borderColor: c.border },
    recalledText: { fontSize: f.sizeSmall, fontStyle: 'italic', color: c.muted },
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
