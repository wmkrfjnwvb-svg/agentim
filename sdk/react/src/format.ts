// How messages are laid out and dated (docs/12 §3.1), without React or the
// DOM, so React Native lays them out the same way.

/** How dates and times are shown: locale, time zone and "now". */
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

/** One message row of a laid-out list. */
export interface Row<M> {
  /** The message. */
  message: M;
  /** A day separator goes above this message. */
  day?: string;
  /** "New messages" goes above this message. */
  unread?: boolean;
  /** First / last of a run from one sender (name and avatar on the first, time on the last). */
  first: boolean;
  /** Whether it ends a run of messages from the same sender. */
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
export function unreadIndex(messages: { sender_id?: string; key: string; mine?: boolean }[], unread: number, me?: string): number | undefined {
  if (unread <= 0) return undefined;
  let left = unread;
  let at: number | undefined;
  for (let i = messages.length - 1; i >= 0 && left > 0; i--) {
    const m = messages[i];
    if (m.mine || m.sender_id === me || m.key.startsWith('local:')) continue;
    at = i;
    left--;
  }
  // More unread than loaded: from the oldest loaded one.
  return at;
}

/** A line in the timeline that is not a message: a system event, a whisper, progress. */
export interface Notice<N = unknown> {
  /** A stable key for the list. */
  key: string;
  /** Goes after the message with this seq (fractions sort between messages). */
  seq: number;
  /** What the notice says. */
  content: N;
  /** `info` (default), `warning` or `private` (only you can see it). */
  tone?: 'info' | 'warning' | 'private';
}

/** One entry of a laid-out list: a message row or a notice. */
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
  /** `left` (I left), `removed` (someone removed me) or `dissolved` (the conversation was closed). */
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

/** The icon a file gets, from its name and type (docs/02 §2.7). */
export type FileType = 'pdf' | 'doc' | 'sheet' | 'slides' | 'archive' | 'audio' | 'video' | 'code' | 'image' | 'other';

const EXT: Record<string, FileType> = {
  pdf: 'pdf',
  doc: 'doc', docx: 'doc', odt: 'doc', rtf: 'doc', txt: 'doc', md: 'doc', pages: 'doc',
  xls: 'sheet', xlsx: 'sheet', ods: 'sheet', csv: 'sheet', tsv: 'sheet', numbers: 'sheet',
  ppt: 'slides', pptx: 'slides', odp: 'slides', key: 'slides',
  zip: 'archive', rar: 'archive', '7z': 'archive', tar: 'archive', gz: 'archive', tgz: 'archive', bz2: 'archive', xz: 'archive',
  mp3: 'audio', wav: 'audio', m4a: 'audio', aac: 'audio', ogg: 'audio', flac: 'audio', opus: 'audio',
  mp4: 'video', mov: 'video', webm: 'video', mkv: 'video', avi: 'video', m4v: 'video',
  js: 'code', ts: 'code', tsx: 'code', jsx: 'code', py: 'code', rs: 'code', go: 'code', java: 'code', kt: 'code', swift: 'code', c: 'code', h: 'code', cpp: 'code', cs: 'code', rb: 'code', php: 'code', sh: 'code', json: 'code', yaml: 'code', yml: 'code', toml: 'code', xml: 'code', html: 'code', css: 'code', sql: 'code',
  jpg: 'image', jpeg: 'image', png: 'image', gif: 'image', webp: 'image', heic: 'image', svg: 'image', bmp: 'image', tiff: 'image',
};

/**
 * The icon a file gets from its name and type: `pdf`, `doc`, `sheet`, `slides`, `archive`,
 * `audio`, `video`, `code`, `image` or `other`.
 */
export function fileType(name?: string, mime?: string): FileType {
  const ext = (name ?? '').toLowerCase().match(/\.([a-z0-9]+)$/)?.[1];
  if (ext && EXT[ext]) return EXT[ext];
  const m = (mime ?? '').toLowerCase();
  if (m === 'application/pdf') return 'pdf';
  if (m.startsWith('image/')) return 'image';
  if (m.startsWith('audio/')) return 'audio';
  if (m.startsWith('video/')) return 'video';
  if (m.startsWith('text/')) return 'doc';
  if (/zip|compressed|x-tar|gzip/.test(m)) return 'archive';
  if (/spreadsheet|excel/.test(m)) return 'sheet';
  if (/presentation|powerpoint/.test(m)) return 'slides';
  if (/word|opendocument\.text/.test(m)) return 'doc';
  return 'other';
}

/** "12 KB", "3.4 MB": decimal units, as the platforms' own file browsers show them. */
export function formatBytes(n: number, locale?: string): string {
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let i = 0;
  let v = Math.max(0, n);
  while (v >= 1000 && i < units.length - 1) {
    v /= 1000;
    i++;
  }
  const digits = i === 0 || v >= 100 ? 0 : 1;
  return `${new Intl.NumberFormat(locale, { maximumFractionDigits: digits }).format(v)} ${units[i]}`;
}

/** The size an image is shown at: its proportions, at most `max` on each side. */
export function fitBox(width?: number, height?: number, max = 240): { width: number; height: number } {
  if (!width || !height) return { width: max, height: Math.round(max * 0.75) };
  const k = Math.min(max / width, max / height, 1);
  // Very long or tall images keep a usable minimum.
  return { width: Math.max(48, Math.round(width * k)), height: Math.max(48, Math.round(height * k)) };
}

/** A long name with its start and its extension kept: "Quarterly re…port.pdf". */
export function shortName(name: string, max = 32): string {
  if (name.length <= max) return name;
  const dot = name.lastIndexOf('.');
  const ext = dot > 0 && name.length - dot <= 8 ? name.slice(dot) : '';
  const keep = max - ext.length - 1;
  const head = Math.ceil(keep * 0.6);
  return `${name.slice(0, head)}…${name.slice(name.length - ext.length - (keep - head), name.length - ext.length)}${ext}`;
}
