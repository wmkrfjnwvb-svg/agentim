// Bundles an example's pages (React + the AgentIM components) into its
// `out/` folder, with the client core (WebAssembly) and the theme next to them.

import { build } from 'esbuild';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.join(path.dirname(fileURLToPath(import.meta.url)), '..');
const pkg = (name) => fs.realpathSync(path.join(root, 'node_modules', name));

export async function bundle(dir, pages) {
  const out = path.join(dir, 'out');
  fs.mkdirSync(out, { recursive: true });
  await build({
    entryPoints: pages.map((p) => path.join(dir, `${p}.tsx`)),
    bundle: true,
    format: 'esm',
    target: 'es2022',
    jsx: 'automatic',
    outdir: out,
    logLevel: 'warning',
    // One React for the page and the components.
    alias: { react: pkg('react'), 'react-dom': pkg('react-dom') },
  });
  for (const p of pages) fs.copyFileSync(path.join(dir, `${p}.html`), path.join(out, `${p}.html`));
  fs.copyFileSync(path.join(pkg('@agentimhq/react'), 'theme.css'), path.join(out, 'theme.css'));
  fs.copyFileSync(path.join(pkg('@agentimhq/client-web'), 'dist/agentim_core.wasm'), path.join(out, 'agentim_core.wasm'));
  fs.copyFileSync(path.join(dir, '../lib/page.css'), path.join(out, 'page.css'));
  return out;
}
