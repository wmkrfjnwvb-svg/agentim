// What every example needs (docs/en/design/developer-experience.md): an `agentim` server of its own, an
// API key and a signing key for "your backend", tokens for the people, a web
// server for the pages, and a clean stop. Nothing here is AgentIM-specific
// magic: it is what your own backend does, in a few lines.

import { spawn, execFileSync } from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { AgentIM, createUserToken } from '@agentimhq/server';
import { Connection } from '@agentimhq/agent';

const here = path.dirname(fileURLToPath(import.meta.url));
export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** The `agentim` program: AGENTIM_BIN, a build in this repository, or the one on PATH. */
function binary() {
  if (process.env.AGENTIM_BIN) return path.resolve(process.env.AGENTIM_BIN);
  const exe = process.platform === 'win32' ? 'agentim.exe' : 'agentim';
  for (const p of ['../../server/target/release', '../../server/target/debug']) {
    const f = path.join(here, p, exe);
    if (fs.existsSync(f)) return f;
  }
  return exe;
}

/**
 * Starts the stack: `agentim` on `imPort`, the pages on `webPort`.
 * `pages`: the folder of built pages; `routes`: extra JSON routes,
 * `{ '/api/x': (url) => value }`.
 */
export async function start({ name, webPort = Number(process.env.PORT ?? 3000), imPort = webPort + 1, pages, routes = {}, config = '' }) {
  const bin = binary();
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), `agentim-${name}-`));
  const db = path.join(dir, 'im.db').replace(/\\/g, '/');
  const web = `http://localhost:${webPort}`;
  const url = `http://127.0.0.1:${imPort}`;
  // The browser connects from the pages' origin; the server allows it.
  const cfg = path.join(dir, 'agentim.toml');
  fs.writeFileSync(cfg, `[storage]\ncontrol = "main"\n[[storage.databases]]\nname = "main"\nurl = "sqlite://${db}?mode=rwc"\n[server]\nallowed_origins = ["${web}", "http://127.0.0.1:${webPort}"]\n${config}`);
  const cli = (...args) => JSON.parse(execFileSync(bin, ['--config', cfg, ...args], { stdio: ['ignore', 'pipe', 'ignore'] }).toString());
  const server = spawn(bin, ['--config', cfg, '--addr', `127.0.0.1:${imPort}`], { stdio: 'ignore', cwd: dir });
  process.on('exit', () => server.kill());
  server.on('error', (e) => {
    console.error(`could not start ${bin}: ${e.message}. Set AGENTIM_BIN, or build it: cd server && cargo build`);
    process.exit(1);
  });
  let up = false;
  for (let i = 0; i < 300 && !up; i++) {
    up = await fetch(`${url}/readyz`).then((r) => r.ok).catch(() => false);
    if (!up) await sleep(100);
  }
  if (!up) throw new Error('agentim did not start');
  // Your backend: an API key for the open API, a signing key for tokens.
  const apiKey = cli('api-key', 'create', '--name', name, '--scopes', 'users:write,users:read,agents:write,conversations:write,conversations:read,messages:write').key;
  const signingKey = cli('signing-key', 'create', '--name', name);
  const im = new AgentIM({ baseUrl: url, apiKey });
  const token = (user) => createUserToken(signingKey, user);

  const out = pages;
  const site = http.createServer(async (req, res) => {
    const u = new URL(req.url, web);
    const route = routes[u.pathname];
    if (route) {
      try {
        const v = await route(u);
        res.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify(v));
      } catch (e) {
        res.writeHead(400, { 'content-type': 'application/json' }).end(JSON.stringify({ error: String(e.message ?? e) }));
      }
      return;
    }
    const name = u.pathname.replace(/^\/+/, '') || 'index.html';
    const f = path.join(out, path.normalize(name).replace(/^(\.\.[/\\])+/, ''));
    fs.readFile(fs.existsSync(f) ? f : path.join(out, `${name}.html`), (e, b) => {
      if (e) return res.writeHead(404).end();
      const type = f.endsWith('.js') ? 'text/javascript' : f.endsWith('.wasm') ? 'application/wasm' : f.endsWith('.css') ? 'text/css' : 'text/html';
      res.writeHead(200, { 'content-type': type }).end(b);
    });
  });
  await new Promise((r) => site.listen(webPort, '127.0.0.1', r));
  const stop = () => {
    site.close();
    server.kill();
  };
  for (const sig of ['SIGINT', 'SIGTERM']) process.on(sig, () => { stop(); process.exit(0); });
  // Never leave the server running, whatever ends this process.
  process.on('exit', () => server.kill());
  process.on('uncaughtException', (e) => { console.error(e); process.exit(1); });
  process.on('unhandledRejection', (e) => { console.error(e); process.exit(1); });
  /** The deployment's admin API as a developer (`/admin/v1/app…`). */
  let adminKey;
  const admin = async (method, path, body) => {
    adminKey ??= cli('admin-key', 'create', '--name', `${name}-setup`, '--role', 'developer').key;
    const r = await fetch(`${url}/admin/v1/app${path}`, {
      method,
      headers: { authorization: `Bearer ${adminKey}`, 'content-type': typeof body === 'string' ? 'text/markdown' : 'application/json' },
      body: body === undefined ? undefined : typeof body === 'string' ? body : JSON.stringify(body),
    });
    const v = await r.json().catch(() => ({}));
    if (!r.ok) throw new Error(`${method} ${path}: ${r.status} ${JSON.stringify(v)}`);
    return v;
  };
  return { url, web, im, token, cli, admin, stop, dir };
}

/** A person's connection from the backend (to set up what only they may set). */
export async function as(url, token) {
  const c = new Connection(url, token, () => {}, { device: 'setup' });
  await c.open();
  return c;
}

/** The ids of users by their names (an agent's id is not its name). */
export async function idsOf(conn, names) {
  const out = {};
  for (const n of names) out[n] = (await conn.call('find_user', { name: n })).user.id;
  return out;
}

/** Waits until `f()` is truthy. */
export async function until(what, f, ms = 10000) {
  for (let t = 0; t < ms; t += 50) {
    const v = await f();
    if (v) return v;
    await sleep(50);
  }
  throw new Error(`timed out: ${what}`);
}
