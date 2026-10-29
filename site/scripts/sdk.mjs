// The public API of every SDK, read from its source with its doc comments
// (docs/12 §5: every SDK function is explained). One page per SDK; the
// completeness check (complete.mjs) fails on a public member without a doc.

import { readFileSync } from 'node:fs';
import { join } from 'node:path';

/** `{ owner, name, signature, doc }` for each public member. */
const jsdoc = (lines, i) => {
  // The /** … */ block ending right above line i (decorators skipped).
  let j = i - 1;
  while (j >= 0 && /^\s*(@|\/\/)/.test(lines[j])) j--;
  if (j < 0 || !/\*\/\s*$/.test(lines[j])) return '';
  const end = j;
  while (j >= 0 && !/\/\*\*/.test(lines[j])) j--;
  return lines
    .slice(j, end + 1)
    .join('\n')
    .replace(/^\s*\/\*\*\s?|\s*\*\/\s*$/g, '')
    .split('\n')
    .map((l) => l.replace(/^\s*\* ?/, ''))
    .filter((l) => !/^@(param|returns?|example)\b/.test(l))
    .join(' ')
    .replace(/\s+/g, ' ')
    .trim();
};
const slashes = (lines, i, mark = '///') => {
  const doc = [];
  let j = i - 1;
  while (j >= 0 && /^\s*(@|\[|#\[)/.test(lines[j])) j--;
  while (j >= 0 && lines[j].trim().startsWith(mark)) doc.unshift(lines[j--].trim().slice(mark.length).trim());
  return doc
    .join(' ')
    .replace(/<\/?(summary|c|para|see[^>]*|paramref[^>]*)\/?>/g, '')
    .replace(/<param name="[^"]*">.*?<\/param>|<returns>.*?<\/returns>/g, '')
    .replace(/\s+/g, ' ')
    .trim();
};

/** TypeScript (.ts / .d.ts / .mjs with JSDoc): exported functions, types, classes and interfaces, their public members. */
export function typescript(file) {
  const lines = readFileSync(file, 'utf8').split('\n');
  const out = [];
  let cls = null;
  let depth = 0;
  for (let i = 0; i < lines.length; i++) {
    const l = lines[i];
    const c = /^export (?:declare )?(?:abstract )?(class|interface) (\w+)/.exec(l);
    if (c) {
      cls = c[2];
      depth = 0;
      out.push({ line: i, owner: '', name: cls, kind: c[1], signature: `${c[1]} ${cls}`, doc: jsdoc(lines, i) });
    }
    const t = /^export (?:declare )?type (\w+)/.exec(l);
    if (t) out.push({ line: i, owner: '', name: t[1], kind: 'type', signature: l.replace(/^export (declare )?/, '').replace(/\s*[{(]\s*$/, '').replace(/;$/, ''), doc: jsdoc(lines, i) });
    const f = /^export (?:declare )?(?:async )?function (\w+)\s*(\(.*)?/.exec(l);
    if (f) out.push({ line: i, owner: '', name: f[1], kind: 'function', signature: l.replace(/^export (declare )?/, '').replace(/\s*\{\s*$/, '').replace(/;$/, ''), doc: jsdoc(lines, i) });
    if (cls) {
      // A member at the class's own level (two spaces).
      const m = /^ {2}(?:readonly )?(?:static )?(?:async )?(get |set )?([a-zA-Z]\w*)(\??)(\(|<|:| =)/.exec(l);
      if (m && depth === 1 && !/^ {2}(private|protected|#)/.test(l) && m[2] !== 'constructor') {
        const isMethod = m[4] === '(' || m[4] === '<';
        const sig = l.trim().replace(/\s*\{\s*$/, '').replace(/;$/, '');
        out.push({ line: i, owner: cls, name: m[2], kind: isMethod ? 'method' : 'property', signature: sig, doc: jsdoc(lines, i) });
      }
      if (/^ {2}constructor\(/.test(l) && depth === 1) out.push({ line: i, owner: cls, name: 'constructor', kind: 'method', signature: l.trim().replace(/\s*\{\s*$/, ''), doc: jsdoc(lines, i), optional: true });
      depth += (l.match(/\{/g) ?? []).length - (l.match(/\}/g) ?? []).length;
      if (depth <= 0 && /^\}/.test(l)) cls = null;
    }
  }
  return out;
}

/** Python: module functions and public methods of public classes (docstrings). */
export function python(file) {
  const lines = readFileSync(file, 'utf8').split('\n');
  const out = [];
  let cls = null;
  const docstring = (i) => {
    let j = i + 1;
    // After the (possibly wrapped) def or class line.
    while (j < lines.length && !/:\s*$/.test(lines[j - 1]) && j < i + 8) j++;
    const first = lines[j]?.trim() ?? '';
    if (!/^("""|''')/.test(first)) return '';
    let text = first.slice(3);
    if (text.includes('"""')) return text.replace(/"""\s*$/, '').trim();
    for (j++; j < lines.length && !lines[j].includes('"""'); j++) text += ' ' + lines[j].trim();
    return (text + ' ' + (lines[j] ?? '').replace(/"""/, '').trim()).replace(/\s+/g, ' ').split(/\s{2}|``/)[0].trim();
  };
  for (let i = 0; i < lines.length; i++) {
    const l = lines[i];
    const c = /^class (\w+)/.exec(l);
    if (c) {
      cls = c[1].startsWith('_') ? null : c[1];
      if (cls) out.push({ line: i, owner: '', name: cls, kind: 'class', signature: l.replace(/:\s*$/, ''), doc: docstring(i) });
      continue;
    }
    const f = /^def ([a-z]\w*)\(/.exec(l);
    if (f) {
      cls = null;
      out.push({ line: i, owner: '', name: f[1], kind: 'function', signature: l.replace(/^def /, '').replace(/:\s*$/, ''), doc: docstring(i) });
    }
    const m = /^ {4}(?:async )?def ([a-z]\w*)\(/.exec(l);
    if (m && cls) out.push({ line: i, owner: cls, name: m[1], kind: 'method', signature: l.trim().replace(/^(async )?def /, '$1').replace(/:\s*$/, ''), doc: docstring(i), inherits: true });
  }
  return out;
}

/** Go: exported functions, methods and types with their comments. */
export function go(file) {
  const lines = readFileSync(file, 'utf8').split('\n');
  const out = [];
  for (let i = 0; i < lines.length; i++) {
    const l = lines[i];
    const m = /^func (?:\(\w+ \*?(\w+)\) )?([A-Z]\w*)\(/.exec(l);
    const t = /^type ([A-Z]\w*) /.exec(l);
    if (m) out.push({ line: i, owner: m[1] ?? '', name: m[2], kind: m[1] ? 'method' : 'function', signature: l.replace(/\s*\{\s*$/, ''), doc: slashes(lines, i, '//') });
    else if (t) out.push({ line: i, owner: '', name: t[1], kind: 'type', signature: l.replace(/\s*\{\s*$/, ''), doc: slashes(lines, i, '//') });
  }
  return out;
}

/** Swift: public functions, properties and types. */
export function swift(file) {
  const lines = readFileSync(file, 'utf8').split('\n');
  const out = [];
  let owner = '';
  for (let i = 0; i < lines.length; i++) {
    const l = lines[i];
    const t = /^\s*public (?:final )?(class|struct|enum|actor|protocol) (\w+)/.exec(l);
    if (t) {
      owner = t[2];
      out.push({ line: i, owner: '', name: t[2], kind: 'type', signature: l.trim().replace(/\s*\{\s*$/, ''), doc: slashes(lines, i) });
      continue;
    }
    const m = /^\s*(?:@\w+ )*public (?:static )?(?:func (\w+)|(?:let|var) (\w+)|init)/.exec(l);
    if (m) out.push({ line: i, owner, name: m[1] ?? m[2] ?? 'init', kind: m[1] ? 'method' : m[2] ? 'property' : 'method', signature: l.trim().replace(/\s*\{\s*$/, ''), doc: slashes(lines, i) });
  }
  return out;
}

/** Kotlin: public (the default) functions and properties of public classes. */
export function kotlin(file) {
  const lines = readFileSync(file, 'utf8').split('\n');
  const out = [];
  let owner = '';
  for (let i = 0; i < lines.length; i++) {
    const l = lines[i];
    const t = /^(?:private |internal )?(?:data |sealed |open |enum |fun )?(?:class|object|interface) (\w+)/.exec(l);
    if (t && /^(private|internal) /.test(l)) owner = null;
    else if (t) {
      owner = t[1];
      out.push({ line: i, owner: '', name: owner, kind: 'type', signature: l.trim().replace(/\s*\{\s*$/, ''), doc: jsdoc(lines, i) });
      continue;
    }
    const m = /^ {4}(?:override )?(?:suspend )?fun (\w+)|^ {4}(?:val|var) (\w+)/.exec(l);
    if (m && owner !== null && !/^\s*(private|internal|protected)/.test(l)) out.push({ line: i, owner, name: m[1] ?? m[2], kind: m[1] ? 'method' : 'property', signature: l.trim().replace(/\s*[={]\s*$/, ''), doc: jsdoc(lines, i) || slashes(lines, i, '//') });
  }
  return out;
}

/** C#: public members with XML doc comments. */
export function csharp(file) {
  const lines = readFileSync(file, 'utf8').split('\n');
  const out = [];
  let owner = '';
  for (let i = 0; i < lines.length; i++) {
    const l = lines[i];
    const t = /^\s*public (?:sealed |static |abstract |partial )*(class|struct|enum|interface|record) (\w+)/.exec(l);
    if (t) {
      owner = t[2];
      out.push({ line: i, owner: '', name: t[2], kind: 'type', signature: l.trim(), doc: slashes(lines, i) });
      continue;
    }
    const m = /^\s*public (?:static |async |override |virtual |event )*[\w<>?,\[\] ]+? (\w+)\s*(\(|\{|=>|;)/.exec(l);
    if (m) out.push({ line: i, owner, name: m[1], kind: m[2] === '(' ? 'method' : 'property', signature: l.trim().replace(/\s*(=>.*|\{\s*)$/, ''), doc: slashes(lines, i) });
  }
  return out;
}

/** Dart: public classes, their public methods and getters (`///`). */
export function dart(file) {
  const lines = readFileSync(file, 'utf8').split('\n');
  const out = [];
  let owner = '';
  for (let i = 0; i < lines.length; i++) {
    const l = lines[i];
    const t = /^(?:final |abstract |sealed )?class ([A-Z]\w*)/.exec(l);
    if (t) {
      owner = t[1];
      out.push({ line: i, owner: '', name: owner, kind: 'type', signature: l.replace(/\s*\{\s*$/, ''), doc: slashes(lines, i) });
      continue;
    }
    const m = /^ {2}(?:static )?(?:Future<[^>]*>+|Stream<[^>]*>+|[A-Z]\w*(?:<[^>]*>)?\??|void|bool|int|String|double|dynamic) (get )?([a-z]\w*)\s*(\(|=>|;|\{)/.exec(l);
    if (m && owner) out.push({ line: i, owner, name: m[2], kind: m[1] ? 'property' : 'method', signature: l.trim().replace(/\s*(=>.*|\{\s*|async\s*\{?\s*)$/, ''), doc: slashes(lines, i) });
  }
  return out;
}

const here = (p) => new URL(`../../sdk/${p}`, import.meta.url).pathname;

/** Every SDK: where its page goes, its language and its source files. */
export const SDKS = [
  { id: 'web', group: 'client-sdks', title: 'Web (@agentimhq/client-web)', lang: 'ts', parse: typescript, files: ['client-web/src/client.ts', 'client-web/src/view.ts', 'client-web/src/store.ts'] },
  { id: 'react', group: 'client-sdks', title: 'React components (@agentimhq/react)', lang: 'tsx', parse: typescript, files: ['react/src/components.tsx', 'react/src/files.tsx', 'react/src/hooks.ts', 'react/src/format.ts'] },
  { id: 'react-native', group: 'client-sdks', title: 'React Native (@agentimhq/client-react-native)', lang: 'ts', parse: typescript, files: ['client-react-native/src/client.ts'] },
  { id: 'react-native-ui', group: 'client-sdks', title: 'React Native components (@agentimhq/react-native-ui)', lang: 'tsx', parse: typescript, files: ['react-native-ui/src/components.tsx', 'react-native-ui/src/files.tsx'] },
  { id: 'swift', group: 'client-sdks', title: 'Swift (iOS, macOS)', lang: 'swift', parse: swift, files: ['client-swift/Sources/AgentIM/AgentIMClient.swift'] },
  { id: 'kotlin', group: 'client-sdks', title: 'Kotlin (Android, JVM)', lang: 'kotlin', parse: kotlin, files: ['client-kotlin/src/main/kotlin/com/agentim/client/AgentIMClient.kt'] },
  { id: 'dotnet', group: 'client-sdks', title: 'C# (.NET, Unity)', lang: 'csharp', parse: csharp, files: ['client-dotnet/AgentIM.Client/Client.cs'] },
  { id: 'flutter', group: 'client-sdks', title: 'Dart (Flutter)', lang: 'dart', parse: dart, files: ['client-flutter/lib/agentim_client.dart'] },
  { id: 'agent-node', group: 'agent-sdks', title: 'Node (@agentimhq/agent)', lang: 'ts', parse: typescript, files: ['agent-node/index.d.ts'] },
  { id: 'agent-python', group: 'agent-sdks', title: 'Python (agentim-agent)', lang: 'python', parse: python, files: ['agent-python/agentim_agent/__init__.py'] },
  { id: 'server-node', group: 'server-sdks', title: 'Node (@agentimhq/server)', lang: 'ts', parse: typescript, files: ['node/index.d.ts'] },
  { id: 'server-python', group: 'server-sdks', title: 'Python (agentim-server)', lang: 'python', parse: python, files: ['python/agentim/__init__.py'] },
  { id: 'server-go', group: 'server-sdks', title: 'Go', lang: 'go', parse: go, files: ['go/agentim/agentim.go'] },
];

export function members(sdk) {
  return sdk.files.flatMap((f) => sdk.parse(here(f)).map((m) => ({ ...m, file: f })));
}
