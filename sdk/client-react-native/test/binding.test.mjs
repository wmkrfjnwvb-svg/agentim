// The TypeScript side of the React Native SDK against a fake JSI binding
// (the C++ binding needs a device or simulator, see README): config, the
// token answers, replies and the order of a view's first snapshot, errors,
// timeouts, events to listeners, dispose.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { AgentIMClient, TokenRefused } from '../dist/index.js';

function fakeBinding() {
  const b = {
    calls: [],
    onEvent: null,
    created: null,
    answer: 0,
    create(cfg, dir, onEvent) {
      b.created = { cfg: JSON.parse(cfg), dir };
      b.onEvent = onEvent;
      return b.answer === 0 ? 7 : 0;
    },
    command(h, id, cmd) {
      b.calls.push(['command', h, id, JSON.parse(cmd)]);
      return JSON.parse(cmd).cmd === 'not_json_ok' ? -1 : 0;
    },
    token: (...a) => b.calls.push(['token', ...a]),
    network: (...a) => b.calls.push(['network', ...a]),
    foreground: (...a) => b.calls.push(['foreground', ...a]),
    free: (...a) => b.calls.push(['free', ...a]),
    version: () => '0.1.0',
    emit: (e) => b.onEvent(JSON.stringify(e)),
    last: (kind) => b.calls.filter((c) => c[0] === kind).at(-1),
  };
  globalThis.__agentim = b;
  return b;
}

const tick = () => new Promise((r) => setTimeout(r, 0));

test('without the native module it says so', () => {
  delete globalThis.__agentim;
  assert.throws(() => AgentIMClient.version(), (e) => e.code === 'unavailable');
});

test('config, store and a refused start', () => {
  const b = fakeBinding();
  const c = AgentIMClient.create({ url: 'https://x', deviceId: 'd1', storeDir: '/data', tokenProvider: async () => 't' });
  assert.equal(b.created.cfg.url, 'https://x');
  assert.equal(b.created.cfg.device_id, 'd1');
  assert.equal(b.created.cfg.platform, 'react-native');
  assert.equal(b.created.dir, '/data');
  assert.equal(AgentIMClient.version(), '0.1.0');
  c.dispose();
  b.answer = 1;
  assert.throws(() => AgentIMClient.create({ url: '', deviceId: '', tokenProvider: async () => 't' }), (e) => e.code === 'bad_request');
});

test('tokens: given, failed (try later), refused (stop)', async () => {
  const b = fakeBinding();
  let mode = 'ok';
  const c = AgentIMClient.create({
    url: 'https://x',
    deviceId: 'd',
    tokenProvider: async () => {
      if (mode === 'refuse') throw new TokenRefused('no');
      if (mode === 'fail') throw new Error('down');
      return 'tok';
    },
  });
  b.emit({ type: 'need_token' });
  await tick();
  assert.deepEqual(b.last('token'), ['token', 7, 'tok', false]);
  mode = 'fail';
  b.emit({ type: 'need_token' });
  await tick();
  assert.deepEqual(b.last('token'), ['token', 7, null, false]);
  mode = 'refuse';
  b.emit({ type: 'need_token' });
  await tick();
  assert.deepEqual(b.last('token'), ['token', 7, null, true]);
  c.dispose();
});

test('replies resolve and reject with the code; state changes come first', async () => {
  const b = fakeBinding();
  const c = AgentIMClient.create({ url: 'https://x', deviceId: 'd', tokenProvider: async () => 't' });
  const v = c.messages('c1');
  const [, , snapId] = b.last('command');
  const p = c.send('c1', 'hi');
  const [, , sendId, cmd] = b.last('command');
  assert.deepEqual(cmd, { cmd: 'send', conv_id: 'c1', text: 'hi' });
  // The snapshot reply, then the send's change, then the send's reply.
  b.emit({ type: 'reply', id: snapId, ok: true, result: { messages: [], has_older: true, older_end: null } });
  b.emit({ type: 'messages', conv_id: 'c1', changes: [{ op: 'upsert', message: { key: 'local:1', content: 'hi', order: 1 } }] });
  b.emit({ type: 'reply', id: sendId, ok: true, result: { key: 'local:1' } });
  assert.deepEqual(await p, { key: 'local:1' });
  assert.deepEqual(v.get().map((m) => m.content), ['hi']);
  assert.equal(v.hasOlder, true);

  const q = c.call('get_request', { request_id: 'x' });
  b.emit({ type: 'reply', id: b.last('command')[2], ok: false, error: { code: 'not_found', message: 'request' } });
  await assert.rejects(q, (e) => e.code === 'not_found');
  await assert.rejects(c.command({ cmd: 'not_json_ok' }), (e) => e.code === 'bad_request');
  c.dispose();
});

test('connection, conversations and listeners follow events', () => {
  const b = fakeBinding();
  const c = AgentIMClient.create({ url: 'https://x', deviceId: 'd', tokenProvider: async () => 't' });
  const seen = [];
  const off = c.on('operation', (e) => seen.push(e.result));
  c.on('operation', () => {
    throw new Error('a listener fails');
  });
  b.emit({ type: 'connection', state: 'connected' });
  assert.equal(c.connection.get().state, 'connected');
  b.emit({ type: 'conversations', changes: [{ conversation: { conv_id: 'a', last_message: { created_at: 1 } } }, { conversation: { conv_id: 'b', last_message: { created_at: 2 } } }, { conversation: { conv_id: 'gone', left: true } }] });
  assert.deepEqual(c.conversations.get().map((x) => x.conv_id), ['b', 'a']);
  b.emit({ type: 'operation', result: 'done' });
  off();
  b.emit({ type: 'operation', result: 'failed' });
  assert.deepEqual(seen, ['done']);
  c.setNetwork(false);
  c.setNetwork(true);
  assert.deepEqual(b.last('network'), ['network', 7, true, 2]);
  c.setForeground(false);
  assert.deepEqual(b.last('foreground'), ['foreground', 7, false]);
  c.dispose();
});

test('timeouts, and dispose (twice) fails what waits and frees once', async () => {
  const b = fakeBinding();
  const c = AgentIMClient.create({ url: 'https://x', deviceId: 'd', commandTimeoutMs: 20, tokenProvider: async () => 't' });
  await assert.rejects(c.debugState(), (e) => e.code === 'timeout' && e.retryable);
  const waiting = c.connect();
  c.dispose();
  c.dispose();
  await assert.rejects(waiting, (e) => e.code === 'closed');
  assert.equal(b.calls.filter((x) => x[0] === 'free').length, 1);
  await assert.rejects(c.connect(), (e) => e.code === 'closed');
  c.setNetwork(true);
  assert.equal(b.calls.filter((x) => x[0] === 'network').length, 0);
});
