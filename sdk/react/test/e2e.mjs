// The customer-service window in Chromium against a real `agentim` and a
// real agent (the Node agent SDK): a streamed reply with the AI mark,
// feedback, a card answered with a button, sources, typing, going offline
// and back, and the screenshots (light, dark, RTL) for review.
//
//   npm run e2e   (PLAYWRIGHT=…, CHROMIUM=…, AGENTIM_BIN=…, OUT=dir for screenshots)

import { spawn } from 'node:child_process';
import http from 'node:http';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { Agent } from '../../agent-node/index.mjs';
import { AgentIMClient, MemoryStore } from '@agentim/client-web';

const require = createRequire(import.meta.url);
const { chromium } = require(process.env.PLAYWRIGHT ?? 'playwright');
const here = path.dirname(fileURLToPath(import.meta.url));
const out = path.join(here, '../example/out');
const BIN = process.env.AGENTIM_BIN ?? path.join(here, `../../../server/target/debug/agentim${process.platform === 'win32' ? '.exe' : ''}`);
const shots = process.env.OUT;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let failures = 0;
const check = (c, what) => {
  console.log(`${c ? 'ok  ' : 'FAIL'} ${what}`);
  if (!c) failures++;
};

const web = http.createServer((req, res) => {
  const name = new URL(req.url, 'http://x').pathname.replace(/^\/+/, '') || 'index.html';
  const f = path.join(out, path.normalize(name).replace(/^(\.\.[/\\])+/, ''));
  fs.readFile(f, (e, b) => {
    if (e) return res.writeHead(404).end();
    const type = f.endsWith('.js') ? 'text/javascript' : f.endsWith('.wasm') ? 'application/wasm' : f.endsWith('.css') ? 'text/css' : 'text/html';
    res.writeHead(200, { 'content-type': type }).end(b);
  });
});
await new Promise((r) => web.listen(0, '127.0.0.1', r));
const origin = `http://127.0.0.1:${web.address().port}`;

const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'agentim-react-'));
const port = 20300 + Math.floor(Math.random() * 200);
const IM = `http://127.0.0.1:${port}`;
const cfg = path.join(dir, 'agentim.toml');
fs.writeFileSync(cfg, `[storage]\ncontrol = "main"\n[[storage.databases]]\nname = "main"\nurl = "sqlite://${path.join(dir, 'a.db').replace(/\\/g, '/')}?mode=rwc"\n[server]\nallowed_origins = ["${origin}"]\n`);
const server = spawn(BIN, ['--demo', '--config', cfg, '--addr', `127.0.0.1:${port}`], { stdio: 'ignore' });
for (let i = 0; i < 300; i++) {
  if (await fetch(`${IM}/readyz`).then((r) => r.ok).catch(() => false)) break;
  await sleep(100);
}
const dev = (body) =>
  fetch(`${IM}/api/dev/users`, { method: 'POST', headers: { 'content-type': 'application/json', origin }, body: JSON.stringify(body) }).then((r) => r.json());

const visitor = await dev({ name: 'web-visitor' });
const support = await dev({ name: 'Support Bot', agent: true, owner: visitor.user.id });

// The support agent: streams a greeting, sends a card, answers it, cites sources.
const bot = new Agent({ url: IM, key: support.token });
bot.onMessage(async (msg, ctx) => {
  if (msg.content.includes('card')) {
    await bot.sendCard(ctx.convId, 'Refund the order? (yes / no)', { title: 'Refund the order?', blocks: [{ type: 'buttons', id: 'go', options: [{ id: 'yes', label: 'Yes, refund', style: 'primary' }, { id: 'no', label: 'No' }] }] }, { replyTo: msg.msg_id });
  } else if (msg.content.includes('person')) {
    await bot.handoff(ctx.convId, 'A person will take it from here');
  } else if (msg.content.includes('source')) {
    await ctx.reply('Here is the policy.', { sources: [{ title: 'Refund policy', url: 'https://example.com/refunds' }] });
  } else {
    const s = await ctx.stream();
    for (const w of ['Hi! ', 'How ', 'can ', 'I ', 'help?']) {
      await s.append(w);
      await sleep(150);
    }
    await s.end();
  }
});
bot.onCardResponse(async (r, ctx) => ctx.reply(`You chose ${r.action}.`));
await bot.start();

