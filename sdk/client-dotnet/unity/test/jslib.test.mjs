// The Unity WebGL bridge (Runtime/Plugins/WebGL/AgentIM.jslib) without
// Unity: the file is loaded the way Unity's emscripten build loads it
// (mergeInto, the makeDynCall macro, UTF-8 strings in the wasm heap), and
// the C# side is played by this test (commands by id, need_token answered
// with agentim_client_token). Runs against a real agentim.
//
//   AGENTIM_BIN=… node --test unity/test/

import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = (p) => fileURLToPath(new URL(p, import.meta.url));
const BIN = process.env.AGENTIM_BIN ?? here(`../../../../server/target/debug/agentim${process.platform === 'win32' ? '.exe' : ''}`);
const PORT = 20500 + Math.floor(Math.random() * 300);
const URL_ = `http://127.0.0.1:${PORT}`;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let server;
let dir;

// ---- a minimal emscripten runtime, as the jslib sees it
const HEAPU8 = new Uint8Array(1 << 22);
let top = 16;
const freed = [];
const enc = new TextEncoder();
const dec = new TextDecoder();
const _malloc = (n) => {
  const p = top;
  top += (n + 7) & ~7;
  if (top > HEAPU8.length) throw new Error('heap full');
  return p;
};
const _free = (p) => freed.push(p);
const lengthBytesUTF8 = (s) => enc.encode(s).length;
const stringToUTF8 = (s, p, max) => {
  const b = enc.encode(s).subarray(0, max - 1);
  HEAPU8.set(b, p);
  HEAPU8[p + b.length] = 0;
};
const UTF8ToString = (p) => {
  let e = p;
  while (HEAPU8[e]) e++;
  return dec.decode(HEAPU8.subarray(p, e));
};
const cstr = (s) => {
  if (s === null) return 0;
  const n = lengthBytesUTF8(s) + 1;
  const p = _malloc(n);
  stringToUTF8(s, p, n);
  return p;
};
// The function table: C# callbacks registered by index.
const table = [null];
const dynCallVii = (i) => (a, b) => table[i](a, b);

