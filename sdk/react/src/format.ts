// How messages are laid out and dated (docs/12 §3.1), without React or the
// DOM, so React Native lays them out the same way.

export interface When {
  /** BCP 47 locale; undefined is the user's own (browser or system). */
  locale?: string;
  /** IANA time zone; undefined is the user's own. */
  timeZone?: string;
  /** "Now", for tests and screenshots. */
  now?: number;
}

const GROUP_MS = 5 * 60 * 1000;
const DAY_MS = 86_400_000;

/**
 * The calendar day of `t` in the zone, as a day number. Where `Intl` cannot
 * split a date into parts (some JavaScript engines in apps), the device's
 * own zone is used.
 */
function dayNumber(t: number, timeZone?: string): number {
  try {
    const p = new Intl.DateTimeFormat('en-US', { timeZone, year: 'numeric', month: 'numeric', day: 'numeric' }).formatToParts(t);
    const v = (k: string) => Number(p.find((x) => x.type === k)!.value);
    const n = Date.UTC(v('year'), v('month') - 1, v('day')) / DAY_MS;
    if (Number.isFinite(n)) return n;
  } catch {}
  const d = new Date(t);
  return Date.UTC(d.getFullYear(), d.getMonth(), d.getDate()) / DAY_MS;
}

function capitalize(s: string, locale?: string): string {
  return s ? s[0].toLocaleUpperCase(locale) + s.slice(1) : s;
}

/**
 * "Today" or "Yesterday" in the locale. Hermes (React Native's engine)
 * has no Intl.RelativeTimeFormat: there it is English.
 */
function relativeDay(daysAgo: 0 | 1, locale?: string): string {
  const R = (Intl as any).RelativeTimeFormat;
  if (typeof R === 'function') {
    try {
      return capitalize(new R(locale, { numeric: 'auto' }).format(-daysAgo, 'day'), locale);
    } catch {}
  }
  return daysAgo === 0 ? 'Today' : 'Yesterday';
}

/** "2:41 PM" (or the locale's own way). */
export function formatTime(t: number, w: When = {}): string {
  return new Intl.DateTimeFormat(w.locale, { timeZone: w.timeZone, hour: 'numeric', minute: '2-digit' }).format(t);
}

/** A day separator: "Today", "Yesterday", "Monday", "Mar 3", "Mar 3, 2025". */
export function formatDay(t: number, w: When = {}): string {
  const now = w.now ?? Date.now();
  const diff = dayNumber(now, w.timeZone) - dayNumber(t, w.timeZone);
  if (diff === 0 || diff === 1) return relativeDay(diff, w.locale);
  if (diff > 1 && diff < 7) return new Intl.DateTimeFormat(w.locale, { timeZone: w.timeZone, weekday: 'long' }).format(t);
  const sameYear = new Intl.DateTimeFormat('en-US', { timeZone: w.timeZone, year: 'numeric' });
  const opts: Intl.DateTimeFormatOptions = { timeZone: w.timeZone, month: 'short', day: 'numeric' };
  if (sameYear.format(t) !== sameYear.format(now)) opts.year = 'numeric';
  return new Intl.DateTimeFormat(w.locale, opts).format(t);
}

/** In a conversation list: the time today, "Yesterday", the weekday this week, else a short date. */
export function formatShort(t: number, w: When = {}): string {
  const now = w.now ?? Date.now();
  const diff = dayNumber(now, w.timeZone) - dayNumber(t, w.timeZone);
  if (diff === 0) return formatTime(t, w);
  if (diff === 1) return relativeDay(1, w.locale);
  if (diff > 1 && diff < 7) return new Intl.DateTimeFormat(w.locale, { timeZone: w.timeZone, weekday: 'short' }).format(t);
  return new Intl.DateTimeFormat(w.locale, { timeZone: w.timeZone, year: '2-digit', month: 'numeric', day: 'numeric' }).format(t);
}

export interface Row<M> {
  message: M;
  /** A day separator goes above this message. */
  day?: string;
  /** "New messages" goes above this message. */
  unread?: boolean;
  /** First / last of a run from one sender (name and avatar on the first, time on the last). */
  first: boolean;
  last: boolean;
}

/** What the layout reads: `sender_id`, `sender_type`, `created_at` (ms) and `state`. */
type Laid = Record<string, any>;

/**
 * Rows for a message list: a run is one sender's messages less than five
 * minutes apart on one day (iMessage, Slack); a recalled message ends a
 * run. `unreadFrom` is the index of the first unread message, if any.
 */
