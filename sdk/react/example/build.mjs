// Bundles the support window and the component gallery into example/out,
// with the core next to them.
import { build } from 'esbuild';
import { copyFileSync, existsSync, mkdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const here = (p) => fileURLToPath(new URL(p, import.meta.url));
mkdirSync(here('out'), { recursive: true });
for (const name of ['app', 'gallery']) {
  await build({
    entryPoints: [here(`${name}.tsx`)],
    bundle: true,
    format: 'esm',
    target: 'es2022',
    jsx: 'automatic',
    outfile: here(`out/${name}.js`),
    logLevel: 'warning',
  });
}
copyFileSync(here('index.html'), here('out/index.html'));
copyFileSync(here('gallery.html'), here('out/gallery.html'));
copyFileSync(here('../theme.css'), here('out/theme.css'));
// The support window needs the core; the gallery does not (the screenshot job
// builds client-web without it).
const wasm = here('../node_modules/@agentimhq/client-web/dist/agentim_core.wasm');
if (existsSync(wasm)) copyFileSync(wasm, here('out/agentim_core.wasm'));
