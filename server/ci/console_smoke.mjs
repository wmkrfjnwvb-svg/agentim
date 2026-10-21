// Browser test of the console (docs/12 §4): set up with the one-time
// link, log in, create keys, read a conversation with a reason (audited),
// decide a report, change client permissions, look at the deployment.
// Starts its own server on a fresh database.
//
//   (cd console && npm ci && npm run build) && cargo build
//   PLAYWRIGHT=... node ci/console_smoke.mjs   [BIN=target/debug/agentim]

import { spawn } from 'node:child_process';
import crypto from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';
const require = createRequire(import.meta.url);
const { chromium } = require(process.env.PLAYWRIGHT ?? 'playwright');

const bin = (process.env.BIN ?? 'target/debug/agentim') + (process.platform === 'win32' ? '.exe' : '');
const out = process.env.OUT ?? 'screenshots';
fs.mkdirSync(out, { recursive: true });
const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'console-'));
const port = 8000 + Math.floor(Math.random() * 1000);
const base = `http://127.0.0.1:${port}`;
let failures = 0;
const check = (cond, what) => {
  console.log(`${cond ? 'ok  ' : 'FAIL'} ${what}`);
  if (!cond) failures++;
};

const server = spawn(bin, ['--db', path.join(dir, 'c.db'), '--addr', `127.0.0.1:${port}`], { stdio: ['ignore', 'pipe', 'inherit'] });
const setupLink = await new Promise((resolve, reject) => {
  let buf = '';
  server.stdout.on('data', (d) => {
    buf += d;
    const m = buf.match(/(http:\/\/\S+\/console\/setup\?token=\S+)/);
    if (m) resolve(m[1]);
  });
  server.on('exit', () => reject(new Error('server exited')));
  setTimeout(() => reject(new Error('no setup link')), 20000);
});

const b64 = (s) => Buffer.from(s).toString('base64url');
const jwt = (kid, secret, sub) => {
  const now = Math.floor(Date.now() / 1000);
  const h = b64(JSON.stringify({ alg: 'HS256', typ: 'JWT', kid }));
  const p = b64(JSON.stringify({ sub, iat: now, exp: now + 600 }));
  const s = crypto.createHmac('sha256', secret).update(`${h}.${p}`).digest('base64url');
  return `${h}.${p}.${s}`;
};
const openApi = async (key, method, p, body) => {
  const r = await fetch(`${base}${p}`, {
    method,
    headers: { authorization: `Bearer ${key}`, 'content-type': 'application/json' },
    body: body && JSON.stringify(body),
  });
  return { status: r.status, body: await r.json().catch(() => null) };
};
/// One request over the client protocol as a person.
const asPerson = (token, msg) =>
  new Promise((resolve, reject) => {
    const ws = new WebSocket(`${base.replace('http', 'ws')}/ws?token=${token}&device=d1`);
    ws.onmessage = (e) => {
      const f = JSON.parse(e.data);
      if (f.type === 'welcome') ws.send(JSON.stringify({ ...msg, rid: 1 }));
      if (f.type === 'res' && f.rid === 1) {
        ws.close();
        resolve(f);
      }
    };
    ws.onerror = () => reject(new Error('ws error'));
  });

