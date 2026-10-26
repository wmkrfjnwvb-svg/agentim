// Generates the themes from tokens.json (docs/12 §3.1): the variables at
// the top of theme.css (the rest of it is src/styles.css) and the React
// Native tokens (sdk/react-native-ui/src/tokens.ts).
//
//   node scripts/tokens.mjs           write them
//   node scripts/tokens.mjs --check   fail if they are out of date or a
//                                     contrast pair is below its minimum

import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = (p) => fileURLToPath(new URL(p, import.meta.url));
export const tokens = JSON.parse(readFileSync(here('../tokens.json'), 'utf8'));

function rgb(hex) {
  const n = parseInt(hex.slice(1), 16);
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
}

/** WCAG 2.1 relative luminance of an sRGB hex colour. */
export function luminance(hex) {
  const [r, g, b] = rgb(hex).map((c) => {
    const s = c / 255;
    return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

export function contrast(a, b) {
  const [x, y] = [luminance(a), luminance(b)].sort((p, q) => q - p);
  return (x + 0.05) / (y + 0.05);
}

/** Every pair below its minimum, in both themes, plus avatar colours under white initials. */
export function contrastFailures(t = tokens) {
  const out = [];
  for (const [theme, c] of Object.entries(t.color)) {
    for (const [fg, bg, min] of t.contrast.pairs) {
      const r = contrast(c[fg], c[bg]);
      if (r < min) out.push(`${theme}: ${fg} on ${bg} is ${r.toFixed(2)}, needs ${min}`);
    }
  }
  for (const a of t.avatar) {
    const r = contrast('#ffffff', a);
    if (r < 4.5) out.push(`avatar ${a}: white initials are ${r.toFixed(2)}, need 4.5`);
  }
  return out;
}

const vars = (c) =>
  Object.entries(c)
    .map(([k, v]) => `  --aim-${k}: ${v};`)
    .join('\n');

function css() {
  const t = tokens;
  const fixed = [
    ...Object.entries(t.radius).map(([k, v]) => `  --aim-radius-${k}: ${v}px;`),
    ...Object.entries(t.space).map(([k, v]) => `  --aim-space-${k}: ${v}px;`),
    `  --aim-font: ${t.font.family};`,
    `  --aim-font-size: ${t.font.size}px;`,
    `  --aim-font-size-small: ${t.font['size-small']}px;`,
    `  --aim-font-size-tiny: ${t.font['size-tiny']}px;`,
    `  --aim-line: ${t.font.line};`,
    `  --aim-target: ${t.target.pointer}px;`,
    ...t.avatar.map((v, i) => `  --aim-avatar-${i}: ${v};`),
  ].join('\n');
  const head = `/* AgentIM default theme (docs/12 §3.1). Generated from tokens.json by
   scripts/tokens.mjs; edit src/styles.css or tokens.json, not this file.
   Override any variable on .aim-root or an ancestor; data-theme="light" or
   "dark" on <html> forces a theme. */
:root {
${fixed}
${vars(t.color.light)}
  color-scheme: light;
}
@media (prefers-color-scheme: dark) {
  :root:not([data-theme="light"]) {
${vars(t.color.dark).replace(/^ {2}/gm, '    ')}
    color-scheme: dark;
  }
}
:root[data-theme="dark"] {
${vars(t.color.dark)}
  color-scheme: dark;
}
`;
  return head + '\n' + readFileSync(here('../src/styles.css'), 'utf8');
}

const camel = (k) => k.replace(/-(\w)/g, (_, c) => c.toUpperCase());

function ts() {
  const t = tokens;
  const obj = (o, ind) =>
    Object.entries(o)
      .filter(([k]) => !k.startsWith('$'))
      .map(([k, v]) => `${ind}${/^\w+$/.test(camel(k)) && !/^\d/.test(k) ? camel(k) : JSON.stringify(k)}: ${JSON.stringify(v)},`)
      .join('\n');
  return `// Generated from sdk/react/tokens.json by sdk/react/scripts/tokens.mjs
// (docs/12 §3.1); the web theme comes from the same file.

export const light = {
${obj(t.color.light, '  ')}
};

export type Colors = typeof light;

export const dark: Colors = {
${obj(t.color.dark, '  ')}
};

export const avatar = ${JSON.stringify(t.avatar)};

export const radius = {
${obj(t.radius, '  ')}
};

export const space = {
${obj(t.space, '  ')}
};

export const font = {
  size: ${t.font.size + 1},
  sizeSmall: ${t.font['size-small'] + 1},
  sizeTiny: ${t.font['size-tiny'] + 1},
  line: ${t.font.line},
};

export const target = ${t.target.touch};
`;
}

const outputs = [
  [here('../theme.css'), css],
  [here('../../react-native-ui/src/tokens.ts'), ts],
];

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const check = process.argv.includes('--check');
  let bad = contrastFailures();
  for (const [path, gen] of outputs) {
    // The React Native package is generated into once it exists.
    if (!existsSync(dirname(path))) continue;
    const want = gen();
    if (check) {
      if (!existsSync(path) || readFileSync(path, 'utf8') !== want) bad.push(`${path} is out of date: run node scripts/tokens.mjs`);
    } else writeFileSync(path, want);
  }
  for (const b of bad) console.error(b);
  if (bad.length) process.exit(1);
}
