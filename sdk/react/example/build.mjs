// Bundles the example into example/out with the core next to it.
import { build } from 'esbuild';
import { copyFileSync, mkdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const here = (p) => fileURLToPath(new URL(p, import.meta.url));
mkdirSync(here('out'), { recursive: true });
await build({
  entryPoints: [here('app.tsx')],
  bundle: true,
  format: 'esm',
  target: 'es2022',
  jsx: 'automatic',
  outfile: here('out/app.js'),
  logLevel: 'warning',
});
copyFileSync(here('index.html'), here('out/index.html'));
copyFileSync(here('../theme.css'), here('out/theme.css'));
copyFileSync(here('../node_modules/@agentimhq/client-web/dist/agentim_core.wasm'), here('out/agentim_core.wasm'));