const lib = {};
function loadJslib() {
  let src = readFileSync(here('../Runtime/Plugins/WebGL/AgentIM.jslib'), 'utf8');
  // Unity's build replaces this macro with a call through the table.
  src = src.replace(/\{\{\{\s*makeDynCall\('vii',\s*'([^']+)'\)\s*\}\}\}/g, 'dynCallVii($1)');
  const LibraryManager = { library: {} };
  const mergeInto = (target, obj) => Object.assign(target, obj);
  const autoAddDeps = () => {};
  new Function('LibraryManager', 'mergeInto', 'autoAddDeps', 'UTF8ToString', 'lengthBytesUTF8', 'stringToUTF8', '_malloc', '_free', 'dynCallVii', src)(
    LibraryManager,
    mergeInto,
    autoAddDeps,
    UTF8ToString,
    lengthBytesUTF8,
    stringToUTF8,
    _malloc,
    _free,
    dynCallVii,
  );
  // `$AIM` is linked in as a global named AIM.
  globalThis.AIM = LibraryManager.library.$AIM;
  for (const [k, v] of Object.entries(LibraryManager.library)) if (typeof v === 'function') lib[k] = v;
}

// ---- the C# side, as Client.cs does it
class CSharpClient {
  constructor(config, token) {
    this.events = [];
    this.pending = new Map();
    this.next = 0;
    this.token = token;
    const cb = table.push((ctx, ptr) => this.onEvent(UTF8ToString(ptr))) - 1;
    this.h = lib.agentim_client_new(cstr(JSON.stringify(config)), 0, cb, 42);
  }
  onEvent(json) {
    const e = JSON.parse(json);
    this.events.push(e);
    if (e.type === 'need_token') {
      this.token().then(
        (t) => lib.agentim_client_token(this.h, cstr(t), 0),
        (err) => lib.agentim_client_token(this.h, 0, err === 'refused' ? 1 : 0),
      );
    } else if (e.type === 'reply') {
      const p = this.pending.get(e.id);
      this.pending.delete(e.id);
      if (e.ok) p?.ok(e.result);
      else p?.no(e.error);
    }
  }
  command(cmd) {
    const id = ++this.next;
    return new Promise((ok, no) => {
      this.pending.set(id, { ok, no });
      assert.equal(lib.agentim_client_command(this.h, id, cstr(JSON.stringify(cmd))), 0);
    });
  }
  state() {
    return this.events.filter((e) => e.type === 'connection').at(-1)?.state;
  }
}

async function until(what, f, ms = 15000) {
  const end = Date.now() + ms;
  while (!(await f())) {
    if (Date.now() > end) assert.fail(`timed out: ${what}`);
    await sleep(25);
  }
}

let n = 0;
async function person(name) {
  const r = await fetch(`${URL_}/api/dev/users`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', origin: URL_ },
    body: JSON.stringify({ name: `${name}${++n}` }),
  }).then((r) => r.json());
  return { id: r.user.id, name: r.user.name, token: r.token };
}

before(async () => {
  globalThis.AgentIMWeb = await import('../../../client-web/dist/index.js');
  loadJslib();
  dir = mkdtempSync(join(tmpdir(), 'agentim-webgl-'));
  server = spawn(BIN, ['--demo', '--db', join(dir, 'a.db'), '--addr', `127.0.0.1:${PORT}`], { stdio: 'ignore' });
  await until('server', () => fetch(`${URL_}/readyz`).then((r) => r.ok).catch(() => false), 30000);
});
after(() => {
  server?.kill();
  rmSync(dir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
});

test('bad arguments are refused like the native library', () => {
  const cb = table.push(() => {}) - 1;
  assert.equal(lib.agentim_client_new(cstr('not json'), 0, cb, 0), 0);
  assert.equal(lib.agentim_client_new(cstr('{"url":""}'), 0, cb, 0), 0);
  assert.equal(lib.agentim_client_command(999, 1, cstr('{}')), -1);
  assert.match(UTF8ToString(lib.agentim_version()), /webgl/);
});

test('two people talk through the bridge; replies, views and errors', async () => {
  const alice = await person('alice');
  const bob = await person('bob');
  const a = new CSharpClient({ url: URL_, device_id: 'a1' }, async () => alice.token);
  const b = new CSharpClient({ url: URL_, device_id: 'b1' }, async () => bob.token);
  // Commands sent before the browser client is ready wait in order.
  assert.equal(lib.agentim_client_command(a.h, 1000, cstr('[]')), -1, 'not an object');
  await Promise.all([a.command({ cmd: 'connect' }), b.command({ cmd: 'connect' })]);
  await until('connected', () => a.state() === 'connected' && b.state() === 'connected');
  const { conv_id } = await a.command({ cmd: 'call', op: 'create_conv', args: { kind: 'group', title: 'webgl', members: [bob.id] } });
  await b.command({ cmd: 'messages', conv_id });
  const sent = await a.command({ cmd: 'send', conv_id, text: 'hello from Unity 👋' });
  assert.match(sent.key, /^local:/);
  await until('bob gets it as a messages event', () =>
    b.events.some((e) => e.type === 'messages' && e.conv_id === conv_id && (e.changes ?? []).some((c) => c.message?.content === 'hello from Unity 👋' && c.message.status === 'sent')),
  );
  await assert.rejects(a.command({ cmd: 'call', op: 'get_request', args: { request_id: 'nope' } }), (e) => e.code === 'not_found');
  await assert.rejects(a.command({ cmd: 'no_such_command' }), (e) => e.code === 'bad_request');
  // Every string the bridge handed over was freed after the call.
  assert.ok(freed.length >= a.events.length + b.events.length);
  // Free: nothing arrives after it.
  lib.agentim_client_free(a.h);
  const seen = a.events.length;
  await b.command({ cmd: 'send', conv_id, text: 'after free' });
  await sleep(500);
  assert.equal(a.events.length, seen);
  lib.agentim_client_free(a.h); // twice is fine
  lib.agentim_client_free(b.h);
});

test('a refused token stops the client', async () => {
  const c = new CSharpClient({ url: URL_, device_id: 'r1' }, async () => {
    throw 'refused';
  });
  await c.command({ cmd: 'connect' });
  await until('failed', () => c.state() === 'failed');
  assert.equal(c.events.filter((e) => e.type === 'connection').at(-1).reason, 'unauthenticated');
  lib.agentim_client_free(c.h);
});
