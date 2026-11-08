// Offline check of writeFiles and mcpServers (docs/en/design/assistants.md): MCP servers are
// merged into .mcp.json next to the developer's own (readable by its owner
// only), API documents get their server address, and only what the SDK
// wrote before is replaced or removed. Skills with files are checked
// against a real server in e2e.mjs.
//
//   node sdk/agent-node/files.test.mjs

import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { Agent } from './index.mjs';

let failures = 0;
const check = (cond, what) => { console.log(`${cond ? 'ok  ' : 'FAIL'} ${what}`); if (!cond) failures++; };
const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'agentim-files-'));
const read = (p) => JSON.parse(fs.readFileSync(path.join(dir, p), 'utf8'));

const agent = new Agent({ url: 'http://127.0.0.1:9', key: 'k' });
let threw = false;
try { await agent.writeFiles({ dir }); } catch { threw = true; }
check(threw, 'nothing to write before the configuration arrives');

fs.writeFileSync(path.join(dir, '.mcp.json'), JSON.stringify({ mcpServers: { mine: { type: 'stdio', command: 'x' } }, other: 1 }));
agent.config = {
  version: 'v1', role: null,
  skills: [{ name: 'notes', description: 'Notes', instructions: 'Write notes.', markdown: '---\nname: notes\ndescription: Notes\n---\nWrite notes.\n', files: [] },
    { name: '../evil', description: 'x', instructions: 'x', files: [] }],
  mcp: [{ name: 'shop', url: 'https://mcp.example.com/mcp', writes: 'read_only', version: 1, headers: { Authorization: 'Bearer s' } }],
  apis: [{ name: 'orders', url: 'https://api.example.com', writes: 'read_only', version: 1, spec: { openapi: '3.0.3', paths: {} } }],
};
check(JSON.stringify(agent.mcpServers()) === JSON.stringify([{ name: 'shop', url: 'https://mcp.example.com/mcp', headers: { Authorization: 'Bearer s' } }]), 'mcpServers gives name, url, headers');
const r = await agent.writeFiles({ dir });
check(JSON.stringify(r) === JSON.stringify({ skills: ['notes'], mcp: ['shop'], apis: ['orders'] }), 'it reports what it wrote; unsafe names are skipped');
check(!fs.existsSync(path.join(dir, '.claude', 'evil')) && !fs.existsSync(path.join(dir, 'evil')), 'nothing is written outside the skills folder');
check(fs.readFileSync(path.join(dir, '.claude/skills/notes/SKILL.md'), 'utf8').startsWith('---\nname: notes'), 'the skill is written as SKILL.md');
const m = read('.mcp.json');
check(m.mcpServers.shop.type === 'http' && m.mcpServers.shop.headers.Authorization === 'Bearer s', 'the MCP server is in .mcp.json with its header');
check(m.mcpServers.mine.command === 'x' && m.other === 1, "the developer's own entries stay");
if (process.platform !== 'win32') check((fs.statSync(path.join(dir, '.mcp.json')).mode & 0o777) === 0o600, '.mcp.json is readable by its owner only');
const spec = read('.agentim/apis/orders.openapi.json');
check(spec.servers[0].url === 'https://api.example.com' && spec.openapi === '3.0.3', 'the API document gets its server address');
// The role drops all three: only what the SDK wrote goes.
agent.config = { ...agent.config, version: 'v2', skills: [], mcp: [], apis: [] };
await agent.writeFiles({ dir });
check(!fs.existsSync(path.join(dir, '.claude/skills/notes')), 'a skill no longer given is removed');
const m2 = read('.mcp.json');
check(!m2.mcpServers.shop && m2.mcpServers.mine.command === 'x', 'its MCP server is removed, the developer\'s stays');
check(!fs.existsSync(path.join(dir, '.agentim/apis/orders.openapi.json')), 'the API document is removed');
// A skill folder the developer made themselves is left alone.
fs.mkdirSync(path.join(dir, '.claude/skills/handmade'), { recursive: true });
await agent.writeFiles({ dir });
check(fs.existsSync(path.join(dir, '.claude/skills/handmade')), "the developer's own skill stays");

fs.rmSync(dir, { recursive: true, force: true });
if (failures) { console.log(`${failures} failed`); process.exit(1); }
console.log('files: all passed');
