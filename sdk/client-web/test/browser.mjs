// The browser SDK in Chromium (Playwright): a real IndexedDB cache
// (encrypted), one connection shared by two tabs (Web Locks +
// BroadcastChannel), the second tab taking over when the first closes,
// and going offline and back. Starts its own servers.
//
//   npm run build && PLAYWRIGHT=… node test/browser.mjs   [AGENTIM_BIN=…]

import { spawn } from 'node:child_process';
import http from 'node:http';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
const require = createRequire(import.meta.url);
const { chromium } = require(process.env.PLAYWRIGHT ?? 'playwright');

const here = path.dirname(fileURLToPath(import.meta.url));
const dist = path.join(here, '..', 'dist');
const BIN = process.env.AGENTIM_BIN ?? path.join(here, `../../../server/target/debug/agentim${process.platform === 'win32' ? '.exe' : ''}`);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let failures = 0;
const check = (c, what) => {
  console.log(`${c ? 'ok  ' : 'FAIL'} ${what}`);
  if (!c) failures++;
};

// The page and the SDK, from a static server (another origin than the IM server).
const page = `<!doctype html><meta charset=utf-8><script type=module>
import * as sdk from './index.js';
window.sdk = sdk;
window.ready = true;
</script>`;
const web = http.createServer((req, res) => {
  const f = req.url === '/' ? null : path.join(dist, path.normalize(req.url).replace(/^(\.\.[/\\])+/, ''));
  if (!f) return res.writeHead(200, { 'content-type': 'text/html' }).end(page);
  fs.readFile(f, (e, b) => {
    if (e) return res.writeHead(404).end();
    const type = f.endsWith('.js') ? 'text/javascript' : f.endsWith('.wasm') ? 'application/wasm' : 'application/octet-stream';
    res.writeHead(200, { 'content-type': type }).end(b);
  });
});
await new Promise((r) => web.listen(0, '127.0.0.1', r));
const origin = `http://127.0.0.1:${web.address().port}`;

const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'agentim-browser-'));
const port = 19500 + Math.floor(Math.random() * 300);
const IM = `http://127.0.0.1:${port}`;
const cfg = path.join(dir, 'agentim.toml');
fs.writeFileSync(cfg, `[storage]\ncontrol = "main"\n[[storage.databases]]\nname = "main"\nurl = "sqlite://${path.join(dir, 'a.db').replace(/\\/g, '/')}?mode=rwc"\n[server]\nallowed_origins = ["${origin}"]\n`);
// In its own folder: files go to ./data/files there.
const server = spawn(BIN, ['--demo', '--config', cfg, '--addr', `127.0.0.1:${port}`], { stdio: 'ignore', cwd: dir });
for (let i = 0; i < 300; i++) {
  if (await fetch(`${IM}/readyz`).then((r) => r.ok).catch(() => false)) break;
  await sleep(100);
}
const person = (name) =>
  fetch(`${IM}/api/dev/users`, { method: 'POST', headers: { 'content-type': 'application/json', origin }, body: JSON.stringify({ name }) }).then((r) => r.json());
const alice = await person('web-alice');
const bob = await person('web-bob');

const browser = await chromium.launch(process.env.CHROMIUM ? { executablePath: process.env.CHROMIUM } : {});
const ctx = await browser.newContext();
const errors = [];
async function tab() {
  const p = await ctx.newPage();
  p.on('pageerror', (e) => errors.push(e.message));
  await p.goto(origin);
  await p.waitForFunction(() => window.ready);
  await p.evaluate(
    async ([url, token]) => {
      window.client = await window.sdk.AgentIMClient.create({ url, deviceId: 'laptop', storeName: 'alice', tokenProvider: async () => token });
      window.views = {};
      window.texts = (c) => (window.views[c] ??= window.client.messages(c)).get().map((m) => `${m.content}:${m.status}`);
    },
    [IM, alice.token],
  );
  return p;
}
const waitIn = (p, f, arg, what) =>
  p.waitForFunction(f, arg, { timeout: 20000 }).then(
    () => check(true, what),
    () => check(false, what),
  );

