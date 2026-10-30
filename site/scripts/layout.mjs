// The homepage and some docs pages at phone, tablet and desktop width, light
// and dark (docs/12 §5): no horizontal scrolling, no page errors, every
// image loads. Screenshots go to OUT for review. Needs `npm run build` first.
import { createReadStream, existsSync, mkdirSync, statSync } from 'node:fs';
import { createServer } from 'node:http';
import { extname, join } from 'node:path';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const { chromium } = require(process.env.PLAYWRIGHT ?? 'playwright');
const out = process.env.OUT ?? 'screenshots';
mkdirSync(out, { recursive: true });
// dist/ as GitHub Pages serves it.
const dist = new URL('../dist/', import.meta.url).pathname;
const types = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml', '.png': 'image/png', '.woff2': 'font/woff2', '.json': 'application/json', '.wasm': 'application/wasm' };
const server = createServer((req, res) => {
  let f = join(dist, decodeURIComponent(new URL(req.url, 'http://x').pathname));
  if (existsSync(f) && statSync(f).isDirectory()) f = join(f, 'index.html');
  if (!existsSync(f)) return res.writeHead(404).end('not found');
  res.writeHead(200, { 'content-type': types[extname(f)] ?? 'application/octet-stream' });
  createReadStream(f).pipe(res);
}).listen(0);
await new Promise((r) => server.on('listening', r));
const base = `http://localhost:${server.address().port}`;

const pages = ['/', '/docs/', '/docs/get-started/quickstart/', '/docs/concepts/agents/', '/docs/reference/open-api/', '/docs/client-sdks/web-api/'];
const widths = [390, 820, 1440];
const failures = [];
const browser = await chromium.launch();
try {
  for (const scheme of ['light', 'dark'])
    for (const width of widths) {
      const page = await browser.newPage({ viewport: { width, height: 900 }, colorScheme: scheme });
      const errors = [];
      page.on('pageerror', (e) => errors.push(e.message));
      page.on('console', (m) => m.type() === 'error' && errors.push(m.text()));
      for (const path of pages) {
        await page.goto(base + path, { waitUntil: 'networkidle' });
        await page.waitForTimeout(path === '/' ? 1500 : 200);
        const wide = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
        if (wide > 1) failures.push(`${path} at ${width}px (${scheme}) scrolls sideways by ${wide}px`);
        const broken = await page.evaluate(() => [...document.images].filter((i) => i.complete && i.naturalWidth === 0).map((i) => i.src));
        for (const b of broken) failures.push(`${path}: image ${b} did not load`);
        if (path === '/' || path === '/docs/') {
          const name = `${path === '/' ? 'home' : 'docs'}-${width}-${scheme}.png`;
          await page.screenshot({ path: `${out}/${name}`, fullPage: true });
        }
      }
      for (const e of errors) failures.push(`page error at ${width}px (${scheme}): ${e}`);
      await page.close();
    }
  // The menu opens on phones.
  const phone = await browser.newPage({ viewport: { width: 390, height: 800 } });
  await phone.goto(base + '/');
  await phone.click('#menu');
  if (!(await phone.isVisible('nav[aria-label=Main] a[href="/docs/"]'))) failures.push('the phone menu does not open');
  // The error pages lead to the reference.
  const r = await fetch(`${base}/errors/forbidden/`);
  if (!r.ok || !(await r.text()).includes('/docs/reference/errors/#forbidden')) failures.push('/errors/forbidden/ does not lead to the reference');
} finally {
  await browser.close();
  server.close();
}
if (failures.length) {
  console.error(failures.join('\n'));
  process.exit(1);
}
console.log(`layout: ${pages.length} pages × ${widths.length} widths × 2 themes, no sideways scrolling or errors`);
