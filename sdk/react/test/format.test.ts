// The layout and date rules (docs/12 §3.1), shared with React Native, and
// the contrast of every token pair.

import { describe, expect, it } from 'vitest';
import { avatarColor, formatDay, formatShort, formatTime, initials, interleave, layout, unreadIndex } from '../src/format.js';
// @ts-expect-error plain JS module
import { contrast, contrastFailures, tokens } from '../scripts/tokens.mjs';

const at = (iso: string) => Date.parse(iso);
const now = at('2026-10-26T20:00:00Z'); // a Monday
const us = { locale: 'en-US', timeZone: 'UTC', now };
// Some ICU versions put a narrow no-break space before AM/PM.
const n = (s: string) => s.replace(/\u202f/g, ' ');

describe('dates', () => {
  it('formats times and days the American way in en-US', () => {
    expect(n(formatTime(at('2026-10-26T14:41:00Z'), us))).toBe('2:41 PM');
    expect(n(formatTime(at('2026-10-26T00:05:00Z'), us))).toBe('12:05 AM');
    expect(formatDay(at('2026-10-26T01:00:00Z'), us)).toBe('Today');
    expect(formatDay(at('2026-10-25T23:59:00Z'), us)).toBe('Yesterday');
    expect(formatDay(at('2026-10-23T12:00:00Z'), us)).toBe('Friday');
    expect(formatDay(at('2026-10-20T12:00:00Z'), us)).toBe('Tuesday');
    expect(formatDay(at('2026-10-19T12:00:00Z'), us)).toBe('Oct 19');
    expect(formatDay(at('2026-03-03T12:00:00Z'), us)).toBe('Mar 3');
    expect(formatDay(at('2025-03-03T12:00:00Z'), us)).toBe('Mar 3, 2025');
  });

  it('follows the locale and the time zone', () => {
    expect(formatDay(at('2026-10-26T01:00:00Z'), { ...us, locale: 'zh-CN' })).toBe('今天');
    expect(formatDay(at('2026-10-26T01:00:00Z'), { ...us, locale: 'de-DE' })).toBe('Heute');
    expect(n(formatTime(at('2026-10-26T14:41:00Z'), { ...us, locale: 'en-GB' }))).toBe('14:41');
    // 02:00 UTC on Monday is still Sunday evening in Los Angeles.
    expect(formatDay(at('2026-10-26T02:00:00Z'), { ...us, timeZone: 'America/Los_Angeles' })).toBe('Yesterday');
  });

  it('works where Intl has no RelativeTimeFormat or formatToParts (Hermes in React Native)', () => {
    const R = (Intl as any).RelativeTimeFormat;
    const parts = Intl.DateTimeFormat.prototype.formatToParts;
    try {
      (Intl as any).RelativeTimeFormat = undefined;
      expect(formatDay(at('2026-10-26T01:00:00Z'), us)).toBe('Today');
      expect(formatDay(at('2026-10-25T12:00:00Z'), { ...us, locale: 'de-DE' })).toBe('Yesterday');
      expect(formatShort(at('2026-10-25T12:00:00Z'), us)).toBe('Yesterday');
      (Intl.DateTimeFormat.prototype as any).formatToParts = undefined;
      // The device's zone then: noon is the same day in any zone the tests run in.
      const noon = new Date(2026, 9, 26, 12).getTime();
      expect(formatDay(noon, { locale: 'en-US', now: noon })).toBe('Today');
      expect(layout([{ sender_id: 'a', created_at: noon }], { now: noon })[0].day).toBe('Today');
    } finally {
      (Intl as any).RelativeTimeFormat = R;
      Intl.DateTimeFormat.prototype.formatToParts = parts;
    }
  });

  it('keeps the conversation list short', () => {
    expect(n(formatShort(at('2026-10-26T09:30:00Z'), us))).toBe('9:30 AM');
    expect(n(formatShort(at('2026-10-25T09:30:00Z'), us))).toBe('Yesterday');
    expect(n(formatShort(at('2026-10-23T09:30:00Z'), us))).toBe('Fri');
    expect(n(formatShort(at('2026-03-03T09:30:00Z'), us))).toBe('3/3/26');
  });
});

