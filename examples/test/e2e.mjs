// Runs each example with the scripted model and walks its main flow in
// Chromium (docs/en/design/developer-experience.md), and times "start → first agent reply".
//
//   npm test     (AGENTIM_BIN=…, PLAYWRIGHT=…, CHROMIUM=…)

import { spawn } from 'node:child_process';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const { chromium } = require(process.env.PLAYWRIGHT ?? 'playwright');
const root = path.join(path.dirname(fileURLToPath(import.meta.url)), '..');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let failures = 0;
const check = (c, what) => {
  console.log(`${c ? 'ok  ' : 'FAIL'} ${what}`);
  if (!c) failures++;
};
// Acceptance (docs/en/design/developer-experience.md): the first agent reply within this long of starting.
const FIRST_REPLY_MS = 30_000;

async function example(name, port) {
  const env = { ...process.env, PORT: String(port) };
  delete env.ANTHROPIC_API_KEY; // always the scripted model here
  const p = spawn(process.execPath, [path.join(root, name, 'start.mjs')], { env, stdio: ['ignore', 'pipe', 'inherit'] });
  let out = '';
  p.stdout.on('data', (d) => (out += d));
  for (let i = 0; i < 600 && !out.includes('Press Ctrl+C'); i++) await sleep(50);
  if (!out.includes('Press Ctrl+C')) throw new Error(`${name} did not start:\n${out}`);
  return { web: `http://127.0.0.1:${port}`, stop: () => p.kill('SIGTERM') };
}

const browser = await chromium.launch(process.env.CHROMIUM ? { executablePath: process.env.CHROMIUM } : {});
const errors = [];
async function page(url) {
  const ctx = await browser.newContext({ viewport: { width: 1280, height: 860 } });
  const p = await ctx.newPage();
  p.on('pageerror', (e) => errors.push(`${url}: ${e.message}`));
  await p.goto(url);
  await p.locator('.aim-composer textarea').waitFor({ timeout: 20000 });
  return p;
}
const say = async (p, text) => {
  const box = p.locator('.aim-composer textarea');
  await box.fill(text);
  await box.press('Enter');
};
// SHOTS=<dir>: screenshots of the main moments, to look at the pages.
const shot = (p, name) => (process.env.SHOTS ? p.screenshot({ path: path.join(process.env.SHOTS, `${name}.png`) }) : undefined);
const seen = (p, sel, what, timeout = 20000) =>
  p.locator(sel).first().waitFor({ timeout }).then(() => check(true, what), () => check(false, what));