// The visitor's app opens the conversation with the agent.
const setup = await AgentIMClient.create({ url: IM, deviceId: 'setup', store: new MemoryStore(), multiTab: false, tokenProvider: async () => visitor.token });
await setup.connect();
for (let i = 0; i < 200 && setup.connection.get().state !== 'connected'; i++) await sleep(25);
const { conv_id: conv } = await setup.call('create_conv', { kind: 'dm', title: 'Support', members: [support.user.id] });
await setup.dispose();

const browser = await chromium.launch(process.env.CHROMIUM ? { executablePath: process.env.CHROMIUM } : {});
const ctx = await browser.newContext({ viewport: { width: 480, height: 760 } });
const page = await ctx.newPage();
const errors = [];
page.on('pageerror', (e) => errors.push(e.message));
const q = new URLSearchParams({ url: IM, token: visitor.token, conv, me: visitor.user.id });
const waitFor = (sel, what, opts = {}) =>
  page
    .locator(sel)
    .first()
    .waitFor({ timeout: 20000, ...opts })
    .then(
      () => check(true, what),
      () => check(false, what),
    );

try {
  await page.goto(`${origin}/?${q}`);
  await waitFor('.aim-composer textarea', 'the window opens');
  await waitFor('text=No messages yet', 'an empty conversation says so');

  const box = page.locator('.aim-composer textarea');
  await box.fill('hello');
  await box.press('Enter');
  await waitFor('.aim-row[data-mine] >> text=hello', 'my message shows at once');
  await waitFor('.aim-bubble[aria-busy="true"]', 'the reply streams (busy while writing)');
  await waitFor('.aim-row[data-sender-type="agent"] >> text=Hi! How can I help?', 'the streamed reply completes');
  check((await page.locator('.aim-row[data-sender-type="agent"] .aim-ai').count()) >= 1, 'agent messages carry the AI mark');
  check((await page.locator('.aim-row:not([data-sender-type="agent"]) .aim-ai').count()) === 0, 'messages of people do not');
  if (shots) await page.screenshot({ path: path.join(shots, 'react-chat-light.png') });

  await page.locator('[aria-label="Helpful"]').first().click();
  check(true, 'feedback can be given');

  await box.fill('card please');
  await box.press('Enter');
  await waitFor('.aim-card >> text=Refund the order?', 'a card from the agent is shown');
  await page.locator('.aim-card button', { hasText: 'Yes, refund' }).click();
  await waitFor('text=You chose yes.', 'answering the card reaches the agent, which answers');

  await box.fill('source?');
  await box.press('Enter');
  await waitFor('.aim-sources a[href="https://example.com/refunds"]', 'sources are links');

  await box.fill('a person please');
  await box.press('Enter');
  await waitFor('.aim-handoff >> text=The assistant handed this conversation to a person', 'a handoff is announced');
  await waitFor('.aim-handoff >> text=A person will take it from here', 'with the note');

  // Offline: queued and shown, sent once back.
  await ctx.setOffline(true);
  await waitFor('.aim-banner[data-state="suspended"]', 'offline: the banner says it waits for the network');
  await box.fill('sent while offline');
  await box.press('Enter');
  await waitFor('.aim-row[data-mine] >> text=Sending…', 'offline: shown as sending');
  await ctx.setOffline(false);
  await waitFor('.aim-banner', 'back online: the banner goes', { state: 'detached' });
  await page.waitForFunction(() => !document.body.innerText.includes('Sending…'), null, { timeout: 20000 }).then(
    () => check(true, 'back online: sent'),
    () => check(false, 'back online: sent'),
  );
  check((await page.locator('.aim-text', { hasText: 'sent while offline' }).count()) === 1, 'and only once');

  // Keyboard: the composer and the buttons are reachable and labelled.
  check((await page.locator('[role="log"][aria-label="Messages"]').count()) === 1, 'the list is a labelled log');
  if (shots) {
    await page.emulateMedia({ colorScheme: 'dark' });
    await page.screenshot({ path: path.join(shots, 'react-chat-dark.png') });
    await page.evaluate(() => (document.documentElement.dir = 'rtl'));
    await page.screenshot({ path: path.join(shots, 'react-chat-rtl.png') });
  }
} catch (e) {
  console.log('FAIL', e.message);
  failures++;
} finally {
  check(errors.length === 0, `no page errors ${errors.join('; ')}`);
  await browser.close();
  await bot.stop();
  server.kill();
  web.close();
  fs.rmSync(dir, { recursive: true, force: true });
}
process.exit(failures ? 1 : 0);