describe('layout', () => {
  const m = (key: string, sender: string, iso?: string, extra: any = {}) => ({ key, sender_id: sender, sender_type: 'human', created_at: iso ? at(iso) : undefined, ...extra });
  const shape = (rows: any[]) => rows.map((r) => `${r.day ? `[${r.day}]` : ''}${r.unread ? '[new]' : ''}${r.message.key}${r.first ? '<' : ''}${r.last ? '>' : ''}`).join(' ');

  it('makes runs per sender, under five minutes apart, on one day', () => {
    const rows = layout(
      [
        m('a', 'bob', '2026-10-25T10:00:00Z'),
        m('b', 'bob', '2026-10-25T10:04:59Z'),
        m('c', 'bob', '2026-10-25T10:10:00Z'),
        m('d', 'amy', '2026-10-25T10:10:30Z'),
        m('e', 'amy', '2026-10-26T09:00:00Z'),
        m('f', 'amy'),
      ],
      us,
    );
    expect(shape(rows)).toBe('[Yesterday]a< b> c<> d<> [Today]e< f>');
  });

  it('ends a run at a deleted message, a change of sender type and the unread line', () => {
    const t = '2026-10-26T10:00:00Z';
    expect(shape(layout([m('a', 'bob', t), m('b', 'bob', t, { state: 'recalled' }), m('c', 'bob', t)], us))).toBe('[Today]a<> b<> c<>');
    expect(shape(layout([m('a', 'x', t), m('b', 'x', t, { sender_type: 'agent' })], us))).toBe('[Today]a<> b<>');
    expect(shape(layout([m('a', 'bob', t), m('b', 'bob', t), m('c', 'bob', t)], us, 1))).toBe('[Today]a<> [new]b< c>');
    expect(layout([], us)).toEqual([]);
  });

  it('places the unread line above the unread-th message that is not mine', () => {
    const list = [m('a', 'bob'), m('b', 'me'), m('c', 'bob'), m('local:x', 'me'), m('d', 'amy')];
    expect(unreadIndex(list, 0, 'me')).toBeUndefined();
    expect(unreadIndex(list, 1, 'me')).toBe(4);
    expect(unreadIndex(list, 2, 'me')).toBe(2);
    expect(unreadIndex(list, 3, 'me')).toBe(0);
    // More unread than loaded: from the oldest loaded one.
    expect(unreadIndex(list, 50, 'me')).toBe(0);
    expect(unreadIndex([m('b', 'me')], 2, 'me')).toBeUndefined();
  });
});

describe('interleave', () => {
  const m = (key: string, seq?: number) => ({ key, sender_id: 'bob', sender_type: 'human', created_seq: seq });
  const shape = (items: any[]) => items.map((i) => (i.notice ? `(${i.notice.key})` : `${i.row.message.key}${i.row.first ? '<' : ''}${i.row.last ? '>' : ''}`)).join(' ');
  it('puts notices between messages by seq, ends runs, and keeps local messages last', () => {
    const rows = layout([m('a', 1), m('b', 2), m('c', 4), m('local:d')]);
    expect(shape(interleave(rows, []))).toBe('a< b c local:d>');
    expect(shape(interleave(rows, [{ key: 'x', seq: 3, content: '' }, { key: 'y', seq: 1.5, content: '' }, { key: 'z', seq: 50, content: '' }]))).toBe('a<> (y) b<> (x) c<> (z) local:d<>');
    expect(shape(interleave([], [{ key: 'only', seq: 1, content: '' }]))).toBe('(only)');
    // A new day goes above the notices that open it.
    const dayRows = layout([{ ...m('a', 5), created_at: Date.parse('2026-10-26T10:00:00Z') }], us);
    const items: any[] = interleave(dayRows, [{ key: 'j', seq: 2, content: '' }, { key: 'k', seq: 3, content: '' }]);
    expect(items.map((i) => i.day ?? i.row?.day ?? '-')).toEqual(['Today', '-', '-']);
    // The rows given are not changed.
    expect(rows.map((r) => r.first)).toEqual([true, false, false, false]);
  });
});

describe('avatars', () => {
  it('uses up to two initials and a stable colour', () => {
    expect(initials('Ada Lovelace')).toBe('AL');
    expect(initials('  aria ')).toBe('A');
    expect(initials('Mary Ann Smith')).toBe('MS');
    expect(initials('')).toBe('?');
    expect(initials(undefined)).toBe('?');
    expect(initials('李 雷')).toBe('李雷');
    const c = avatarColor('u_123', 8);
    expect(c).toBe(avatarColor('u_123', 8));
    for (const id of ['', 'a', 'u_1', 'u_2', 'x'.repeat(500)]) {
      const i = avatarColor(id, 8);
      expect(i >= 0 && i < 8 && Number.isInteger(i)).toBe(true);
    }
  });
});

describe('contrast (WCAG 2.1 AA)', () => {
  it('every token pair meets its minimum in both themes', () => {
    expect(contrastFailures()).toEqual([]);
  });

  it('the check catches a pair that does not', () => {
    expect(contrast('#ffffff', '#000000')).toBeCloseTo(21, 5);
    expect(contrast('#777777', '#ffffff')).toBeLessThan(4.5);
    const bad = structuredClone(tokens);
    bad.color.dark.muted = '#3f3f46';
    bad.avatar.push('#facc15');
    const f = contrastFailures(bad);
    expect(f.some((x: string) => x.startsWith('dark: muted on bg'))).toBe(true);
    expect(f.some((x: string) => x.startsWith('avatar #facc15'))).toBe(true);
  });
});
