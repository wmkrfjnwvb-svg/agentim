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

/** The calendar day of `t` in the zone, as a day number. */
function dayNumber(t: number, timeZone?: string): number {
  const p = new Intl.DateTimeFormat('en-US', { timeZone, year: 'numeric', month: 'numeric', day: 'numeric' }).formatToParts(t);
  const v = (k: string) => Number(p.find((x) => x.type === k)!.value);
  return Date.UTC(v('year'), v('month') - 1, v('day')) / DAY_MS;
}

function capitalize(s: string, locale?: string): string {
  return s ? s[0].toLocaleUpperCase(locale) + s.slice(1) : s;
}

/** "2:41 PM" (or the locale's own way). */
export function formatTime(t: number, w: When = {}): string {
  return new Intl.DateTimeFormat(w.locale, { timeZone: w.timeZone, hour: 'numeric', minute: '2-digit' }).format(t);
}

/** A day separator: "Today", "Yesterday", "Monday", "Mar 3", "Mar 3, 2025". */
export function formatDay(t: number, w: When = {}): string {
  const now = w.now ?? Date.now();
  const diff = dayNumber(now, w.timeZone) - dayNumber(t, w.timeZone);
  if (diff === 0 || diff === 1) {
    return capitalize(new Intl.RelativeTimeFormat(w.locale, { numeric: 'auto' }).format(-diff, 'day'), w.locale);
  }
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
  if (diff === 1) return capitalize(new Intl.RelativeTimeFormat(w.locale, { numeric: 'auto' }).format(-1, 'day'), w.locale);
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