// ------------------------------------------------------------ after-sales
const only = process.env.ONLY ? process.env.ONLY.split(',') : null;
const run = (name) => !only || only.includes(name);
if (run('aftersales')) {
  const t0 = Date.now();
  const ex = await example('aftersales', 3600);
  try {
    const maya = await page(`${ex.web}/`);
    const sam = await page(`${ex.web}/desk`);
    const bot = '.aim-row[data-sender-type="agent"]';
    await say(maya, 'Where is order 1001?');
    // The order as a data card (show_card), its look-up and the card as steps.
    await seen(maya, `${bot} .aim-card >> text=Wireless headphones`, 'aftersales: the order shows as a data card');
    const first = Date.now() - t0;
    check(first < FIRST_REPLY_MS, `aftersales: start → first agent reply in ${(first / 1000).toFixed(1)} s`);
    check((await maya.locator(`${bot} .aim-fact >> text=$25.00`).count()) === 1, 'aftersales: the total in the viewer’s currency format');
    check((await maya.locator(`${bot} .aim-tone[data-tone="success"] >> text=Delivered`).count()) === 1, 'aftersales: the status as a badge');
    await seen(maya, `${bot} >> text=Here are the details`, 'aftersales: then one short sentence');
    await seen(maya, '.aim-steps >> text=2 steps', 'aftersales: the steps fold into one line when done');
    check((await maya.locator(`${bot} .aim-ai`).count()) >= 1, 'aftersales: the replies carry the AI mark');
    // Up to $30: the reason in a form, then the customer confirms it.
    await say(maya, 'Please refund order 1001');
    await seen(maya, '.aim-card-form >> text=Reason', 'aftersales: a refund asks why, with a form');
    check((await sam.locator('.aim-card-form').count()) === 0 || (await sam.locator('.aim-card-form button[disabled]').count()) > 0, 'aftersales: the form is the customer’s to fill in');
    await maya.locator('.aim-card-form button', { hasText: 'Request refund' }).click();
    await seen(maya, '.aim-field-error >> text=Required', 'aftersales: a required field is checked before sending');
    await maya.locator('.aim-card-form select').selectOption('Arrived late');
    await maya.locator('.aim-card-form button', { hasText: 'Request refund' }).click();
    await seen(maya, '.aim-form-summary >> text=Arrived late', 'aftersales: sent, the form shows what was sent');
    await seen(maya, '.aim-approval >> text=createRefund', 'aftersales: then the $25 refund asks the customer to confirm');
    await shot(maya, 'aftersales-customer');
    await sleep(500);
    check((await sam.locator('.aim-inbox-row', { hasText: 'Refunds an order' }).count()) === 0, 'aftersales: the manager has nothing to approve for it');
    await maya.locator('.aim-approval button', { hasText: 'Approve' }).click();
    await seen(maya, `${bot} >> text=Done: $25 for order 1001`, 'aftersales: approved, the refund ran and the assistant says so');
    // Above $30: the manager approves in the inbox; the customer can't.
    await say(maya, 'Please refund order 1002');
    await seen(maya, '.aim-card-form >> nth=-1', 'aftersales: the second refund asks why too');
    await maya.locator('.aim-card-form select').last().selectOption('Damaged');
    await maya.locator('.aim-card-form button', { hasText: 'Request refund' }).last().click();
    await seen(maya, `${bot} >> text=need a manager to approve`, 'aftersales: a $120 refund waits for a manager');
    await seen(sam, '.aim-inbox-row >> text=Refunds an order', 'aftersales: it is in the manager’s inbox');
    await sam.locator('.aim-inbox-row', { hasText: 'Refunds an order' }).click();
    const detail = sam.locator('.aim-inbox-detail');
    check((await detail.locator('.aim-fact', { hasText: 'amount' }).innerText()).includes('120'), 'aftersales: the detail shows the amount');
    check((await detail.innerText()).includes('amount > 30'), 'aftersales: and why it needs approval');
    check((await maya.locator('.aim-approval[data-state="pending"]').count()) === 0, 'aftersales: the customer gets no card for it');
    await shot(sam, 'aftersales-desk');
    await detail.locator('button', { hasText: 'Approve' }).click();
    await seen(sam, '.aim-inbox-detail >> text=Approved by you', 'aftersales: the inbox says it is decided');
    await seen(maya, `${bot} >> text=Done: $120 for order 1002`, 'aftersales: approved by the manager, refunded');
    await seen(maya, '.aim-step[data-state="done"] >> text=Refunds an order', 'aftersales: the waiting step is done');
    // A rule lets address changes through.
    await say(maya, 'Change the address of order 1002 to 5 Main St, Boston');
    await seen(maya, `${bot} >> text=now ships to 5 Main St, Boston`, 'aftersales: an address change runs at once');
    // A person.
    await say(maya, 'I want to talk to a person');
    await seen(maya, `${bot} >> text=passed you to a person`, 'aftersales: asked for a person, the assistant hands over');
    await seen(sam, '[data-testid="status"] >> text=You have it', 'aftersales: the desk has the conversation');
    await seen(sam, '.aim-inbox-row >> text=The customer asked', 'aftersales: the hand-off is in the inbox, with its note');
    await say(sam, 'Hi Maya, Sam here.');
    await seen(maya, 'text=Sam here', 'aftersales: the customer sees the manager');
    const before = await maya.locator(bot).count();
    await say(maya, 'Thanks!');
    await sleep(1000);
    check((await maya.locator(bot).count()) === before, 'aftersales: the assistant stays quiet while a person has it');
    await sam.locator('.aim-inbox-row', { hasText: 'The customer asked' }).click();
    await sam.locator('.aim-inbox-detail button', { hasText: 'Hand back' }).click();
    await seen(sam, '[data-testid="status"] >> text=The assistant is answering', 'aftersales: handed back from the inbox');
  } finally {
    ex.stop();
  }
}