const browser = await chromium.launch(process.env.CHROMIUM ? { executablePath: process.env.CHROMIUM } : {});
const errors = [];
const page = await browser.newPage({ viewport: { width: 1360, height: 860 } });
page.on('pageerror', (e) => errors.push(e.message));
page.on('dialog', (d) => d.accept(d.type() === 'prompt' ? 'checking a loop' : undefined));
try {
  // Set up: the first owner.
  await page.goto(setupLink);
  await page.fill('input[name=name]', 'root');
  await page.fill('input[name=password]', 'correct horse battery');
  await page.fill('input[name=again]', 'correct horse battery');
  await page.click('button:has-text("Create owner")');
  await page.waitForSelector('h1:has-text("Overview")');
  check(true, 'setup creates the owner and opens the overview');
  check(!page.url().includes('token='), 'the setup token leaves the address bar');
  await page.screenshot({ path: `${out}/console-overview.png` });
  // The link works once.
  const again = await fetch(`${base}/admin/v1/setup`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ token: new URL(setupLink).searchParams.get('token'), name: 'x', password: 'correct horse battery' }),
  });
  check(again.status === 403, 'the setup link works once');

  // Keys: a signing key and an open API key, each shown once.
  await page.click('nav a:has-text("Keys")');
  await page.fill('input[placeholder=Name] >> nth=0', 'web');
  await page.click('button:has-text("Create signing key")');
  await page.waitForSelector('[data-testid=secret]');
  const signing = await page.locator('[data-testid=secret] code').innerText();
  const kid = await page.locator('table >> nth=0 >> code').first().innerText();
  check(signing.startsWith('sks_') && kid.startsWith('sk_'), 'a signing key is created and its secret shown');
  await page.fill('input[placeholder=Name] >> nth=1', 'backend');
  for (const s of ['users:write', 'conversations:write', 'messages:write']) await page.check(`label:has-text("${s}") input`);
  await page.click('button:has-text("Create API key")');
  await page.waitForFunction(() => document.querySelector('[data-testid=secret] code')?.textContent?.startsWith('ak_'));
  const apiKey = await page.locator('[data-testid=secret] code').innerText();
  check(apiKey.startsWith('ak_'), 'an API key is created and shown once');
  await page.screenshot({ path: `${out}/console-keys.png` });

  // Some activity: two people and a message, then a report.
  for (const u of ['alice', 'bob']) await openApi(apiKey, 'PUT', `/v1/users/${u}`, { display_name: u });
  const conv = await openApi(apiKey, 'POST', '/v1/conversations', { as: 'alice', kind: 'group', title: 'Team room', members: ['bob'] });
  check(conv.status === 201 || conv.status === 200, `a conversation through the open API (${conv.status})`);
  const convId = conv.body?.conv_id ?? conv.body?.id;
  const sent = await openApi(apiKey, 'POST', `/v1/conversations/${convId}/messages`, { as: 'alice', text: 'buy cheap pills here' });
  const msgId = sent.body?.msg_id;
  check(!!msgId, `a message (${sent.status})`);
  const rep = await asPerson(jwt(kid, signing, 'bob'), { op: 'report', target_kind: 'message', target_id: msgId, category: 'spam' });
  check(rep.ok === true, `bob reports it from his app (${JSON.stringify(rep.error ?? '')})`);

  // Conversations: metadata, then content with a reason.
  await page.click('nav a:has-text("Conversations")');
  await page.click('tr:has-text("Team room")');
  await page.waitForSelector('.drawer h3:has-text("Members")');
  check(!(await page.locator('.drawer').innerText()).includes('cheap pills'), 'metadata shows no content');
  await page.fill('.drawer input[name=reason]', 'report from bob');
  await page.click('.drawer button:has-text("Read messages")');
  await page.waitForSelector('.drawer .message');
  check((await page.locator('.drawer .messages').innerText()).includes('cheap pills'), 'content after giving a reason');
  await page.screenshot({ path: `${out}/console-conversation.png` });
  await page.click('.drawer button:has-text("Close")');

  // Reports: decide with a statement of reasons.
  await page.click('nav a:has-text("Reports")');
  await page.click('tbody tr >> nth=0');
  await page.selectOption('.drawer select[name=action]', 'remove_content');
  await page.fill('.drawer textarea[name=reason]', 'Spam is not allowed.');
  await page.click('.drawer button:has-text("Decide")');
  await page.waitForFunction(() => !document.querySelector('.drawer button') || ![...document.querySelectorAll('.drawer button')].some((b) => b.textContent === 'Decide'));
  check((await page.locator('.drawer pre').innerText()).includes('remove_content'), 'the report is decided');
  await page.click('.drawer button:has-text("Close")');

  // Client permissions.
  await page.click('nav a:has-text("Client permissions")');
  await page.check('input[name=create_agents]');
  await page.click('button:has-text("Save")');
  await page.waitForFunction(() => document.querySelector('button')?.textContent !== null && [...document.querySelectorAll('button')].some((b) => b.textContent === 'Save' && b.disabled));
  const hello = await asPerson(jwt(kid, signing, 'alice'), { op: 'hello' });
  check(hello.me?.permissions?.create_agents === true, 'people may now create agents in their app');

  // The audit log has the reasoned read and the decisions.
  await page.click('nav a:has-text("Audit log")');
  await page.fill('input[placeholder^="Action"]', 'admin.');
  await page.waitForSelector('td code:has-text("admin.conversations.read")');
  const audit = await page.locator('table').innerText();
  check(audit.includes('report from bob'), 'the read is audited with its reason');
  check(audit.includes('admin.reports.resolve') && audit.includes('admin.settings.clients'), 'decisions are audited');
  await page.screenshot({ path: `${out}/console-audit.png` });

  // Deployment status and the admins page.
  await page.click('nav a:has-text("Status")');
  await page.waitForSelector('td:has-text("sqlite")');
  check(true, 'the deployment status lists the database');
  await page.screenshot({ path: `${out}/console-status.png` });
  await page.click('nav a:has-text("Admins")');
  await page.fill('.card >> nth=0 >> input[placeholder=Name]', 'mod');
  await page.fill('input[placeholder="Initial password"]', 'another long password');
  await page.selectOption('.card >> nth=0 >> select >> nth=0', 'moderator');
  await page.click('button:has-text("Add admin")');
  await page.waitForSelector('td:text-is("mod")');

  // A moderator sees only what the role allows.
  await page.click('button:has-text("Log out")');
  await page.waitForSelector('h1:has-text("AgentIM console")');
  await page.fill('input[name=name]', 'mod');
  await page.fill('input[name=password]', 'another long password');
  await page.click('button:has-text("Log in")');
  await page.waitForSelector('nav');
  const nav = await page.locator('nav').innerText();
  check(nav.includes('Reports') && !nav.includes('Keys') && !nav.includes('Status'), 'a moderator sees reports, not keys or deployment');
} catch (e) {
  console.log('FAIL', e.message);
  failures++;
  await page.screenshot({ path: `${out}/console-failure.png` }).catch(() => {});
} finally {
  check(errors.length === 0, `no page errors ${errors.join('; ')}`);
  await browser.close();
  server.kill();
}
process.exit(failures ? 1 : 0);
