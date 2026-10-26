// Renders the React Native components in a browser through react-native-web,
// in fixed states (docs/12 §3.5): for looking at the design and for the
// screenshot comparison. Bundles example/gallery.tsx into example/out.
import { build } from 'esbuild';
import { copyFileSync, mkdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const here = (p) => fileURLToPath(new URL(p, import.meta.url));
mkdirSync(here('out'), { recursive: true });
await build({
  entryPoints: [here('gallery.tsx')],
  bundle: true,
  format: 'esm',
  target: 'es2022',
  jsx: 'automatic',
  outfile: here('out/gallery.js'),
  // One React: the SDK packages are linked from sdk/ with their own copies.
  alias: { 'react-native': 'react-native-web', react: here('../node_modules/react'), 'react-dom': here('../node_modules/react-dom') },
  resolveExtensions: ['.web.tsx', '.web.ts', '.tsx', '.ts', '.web.js', '.js'],
  nodePaths: [here('../node_modules')],
  define: { 'process.env.NODE_ENV': '"production"', __DEV__: 'false' },
  logLevel: 'warning',
});
copyFileSync(here('gallery.html'), here('out/gallery.html'));
