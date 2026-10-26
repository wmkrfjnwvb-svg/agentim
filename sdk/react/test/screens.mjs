// Screenshot comparison (docs/12 §3.5): the web and React Native galleries
// in fixed states, against the baselines in test/screens. Baselines are
// made in the CI image (mcr.microsoft.com/playwright), where the fonts and
// the renderer are fixed; screenshots from elsewhere differ a little.
//
//   node test/screens.mjs             compare; new and differing shots go to $OUT
//   UPDATE=1 node test/screens.mjs    write the baselines
//
// Build the galleries first: `npm run screens` does both.

import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import pixelmatch from 'pixelmatch';
import { PNG } from 'pngjs';

const require = createRequire(import.meta.url);
const { chromium } = require(process.env.PLAYWRIGHT ?? 'playwright');
const here = path.dirname(fileURLToPath(import.meta.url));
const base = path.join(here, 'screens');
const out = process.env.OUT ?? path.join(here, '../screens-out');
const update = process.env.UPDATE === '1';
// As Playwright's toHaveScreenshot by default: a colour distance of 0.2
// per pixel (anti-aliasing ignored), and not one pixel more. In the same
// image the same build gives the same pixels.
const THRESHOLD = 0.2;
const MAX_PIXELS = Number(process.env.MAX_PIXELS ?? 0);

const roots = { web: path.join(here, '../example/out'), rn: path.join(here, '../../react-native-ui/example/out') };
const types = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.wasm': 'application/wasm' };
const server = http
  .createServer((q, r) => {
    const [, root, ...rest] = decodeURIComponent(new URL(q.url, 'http://x').pathname).split('/');
    const f = roots[root] && path.join(roots[root], ...rest);
    if (!f || !f.startsWith(roots[root]) || !fs.existsSync(f)) {
      r.writeHead(404);
      return r.end();
    }
    r.writeHead(200, { 'content-type': types[path.extname(f)] ?? 'application/octet-stream' });
    r.end(fs.readFileSync(f));
  })
  .listen(0, '127.0.0.1');
await new Promise((r) => server.once('listening', r));
const url = `http://127.0.0.1:${server.address().port}`;

const shots = [
  ['web-app-light', 'web/gallery.html', 1360, 820],
  ['web-app-dark', 'web/gallery.html?theme=dark', 1360, 820],
  ['web-app-rtl', 'web/gallery.html?dir=rtl', 1360, 820],
  ['web-support', 'web/gallery.html?view=support', 1000, 760],
  ['web-phone', 'web/gallery.html?view=support', 390, 844],
  ['web-offline', 'web/gallery.html?view=support&offline=1', 480, 760],
  ['rn-light', 'rn/gallery.html', 900, 830],
  ['rn-dark', 'rn/gallery.html?theme=dark', 900, 830],
];

fs.mkdirSync(out, { recursive: true });
if (update) fs.mkdirSync(base, { recursive: true });
const browser = await chromium.launch(process.env.CHROMIUM ? { executablePath: process.env.CHROMIUM } : {});
let failed = 0;
for (const [name, page, width, height] of shots) {
  const ctx = await browser.newContext({ viewport: { width, height }, deviceScaleFactor: 1, reducedMotion: 'reduce', locale: 'en-US', timezoneId: 'America/Los_Angeles' });
  const p = await ctx.newPage();
  const errors = [];
  p.on('pageerror', (e) => errors.push(e.message));
  await p.goto(`${url}/${page}`);
  // No caret, no animation: the same pixels every time.
  await p.addStyleTag({ content: '*, *::before, *::after { animation: none !important; transition: none !important; caret-color: transparent !important; }' });
  await p.evaluate(() => document.fonts.ready);
  await p.waitForTimeout(300);
  const png = await p.screenshot();
  await ctx.close();
  if (errors.length) {
    console.log(`FAIL ${name}: page errors ${JSON.stringify(errors)}`);
    failed++;
    continue;
  }
  const file = path.join(base, `${name}.png`);
  if (update) {
    fs.writeFileSync(file, png);
    console.log(`wrote ${name}`);
    continue;
  }
  if (!fs.existsSync(file)) {
    fs.writeFileSync(path.join(out, `${name}.png`), png);
    console.log(`FAIL ${name}: no baseline (the new shot is in ${out})`);
    failed++;
    continue;
  }
  const a = PNG.sync.read(fs.readFileSync(file));
  const b = PNG.sync.read(png);
  if (a.width !== b.width || a.height !== b.height) {
    fs.writeFileSync(path.join(out, `${name}.png`), png);
    console.log(`FAIL ${name}: ${b.width}x${b.height}, the baseline is ${a.width}x${a.height}`);
    failed++;
    continue;
  }
  const diff = new PNG({ width: a.width, height: a.height });
  const n = pixelmatch(a.data, b.data, diff.data, a.width, a.height, { threshold: THRESHOLD });
  if (n > MAX_PIXELS) {
    fs.writeFileSync(path.join(out, `${name}.png`), png);
    fs.writeFileSync(path.join(out, `${name}.diff.png`), PNG.sync.write(diff));
    console.log(`FAIL ${name}: ${n} pixels differ; the shot and the diff are in ${out}`);
    failed++;
  } else {
    console.log(`ok   ${name}${n ? ` (${n} pixels within tolerance)` : ''}`);
  }
}
await browser.close();
server.close();
if (failed) {
  console.error(`${failed} screenshot(s) failed. After a deliberate change, look at the new shots and copy them to test/screens (or run with UPDATE=1 in the CI image).`);
  process.exit(1);
}
