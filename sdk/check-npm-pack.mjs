// What an npm package would publish (docs/12 §2.12.1): every entry point it
// declares (main, types, exports) is in it, and nothing outside its `files`
// (plus what npm always adds: package.json, README, LICENSE).
//
//   node sdk/check-npm-pack.mjs <package dir>...   (after `npm run build`)
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

let failed = false;
for (const dir of process.argv.slice(2)) {
  const pkg = JSON.parse(readFileSync(join(dir, 'package.json'), 'utf8'));
  const out = execFileSync('npm', ['pack', '--dry-run', '--json'], { cwd: dir, encoding: 'utf8', shell: process.platform === 'win32' });
  const files = JSON.parse(out)[0].files.map((f) => f.path);
  const allowed = (pkg.files ?? []).map((f) => f.replace(/^\.\//, '').replace(/\/$/, ''));
  const always = /^(package\.json|README(\.md)?|LICENSE(\.md|\.txt)?)$/i;
  const stray = files.filter((f) => !always.test(f) && !allowed.some((a) => f === a || f.startsWith(`${a}/`)));
  const entries = new Set();
  const walk = (e) => {
    if (typeof e === 'string') entries.add(e.replace(/^\.\//, ''));
    else if (e && typeof e === 'object') Object.values(e).forEach(walk);
  };
  [pkg.main, pkg.types, pkg.module, pkg.exports].forEach(walk);
  const missing = [...entries].filter((e) => !files.includes(e));
  const problems = [
    ...stray.map((f) => `not in "files": ${f}`),
    ...missing.map((f) => `entry point not packed: ${f}`),
    ...(files.some((f) => /^README/i.test(f)) ? [] : ['no README']),
    ...(pkg.repository?.url ? [] : ['no repository (npm provenance needs it)']),
  ];
  if (problems.length) {
    failed = true;
    console.error(`${pkg.name}:\n  ${problems.join('\n  ')}`);
  } else {
    console.log(`${pkg.name}@${pkg.version}: ${files.length} files, ${entries.size} entry points, ok`);
  }
}
process.exit(failed ? 1 : 0);
