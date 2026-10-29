// The completeness check (docs/12 §5): every open API endpoint, client
// request, MCP tool, configuration key and public SDK member has its
// explanation, and every /docs link of the homepage has its page. Exits 1
// with the list of what is missing; runs in CI (site.yml).

import { existsSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { clientOps, configSections } from './reference.mjs';
import { SDKS, members } from './sdk.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const core = join(here, '../../server/crates/agentim-core');
const missing = [];

const api = JSON.parse(readFileSync(join(core, 'openapi.json'), 'utf8'));
for (const [path, ops] of Object.entries(api.paths))
  for (const [method, op] of Object.entries(ops)) {
    if (!op.summary) missing.push(`open API ${method.toUpperCase()} ${path}: no summary`);
    for (const p of op.parameters ?? []) if (!p.description) missing.push(`open API ${method.toUpperCase()} ${path}: parameter ${p.name} has no description`);
  }
for (const op of clientOps()) if (!op.doc) missing.push(`client request ${op.name}: no doc comment (model.rs ClientMsg)`);
for (const t of JSON.parse(readFileSync(join(core, 'mcp-tools.json'), 'utf8'))) if (!t.description) missing.push(`MCP tool ${t.name}: no description`);
for (const s of configSections()) for (const f of s.fields) if (!f.doc) missing.push(`config ${s.path}.${f.name}: no doc comment (config.rs)`);
for (const sdk of SDKS) for (const m of members(sdk)) if (!m.doc && !m.optional) missing.push(`SDK ${sdk.id}: ${m.owner ? `${m.owner}.` : ''}${m.name} (sdk/${m.file}:${m.line + 1}) has no doc comment`);

const docs = join(here, '../src/content/docs');
const page = (href) => {
  const p = href.replace(/#.*$/, '').replace(/^\/|\/$/g, '');
  return ['.md', '.mdx', '/index.md', '/index.mdx'].some((x) => existsSync(join(docs, p + x)));
};
const home = readFileSync(join(here, '../src/pages/index.astro'), 'utf8');
for (const [, href] of home.matchAll(/href="(\/docs[^"]*)"/g)) if (!page(href)) missing.push(`homepage link ${href}: no such page`);

if (missing.length) {
  console.error(missing.join('\n'));
  console.error(`\n${missing.length} missing (docs/12 §5: every API and SDK function is explained).`);
  process.exit(1);
}
console.log('complete: every endpoint, request, tool, config key and SDK member is documented');