export function layout<M extends Laid>(messages: M[], w: When = {}, unreadFrom?: number): Row<M>[] {
  const rows: Row<M>[] = [];
  let prevDay: number | undefined;
  messages.forEach((m, i) => {
    const t = m.created_at;
    const day = t === undefined ? prevDay : dayNumber(t, w.timeZone);
    const prev = messages[i - 1];
    const newDay = day !== undefined && day !== prevDay;
    const unread = i === unreadFrom;
    const joins =
      !!prev &&
      !newDay &&
      !unread &&
      prev.sender_id === m.sender_id &&
      prev.sender_type === m.sender_type &&
      prev.state !== 'recalled' &&
      m.state !== 'recalled' &&
      (t === undefined || prev.created_at === undefined || t - prev.created_at < GROUP_MS);
    if (joins) rows[rows.length - 1].last = false;
    rows.push({ message: m, day: newDay ? formatDay(t!, w) : undefined, unread: unread || undefined, first: !joins, last: true });
    if (day !== undefined) prevDay = day;
  });
  return rows;
}

/** Up to two initials: "Ada Lovelace" → "AL", "aria" → "A". */
export function initials(name: string | undefined): string {
  const words = (name ?? '').trim().split(/\s+/).filter(Boolean);
  if (!words.length) return '?';
  const first = [...words[0]][0];
  const last = words.length > 1 ? [...words[words.length - 1]][0] : '';
  return (first + last).toLocaleUpperCase();
}

/** The same colour for the same id, everywhere (an index into the avatar palette). */
export function avatarColor(id: string | undefined, n: number): number {
  let h = 0;
  for (const c of id ?? '') h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return h % n;
}

/**
 * Where "New messages" goes when a conversation opens with `unread` unread:
 * above the unread-th message from the end that is not mine.
 */
export function unreadIndex(messages: { sender_id?: string; key: string }[], unread: number, me?: string): number | undefined {
  if (unread <= 0) return undefined;
  let left = unread;
  let at: number | undefined;
  for (let i = messages.length - 1; i >= 0 && left > 0; i--) {
    const m = messages[i];
    if (m.sender_id === me || m.key.startsWith('local:')) continue;
    at = i;
    left--;
  }
  // More unread than loaded: from the oldest loaded one.
  return at;
}

/** A line in the timeline that is not a message: a system event, a whisper, progress. */
export interface Notice<N = unknown> {
  key: string;
  /** Goes after the message with this seq (fractions sort between messages). */
  seq: number;
  content: N;
  tone?: 'info' | 'warning' | 'private';
}

export type Item<M, N> = { row: Row<M>; notice?: undefined } | { notice: Notice<N>; row?: undefined; /** A day separator goes above this notice. */ day?: string };

/**
 * Notices merged into the rows by seq: a notice goes before the first
 * message with a later seq (local messages, without one, come last) and
 * ends the run around it.
 */
export function interleave<M extends Laid, N>(rows: Row<M>[], notices: Notice<N>[]): Item<M, N>[] {
  const pending = [...notices].sort((a, b) => a.seq - b.seq);
  const out: Item<M, N>[] = [];
  let i = 0;
  let prev: Row<M> | undefined;
  for (const r of rows) {
    const seq = typeof r.message.created_seq === 'number' ? r.message.created_seq : Infinity;
    let split = false;
    while (i < pending.length && pending[i].seq < seq) {
      // The day starts above its first notice, not between it and the message.
      out.push({ notice: pending[i++], day: split ? undefined : r.day });
      split = true;
    }
    const row = split ? { ...r, first: true, day: undefined } : r;
    if (split && prev) prev.last = true;
    const copy = { ...row };
    out.push({ row: copy });
    prev = copy;
  }
  while (i < pending.length) out.push({ notice: pending[i++] });
  return out;
}

/** How this person's part in a conversation ended, from the client core. */
export interface Ended {
  reason: 'left' | 'removed' | 'dissolved';
  /** The name of who removed them or closed it (not when it was themselves). */
  by?: string;
}

/**
 * Whether this person left, was removed from or saw the conversation
 * closed: it stays readable, and nothing can be sent (L4).
 */
export function ended(conv: { ended?: any; members?: any[] } | undefined, me?: string): Ended | undefined {
  const e = conv?.ended;
  if (!e || typeof e !== 'object') return undefined;
  const reason = e.reason === 'removed' || e.reason === 'dissolved' ? e.reason : 'left';
  const by = e.by && e.by !== me ? conv?.members?.find((m) => m.user_id === e.by)?.name : undefined;
  // Removing oneself is leaving.
  if (reason === 'removed' && e.by && e.by === me) return { reason: 'left' };
  return by ? { reason, by } : { reason };
}