// ------------------------------------------------------------ advisor
if (run('advisor')) {
  const ex = await example('advisor', 3610);
  try {
    const ava = await page(`${ex.web}/`);
    const chris = await page(`${ex.web}/?as=chris`);
    await say(chris, 'Markets are down, should I sell everything?');
    await seen(ava, '[data-testid="note"] >> text=Check the risk profile', 'advisor: the aide passes Ava a private note');
    await seen(ava, '[data-testid="drafts"] .aim-inbox-row', 'advisor: the aide’s reply waits as a draft in Ava’s inbox');
    await ava.locator('[data-testid="drafts"] .aim-inbox-row').first().click();
    check((await ava.locator('.aim-inbox-draft').inputValue()).includes('lock in losses'), 'advisor: the draft is the aide’s text');
    await sleep(500);
    check((await chris.locator('text=lock in losses').count()) === 0, 'advisor: Chris doesn’t see the draft before approval');
    check((await chris.locator('text=Check the risk profile').count()) === 0, 'advisor: Chris never sees the private note');
    await ava.locator('.aim-inbox-draft').fill('Please don’t sell in a hurry: it would lock in losses. Can we talk Thursday at 10? — Ava');
    await shot(ava, 'advisor');
    await ava.locator('.aim-inbox-detail button', { hasText: 'Send edited' }).click();
    await seen(chris, '.aim-row[data-sender-type="agent"] >> text=Thursday at 10', 'advisor: approved (edited), Chris sees it');
    await say(chris, 'What about bitcoin?');
    await seen(ava, '[data-testid="note"] >> text=at most 5%', 'advisor: a second note');
    await ava.locator('.aim-inbox-back').click().catch(() => {});
    await seen(ava, '[data-testid="drafts"] .aim-inbox-row:not([data-done]) >> text=small part', 'advisor: a second draft');
    await ava.locator('[data-testid="drafts"] .aim-inbox-row:not([data-done])', { hasText: 'small part' }).click();
    await ava.locator('.aim-inbox-detail button', { hasText: 'Discard' }).click();
    await sleep(800);
    check((await chris.locator('text=only a small part').count()) === 0, 'advisor: a discarded draft is never sent');
    await say(ava, 'Chris, I’ll call you about it tomorrow.');
    await seen(chris, 'text=call you about it tomorrow', 'advisor: Ava writes directly');
    await sleep(800);
    check((await ava.locator('[data-testid="drafts"] .aim-inbox-row:not([data-done])').count()) === 0, 'advisor: the aide doesn’t answer the advisor');
  } finally {
    ex.stop();
  }
}

// ------------------------------------------------------------ task room
if (run('tasks')) {
  const ex = await example('tasks', 3620);
  try {
    const pat = await page(`${ex.web}/`);
    await say(pat, 'a short post about our v2 launch');
    await seen(pat, 'text=Plan: 1) research, 2) write', 'tasks: the lead plans');
    await seen(pat, 'text=Facts: v2 syncs 3x faster', 'tasks: a worker claimed the research and reported');
    await seen(pat, 'text=Draft post', 'tasks: a worker claimed the writing and reported');
    await seen(pat, '[data-testid="task"] >> text=completed', 'tasks: the lead closed the task');
    await shot(pat, 'tasks');
    const claims = await pat.locator('[data-testid="claims"] li').count();
    check(claims === 2, `tasks: each step claimed exactly once (${claims})`);
    // The research report once (the lead's next request quotes it).
    const reports = pat.locator('.aim-row[data-sender-type="agent"]').filter({ hasText: 'Facts: v2 syncs' }).filter({ hasNotText: 'Write:' });
    check((await reports.count()) === 1, 'tasks: only the winner did the step');
  } finally {
    ex.stop();
  }
}

check(errors.length === 0, `no page errors ${errors.join('; ')}`);
await browser.close();
console.log(failures ? `examples: ${failures} failed` : 'examples: all passed');
process.exit(failures ? 1 : 0);
