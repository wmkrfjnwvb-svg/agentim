// A conversation view applies each `messages` event in one step: a reset
// replaces the list without showing it empty in between (B12).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { Messages } from '../dist/index.js';

const msg = (key, order, content) => ({ key, order, content, status: 'sent' });
const seen = (v) => {
  const lists = [];
  v.subscribe((l) => lists.push(l.map((m) => m.key)));
  lists.length = 0; // the current value, given at once
  return lists;
};

test('a reset replaces the view in one update, never empty in between', () => {
  const v = new Messages('c1', null);
  v.applyEvent({ changes: [{ op: 'upsert', message: msg('a', 1) }, { op: 'upsert', message: msg('b', 2) }] });
  const lists = seen(v);
  v.applyEvent({ reset: true, changes: [{ op: 'upsert', message: msg('b', 2) }, { op: 'upsert', message: msg('c', 3) }] });
  assert.deepEqual(lists, [['b', 'c']], 'one update, with what the reset holds');
  assert.deepEqual(v.get().map((m) => m.key), ['b', 'c']);
});

test('a reset with nothing empties the view once; ordinary events keep the rest', () => {
  const v = new Messages('c1', null);
  v.applyEvent({ changes: [{ op: 'upsert', message: msg('a', 1) }] });
  const lists = seen(v);
  v.applyEvent({ changes: [{ op: 'upsert', message: msg('b', 2) }, { op: 'remove', key: 'a' }] });
  v.applyEvent({ reset: true });
  assert.deepEqual(lists, [['b'], []]);
});