try {
  const a = await tab();
  await a.evaluate(() => window.client.connect());
  await waitIn(a, () => window.client.connection.get().state === 'connected', null, 'the first tab connects');
  const conv = await a.evaluate(async (bobId) => (await window.client.call('create_conv', { kind: 'group', title: 'tabs', members: [bobId] })).conv_id, bob.user.id);
  await a.evaluate((c) => window.client.send(c, 'from tab A'), conv);
  await waitIn(a, (c) => window.texts(c).includes('from tab A:sent'), conv, 'a message from tab A is sent');

  // A second tab follows the first: same state, same messages, one connection.
  const b = await tab();
  await waitIn(b, () => window.client.connection.get().state === 'connected', null, 'tab B sees the shared connection');
  await b.evaluate((c) => window.texts(c), conv);
  await waitIn(b, (c) => window.texts(c).includes('from tab A:sent'), conv, 'tab B shows the messages');
  await b.evaluate((c) => window.client.send(c, 'from tab B'), conv);
  await waitIn(a, (c) => window.texts(c).includes('from tab B:sent'), conv, 'tab B sends through tab A');
  const ws = await b.evaluate(() => performance.getEntriesByType('resource').filter((r) => r.name.startsWith('ws')).length);
  check(ws === 0, 'tab B opened no connection of its own');

  // The cache is encrypted.
  const plain = await b.evaluate(
    () =>
      new Promise((ok) => {
        const r = indexedDB.open('alice-cache');
        r.onsuccess = () => {
          const g = r.result.transaction('messages').objectStore('messages').getAll();
          g.onsuccess = () => ok(JSON.stringify(g.result.map((x) => Object.keys(x))) + String(g.result.length));
        };
      }),
  );
  check(!plain.includes('from tab'), `no plain text in IndexedDB (${plain})`);

  // Tab A closes: tab B takes over the connection and the queue.
  // Sent while no tab leads (the lock not yet handed over): kept, sent once.
  await a.close();
  await b.evaluate((c) => window.client.send(c, 'after takeover'), conv);
  await waitIn(b, () => window.client.connection.get().state === 'connected', null, 'tab B takes over and connects');
  await waitIn(b, (c) => window.texts(c).filter((t) => t === 'after takeover:sent').length === 1, conv, 'tab B sends on its own connection, once');

  // Offline: queued; back online: sent once.
  await ctx.setOffline(true);
  await waitIn(b, () => window.client.connection.get().state === 'suspended', null, 'offline: suspended');
  await b.evaluate((c) => window.client.send(c, 'while offline'), conv);
  await waitIn(b, (c) => window.texts(c).includes('while offline:sending'), conv, 'shown as sending while offline');
  await ctx.setOffline(false);
  await waitIn(b, (c) => window.texts(c).filter((t) => t === 'while offline:sent').length === 1, conv, 'sent once back online');

  // An image from another origin: a thumbnail made here, PUTs across
  // origins with their ETag, progress, and the other side reads it.
  const shot = await b.evaluate(async (c) => {
    const cv = new OffscreenCanvas(1000, 600);
    const g = cv.getContext('2d');
    g.fillStyle = '#36c';
    g.fillRect(0, 0, 1000, 600);
    g.fillStyle = '#fff';
    g.fillRect(100, 100, 300, 200);
    const png = await cv.convertToBlob({ type: 'image/png' });
    const seen = [];
    const v = window.client.messages(c);
    const off = v.subscribe(() => {
      for (const m of v.get()) if (m.upload) seen.push(m.status);
    });
    const r = await window.client.sendFile(c, new File([png], 'chart.png', { type: 'image/png' }), { caption: 'chart' });
    for (let i = 0; i < 200 && !v.get().some((m) => m.kind === 'image' && m.status === 'sent' && !m.key.startsWith('local:')); i++) await new Promise((ok) => setTimeout(ok, 50));
    off();
    const m = v.get().find((x) => x.kind === 'image' && !x.key.startsWith('local:'));
    const t = await window.client.fileUrl(c, m.payload.file_id, 'thumb');
    const tb = await createImageBitmap(await (await fetch(t.url)).blob());
    return { key: r.key, seen: [...new Set(seen)], payload: m.payload, thumb: [tb.width, tb.height], variant: t.variant };
  }, conv);
  check(shot.seen.includes('uploading'), `shown as uploading first (${shot.seen})`);
  check(shot.payload.thumb === true && shot.payload.width === 1000 && shot.payload.caption === 'chart', `sent as an image with its thumbnail (${JSON.stringify(shot.payload)})`);
  check(shot.variant === 'thumb' && shot.thumb.join('x') === '320x192', `the thumbnail is 320 x 192 (${shot.thumb})`);
  // Picked while not connected, the page reloaded: the kept file goes up after.
  await b.evaluate(() => window.client.close());
  await waitIn(b, () => window.client.connection.get().state === 'closed', null, 'closed');
  await b.evaluate((c) => window.client.sendFile(c, new Blob([new Uint8Array(300000).fill(7)]), { name: 'data.bin', clientMsgId: 'file-offline' }), conv);
  await waitIn(b, (c) => window.client.messages(c).get().some((m) => m.client_msg_id === 'file-offline' && m.status === 'uploading'), conv, 'a file waits while not connected');
  await b.reload();
  await b.waitForFunction(() => window.ready);
  await b.evaluate(
    async ([url, token]) => {
      window.client = await window.sdk.AgentIMClient.create({ url, deviceId: 'laptop', storeName: 'alice', tokenProvider: async () => token });
      window.views = {};
      window.texts = (c) => (window.views[c] ??= window.client.messages(c)).get().map((m) => `${m.content}:${m.status}`);
      await window.client.connect();
    },
    [IM, alice.token],
  );
  await waitIn(b, (c) => window.texts(c).includes('[File] data.bin:sent'), conv, 'after a reload the kept file is uploaded and sent');
  // A reload keeps the cache: the messages show before connecting.
  await b.reload();
  await b.waitForFunction(() => window.ready);
  await b.evaluate(
    async ([url, token]) => {
      window.client = await window.sdk.AgentIMClient.create({ url, deviceId: 'laptop', storeName: 'alice', tokenProvider: async () => token });
      window.views = {};
      window.texts = (c) => (window.views[c] ??= window.client.messages(c)).get().map((m) => `${m.content}:${m.status}`);
    },
    [IM, alice.token],
  );
  await b.evaluate((c) => window.texts(c), conv);
  await waitIn(b, (c) => window.texts(c).includes('while offline:sent'), conv, 'after a reload, from the cache, offline');
} catch (e) {
  console.log('FAIL', e.message);
  failures++;
} finally {
  check(errors.length === 0, `no page errors ${errors.join('; ')}`);
  await browser.close();
  server.kill();
  web.close();
  fs.rmSync(dir, { recursive: true, force: true });
}
process.exit(failures ? 1 : 0);
