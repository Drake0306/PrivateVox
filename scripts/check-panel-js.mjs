#!/usr/bin/env node
// Lints the inline scripts of a UI page without any dependency: syntax (node --check) plus a block-scoped
// "assignment to a const" check, the one error V8 only reports when the line runs.
import { readFileSync, writeFileSync, mkdtempSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const files = process.argv.slice(2);
if (!files.length) { console.error('usage: check-panel-js.mjs <html file>...'); process.exit(2); }
let failed = false;
for (const file of files) {
  const html = readFileSync(file, 'utf8');
  const scripts = [...html.matchAll(/<script>([\s\S]*?)<\/script>/g)].map(m => m[1]);
  const dir = mkdtempSync(join(tmpdir(), 'panel-lint-'));
  const tmp = join(dir, 'inline.js');
  writeFileSync(tmp, scripts.join('\n'));
  try { execFileSync(process.execPath, ['--check', tmp], { stdio: 'pipe' }); } catch (e) { failed = true; console.error(`${file}: ${e.stderr}`); continue; }
  for (const f of constAssignments(scripts.join('\n'))) { failed = true; console.error(`${file}: line ${f.line}: assignment to const "${f.name}" (declared line ${f.declared})`); }
}
process.exit(failed ? 1 : 0);

/** Walks lines tracking brace depth; a `const` name is live in its block and flagged when a later line in that block assigns it. */
function constAssignments(src) {
  const out = [];
  const scopes = [new Map()];
  src.split('\n').forEach((line, i) => {
    const n = i + 1;
    const code = line.replace(/\/\/.*$/, '').replace(/(["'`])(?:\\.|(?!\1).)*\1/g, '""');
    const decl = code.match(/^\s*const\s+(?:\[([^\]]*)\]|\{([^}]*)\}|([A-Za-z_$][\w$]*))\s*=/);
    const assign = code.match(/^\s*([A-Za-z_$][\w$]*)\s*(?:[-+*/%]|\*\*|<<|>>|&|\||\^|\?\?|&&|\|\|)?=(?!=)/);
    if (assign && !decl && !/^\s*(const|let|var)\b/.test(code)) {
      for (let d = scopes.length - 1; d >= 0; d--) { const at = scopes[d].get(assign[1]); if (at) { out.push({ line: n, name: assign[1], declared: at }); break; } }
    }
    for (const ch of code) { if (ch === '{') scopes.push(new Map()); else if (ch === '}' && scopes.length > 1) scopes.pop(); }
    if (decl) {
      const names = (decl[1] || decl[2] || decl[3]).split(',').map(x => x.trim().replace(/^\.\.\./, '').split(/[:=\s]/)[0]).filter(Boolean);
      for (const name of names) scopes[scopes.length - 1].set(name, n);
    }
  });
  return out;
}
