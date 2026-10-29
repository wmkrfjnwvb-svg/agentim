// Inserts doc comments above SDK declarations: `node scripts/add-docs.mjs <sdk id> <docs.json>`,
// docs.json mapping "Owner.name" (or ".name" for top-level) to the text.
// A maintenance tool for docs/12 §5; the comments then live in the source.
import { readFileSync, writeFileSync } from 'node:fs';
import { SDKS, members } from './sdk.mjs';

const [id, file] = process.argv.slice(2);
const sdk = SDKS.find((s) => s.id === id);
const docs = JSON.parse(readFileSync(file, 'utf8'));
const style = { ts: 'jsdoc', tsx: 'jsdoc', kotlin: 'jsdoc', python: 'docstring', go: '//', swift: '///', dart: '///', csharp: 'xml' }[sdk.lang];
const wrap = (text, width) => {
  const words = text.split(/\s+/);
  const lines = [];
  let cur = '';
  for (const w of words) {
    if (cur && (cur + ' ' + w).length > width) {
      lines.push(cur);
      cur = w;
    } else cur = cur ? `${cur} ${w}` : w;
  }
  if (cur) lines.push(cur);
  return lines;
};
const byFile = new Map();
for (const m of members(sdk)) {
  const key = `${m.owner}.${m.name}`;
  if (!(key in docs) || m.doc) continue;
  if (!byFile.has(m.file)) byFile.set(m.file, []);
  byFile.get(m.file).push({ ...m, text: docs[key] });
  delete docs[key];
}
for (const [f, list] of byFile) {
  const path = new URL(`../../sdk/${f}`, import.meta.url).pathname;
  const lines = readFileSync(path, 'utf8').split('\n');
  for (const m of list.sort((a, b) => b.line - a.line)) {
    let at = m.line;
    // Above decorators and attributes.
    while (at > 0 && /^\s*(@|\[[A-Z])/.test(lines[at - 1]) && style !== 'docstring') at--;
    const ind = lines[m.line].match(/^\s*/)[0];
    let block;
    if (style === 'jsdoc') {
      const w = wrap(m.text, 96 - ind.length);
      block = w.length === 1 ? [`${ind}/** ${w[0]} */`] : [`${ind}/**`, ...w.map((l) => `${ind} * ${l}`), `${ind} */`];
    } else if (style === 'xml') {
      block = [`${ind}/// <summary>${m.text}</summary>`];
    } else if (style === 'docstring') {
      // After the def line(s), indented one step further.
      let end = m.line;
      while (!/:\s*$/.test(lines[end])) end++;
      const inner = ind + '    ';
      const w = wrap(m.text, 88 - inner.length);
      const doc = w.length === 1 ? [`${inner}"""${w[0]}"""`] : [`${inner}"""${w[0]}`, ...w.slice(1).map((l) => inner + l), `${inner}"""`];
      lines.splice(end + 1, 0, ...doc);
      continue;
    } else {
      block = wrap(m.text, 96 - ind.length).map((l) => `${ind}${style} ${l}`);
    }
    lines.splice(at, 0, ...block);
  }
  writeFileSync(path, lines.join('\n'));
}
const left = Object.keys(docs);
if (left.length) console.log('not found:', left.join(', '));
